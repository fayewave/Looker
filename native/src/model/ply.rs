//! PLY (Stanford): a header naming elements and their typed properties, then the data as text or binary
//! (either byte order). Vertices give position and optionally normal, colour (`red green blue`, bytes or
//! floats) and texture coordinates (`s t`, `u v`, `texture_u texture_v`); faces give index lists, fanned into
//! triangles. MeshLab names the texture in a `comment TextureFile` line. Elements we don't use are read past.

use std::path::Path;
use std::sync::Arc;

use windows::Win32::Graphics::Imaging::IWICImagingFactory;

use super::{Image, Material, Mesh, Vertex, linear, normalized, soup};

#[derive(Clone, Copy, PartialEq, Debug)]
enum Ty {
    I8,
    U8,
    I16,
    U16,
    I32,
    U32,
    F32,
    F64,
}

impl Ty {
    fn parse(s: &str) -> Option<Ty> {
        Some(match s {
            "char" | "int8" => Ty::I8,
            "uchar" | "uint8" => Ty::U8,
            "short" | "int16" => Ty::I16,
            "ushort" | "uint16" => Ty::U16,
            "int" | "int32" => Ty::I32,
            "uint" | "uint32" => Ty::U32,
            "float" | "float32" => Ty::F32,
            "double" | "float64" => Ty::F64,
            _ => return None,
        })
    }

    fn size(self) -> usize {
        match self {
            Ty::I8 | Ty::U8 => 1,
            Ty::I16 | Ty::U16 => 2,
            Ty::I32 | Ty::U32 | Ty::F32 => 4,
            Ty::F64 => 8,
        }
    }
}

struct Prop {
    name: String,
    ty: Ty,
    /// A list property: the count's type.
    list: Option<Ty>,
}

struct Element {
    name: String,
    count: usize,
    props: Vec<Prop>,
}

#[derive(Clone, Copy, PartialEq)]
enum Encoding {
    Ascii,
    Little,
    Big,
}

/// Values in the order the data has them, as text words or binary.
struct Reader<'a> {
    data: &'a [u8],
    at: usize,
    enc: Encoding,
    words: Option<std::str::SplitAsciiWhitespace<'a>>,
}

impl<'a> Reader<'a> {
    fn value(&mut self, ty: Ty) -> Result<f64, String> {
        if let Some(w) = &mut self.words {
            return w.next().and_then(|s| s.parse().ok()).ok_or_else(|| "truncated".to_string());
        }
        let n = ty.size();
        let b = self.data.get(self.at..self.at + n).ok_or("truncated")?;
        self.at += n;
        let mut a = [0u8; 8];
        a[..n].copy_from_slice(b);
        if self.enc == Encoding::Big {
            a[..n].reverse();
        }
        Ok(match ty {
            Ty::I8 => a[0] as i8 as f64,
            Ty::U8 => a[0] as f64,
            Ty::I16 => i16::from_le_bytes([a[0], a[1]]) as f64,
            Ty::U16 => u16::from_le_bytes([a[0], a[1]]) as f64,
            Ty::I32 => i32::from_le_bytes([a[0], a[1], a[2], a[3]]) as f64,
            Ty::U32 => u32::from_le_bytes([a[0], a[1], a[2], a[3]]) as f64,
            Ty::F32 => f32::from_le_bytes([a[0], a[1], a[2], a[3]]) as f64,
            Ty::F64 => f64::from_le_bytes(a),
        })
    }
}

pub fn parse(f: &IWICImagingFactory, path: &Path, bytes: &[u8]) -> Result<Mesh, String> {
    if !bytes.starts_with(b"ply") {
        return Err("not a PLY file".into());
    }
    let end = bytes.windows(10).position(|w| w == b"end_header").ok_or("no end_header")?;
    let header = String::from_utf8_lossy(&bytes[..end]);
    let mut data_at = end + 10;
    // The header's last line ends in \n (or \r\n).
    while data_at < bytes.len() && bytes[data_at] != b'\n' {
        data_at += 1;
    }
    data_at += 1;
    let mut enc = Encoding::Ascii;
    let mut elements: Vec<Element> = Vec::new();
    let mut texture: Option<String> = None;
    for line in header.lines() {
        let w: Vec<&str> = line.split_whitespace().collect();
        match w.as_slice() {
            ["format", "ascii", ..] => enc = Encoding::Ascii,
            ["format", "binary_little_endian", ..] => enc = Encoding::Little,
            ["format", "binary_big_endian", ..] => enc = Encoding::Big,
            ["element", name, count] => elements.push(Element { name: name.to_string(), count: count.parse().map_err(|_| "bad count")?, props: Vec::new() }),
            ["property", "list", count_ty, ty, name] => {
                let p = Prop { name: name.to_string(), ty: Ty::parse(ty).ok_or("bad type")?, list: Some(Ty::parse(count_ty).ok_or("bad type")?) };
                elements.last_mut().ok_or("property before element")?.props.push(p);
            }
            ["property", ty, name] => {
                let p = Prop { name: name.to_string(), ty: Ty::parse(ty).ok_or("bad type")?, list: None };
                elements.last_mut().ok_or("property before element")?.props.push(p);
            }
            ["comment", "TextureFile", rest @ ..] => texture = Some(rest.join(" ")),
            _ => {}
        }
    }
    let body = &bytes[data_at.min(bytes.len())..];
    let text;
    let mut r = Reader { data: body, at: 0, enc, words: None };
    if enc == Encoding::Ascii {
        text = String::from_utf8_lossy(body);
        r.words = Some(text.split_ascii_whitespace());
    }

    let mut verts: Vec<Vertex> = Vec::new();
    let mut has_normals = false;
    let mut has_colours = false;
    let mut has_uv = false;
    let mut faces: Vec<u32> = Vec::new();
    let mut list = Vec::new();
    for el in &elements {
        let idx = |names: &[&str]| el.props.iter().position(|p| names.contains(&p.name.as_str()));
        let (x, y, z) = (idx(&["x"]), idx(&["y"]), idx(&["z"]));
        let (nx, ny, nz) = (idx(&["nx"]), idx(&["ny"]), idx(&["nz"]));
        let (cr, cg, cb) = (idx(&["red", "r", "diffuse_red"]), idx(&["green", "g", "diffuse_green"]), idx(&["blue", "b", "diffuse_blue"]));
        let (tu, tv) = (idx(&["s", "u", "texture_u", "texture_s"]), idx(&["t", "v", "texture_v", "texture_t"]));
        let face_list = idx(&["vertex_indices", "vertex_index"]);
        let is_vertex = el.name == "vertex";
        let is_face = el.name == "face";
        if is_vertex {
            has_normals = nx.is_some() && ny.is_some() && nz.is_some();
            has_colours = cr.is_some() && cg.is_some() && cb.is_some();
            has_uv = tu.is_some() && tv.is_some();
            verts.reserve(el.count);
        }
        let mut vals = vec![0f64; el.props.len()];
        for _ in 0..el.count {
            for (i, p) in el.props.iter().enumerate() {
                match p.list {
                    Some(count_ty) => {
                        let n = r.value(count_ty)? as usize;
                        list.clear();
                        for _ in 0..n {
                            list.push(r.value(p.ty)?);
                        }
                        if is_face && Some(i) == face_list && n >= 3 {
                            for k in 1..n - 1 {
                                faces.extend([list[0] as u32, list[k] as u32, list[k + 1] as u32]);
                            }
                        }
                    }
                    None => vals[i] = r.value(p.ty)?,
                }
            }
            if is_vertex {
                let g = |i: Option<usize>| i.map_or(0.0, |i| vals[i] as f32);
                // Colour bytes are sRGB 0..255; float colours 0..1.
                let c = |i: Option<usize>| {
                    let Some(i) = i else { return 1.0 };
                    let v = if el.props[i].ty == Ty::U8 { vals[i] as f32 / 255.0 } else { vals[i] as f32 };
                    linear(v.clamp(0.0, 1.0))
                };
                verts.push(Vertex {
                    pos: [g(x), g(y), g(z)],
                    normal: if has_normals { normalized([g(nx), g(ny), g(nz)]) } else { [0.0; 3] },
                    uv: [g(tu), g(tv)],
                    colour: if has_colours { [c(cr), c(cg), c(cb)] } else { [1.0; 3] },
                });
            }
        }
    }
    if faces.iter().any(|&i| i as usize >= verts.len()) {
        return Err("a face names a vertex that isn't there".into());
    }
    let map_kd = texture.filter(|_| has_uv).and_then(|t| crate::imaging::mtl::resolve(path.parent()?, &t)).and_then(|p| {
        let d = crate::imaging::decode_srgb(f, &p, super::TEXTURE_EDGE, super::TEXTURE_EDGE).ok()?;
        Some(Arc::new(Image { width: d.width, height: d.height, pixels: d.frames.into_iter().next()?.pixels }))
    });
    let mut material = Material::fallback(has_colours || map_kd.is_some());
    material.map_kd = map_kd;
    let (vertices, indices) = if has_normals {
        (verts, faces)
    } else {
        soup(faces.iter().map(|&i| verts[i as usize]).collect())
    };
    Ok(Mesh::single(vertices, indices, material))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wic() -> IWICImagingFactory {
        use windows::Win32::Graphics::Imaging::CLSID_WICImagingFactory2;
        use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx};
        unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
            CoCreateInstance(&CLSID_WICImagingFactory2, None, CLSCTX_INPROC_SERVER).unwrap()
        }
    }

    #[test]
    fn ascii_quad_with_colours() {
        let ply = "ply\nformat ascii 1.0\ncomment made by hand\nelement vertex 4\nproperty float x\nproperty float y\nproperty float z\n\
                   property uchar red\nproperty uchar green\nproperty uchar blue\nelement face 1\nproperty list uchar int vertex_indices\nend_header\n\
                   0 0 0 255 0 0\n1 0 0 0 255 0\n1 1 0 0 0 255\n0 1 0 255 255 255\n4 0 1 2 3\n";
        let m = parse(&wic(), Path::new("x.ply"), ply.as_bytes()).unwrap();
        assert_eq!(m.triangles(), 2);
        assert_eq!(m.vertices[0].colour, [1.0, 0.0, 0.0]);
        assert_eq!(m.vertices[0].normal, [0.0, 0.0, 1.0]);
    }

    #[test]
    fn binary_big_endian_skips_other_elements() {
        let mut b = b"ply\r\nformat binary_big_endian 1.0\r\nelement vertex 3\r\nproperty float x\r\nproperty float y\r\nproperty float z\r\n\
                      element face 1\r\nproperty list uchar uint vertex_indices\r\nelement edge 1\r\nproperty int a\r\nproperty int b\r\nend_header\r\n"
            .to_vec();
        for v in [0f32, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 0.0] {
            b.extend(v.to_be_bytes());
        }
        b.push(3);
        for i in [0u32, 1, 2] {
            b.extend(i.to_be_bytes());
        }
        b.extend([0u8; 8]);
        let m = parse(&wic(), Path::new("x.ply"), &b).unwrap();
        assert_eq!(m.triangles(), 1);
        assert_eq!(m.vertices[1].pos, [1.0, 0.0, 0.0]);
    }
}
