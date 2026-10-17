//! STL: a soup of triangles, binary (80-byte header, count, 50 bytes each) or ASCII (`facet` ... `vertex x y z`).
//! A binary file's header may itself start with "solid", so the size decides. The facet normals are ignored
//! (often wrong or zero): normals come from the triangles, smoothed by angle (`soup`). Z-up, as slicers are.

use super::{Material, Mesh, Vertex, soup, z_up};

pub fn parse(bytes: &[u8]) -> Result<Mesh, String> {
    let corners = if is_binary(bytes) { binary(bytes) } else { ascii(&String::from_utf8_lossy(bytes)) };
    if corners.is_empty() {
        return Err("no triangles".into());
    }
    let (vertices, indices) = soup(corners);
    Ok(Mesh::single(vertices, indices, Material::fallback(false)))
}

fn is_binary(b: &[u8]) -> bool {
    b.len() >= 84 && 84 + 50 * u32::from_le_bytes([b[80], b[81], b[82], b[83]]) as usize == b.len()
}

fn corner(p: [f32; 3]) -> Vertex {
    Vertex { pos: z_up(p), colour: [1.0; 3], ..Default::default() }
}

fn binary(b: &[u8]) -> Vec<Vertex> {
    let f = |o: usize| f32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]]);
    b[84..]
        .chunks_exact(50)
        .enumerate()
        .flat_map(|(i, _)| {
            let o = 84 + i * 50 + 12; // past the facet normal
            (0..3).map(move |k| corner([f(o + k * 12), f(o + k * 12 + 4), f(o + k * 12 + 8)]))
        })
        .collect()
}

fn ascii(text: &str) -> Vec<Vertex> {
    let mut out = Vec::new();
    for line in text.lines() {
        let mut w = line.split_whitespace();
        if w.next() == Some("vertex") {
            let n: Vec<f32> = w.filter_map(|s| s.parse().ok()).collect();
            if n.len() >= 3 {
                out.push(corner([n[0], n[1], n[2]]));
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const ASCII: &str = "solid t\n facet normal 0 0 1\n  outer loop\n   vertex 0 0 0\n   vertex 1 0 0\n   vertex 0 1 0\n  endloop\n endfacet\nendsolid t\n";

    #[test]
    fn reads_ascii_and_binary_alike() {
        let a = parse(ASCII.as_bytes()).unwrap();
        let mut bin = b"solid but really binary".to_vec();
        bin.resize(80, 0);
        bin.extend(1u32.to_le_bytes());
        for v in [0f32, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 0.0] {
            bin.extend(v.to_le_bytes());
        }
        bin.extend([0, 0]);
        assert!(is_binary(&bin));
        let b = parse(&bin).unwrap();
        assert_eq!(a.triangles(), 1);
        assert_eq!(a.vertices, b.vertices);
        // Z-up turned Y-up: the triangle in the XY plane now lies flat, facing up the screen (-Z -> +Y).
        assert_eq!(a.vertices[2].pos, [0.0, 0.0, -1.0]);
        assert_eq!(a.vertices[0].normal, [0.0, 1.0, 0.0]);
    }
}
