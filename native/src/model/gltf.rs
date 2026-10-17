//! glTF 2.0, as JSON (`.gltf`, buffers and images beside it or in data URIs) or binary (`.glb`: the JSON and
//! one buffer in a single file). The default scene's node tree is walked with its transforms; each triangle
//! primitive becomes a part. Accessors of any component type are read (quantized meshes too); Draco and
//! meshopt compression aren't. PBR materials are approximated for the Blinn-Phong shader: base colour (and
//! map) as diffuse, metallic and roughness as the highlight's colour and tightness, emission, and the alpha
//! modes (MASK cuts out, BLEND blends). Metres, Y-up, as Looker shows everything.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use serde_json::Value;
use windows::Win32::Graphics::Imaging::IWICImagingFactory;

use super::{Image, Material, Mesh, Part, TEXTURE_EDGE, Vertex, bounds, normalized, soup};

/// Column-major 4x4, as glTF writes it.
type Mat4 = [f32; 16];
const IDENTITY: Mat4 = [1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0];

fn mul(a: &Mat4, b: &Mat4) -> Mat4 {
    std::array::from_fn(|i| {
        let (col, row) = (i / 4, i % 4);
        (0..4).map(|k| a[k * 4 + row] * b[col * 4 + k]).sum()
    })
}

fn floats(v: &Value) -> Vec<f32> {
    v.as_array().map(|a| a.iter().filter_map(|x| x.as_f64()).map(|x| x as f32).collect()).unwrap_or_default()
}

/// A node's own transform: `matrix`, else translation x rotation x scale.
fn local(node: &Value) -> Mat4 {
    if let Ok(m) = <[f32; 16]>::try_from(floats(&node["matrix"])) {
        return m;
    }
    let t = <[f32; 3]>::try_from(floats(&node["translation"])).unwrap_or([0.0; 3]);
    let [x, y, z, w] = <[f32; 4]>::try_from(floats(&node["rotation"])).unwrap_or([0.0, 0.0, 0.0, 1.0]);
    let s = <[f32; 3]>::try_from(floats(&node["scale"])).unwrap_or([1.0; 3]);
    let r = [
        [1.0 - 2.0 * (y * y + z * z), 2.0 * (x * y - z * w), 2.0 * (x * z + y * w)],
        [2.0 * (x * y + z * w), 1.0 - 2.0 * (x * x + z * z), 2.0 * (y * z - x * w)],
        [2.0 * (x * z - y * w), 2.0 * (y * z + x * w), 1.0 - 2.0 * (x * x + y * y)],
    ];
    let mut m = IDENTITY;
    for col in 0..3 {
        for row in 0..3 {
            m[col * 4 + row] = r[row][col] * s[col];
        }
    }
    m[12..15].copy_from_slice(&t);
    m
}

fn point(m: &Mat4, p: [f32; 3]) -> [f32; 3] {
    std::array::from_fn(|r| m[r] * p[0] + m[4 + r] * p[1] + m[8 + r] * p[2] + m[12 + r])
}

/// Normals transform by the inverse transpose; the cofactor matrix is that times the determinant, which the
/// normalising divides out (a mirroring flips them, which the two-sided shader doesn't mind).
fn normal(m: &Mat4, n: [f32; 3]) -> [f32; 3] {
    let l = |r: usize, c: usize| m[c * 4 + r];
    let cof = |r: usize, c: usize| {
        let (r1, r2) = ((r + 1) % 3, (r + 2) % 3);
        let (c1, c2) = ((c + 1) % 3, (c + 2) % 3);
        l(r1, c1) * l(r2, c2) - l(r1, c2) * l(r2, c1)
    };
    normalized(std::array::from_fn(|r| (0..3).map(|c| cof(r, c) * n[c]).sum()))
}

fn base64(s: &str) -> Vec<u8> {
    let val = |c: u8| match c {
        b'A'..=b'Z' => Some(c - b'A'),
        b'a'..=b'z' => Some(c - b'a' + 26),
        b'0'..=b'9' => Some(c - b'0' + 52),
        b'+' | b'-' => Some(62),
        b'/' | b'_' => Some(63),
        _ => None,
    };
    let mut out = Vec::with_capacity(s.len() * 3 / 4);
    let (mut acc, mut bits) = (0u32, 0);
    for v in s.bytes().filter_map(val) {
        acc = acc << 6 | v as u32;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
    }
    out
}

fn percent_decoded(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

enum Source {
    Bytes(Vec<u8>),
    File(std::path::PathBuf),
}

fn uri(dir: &Path, uri: &str) -> Source {
    match uri.strip_prefix("data:") {
        Some(data) => Source::Bytes(base64(data.split_once(',').map_or("", |(_, d)| d))),
        None => Source::File(dir.join(percent_decoded(uri))),
    }
}

/// The JSON chunk and the binary chunk of a .glb.
fn glb(b: &[u8]) -> Result<(&[u8], Option<&[u8]>), String> {
    let u = |o: usize| b.get(o..o + 4).map(|s| u32::from_le_bytes([s[0], s[1], s[2], s[3]]) as usize);
    let mut at = 12;
    let (mut json, mut bin) = (None, None);
    while let (Some(len), Some(kind)) = (u(at), u(at + 4)) {
        let chunk = b.get(at + 8..at + 8 + len).ok_or("truncated GLB")?;
        match kind {
            0x4E4F_534A => json = json.or(Some(chunk)),
            0x004E_4942 => bin = bin.or(Some(chunk)),
            _ => {}
        }
        at += 8 + len;
    }
    Ok((json.ok_or("GLB without JSON")?, bin))
}

struct Gltf<'a> {
    doc: &'a Value,
    buffers: Vec<Vec<u8>>,
}

impl Gltf<'_> {
    /// The bytes of buffer view `i`.
    fn view(&self, i: usize) -> Result<(&[u8], usize), String> {
        let v = &self.doc["bufferViews"][i];
        let buf = self.buffers.get(v["buffer"].as_u64().unwrap_or(0) as usize).ok_or("missing buffer")?;
        let off = v["byteOffset"].as_u64().unwrap_or(0) as usize;
        let len = v["byteLength"].as_u64().unwrap_or(0) as usize;
        let bytes = buf.get(off..off + len).ok_or("buffer view out of range")?;
        Ok((bytes, v["byteStride"].as_u64().unwrap_or(0) as usize))
    }

    /// Accessor `i`'s values, `comps` per element, as numbers (normalised integers scaled to 0..1 or -1..1).
    fn read(&self, i: usize) -> Result<(Vec<f64>, usize), String> {
        let a = &self.doc["accessors"][i];
        let count = a["count"].as_u64().unwrap_or(0) as usize;
        let comps = match a["type"].as_str().unwrap_or("") {
            "SCALAR" => 1,
            "VEC2" => 2,
            "VEC3" => 3,
            "VEC4" => 4,
            "MAT4" => 16,
            t => return Err(format!("accessor type {t}")),
        };
        let ty = a["componentType"].as_u64().unwrap_or(0);
        let size = match ty {
            5120 | 5121 => 1,
            5122 | 5123 => 2,
            5125 | 5126 => 4,
            _ => return Err(format!("component type {ty}")),
        };
        let Some(view) = a["bufferView"].as_u64() else { return Ok((vec![0.0; count * comps], comps)) };
        let (bytes, stride) = self.view(view as usize)?;
        let stride = if stride == 0 { comps * size } else { stride };
        let base = a["byteOffset"].as_u64().unwrap_or(0) as usize;
        let norm = a["normalized"].as_bool().unwrap_or(false);
        if count > 0 && base + (count - 1) * stride + comps * size > bytes.len() {
            return Err("accessor out of range".into());
        }
        let mut out = Vec::with_capacity(count * comps);
        for e in 0..count {
            for c in 0..comps {
                let o = base + e * stride + c * size;
                let b = &bytes[o..o + size];
                let v = match ty {
                    5120 => b[0] as i8 as f64 / if norm { 127.0 } else { 1.0 },
                    5121 => b[0] as f64 / if norm { 255.0 } else { 1.0 },
                    5122 => i16::from_le_bytes([b[0], b[1]]) as f64 / if norm { 32767.0 } else { 1.0 },
                    5123 => u16::from_le_bytes([b[0], b[1]]) as f64 / if norm { 65535.0 } else { 1.0 },
                    5125 => u32::from_le_bytes([b[0], b[1], b[2], b[3]]) as f64,
                    _ => f32::from_le_bytes([b[0], b[1], b[2], b[3]]) as f64,
                };
                out.push(if norm { v.max(-1.0) } else { v });
            }
        }
        Ok((out, comps))
    }
}

pub fn parse(f: &IWICImagingFactory, path: &Path, bytes: &[u8]) -> Result<Mesh, String> {
    let (json, bin) = if bytes.starts_with(b"glTF") { glb(bytes)? } else { (bytes, None) };
    let doc: Value = serde_json::from_slice(json).map_err(|e| format!("glTF: {e}"))?;
    for e in doc["extensionsRequired"].as_array().into_iter().flatten().filter_map(|e| e.as_str()) {
        if matches!(e, "KHR_draco_mesh_compression" | "EXT_meshopt_compression") {
            return Err(format!("compressed glTF ({e}) isn't supported"));
        }
    }
    let dir = path.parent().unwrap_or(Path::new("."));
    let mut buffers = Vec::new();
    for b in doc["buffers"].as_array().into_iter().flatten() {
        buffers.push(match b["uri"].as_str() {
            Some(u) => match uri(dir, u) {
                Source::Bytes(b) => b,
                Source::File(p) => std::fs::read(&p).map_err(|e| format!("{}: {e}", p.display()))?,
            },
            None => bin.ok_or("buffer without data")?.to_vec(),
        });
    }
    let g = Gltf { doc: &doc, buffers };

    // Textures, decoded once per image.
    let mut images: HashMap<usize, Option<Arc<Image>>> = HashMap::new();
    let mut texture = |info: &Value| -> Option<Arc<Image>> {
        let t = &doc["textures"][info["index"].as_u64()? as usize];
        let src = t["source"].as_u64().or_else(|| t["extensions"]["EXT_texture_webp"]["source"].as_u64())? as usize;
        images
            .entry(src)
            .or_insert_with(|| {
                let img = &doc["images"][src];
                let decoded = match (img["uri"].as_str(), img["bufferView"].as_u64()) {
                    (Some(u), _) => match uri(dir, u) {
                        Source::Bytes(b) => crate::imaging::decode_memory_srgb(f, &b, TEXTURE_EDGE, TEXTURE_EDGE),
                        Source::File(p) => crate::imaging::decode_srgb(f, &p, TEXTURE_EDGE, TEXTURE_EDGE),
                    },
                    (None, Some(v)) => g.view(v as usize).and_then(|(b, _)| crate::imaging::decode_memory_srgb(f, b, TEXTURE_EDGE, TEXTURE_EDGE)),
                    _ => Err("image without data".into()),
                };
                match decoded {
                    Ok(d) => Some(Arc::new(Image { width: d.width, height: d.height, pixels: d.frames.into_iter().next()?.pixels })),
                    Err(e) => {
                        crate::trace::mark(format!("glTF: image {src} failed: {e}"));
                        None
                    }
                }
            })
            .clone()
    };
    let mut materials: Vec<Material> = doc["materials"].as_array().into_iter().flatten().map(|m| material(m, &mut texture)).collect();
    let default_material = materials.len();
    materials.push(Material::fallback(false));

    // The scene's nodes, with their world transforms.
    let nodes = doc["nodes"].as_array().map_or(&[][..], |v| v.as_slice());
    let scene = doc["scene"].as_u64().unwrap_or(0) as usize;
    let mut roots: Vec<usize> = doc["scenes"][scene]["nodes"].as_array().into_iter().flatten().filter_map(|n| n.as_u64()).map(|n| n as usize).collect();
    if roots.is_empty() {
        let children: Vec<usize> = nodes.iter().flat_map(|n| n["children"].as_array().into_iter().flatten().filter_map(|c| c.as_u64())).map(|c| c as usize).collect();
        roots = (0..nodes.len()).filter(|i| !children.contains(i)).collect();
    }
    let mut stack: Vec<(usize, Mat4, u32)> = roots.into_iter().map(|n| (n, IDENTITY, 0)).collect();
    let mut vertices: Vec<Vertex> = Vec::new();
    let mut indices: Vec<u32> = Vec::new();
    let mut parts: Vec<Part> = Vec::new();
    while let Some((n, parent, depth)) = stack.pop() {
        let Some(node) = nodes.get(n) else { continue };
        let m = mul(&parent, &local(node));
        if depth < 64 {
            for c in node["children"].as_array().into_iter().flatten().filter_map(|c| c.as_u64()) {
                stack.push((c as usize, m, depth + 1));
            }
        }
        let Some(mesh) = node["mesh"].as_u64() else { continue };
        for prim in doc["meshes"][mesh as usize]["primitives"].as_array().into_iter().flatten() {
            match primitive(&g, prim, &m) {
                Ok(Some((vs, is))) => {
                    let material = prim["material"].as_u64().map_or(default_material, |i| (i as usize).min(default_material));
                    let base = vertices.len() as u32;
                    parts.push(Part { start: indices.len() as u32, count: is.len() as u32, material });
                    indices.extend(is.iter().map(|i| i + base));
                    vertices.extend(vs);
                }
                Ok(None) => {}
                Err(e) => crate::trace::mark(format!("glTF: primitive skipped: {e}")),
            }
        }
    }
    let (min, max) = bounds(&vertices);
    Ok(Mesh { vertices, indices, parts, materials, min, max })
}

/// One primitive's triangles in world space, or None for points and lines.
fn primitive(g: &Gltf, prim: &Value, m: &Mat4) -> Result<Option<(Vec<Vertex>, Vec<u32>)>, String> {
    let mode = prim["mode"].as_u64().unwrap_or(4);
    if !matches!(mode, 4..=6) {
        return Ok(None);
    }
    let attr = |name: &str| prim["attributes"][name].as_u64().map(|i| i as usize);
    let (pos, _) = g.read(attr("POSITION").ok_or("no positions")?)?;
    let count = pos.len() / 3;
    let normals = attr("NORMAL").map(|i| g.read(i)).transpose()?;
    let uvs = attr("TEXCOORD_0").map(|i| g.read(i)).transpose()?;
    let colours = attr("COLOR_0").map(|i| g.read(i)).transpose()?;
    let mut verts: Vec<Vertex> = (0..count)
        .map(|i| {
            let p = [pos[i * 3] as f32, pos[i * 3 + 1] as f32, pos[i * 3 + 2] as f32];
            let mut v = Vertex { pos: point(m, p), colour: [1.0; 3], ..Default::default() };
            if let Some((n, _)) = &normals {
                v.normal = normal(m, [n[i * 3] as f32, n[i * 3 + 1] as f32, n[i * 3 + 2] as f32]);
            }
            if let Some((t, _)) = &uvs {
                // glTF's v runs down the image; the shader flips OBJ's, which runs up.
                v.uv = [t[i * 2] as f32, 1.0 - t[i * 2 + 1] as f32];
            }
            if let Some((c, k)) = &colours {
                v.colour = [c[i * k] as f32, c[i * k + 1] as f32, c[i * k + 2] as f32];
            }
            v
        })
        .collect();
    let list: Vec<u32> = match prim["indices"].as_u64() {
        Some(i) => g.read(i as usize)?.0.into_iter().map(|v| v as u32).collect(),
        None => (0..count as u32).collect(),
    };
    if list.iter().any(|&i| i as usize >= count) {
        return Err("index out of range".into());
    }
    let tris: Vec<u32> = match mode {
        5 => (2..list.len()).flat_map(|k| if k % 2 == 0 { [list[k - 2], list[k - 1], list[k]] } else { [list[k - 1], list[k - 2], list[k]] }).collect(),
        6 => (2..list.len()).flat_map(|k| [list[0], list[k - 1], list[k]]).collect(),
        _ => list[..list.len() / 3 * 3].to_vec(),
    };
    if normals.is_none() {
        verts = tris.iter().map(|&i| verts[i as usize]).collect();
        return Ok(Some(soup(verts)));
    }
    Ok(Some((verts, tris)))
}

/// A PBR material approximated for the Blinn-Phong shader.
fn material(m: &Value, texture: &mut impl FnMut(&Value) -> Option<Arc<Image>>) -> Material {
    let pbr = &m["pbrMetallicRoughness"];
    let sg = &m["extensions"]["KHR_materials_pbrSpecularGlossiness"];
    let (base, map_kd, metal, rough) = if sg.is_object() {
        let base = <[f32; 4]>::try_from(floats(&sg["diffuseFactor"])).unwrap_or([1.0; 4]);
        (base, texture(&sg["diffuseTexture"]), 0.0, 1.0 - sg["glossinessFactor"].as_f64().unwrap_or(1.0) as f32)
    } else {
        let base = <[f32; 4]>::try_from(floats(&pbr["baseColorFactor"])).unwrap_or([1.0; 4]);
        let metal = pbr["metallicFactor"].as_f64().unwrap_or(1.0) as f32;
        let rough = pbr["roughnessFactor"].as_f64().unwrap_or(1.0) as f32;
        // The metallic-roughness map isn't read: where there is one, the factor is mostly a ceiling, and
        // taking it at face value turns whole models into dark mirrors.
        let metal = if pbr["metallicRoughnessTexture"].is_object() { metal * 0.25 } else { metal };
        (base, texture(&pbr["baseColorTexture"]), metal, rough)
    };
    let shine = (1.0 - rough).clamp(0.0, 1.0).powi(2);
    let mode = m["alphaMode"].as_str().unwrap_or("OPAQUE");
    let ke = <[f32; 3]>::try_from(floats(&m["emissiveFactor"])).unwrap_or([0.0; 3]);
    Material {
        kd: std::array::from_fn(|i| base[i] * (1.0 - 0.7 * metal)),
        d: if mode == "BLEND" { base[3] } else { 1.0 },
        ks: std::array::from_fn(|i| ((0.04 + (base[i] - 0.04) * metal) * (0.2 + 1.6 * shine)).min(1.0)),
        ns: (2.0 / rough.powi(4).max(2e-3) - 2.0).clamp(2.0, 512.0),
        ke,
        map_ke: texture(&m["emissiveTexture"]),
        cutout: mode == "MASK" && map_kd.is_some(),
        cutoff: m["alphaCutoff"].as_f64().unwrap_or(0.5) as f32,
        blend: mode == "BLEND",
        map_kd,
    }
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

    /// A triangle in a data-URI buffer (three float positions, three u16 indices), under a node that moves it.
    fn triangle_gltf() -> String {
        let mut b = Vec::new();
        for v in [0f32, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 0.0] {
            b.extend(v.to_le_bytes());
        }
        for i in [0u16, 1, 2] {
            b.extend(i.to_le_bytes());
        }
        b.extend([0, 0]);
        const A: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut enc = String::new();
        for c in b.chunks(3) {
            let n = (c[0] as u32) << 16 | (*c.get(1).unwrap_or(&0) as u32) << 8 | *c.get(2).unwrap_or(&0) as u32;
            for k in 0..=c.len() {
                enc.push(A[(n >> (18 - 6 * k) & 63) as usize] as char);
            }
        }
        format!(
            r#"{{"asset":{{"version":"2.0"}},"scene":0,"scenes":[{{"nodes":[0]}}],
            "nodes":[{{"translation":[5,0,0],"children":[1]}},{{"mesh":0,"scale":[2,2,2]}}],
            "meshes":[{{"primitives":[{{"attributes":{{"POSITION":0}},"indices":1}}]}}],
            "buffers":[{{"byteLength":44,"uri":"data:application/octet-stream;base64,{enc}"}}],
            "bufferViews":[{{"buffer":0,"byteOffset":0,"byteLength":36}},{{"buffer":0,"byteOffset":36,"byteLength":6}}],
            "accessors":[{{"bufferView":0,"componentType":5126,"count":3,"type":"VEC3"}},{{"bufferView":1,"componentType":5123,"count":3,"type":"SCALAR"}}]}}"#
        )
    }

    #[test]
    fn walks_nodes_with_their_transforms() {
        let m = parse(&wic(), Path::new("x.gltf"), triangle_gltf().as_bytes()).unwrap();
        assert_eq!(m.triangles(), 1);
        let xs: Vec<f32> = m.vertices.iter().map(|v| v.pos[0]).collect();
        assert_eq!(xs, vec![5.0, 7.0, 5.0]);
        assert_eq!(m.max, [7.0, 2.0, 0.0]);
        // No normals in the file: from the winding.
        assert_eq!(m.vertices[0].normal, [0.0, 0.0, 1.0]);
    }

    #[test]
    fn glb_chunks_and_rotations() {
        let json = br#"{"asset":{"version":"2.0"}}"#;
        let mut b = b"glTF".to_vec();
        b.extend(2u32.to_le_bytes());
        b.extend(((12 + 8 + json.len()) as u32).to_le_bytes());
        b.extend((json.len() as u32).to_le_bytes());
        b.extend(0x4E4F_534Au32.to_le_bytes());
        b.extend(json);
        assert_eq!(glb(&b).unwrap().0, json);
        // A quarter turn about +Y takes +X to -Z.
        let s = std::f32::consts::FRAC_1_SQRT_2;
        let r = local(&serde_json::json!({"rotation": [0.0, s, 0.0, s]}));
        let p = point(&r, [1.0, 0.0, 0.0]);
        assert!((p[0]).abs() < 1e-6 && (p[2] + 1.0).abs() < 1e-6, "{p:?}");
        assert_eq!(base64("aGVsbG8="), b"hello");
        assert_eq!(percent_decoded("my%20model.bin"), "my model.bin");
    }
}
