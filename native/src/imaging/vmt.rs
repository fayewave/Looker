//! Source engine materials (.vmt): one material, drawn as a card on the .mtl sheet (`mtl::decode`). The shader
//! and its parameters are the card's rows, `$color` tints the sphere, and the textures (`$basetexture`,
//! `$bumpmap`, ...) are its tiles, found from the `materials` folder the file sits in (`resolve`) and read from
//! their .vtf (`vtf.rs`) or a loose image of the same name.

use std::path::{Path, PathBuf};

use super::mtl::Material;
use crate::keyvalues::{self, Value};

/// Texture parameters, the tile label each gets, in the order shown. `$basetexture` is the diffuse map: it
/// textures the sphere.
const TEXTURES: &[(&str, &str)] = &[
    ("$basetexture", "map_Kd"),
    ("$basetexture2", "Base 2"),
    ("$bumpmap", "Normal"),
    ("$normalmap", "Normal"),
    ("$bumpmap2", "Normal 2"),
    ("$detail", "Detail"),
    ("$envmapmask", "Env mask"),
    ("$selfillummask", "Glow mask"),
    ("$phongexponenttexture", "Phong exp"),
    ("$blendmodulatetexture", "Blend mask"),
    ("$lightwarptexture", "Light warp"),
    ("$iris", "Iris"),
];

/// Parameters left off the rows (shown otherwise, or noise).
fn hidden(key: &str) -> bool {
    key.eq_ignore_ascii_case("$color") || TEXTURES.iter().any(|(k, _)| k.eq_ignore_ascii_case(key))
}

/// "[1 0.5 0]" or "{255 128 0}" (0-255) as linear-ish 0-1 floats, as `Kd` holds them.
fn colour(v: &str) -> Option<[f32; 3]> {
    let t = v.trim();
    let bytes = t.starts_with('{');
    let n: Vec<f32> = t.trim_matches(|c| c == '[' || c == ']' || c == '{' || c == '}').split_whitespace().filter_map(|w| w.parse().ok()).collect();
    let k = if bytes { 255.0 } else { 1.0 };
    match n.len() {
        3.. => Some([n[0] / k, n[1] / k, n[2] / k]),
        1 => Some([n[0] / k; 3]),
        _ => None,
    }
}

/// The material in a .vmt's text, named `name`; None if it has no shader block.
pub fn parse(text: &str, name: &str) -> Option<Material> {
    let kv = keyvalues::parse(text);
    let (shader, body) = kv.iter().find_map(|(k, v)| matches!(v, Value::Block(_)).then(|| (k.clone(), v.block())))?;
    let mut m = Material { name: name.to_string(), ..Default::default() };
    m.extra.push(("Shader".into(), shader.clone()));
    // A patch material: what it includes, and its replaced/inserted parameters as its own.
    let mut params: Vec<(String, Value)> = Vec::new();
    if shader.eq_ignore_ascii_case("patch") {
        if let Some(inc) = keyvalues::get(body, "include") {
            m.extra.push(("Includes".into(), inc.to_string()));
        }
        for part in ["insert", "replace"] {
            for b in keyvalues::blocks(body, part) {
                params.extend(b.iter().cloned());
            }
        }
    } else {
        params.extend(body.iter().cloned());
    }
    for (key, label) in TEXTURES {
        if let Some(t) = keyvalues::get(&params, key) {
            if !m.maps.iter().any(|(l, _)| l == label) {
                m.maps.push((label.to_string(), t.to_string()));
            }
        }
    }
    m.kd = keyvalues::get(&params, "$color").and_then(colour);
    if let Some(a) = keyvalues::get(&params, "$alpha").and_then(|a| a.parse::<f32>().ok()) {
        m.d = Some(a);
    }
    for (k, v) in &params {
        match v {
            Value::Text(t) if !hidden(k) => m.extra.push((k.clone(), t.clone())),
            Value::Block(_) if !k.is_empty() => m.extra.push((k.clone(), "\u{2026}".into())),
            _ => {}
        }
    }
    Some(m)
}

/// A texture named in a material ("brick/wall01", relative to the game's `materials` folder, no extension) as
/// a file: its .vtf (or a loose .png/.tga/.jpg) under the `materials` folder the .vmt is in, else beside it.
pub fn resolve(vmt: &Path, name: &str) -> Option<PathBuf> {
    let name = name.trim().replace('/', "\\");
    let rel = Path::new(&name);
    let dir = vmt.parent()?;
    let root = vmt.ancestors().find(|a| a.file_name().is_some_and(|n| n.eq_ignore_ascii_case("materials")));
    let bases: Vec<PathBuf> = root.map(|r| r.join(rel)).into_iter().chain(std::iter::once(dir.join(rel.file_name()?))).collect();
    for base in &bases {
        if base.extension().is_some() && base.is_file() {
            return Some(base.clone());
        }
        for ext in ["vtf", "png", "tga", "jpg"] {
            let mut s = base.clone().into_os_string();
            s.push(".");
            s.push(ext);
            let p = PathBuf::from(s);
            if p.is_file() {
                return Some(p);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shader_params_textures_and_colour() {
        let m = parse(
            "\"VertexLitGeneric\"\n{\n\t\"$basetexture\" \"models/props/crate01\"\n\t\"$bumpmap\" \"models/props/crate01_normal\"\n\t\"$color\" \"[1 0.5 0]\"\n\t\"$surfaceprop\" \"wood\"\n\tProxies { Sine { resultVar $alpha } }\n}\n",
            "crate01",
        )
        .unwrap();
        assert_eq!(m.extra[0], ("Shader".into(), "VertexLitGeneric".into()));
        assert_eq!(m.map("map_Kd"), Some("models/props/crate01"));
        assert_eq!(m.map("Normal"), Some("models/props/crate01_normal"));
        assert_eq!(m.kd, Some([1.0, 0.5, 0.0]));
        assert!(m.extra.iter().any(|(k, v)| k == "$surfaceprop" && v == "wood"));
        assert!(m.extra.iter().any(|(k, _)| k == "Proxies"));
    }

    #[test]
    fn patch_materials_show_their_include() {
        let m = parse("patch { include \"materials/a/b.vmt\" replace { $basetexture x/y } }", "p").unwrap();
        assert!(m.extra.iter().any(|(k, v)| k == "Includes" && v == "materials/a/b.vmt"));
        assert_eq!(m.map("map_Kd"), Some("x/y"));
    }

    #[test]
    fn finds_textures_under_the_materials_folder() {
        let root = std::env::temp_dir().join(format!("looker-vmt-{}", std::process::id()));
        let mats = root.join("materials");
        std::fs::create_dir_all(mats.join("brick")).unwrap();
        std::fs::create_dir_all(mats.join("walls")).unwrap();
        std::fs::write(mats.join("brick").join("wall01.vtf"), b"x").unwrap();
        let vmt = mats.join("walls").join("w.vmt");
        assert_eq!(resolve(&vmt, "brick/wall01"), Some(mats.join("brick").join("wall01.vtf")));
        assert_eq!(resolve(&vmt, "brick/missing"), None);
        let _ = std::fs::remove_dir_all(root);
    }
}
