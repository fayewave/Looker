//! 3MF: a zip with the model as XML (`3D/3dmodel.model`, or wherever `_rels/.rels` points). Objects are meshes
//! (`vertex`, `triangle`) or assemblies of other objects (`component`, with a transform; the production
//! extension's `p:path` takes them from another part of the zip, as Bambu Studio and PrusaSlicer write them).
//! The `build` items place objects. Colours come from `basematerials` and the materials extension's
//! `colorgroup`, per triangle or per object. Millimetres, Z-up.

use std::collections::HashMap;
use std::io::Read;

use super::{Material, Mesh, Vertex, linear, soup, z_up};

/// A tag's local name (namespace prefix dropped), its attributes' text, and whether it closes (`</x>`).
struct Tag<'a> {
    name: &'a str,
    attrs: &'a str,
    close: bool,
}

fn tags(xml: &str) -> impl Iterator<Item = Tag<'_>> {
    let mut rest = xml;
    std::iter::from_fn(move || {
        loop {
            let start = rest.find('<')?;
            let end = start + rest[start..].find('>')?;
            let inner = &rest[start + 1..end];
            rest = &rest[end + 1..];
            if inner.starts_with('?') || inner.starts_with('!') {
                continue;
            }
            let close = inner.starts_with('/');
            let inner = inner.trim_start_matches('/').trim_end_matches('/');
            let split = inner.find(char::is_whitespace).unwrap_or(inner.len());
            let full = &inner[..split];
            let name = full.rsplit(':').next().unwrap_or(full);
            return Some(Tag { name, attrs: &inner[split..], close });
        }
    })
}

/// An attribute by local name (any prefix).
fn attr<'a>(attrs: &'a str, name: &str) -> Option<&'a str> {
    let mut rest = attrs;
    while let Some(eq) = rest.find('=') {
        let key = rest[..eq].trim();
        let key = key.rsplit(':').next().unwrap_or(key);
        let after = rest[eq + 1..].trim_start();
        let q = after.chars().next()?;
        let close = after[1..].find(q)? + 1;
        if key == name {
            return Some(&after[1..close]);
        }
        rest = &after[close + 1..];
    }
    None
}

fn num<T: std::str::FromStr>(attrs: &str, name: &str) -> Option<T> {
    attr(attrs, name)?.trim().parse().ok()
}

/// "#RRGGBB" or "#RRGGBBAA", in linear light.
fn colour(hex: &str) -> Option<[f32; 3]> {
    let h = hex.trim().trim_start_matches('#');
    let c = |i: usize| u8::from_str_radix(h.get(i..i + 2)?, 16).ok().map(|v| linear(v as f32 / 255.0));
    Some([c(0)?, c(2)?, c(4)?])
}

/// A 3MF transform: 12 numbers, row vectors ([x y z 1] times a 4x3 matrix).
type Affine = [f32; 12];
const IDENTITY: Affine = [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0];

fn affine(attrs: &str) -> Affine {
    let v: Vec<f32> = attr(attrs, "transform").map(|t| t.split_whitespace().filter_map(|s| s.parse().ok()).collect()).unwrap_or_default();
    v.try_into().unwrap_or(IDENTITY)
}

fn apply(m: &Affine, p: [f32; 3]) -> [f32; 3] {
    std::array::from_fn(|j| p[0] * m[j] + p[1] * m[3 + j] + p[2] * m[6 + j] + m[9 + j])
}

/// `a` then `b`.
fn then(a: &Affine, b: &Affine) -> Affine {
    let mut out = [0.0; 12];
    for r in 0..4 {
        for j in 0..3 {
            let base = if r == 3 { b[9 + j] } else { 0.0 };
            out[r * 3 + j] = (0..3).map(|k| a[r * 3 + k] * b[k * 3 + j]).sum::<f32>() + base;
        }
    }
    out
}

#[derive(Default)]
struct Object {
    vertices: Vec<[f32; 3]>,
    /// Corner indices and the triangle's colour, if any.
    triangles: Vec<([u32; 3], Option<[f32; 3]>)>,
    /// (part path or "" for this one, object id, transform).
    components: Vec<(String, u32, Affine)>,
}

/// The objects of one model part, by id, and its build items.
fn read_model(xml: &str) -> (HashMap<u32, Object>, Vec<(u32, Affine)>) {
    let mut objects = HashMap::new();
    let mut build = Vec::new();
    // Colour groups by id: basematerials' displaycolor, colorgroup's color.
    let mut groups: HashMap<u32, Vec<[f32; 3]>> = HashMap::new();
    let mut group: Option<u32> = None;
    let mut current: Option<(u32, Object, Option<[f32; 3]>)> = None;
    for t in tags(xml) {
        match (t.name, t.close) {
            ("basematerials" | "colorgroup", false) => {
                group = num(t.attrs, "id");
                if let Some(g) = group {
                    groups.entry(g).or_default();
                }
            }
            ("basematerials" | "colorgroup", true) => group = None,
            ("base" | "color", false) => {
                if let (Some(g), Some(c)) = (group, attr(t.attrs, "displaycolor").or(attr(t.attrs, "color")).and_then(colour)) {
                    groups.entry(g).or_default().push(c);
                }
            }
            ("object", false) => {
                let id = num(t.attrs, "id").unwrap_or(0);
                let c = num::<u32>(t.attrs, "pid").and_then(|g| groups.get(&g)?.get(num::<usize>(t.attrs, "pindex").unwrap_or(0)).copied());
                current = Some((id, Object::default(), c));
            }
            ("object", true) => {
                if let Some((id, o, _)) = current.take() {
                    objects.insert(id, o);
                }
            }
            ("vertex", false) => {
                if let Some((_, o, _)) = &mut current {
                    o.vertices.push([num(t.attrs, "x").unwrap_or(0.0), num(t.attrs, "y").unwrap_or(0.0), num(t.attrs, "z").unwrap_or(0.0)]);
                }
            }
            ("triangle", false) => {
                if let Some((_, o, object_colour)) = &mut current {
                    let (Some(a), Some(b), Some(c)) = (num(t.attrs, "v1"), num(t.attrs, "v2"), num(t.attrs, "v3")) else { continue };
                    let own = num::<u32>(t.attrs, "pid").and_then(|g| groups.get(&g)?.get(num::<usize>(t.attrs, "p1").unwrap_or(0)).copied());
                    o.triangles.push(([a, b, c], own.or(*object_colour)));
                }
            }
            ("component", false) => {
                if let Some((_, o, _)) = &mut current {
                    let path = attr(t.attrs, "path").unwrap_or("").trim_start_matches('/').to_string();
                    o.components.push((path, num(t.attrs, "objectid").unwrap_or(0), affine(t.attrs)));
                }
            }
            ("item", false) => build.push((num(t.attrs, "objectid").unwrap_or(0), affine(t.attrs))),
            _ => {}
        }
    }
    (objects, build)
}

pub fn parse(bytes: &[u8]) -> Result<Mesh, String> {
    let mut zip = zip::ZipArchive::new(std::io::Cursor::new(bytes)).map_err(|e| e.to_string())?;
    let mut read = |name: &str| -> Option<String> {
        let mut s = String::new();
        zip.by_name(name).ok()?.read_to_string(&mut s).ok()?;
        Some(s)
    };
    // The root model: as `_rels/.rels` says, else the usual place.
    let root = read("_rels/.rels")
        .and_then(|rels| {
            tags(&rels).find(|t| t.name == "Relationship" && attr(t.attrs, "Type").is_some_and(|ty| ty.ends_with("/3dmodel"))).and_then(|t| attr(t.attrs, "Target")).map(|s| s.trim_start_matches('/').to_string())
        })
        .unwrap_or_else(|| "3D/3dmodel.model".to_string());
    let mut parts: HashMap<String, HashMap<u32, Object>> = HashMap::new();
    let (objects, build) = read_model(&read(&root).ok_or("no 3D model in the package")?);
    parts.insert(String::new(), objects);
    // Parts the components point into.
    let wanted: Vec<String> = parts[""].values().flat_map(|o| o.components.iter().map(|c| c.0.clone())).filter(|p| !p.is_empty()).collect();
    for p in wanted {
        if !parts.contains_key(&p) {
            if let Some(xml) = read(&p) {
                parts.insert(p, read_model(&xml).0);
            }
        }
    }

    let mut corners: Vec<Vertex> = Vec::new();
    let mut coloured = false;
    // Objects to place: (part, id, transform), from the build (or every object when it is empty).
    let mut stack: Vec<(String, u32, Affine, u32)> = if build.is_empty() {
        parts[""].keys().map(|&id| (String::new(), id, IDENTITY, 0)).collect()
    } else {
        build.into_iter().map(|(id, m)| (String::new(), id, m, 0)).collect()
    };
    while let Some((part, id, m, depth)) = stack.pop() {
        let Some(o) = parts.get(&part).and_then(|p| p.get(&id)) else { continue };
        for (path, child, cm) in &o.components {
            // A component in "" means the same part as its parent.
            let in_part = if path.is_empty() { part.clone() } else { path.clone() };
            if depth < 16 {
                stack.push((in_part, *child, then(cm, &m), depth + 1));
            }
        }
        for (tri, c) in &o.triangles {
            if tri.iter().any(|&i| i as usize >= o.vertices.len()) {
                continue;
            }
            coloured |= c.is_some();
            for &i in tri {
                corners.push(Vertex { pos: z_up(apply(&m, o.vertices[i as usize])), colour: c.unwrap_or([1.0; 3]), ..Default::default() });
            }
        }
    }
    if corners.is_empty() {
        return Err("no triangles".into());
    }
    let (vertices, indices) = soup(corners);
    Ok(Mesh::single(vertices, indices, Material::fallback(coloured)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_tags_attributes_and_transforms() {
        let xml = r##"<?xml version="1.0"?><model xmlns:m="x"><resources><basematerials id="1"><base name="r" displaycolor="#FF0000FF"/></basematerials>
            <object id="2" pid="1" pindex="0"><mesh><vertices><vertex x="0" y="0" z="0"/><vertex x="1" y="0" z="0"/><vertex x="0" y="1" z="0"/></vertices>
            <triangles><triangle v1="0" v2="1" v3="2"/></triangles></mesh></object>
            <object id="3"><components><component objectid="2" transform="1 0 0 0 1 0 0 0 1 10 0 0"/></components></object></resources>
            <build><item objectid="3" transform="2 0 0 0 2 0 0 0 2 0 0 5"/></build></model>"##;
        let (objects, build) = read_model(xml);
        assert_eq!(objects[&2].triangles, vec![([0, 1, 2], Some([1.0, 0.0, 0.0]))]);
        assert_eq!(objects[&3].components[0].1, 2);
        let m = then(&objects[&3].components[0].2, &build[0].1);
        // Moved 10 along x, then scaled 2 and lifted 5.
        assert_eq!(apply(&m, [1.0, 0.0, 0.0]), [22.0, 0.0, 5.0]);
        assert_eq!(attr(r#" p:path="/3D/Objects/a.model" objectid="4""#, "path"), Some("/3D/Objects/a.model"));
    }
}
