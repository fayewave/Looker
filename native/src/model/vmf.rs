//! Hammer maps (.vmf, Source engine level sources): the world's and entities' brushes as solid geometry. A
//! brush is a convex solid given as planes (three points each, in its `side`s); each side's face is a big
//! square on its plane cut down by every other plane. Faces with tool textures (nodraw, skybox, triggers,
//! clips...) are left out, so the level shows rather than the boxes around it. Each material gets a colour of
//! its own (the textures live in the game, not beside the map). Source is Z-up: turned Y-up.

use std::collections::HashMap;

use super::{Material, Mesh, Part, Vertex, bounds, z_up};
use crate::keyvalues::{self, Value};

type V3 = [f64; 3];

fn sub(a: V3, b: V3) -> V3 {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

fn add(a: V3, b: V3) -> V3 {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}

fn mul(a: V3, k: f64) -> V3 {
    [a[0] * k, a[1] * k, a[2] * k]
}

fn dot(a: V3, b: V3) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn cross(a: V3, b: V3) -> V3 {
    [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]]
}

fn unit(a: V3) -> Option<V3> {
    let l = dot(a, a).sqrt();
    (l > 1e-9).then(|| mul(a, 1.0 / l))
}

/// "(x y z) (x y z) (x y z)".
fn points(s: &str) -> Option<[V3; 3]> {
    let n: Vec<f64> = s.split(|c: char| c == '(' || c == ')' || c.is_whitespace()).filter_map(|w| w.parse().ok()).collect();
    (n.len() == 9).then(|| [[n[0], n[1], n[2]], [n[3], n[4], n[5]], [n[6], n[7], n[8]]])
}

/// A plane, `dot(normal, p) = d`, normal pointing out of the brush.
#[derive(Clone, Copy)]
struct Plane {
    n: V3,
    d: f64,
}

/// The part of `poly` behind `p` (inside the brush).
fn clip(poly: &[V3], p: &Plane) -> Vec<V3> {
    const EPS: f64 = 1e-4;
    let mut out = Vec::with_capacity(poly.len() + 1);
    for i in 0..poly.len() {
        let (a, b) = (poly[i], poly[(i + 1) % poly.len()]);
        let (da, db) = (dot(p.n, a) - p.d, dot(p.n, b) - p.d);
        if da <= EPS {
            out.push(a);
        }
        if (da > EPS && db < -EPS) || (da < -EPS && db > EPS) {
            out.push(add(a, mul(sub(b, a), da / (da - db))));
        }
    }
    out
}

/// Each side's face of one brush: (side index, polygon wound counter-clockwise seen from outside).
fn faces(planes: &[Plane]) -> Vec<(usize, Vec<V3>)> {
    const HUGE: f64 = 1.0e6;
    let mut out = Vec::new();
    for (i, p) in planes.iter().enumerate() {
        let helper = if p.n[2].abs() < 0.9 { [0.0, 0.0, 1.0] } else { [1.0, 0.0, 0.0] };
        let Some(u) = unit(cross(helper, p.n)) else { continue };
        let v = cross(p.n, u);
        let c = mul(p.n, p.d);
        // Counter-clockwise around the outward normal: u, v, n is right-handed.
        let mut poly = vec![
            add(c, add(mul(u, -HUGE), mul(v, -HUGE))),
            add(c, add(mul(u, HUGE), mul(v, -HUGE))),
            add(c, add(mul(u, HUGE), mul(v, HUGE))),
            add(c, add(mul(u, -HUGE), mul(v, HUGE))),
        ];
        for (j, q) in planes.iter().enumerate() {
            if j != i {
                poly = clip(&poly, q);
                if poly.len() < 3 {
                    break;
                }
            }
        }
        if poly.len() >= 3 {
            out.push((i, poly));
        }
    }
    out
}

/// Tool textures: they shape the map (clips, triggers, the skybox shell) but aren't seen in the game.
fn tool(material: &str) -> bool {
    let m = material.to_ascii_lowercase().replace('\\', "/");
    m.starts_with("tools/")
}

/// A steady colour per material name: a soft hue from its hash, in linear light.
fn colour_for(name: &str) -> [f32; 3] {
    let h = name.to_ascii_lowercase().bytes().fold(0xcbf29ce484222325u64, |h, b| (h ^ b as u64).wrapping_mul(0x100000001b3));
    let hue = (h % 360) as f32 / 60.0;
    let (s, v) = (0.35 + (h >> 16 & 0xFF) as f32 / 255.0 * 0.2, 0.62 + (h >> 24 & 0xFF) as f32 / 255.0 * 0.25);
    let c = v * s;
    let x = c * (1.0 - (hue % 2.0 - 1.0).abs());
    let (r, g, b) = match hue as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let m = v - c;
    [r + m, g + m, b + m].map(super::linear)
}

/// The brushes of a map: the world's, and those of brush entities (doors, func_detail...).
fn solids(kv: &[(String, Value)]) -> Vec<&[(String, Value)]> {
    let mut out = Vec::new();
    for top in ["world", "entity"] {
        for b in keyvalues::blocks(kv, top) {
            out.extend(keyvalues::blocks(b, "solid"));
        }
    }
    out
}

pub fn parse(bytes: &[u8]) -> Result<Mesh, String> {
    let kv = keyvalues::parse(&String::from_utf8_lossy(bytes));
    if keyvalues::blocks(&kv, "world").next().is_none() {
        return Err("not a Hammer map (no world)".into());
    }
    let mut by_material: HashMap<String, usize> = HashMap::new();
    let mut names: Vec<String> = Vec::new();
    // Per material: its triangles' corners.
    let mut corners: Vec<Vec<Vertex>> = Vec::new();
    for solid in solids(&kv) {
        let sides: Vec<(&str, [V3; 3])> = keyvalues::blocks(solid, "side")
            .filter_map(|s| Some((keyvalues::get(s, "material").unwrap_or(""), points(keyvalues::get(s, "plane")?)?)))
            .collect();
        if sides.len() < 4 {
            continue;
        }
        // Every plane's points lie on the brush, so their centre is inside it: it decides which way each
        // normal points, whatever winding the file uses.
        let inside = mul(sides.iter().flat_map(|(_, p)| p.iter()).fold([0.0; 3], |a, &p| add(a, p)), 1.0 / (sides.len() * 3) as f64);
        let planes: Vec<Option<Plane>> = sides
            .iter()
            .map(|(_, [a, b, c])| {
                let mut n = unit(cross(sub(*b, *a), sub(*c, *a)))?;
                if dot(n, inside) - dot(n, *a) > 0.0 {
                    n = mul(n, -1.0);
                }
                Some(Plane { n, d: dot(n, *a) })
            })
            .collect();
        if planes.iter().any(Option::is_none) {
            continue;
        }
        let planes: Vec<Plane> = planes.into_iter().flatten().collect();
        for (i, poly) in faces(&planes) {
            let material = sides[i].0;
            if tool(material) {
                continue;
            }
            let id = *by_material.entry(material.to_ascii_lowercase()).or_insert_with(|| {
                names.push(material.to_string());
                corners.push(Vec::new());
                names.len() - 1
            });
            let n = z_up(planes[i].n.map(|v| v as f32));
            let vert = |p: V3| Vertex { pos: z_up(p.map(|v| v as f32)), normal: n, uv: [0.0; 2], colour: [1.0; 3] };
            for k in 1..poly.len() - 1 {
                corners[id].extend([vert(poly[0]), vert(poly[k]), vert(poly[k + 1])]);
            }
        }
    }
    let mut vertices = Vec::new();
    let mut parts = Vec::new();
    let mut materials = Vec::new();
    for (id, c) in corners.into_iter().enumerate() {
        if c.is_empty() {
            continue;
        }
        parts.push(Part { start: vertices.len() as u32, count: c.len() as u32, material: materials.len() });
        let mut m = Material::fallback(false);
        m.kd = colour_for(&names[id]);
        m.ks = [0.05; 3];
        materials.push(m);
        vertices.extend(c);
    }
    let indices = (0..vertices.len() as u32).collect();
    let (min, max) = bounds(&vertices);
    Ok(Mesh { vertices, indices, parts, materials, min, max })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 64-unit cube brush as Hammer writes it, one face with nodraw.
    const CUBE: &str = r#"versioninfo { "editorversion" "400" }
world
{
	"id" "1"
	"classname" "worldspawn"
	solid
	{
		"id" "2"
		side { "id" "1" "plane" "(-32 32 32) (32 32 32) (32 -32 32)" "material" "BRICK/WALL01" }
		side { "id" "2" "plane" "(-32 -32 -32) (32 -32 -32) (32 32 -32)" "material" "TOOLS/TOOLSNODRAW" }
		side { "id" "3" "plane" "(-32 32 32) (-32 -32 32) (-32 -32 -32)" "material" "BRICK/WALL01" }
		side { "id" "4" "plane" "(32 32 -32) (32 -32 -32) (32 -32 32)" "material" "BRICK/WALL01" }
		side { "id" "5" "plane" "(32 32 32) (-32 32 32) (-32 32 -32)" "material" "CONCRETE/FLOOR" }
		side { "id" "6" "plane" "(32 -32 -32) (-32 -32 -32) (-32 -32 32)" "material" "BRICK/WALL01" }
	}
}
"#;

    #[test]
    fn a_cube_brush_becomes_five_faces() {
        let m = parse(CUBE.as_bytes()).unwrap();
        // Six quads less the nodraw one, two triangles each.
        assert_eq!(m.triangles(), 10);
        assert_eq!(m.materials.len(), 2);
        for k in 0..3 {
            assert!((m.min[k] + 32.0).abs() < 1e-3 && (m.max[k] - 32.0).abs() < 1e-3, "{:?} {:?}", m.min, m.max);
        }
        // Normals point out: each face's vertices sit on the side of the centre its normal points to.
        for v in &m.vertices {
            assert!(v.pos.iter().zip(v.normal).map(|(p, n)| p * n).sum::<f32>() > 0.0);
        }
        // Wound counter-clockwise from outside, as the other formats.
        for t in m.vertices.chunks_exact(3) {
            let (a, b, c) = (t[0].pos, t[1].pos, t[2].pos);
            let u = [b[0] - a[0], b[1] - a[1], b[2] - a[2]];
            let w = [c[0] - a[0], c[1] - a[1], c[2] - a[2]];
            let n = [u[1] * w[2] - u[2] * w[1], u[2] * w[0] - u[0] * w[2], u[0] * w[1] - u[1] * w[0]];
            assert!(n.iter().zip(t[0].normal).map(|(a, b)| a * b).sum::<f32>() > 0.0);
        }
    }

    #[test]
    fn a_file_without_a_world_is_refused() {
        assert!(parse(b"versioninfo { }").is_err());
    }
}
