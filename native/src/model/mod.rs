//! 3D models: a Wavefront .obj with its .mtl materials becomes a [`Mesh`] (triangles, one part per material,
//! textures decoded), drawn by `render.rs` with Direct3D 11. The decode pool renders a still of it (thumbnails,
//! the strip, the first frame), and the window swaps in a live view to orbit once the mesh has loaded
//! (`app/model.rs`). One parse serves both: the last mesh loaded is kept (`load`).

pub mod render;

use std::collections::HashMap;
use std::os::windows::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use windows::Win32::Graphics::Imaging::IWICImagingFactory;

use crate::imaging::mtl;

/// Longest edge a model's texture is decoded at.
const TEXTURE_EDGE: u32 = 2048;

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Vertex {
    pub pos: [f32; 3],
    pub normal: [f32; 3],
    pub uv: [f32; 2],
    pub colour: [f32; 3],
}

/// Premultiplied sRGB BGRA, as the image decoders hand it over.
pub struct Image {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>,
}

pub struct Material {
    /// Linear diffuse colour and opacity.
    pub kd: [f32; 3],
    pub d: f32,
    pub ks: [f32; 3],
    pub ns: f32,
    pub ke: [f32; 3],
    pub map_kd: Option<Arc<Image>>,
    pub map_ke: Option<Arc<Image>>,
    /// Cut out where the diffuse map is transparent (foliage, fences) rather than blended.
    pub cutout: bool,
}

impl Material {
    fn fallback(vertex_colours: bool) -> Material {
        Material {
            kd: if vertex_colours { [1.0; 3] } else { [0.7; 3] },
            d: 1.0,
            ks: [0.15; 3],
            ns: 40.0,
            ke: [0.0; 3],
            map_kd: None,
            map_ke: None,
            cutout: false,
        }
    }

    pub fn transparent(&self) -> bool {
        self.d < 0.999
    }
}

/// `indices[start..start + count]` drawn with `materials[material]`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Part {
    pub start: u32,
    pub count: u32,
    pub material: usize,
}

pub struct Mesh {
    pub vertices: Vec<Vertex>,
    pub indices: Vec<u32>,
    pub parts: Vec<Part>,
    pub materials: Vec<Material>,
    pub min: [f32; 3],
    pub max: [f32; 3],
}

impl Mesh {
    pub fn triangles(&self) -> usize {
        self.indices.len() / 3
    }

    pub fn centre(&self) -> [f32; 3] {
        std::array::from_fn(|i| (self.min[i] + self.max[i]) / 2.0)
    }

    /// The bounding sphere's radius (half the box's diagonal).
    pub fn radius(&self) -> f32 {
        let d: f32 = (0..3).map(|i| (self.max[i] - self.min[i]).powi(2)).sum();
        (d.sqrt() / 2.0).max(1e-6)
    }
}

/// Which file a mesh came from, and its version (size and write time).
pub fn stamp(path: &Path) -> u64 {
    std::fs::metadata(path).map_or(0, |m| m.last_write_time() ^ m.len().rotate_left(32))
}

static LAST: Mutex<Option<(PathBuf, u64, Arc<Mesh>)>> = Mutex::new(None);

/// The model at `path`, parsed with its materials and textures: the one loaded last when it is the same file
/// (the still and the live view each ask), else from disk.
pub fn load(f: &IWICImagingFactory, path: &Path) -> Result<Arc<Mesh>, String> {
    let st = stamp(path);
    if let Some((p, s, m)) = &*LAST.lock().unwrap() {
        if p == path && *s == st {
            return Ok(m.clone());
        }
    }
    let t = std::time::Instant::now();
    let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
    let obj = parse_obj(&String::from_utf8_lossy(&bytes));
    if obj.mesh.indices.is_empty() {
        return Err("the file has no faces".into());
    }
    let mesh = Arc::new(finish(f, path, obj));
    crate::trace::mark(format!(
        "model: {} triangles, {} materials in {:.0} ms",
        mesh.triangles(),
        mesh.materials.len(),
        t.elapsed().as_secs_f64() * 1000.0
    ));
    *LAST.lock().unwrap() = Some((path.to_path_buf(), st, mesh.clone()));
    Ok(mesh)
}

/// A parsed .obj: the mesh with placeholder materials, plus what names them.
struct Obj {
    mesh: Mesh,
    libraries: Vec<String>,
    /// Each part's material name (`usemtl`), "" for none.
    names: Vec<String>,
    vertex_colours: bool,
}

fn index(word: &str, count: usize) -> Option<usize> {
    let i: i64 = word.parse().ok()?;
    let i = if i < 0 { count as i64 + i } else { i - 1 };
    (0..count as i64).contains(&i).then_some(i as usize)
}

fn normalized(v: [f32; 3]) -> [f32; 3] {
    let l = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
    if l > 0.0 { [v[0] / l, v[1] / l, v[2] / l] } else { [0.0, 1.0, 0.0] }
}

/// Parses the geometry: polygons fanned into triangles, negative (relative) indices, `usemtl` parts, `s`
/// smoothing groups for files without normals (smooth within a group, flat with `s off`), and vertex colours
/// (`v x y z r g b`). A file with neither normals nor `s` (scans, CAD exports) is smoothed by angle: faces
/// meeting at under `AUTO_SMOOTH` share a normal, sharper edges stay hard.
fn parse_obj(text: &str) -> Obj {
    let mut pos: Vec<[f32; 3]> = Vec::new();
    let mut cols: Vec<[f32; 3]> = Vec::new();
    let mut uvs: Vec<[f32; 2]> = Vec::new();
    let mut normals: Vec<[f32; 3]> = Vec::new();
    let mut vertices: Vec<Vertex> = Vec::new();
    // Per material name: its triangles' indices, in the order the names first appear.
    let mut groups: Vec<(String, Vec<u32>)> = vec![(String::new(), Vec::new())];
    let mut current = 0usize;
    let mut libraries = Vec::new();
    let mut smooth = 0u32;
    // (position, uv, normal, smoothing group) -> vertex.
    let mut seen: HashMap<(u32, u32, u32, u32), u32> = HashMap::new();
    // Smooth vertices without a normal in the file: summed face normals per (position, group).
    let mut sums: HashMap<(u32, u32), [f32; 3]> = HashMap::new();
    let mut smooth_vertices: Vec<(u32, (u32, u32))> = Vec::new();
    let mut corners: Vec<(usize, Option<usize>, Option<usize>)> = Vec::new();
    let mut vertex_colours = false;
    // Flat corners: (vertex, position, face normal), for smoothing by angle when the file has no `s` at all.
    let mut flat: Vec<(u32, u32, [f32; 3])> = Vec::new();
    let mut any_s = false;

    for line in text.lines() {
        let line = line.trim();
        let mut words = line.split_whitespace();
        let Some(key) = words.next() else { continue };
        match key {
            "v" => {
                let n: Vec<f32> = words.map_while(|w| w.parse().ok()).collect();
                if n.len() >= 3 {
                    pos.push([n[0], n[1], n[2]]);
                    // x y z r g b (some exporters write x y z w; a fourth value alone is w).
                    if n.len() >= 6 {
                        cols.push([n[3], n[4], n[5]]);
                        vertex_colours = true;
                    } else {
                        cols.push([1.0; 3]);
                    }
                }
            }
            "vt" => {
                let n: Vec<f32> = words.map_while(|w| w.parse().ok()).collect();
                uvs.push([n.first().copied().unwrap_or(0.0), n.get(1).copied().unwrap_or(0.0)]);
            }
            "vn" => {
                let n: Vec<f32> = words.map_while(|w| w.parse().ok()).collect();
                if n.len() >= 3 {
                    normals.push(normalized([n[0], n[1], n[2]]));
                }
            }
            "s" => {
                any_s = true;
                smooth = match words.next() {
                    Some("off") | None => 0,
                    Some(w) => w.parse().unwrap_or(1),
                };
            }
            "usemtl" => {
                let name = line[key.len()..].trim().to_string();
                current = match groups.iter().position(|(n, _)| *n == name) {
                    Some(i) => i,
                    None => {
                        groups.push((name, Vec::new()));
                        groups.len() - 1
                    }
                };
            }
            "mtllib" => libraries.push(line[key.len()..].trim().to_string()),
            "f" => {
                corners.clear();
                for w in words {
                    let mut it = w.split('/');
                    let Some(p) = it.next().and_then(|s| index(s, pos.len())) else { continue };
                    let t = it.next().filter(|s| !s.is_empty()).and_then(|s| index(s, uvs.len()));
                    let n = it.next().filter(|s| !s.is_empty()).and_then(|s| index(s, normals.len()));
                    corners.push((p, t, n));
                }
                if corners.len() < 3 {
                    continue;
                }
                let face_normal = {
                    // Newell's method: right for any planar polygon, concave or not.
                    let mut n = [0.0f32; 3];
                    for i in 0..corners.len() {
                        let a = pos[corners[i].0];
                        let b = pos[corners[(i + 1) % corners.len()].0];
                        n[0] += (a[1] - b[1]) * (a[2] + b[2]);
                        n[1] += (a[2] - b[2]) * (a[0] + b[0]);
                        n[2] += (a[0] - b[0]) * (a[1] + b[1]);
                    }
                    n
                };
                let mut ids = Vec::with_capacity(corners.len());
                for &(p, t, n) in &corners {
                    let uv = t.map_or([0.0; 2], |t| uvs[t]);
                    let id = match n {
                        Some(n) => *seen.entry((p as u32, t.map_or(u32::MAX, |t| t as u32), n as u32, 0)).or_insert_with(|| {
                            vertices.push(Vertex { pos: pos[p], normal: normals[n], uv, colour: cols[p] });
                            vertices.len() as u32 - 1
                        }),
                        None if smooth != 0 => {
                            let s = sums.entry((p as u32, smooth)).or_insert([0.0; 3]);
                            for k in 0..3 {
                                s[k] += face_normal[k];
                            }
                            *seen.entry((p as u32, t.map_or(u32::MAX, |t| t as u32), u32::MAX, smooth)).or_insert_with(|| {
                                vertices.push(Vertex { pos: pos[p], normal: [0.0; 3], uv, colour: cols[p] });
                                smooth_vertices.push((vertices.len() as u32 - 1, (p as u32, smooth)));
                                vertices.len() as u32 - 1
                            })
                        }
                        None => {
                            vertices.push(Vertex { pos: pos[p], normal: normalized(face_normal), uv, colour: cols[p] });
                            flat.push((vertices.len() as u32 - 1, p as u32, face_normal));
                            vertices.len() as u32 - 1
                        }
                    };
                    ids.push(id);
                }
                let out = &mut groups[current].1;
                for i in 1..ids.len() - 1 {
                    out.extend_from_slice(&[ids[0], ids[i], ids[i + 1]]);
                }
            }
            _ => {}
        }
    }
    for (v, key) in smooth_vertices {
        vertices[v as usize].normal = normalized(sums[&key]);
    }
    if !any_s {
        smooth_by_angle(&mut vertices, &mut flat);
    }
    let mut indices = Vec::new();
    let mut parts = Vec::new();
    let mut names = Vec::new();
    for (name, idx) in groups {
        if idx.is_empty() {
            continue;
        }
        parts.push(Part { start: indices.len() as u32, count: idx.len() as u32, material: names.len() });
        names.push(name);
        indices.extend(idx);
    }
    let (mut min, mut max) = ([f32::MAX; 3], [f32::MIN; 3]);
    for v in &vertices {
        for k in 0..3 {
            min[k] = min[k].min(v.pos[k]);
            max[k] = max[k].max(v.pos[k]);
        }
    }
    if vertices.is_empty() {
        (min, max) = ([0.0; 3], [0.0; 3]);
    }
    Obj { mesh: Mesh { vertices, indices, parts, materials: Vec::new(), min, max }, libraries, names, vertex_colours }
}

/// Faces meeting at less than this share their normals in a file without smoothing information.
const AUTO_SMOOTH: f32 = 40.0;

/// Each flat corner's normal becomes the (area-weighted) sum of the face normals at its position that lie
/// within `AUTO_SMOOTH` of its own face's.
fn smooth_by_angle(vertices: &mut [Vertex], flat: &mut [(u32, u32, [f32; 3])]) {
    let cos = AUTO_SMOOTH.to_radians().cos();
    flat.sort_unstable_by_key(|c| c.1);
    for group in flat.chunk_by(|a, b| a.1 == b.1) {
        let unit: Vec<[f32; 3]> = group.iter().map(|c| normalized(c.2)).collect();
        for (i, c) in group.iter().enumerate() {
            let mut n = [0.0f32; 3];
            for (j, d) in group.iter().enumerate() {
                if unit[i][0] * unit[j][0] + unit[i][1] * unit[j][1] + unit[i][2] * unit[j][2] >= cos {
                    for k in 0..3 {
                        n[k] += d.2[k];
                    }
                }
            }
            vertices[c.0 as usize].normal = normalized(n);
        }
    }
}

/// Reads the .mtl libraries the model names (else one beside it with its name) and gives each part its
/// material, textures decoded once each.
fn finish(f: &IWICImagingFactory, path: &Path, obj: Obj) -> Mesh {
    let dir = path.parent().unwrap_or(Path::new("."));
    let mut defs: Vec<mtl::Material> = Vec::new();
    let mut libs: Vec<PathBuf> = Vec::new();
    for l in &obj.libraries {
        // One name with spaces, else several names.
        match mtl::resolve(dir, l) {
            Some(p) => libs.push(p),
            None => libs.extend(l.split_whitespace().filter_map(|w| mtl::resolve(dir, w))),
        }
    }
    if libs.is_empty() {
        let beside = path.with_extension("mtl");
        if beside.is_file() {
            libs.push(beside);
        }
    }
    for lib in &libs {
        if let Ok(bytes) = std::fs::read(lib) {
            defs.extend(mtl::parse(&String::from_utf8_lossy(&bytes)));
        }
    }
    let mut images: HashMap<PathBuf, Option<Arc<Image>>> = HashMap::new();
    let mut image = |lib_dir: &Path, name: Option<&str>| -> Option<Arc<Image>> {
        let p = mtl::resolve(lib_dir, name?)?;
        images
            .entry(p.clone())
            .or_insert_with(|| match crate::imaging::decode_srgb(f, &p, TEXTURE_EDGE, TEXTURE_EDGE) {
                Ok(d) => {
                    let pixels = d.frames.into_iter().next()?.pixels;
                    Some(Arc::new(Image { width: d.width, height: d.height, pixels }))
                }
                Err(e) => {
                    crate::trace::mark(format!("model: texture {} failed: {e}", p.display()));
                    None
                }
            })
            .clone()
    };
    let lib_dir = libs.first().and_then(|l| l.parent()).unwrap_or(dir).to_path_buf();
    let mut mesh = obj.mesh;
    mesh.materials = obj
        .names
        .iter()
        .map(|name| {
            let Some(m) = defs.iter().find(|m| m.name == *name) else {
                return Material::fallback(obj.vertex_colours);
            };
            let map_kd = image(&lib_dir, m.map("map_Kd"));
            // A diffuse map with transparent texels (or an opacity map) cuts out; whole-surface opacity blends.
            let cutout = m.map("map_d").is_some() || map_kd.as_ref().is_some_and(|i| i.pixels.chunks_exact(4).any(|p| p[3] < 250));
            Material {
                kd: m.kd.unwrap_or(if obj.vertex_colours { [1.0; 3] } else { [0.8; 3] }),
                d: m.d.unwrap_or(1.0).clamp(0.0, 1.0),
                ks: m.ks.unwrap_or([0.0; 3]),
                ns: m.ns.unwrap_or(10.0).clamp(1.0, 1000.0),
                ke: m.ke.unwrap_or([0.0; 3]),
                map_ke: image(&lib_dir, m.map("map_Ke")),
                map_kd,
                cutout,
            }
        })
        .collect();
    mesh
}

#[cfg(test)]
mod tests {
    use super::*;

    const CUBE: &str = "mtllib cube.mtl\n\
        o Cube\n\
        v -1 -1 -1\nv 1 -1 -1\nv 1 1 -1\nv -1 1 -1\nv -1 -1 1\nv 1 -1 1\nv 1 1 1\nv -1 1 1\n\
        vt 0 0\nvt 1 0\nvt 1 1\nvt 0 1\n\
        usemtl Red\n\
        f 1/1 2/2 3/3 4/4\nf 5/1 8/4 7/3 6/2\n\
        usemtl Blue\n\
        f 1 5 6 2\nf -5 -1 -2 -6\n\
        usemtl Red\n\
        f 2 6 7 3\nf 1 4 8 5\n";

    #[test]
    fn quads_fan_into_triangles_grouped_by_material() {
        let o = parse_obj(CUBE);
        assert_eq!(o.mesh.triangles(), 12);
        assert_eq!(o.names, vec!["Red".to_string(), "Blue".to_string()]);
        assert_eq!(o.mesh.parts, vec![Part { start: 0, count: 24, material: 0 }, Part { start: 24, count: 12, material: 1 }]);
        assert_eq!(o.libraries, vec!["cube.mtl".to_string()]);
        assert_eq!((o.mesh.min, o.mesh.max), ([-1.0; 3], [1.0; 3]));
        // No normals, no `s`: every corner its own vertex; a cube's 90° edges stay hard under smoothing by
        // angle, so each keeps its face's normal (from the winding).
        assert_eq!(o.mesh.vertices.len(), 24);
        assert_eq!(o.mesh.vertices[0].normal, [0.0, 0.0, 1.0]);
    }

    #[test]
    fn smoothing_groups_share_normals_and_negative_indices_count_back() {
        let o = parse_obj("v 0 0 0\nv 1 0 0\nv 0 1 0\nv 0 0 1\ns 1\nf 1 2 3\nf 1 4 2\nf -1 -2 -3\n");
        assert_eq!(o.mesh.triangles(), 3);
        // Vertex 1 is shared by the first two faces: one vertex, a normal between theirs.
        let v1 = o.mesh.vertices[0];
        assert!(v1.normal[1] > 0.7 && v1.normal[2] > 0.7 && v1.normal[0].abs() < 1e-6, "{:?}", v1.normal);
        let last = &o.mesh.indices[6..9];
        assert_eq!(last.iter().map(|&i| o.mesh.vertices[i as usize].pos).collect::<Vec<_>>(), vec![[0.0, 0.0, 1.0], [0.0, 1.0, 0.0], [1.0, 0.0, 0.0]]);
    }

    #[test]
    fn shallow_creases_smooth_and_s_off_stays_flat() {
        // Two triangles folded 20° along the edge 1-2.
        let fold = "v 0 0 0
v 0 1 0
v 1 0 0
v -1 0 0.364
f 1 3 2
f 1 2 4
";
        let o = parse_obj(fold);
        let at_origin: Vec<[f32; 3]> = o.mesh.vertices.iter().filter(|v| v.pos == [0.0; 3]).map(|v| v.normal).collect();
        assert_eq!(at_origin.len(), 2);
        assert!((at_origin[0][0] - at_origin[1][0]).abs() < 1e-5, "{at_origin:?}");
        let o = parse_obj(&format!("s off
{fold}"));
        let at_origin: Vec<[f32; 3]> = o.mesh.vertices.iter().filter(|v| v.pos == [0.0; 3]).map(|v| v.normal).collect();
        assert!((at_origin[0][0] - at_origin[1][0]).abs() > 0.1, "{at_origin:?}");
    }

    #[test]
    fn vertex_colours_and_given_normals() {
        let o = parse_obj("v 0 0 0 1 0 0\nv 1 0 0 0 1 0\nv 0 1 0 0 0 1\nvn 0 0 2\nf 1//1 2//1 3//1\n");
        assert!(o.vertex_colours);
        assert_eq!(o.mesh.vertices[1].colour, [0.0, 1.0, 0.0]);
        assert_eq!(o.mesh.vertices[2].normal, [0.0, 0.0, 1.0]);
    }
}
