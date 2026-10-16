//! Wavefront material libraries (.mtl): no pixels of their own, so the decode draws a sheet with one card per
//! material: a lit preview sphere in its colours (textured with its diffuse map), its values, and thumbnails of
//! the maps it references. Drawn like an SVG (software Direct2D into a WIC bitmap) and flagged as a vector, so
//! zoom re-renders it sharp. The parser is the half the .obj viewer will share.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

use windows::Win32::Graphics::Direct2D::Common::*;
use windows::Win32::Graphics::Direct2D::*;
use windows::Win32::Graphics::DirectWrite::*;
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
use windows::Win32::Graphics::Imaging::*;
use windows::core::Interface;

use super::{Decoded, VECTOR_MAX_EDGE};

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Material {
    pub name: String,
    pub ka: Option<[f32; 3]>,
    pub kd: Option<[f32; 3]>,
    pub ks: Option<[f32; 3]>,
    pub ke: Option<[f32; 3]>,
    /// Transmission filter.
    pub tf: Option<[f32; 3]>,
    pub ns: Option<f32>,
    /// Opacity: `d`, or `1 - Tr`.
    pub d: Option<f32>,
    pub ni: Option<f32>,
    pub illum: Option<u32>,
    /// PBR extension: roughness and metallic.
    pub pr: Option<f32>,
    pub pm: Option<f32>,
    /// (statement, file name as written), in file order.
    pub maps: Vec<(String, String)>,
}

impl Material {
    pub fn map(&self, key: &str) -> Option<&str> {
        self.maps.iter().find(|(k, _)| k.eq_ignore_ascii_case(key)).map(|(_, f)| f.as_str())
    }
}

/// Map statements, with the name a card shows for each.
const MAPS: &[(&str, &str)] = &[
    ("map_Kd", "Diffuse"),
    ("map_Ka", "Ambient"),
    ("map_Ks", "Specular"),
    ("map_Ke", "Emission"),
    ("map_Ns", "Shininess"),
    ("map_d", "Opacity"),
    ("map_Tr", "Transparency"),
    ("map_bump", "Bump"),
    ("bump", "Bump"),
    ("norm", "Normal"),
    ("map_Kn", "Normal"),
    ("disp", "Displacement"),
    ("decal", "Decal"),
    ("refl", "Reflection"),
    ("map_Pr", "Roughness"),
    ("map_Pm", "Metallic"),
    ("map_Ps", "Sheen"),
    ("map_RMA", "Rough/Metal/AO"),
    ("map_ORM", "AO/Rough/Metal"),
];

fn map_label(key: &str) -> Option<&'static str> {
    MAPS.iter().find(|(k, _)| k.eq_ignore_ascii_case(key)).map(|(_, l)| *l)
}

/// The file name of a map statement's arguments: the options (`-o 1 1 1`, `-clamp on`, …) come first and the
/// name, which may contain spaces, is the rest.
fn map_file(args: &[&str]) -> Option<String> {
    let mut i = 0;
    while i < args.len() && args[i].starts_with('-') && args[i].len() > 1 {
        let opt = args[i].to_ascii_lowercase();
        i += 1;
        // Options with a fixed count of words; -o/-s/-t take one to three numbers.
        let n = match opt.as_str() {
            "-blendu" | "-blendv" | "-cc" | "-clamp" | "-texres" | "-bm" | "-boost" | "-imfchan" | "-type" => 1,
            "-mm" => 2,
            "-o" | "-s" | "-t" => {
                let mut n = 0;
                while n < 3 && args.get(i + n).is_some_and(|a| a.parse::<f32>().is_ok()) {
                    n += 1;
                }
                n
            }
            _ => 0,
        };
        i += n;
    }
    (i < args.len()).then(|| args[i..].join(" "))
}

fn floats<const N: usize>(args: &[&str]) -> Option<[f32; N]> {
    let v: Vec<f32> = args.iter().map_while(|a| a.parse().ok()).collect();
    match v.len() {
        0 => None,
        // `Kd 0.5` means grey.
        n if n < N => Some([v[0]; N]),
        _ => Some(std::array::from_fn(|i| v[i])),
    }
}

pub fn parse(text: &str) -> Vec<Material> {
    let mut out: Vec<Material> = Vec::new();
    let mut pending = String::new();
    for raw in text.lines() {
        // A trailing backslash continues the statement on the next line.
        if let Some(head) = raw.strip_suffix('\\') {
            pending.push_str(head);
            pending.push(' ');
            continue;
        }
        pending.push_str(raw);
        let line = std::mem::take(&mut pending);
        let line = line.split('#').next().unwrap_or("").trim();
        let mut words = line.split_whitespace();
        let Some(key) = words.next() else { continue };
        let args: Vec<&str> = words.collect();
        if key == "newmtl" {
            out.push(Material { name: args.join(" "), ..Default::default() });
            continue;
        }
        let Some(m) = out.last_mut() else { continue };
        match key {
            "Ka" => m.ka = floats(&args),
            "Kd" => m.kd = floats(&args),
            "Ks" => m.ks = floats(&args),
            "Ke" => m.ke = floats(&args),
            "Tf" => m.tf = floats(&args),
            "Ns" => m.ns = floats::<1>(&args).map(|v| v[0]),
            "d" => m.d = floats::<1>(&args.iter().copied().filter(|a| *a != "-halo").collect::<Vec<_>>()).map(|v| v[0]),
            "Tr" => m.d = floats::<1>(&args).map(|v| 1.0 - v[0]),
            "Ni" => m.ni = floats::<1>(&args).map(|v| v[0]),
            "Pr" => m.pr = floats::<1>(&args).map(|v| v[0]),
            "Pm" => m.pm = floats::<1>(&args).map(|v| v[0]),
            "illum" => m.illum = args.first().and_then(|a| a.parse().ok()),
            k if map_label(k).is_some() => {
                if let Some(f) = map_file(&args) {
                    m.maps.push((k.to_string(), f));
                }
            }
            _ => {}
        }
    }
    out
}

/// A map's file next to the library: as written, else its bare name in the library's folder or one of its
/// subfolders (exporters write absolute paths from the machine they ran on, the textures usually in
/// `textures\`).
pub fn resolve(dir: &Path, name: &str) -> Option<PathBuf> {
    let name = name.trim().trim_matches('"').replace('/', "\\");
    let p = Path::new(&name);
    let direct = if p.is_absolute() { p.to_path_buf() } else { dir.join(p) };
    if direct.is_file() {
        return Some(direct);
    }
    let file = p.file_name()?;
    let bare = dir.join(file);
    if bare.is_file() {
        return Some(bare);
    }
    std::fs::read_dir(dir).ok()?.flatten().filter(|e| e.file_type().is_ok_and(|t| t.is_dir())).map(|e| e.path().join(file)).find(|p| p.is_file())
}

fn illum_name(n: u32) -> &'static str {
    match n {
        0 => "colour only",
        1 => "diffuse",
        2 => "highlights",
        3 => "reflection",
        4 => "glass",
        5 => "Fresnel reflection",
        6 => "refraction",
        7 => "Fresnel refraction",
        8 => "reflection, no ray trace",
        9 => "glass, no ray trace",
        10 => "shadows on invisible",
        _ => "",
    }
}

// --- Sheet layout, in DIPs ---

const MARGIN: f32 = 24.0;
const CARD_GAP: f32 = 16.0;
const CARD_W: f32 = 440.0;
const PAD: f32 = 20.0;
const SPHERE: f32 = 132.0;
const NAME_H: f32 = 28.0;
const ROW_H: f32 = 20.0;
const TILE: f32 = 88.0;
const TILE_GAP: f32 = 12.0;
const TILE_LABEL_H: f32 = 34.0;
const TILES_PER_ROW: usize = ((CARD_W - 2.0 * PAD + TILE_GAP) / (TILE + TILE_GAP)) as usize;

enum Value {
    Colour([f32; 3]),
    Text(String),
}

fn rows(m: &Material) -> Vec<(&'static str, Value)> {
    let mut r = Vec::new();
    for (label, c) in [("Diffuse", m.kd), ("Ambient", m.ka), ("Specular", m.ks), ("Emission", m.ke), ("Transmission", m.tf)] {
        if let Some(c) = c {
            r.push((label, Value::Colour(c)));
        }
    }
    let num = |v: f32| format!("{}", (v * 1000.0).round() / 1000.0);
    if let Some(v) = m.ns {
        r.push(("Shininess", Value::Text(num(v))));
    }
    if let Some(v) = m.d {
        r.push(("Opacity", Value::Text(num(v))));
    }
    if let Some(v) = m.ni {
        r.push(("Refraction", Value::Text(num(v))));
    }
    if let Some(v) = m.pr {
        r.push(("Roughness", Value::Text(num(v))));
    }
    if let Some(v) = m.pm {
        r.push(("Metallic", Value::Text(num(v))));
    }
    if let Some(n) = m.illum {
        let name = illum_name(n);
        r.push(("Illumination", Value::Text(if name.is_empty() { n.to_string() } else { format!("{n} · {name}") })));
    }
    r
}

fn card_height(m: &Material) -> f32 {
    let top = SPHERE.max(NAME_H + rows(m).len() as f32 * ROW_H);
    let tiles = m.maps.len().div_ceil(TILES_PER_ROW);
    let maps = if tiles == 0 { 0.0 } else { 16.0 + tiles as f32 * (TILE + TILE_LABEL_H) + (tiles - 1) as f32 * TILE_GAP };
    PAD + top + maps + PAD
}

fn columns(n: usize) -> usize {
    match n {
        0 | 1 => 1,
        2..=4 => 2,
        _ => 3,
    }
}

/// Card origins and heights (each grid row as tall as its tallest card), and the sheet's size.
fn layout(mats: &[Material]) -> (Vec<(f32, f32, f32)>, f32, f32) {
    let cols = columns(mats.len());
    let mut cards = Vec::with_capacity(mats.len());
    let mut y = MARGIN;
    for row in mats.chunks(cols) {
        let h = row.iter().map(card_height).fold(0.0, f32::max);
        for i in 0..row.len() {
            cards.push((MARGIN + i as f32 * (CARD_W + CARD_GAP), y, h));
        }
        y += h + CARD_GAP;
    }
    let w = 2.0 * MARGIN + cols as f32 * CARD_W + (cols - 1) as f32 * CARD_GAP;
    (cards, w, y - CARD_GAP + MARGIN)
}

// --- Pixels ---

fn to_linear(c: u8) -> f32 {
    let c = c as f32 / 255.0;
    if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
}

fn to_srgb(v: f32) -> f32 {
    let v = v.clamp(0.0, 1.0);
    if v <= 0.0031308 { v * 12.92 } else { 1.055 * v.powf(1.0 / 2.4) - 0.055 }
}

/// A decoded map: straight-alpha linear RGB + alpha, for the sphere.
struct Texture {
    w: u32,
    h: u32,
    /// Premultiplied sRGB BGRA as decoded (the thumbnail).
    pixels: Vec<u8>,
    linear: Vec<[f32; 4]>,
}

impl Texture {
    fn new(w: u32, h: u32, pixels: Vec<u8>) -> Texture {
        let lut: Vec<f32> = (0..=255u8).map(to_linear).collect();
        let linear = pixels
            .chunks_exact(4)
            .map(|p| {
                let a = p[3];
                if a == 0 {
                    return [0.0; 4];
                }
                let un = |c: u8| lut[((c as u32 * 255 + a as u32 / 2) / a as u32).min(255) as usize];
                [un(p[2]), un(p[1]), un(p[0]), a as f32 / 255.0]
            })
            .collect();
        Texture { w, h, pixels, linear }
    }

    /// Bilinear, wrapping.
    fn sample(&self, u: f32, v: f32) -> [f32; 4] {
        let x = u.rem_euclid(1.0) * self.w as f32 - 0.5;
        let y = v.rem_euclid(1.0) * self.h as f32 - 0.5;
        let (x0, y0) = (x.floor(), y.floor());
        let (fx, fy) = (x - x0, y - y0);
        let at = |xi: f32, yi: f32| {
            let xi = (xi as i64).rem_euclid(self.w as i64) as usize;
            let yi = (yi as i64).rem_euclid(self.h as i64) as usize;
            self.linear[yi * self.w as usize + xi]
        };
        let (a, b, c, d) = (at(x0, y0), at(x0 + 1.0, y0), at(x0, y0 + 1.0), at(x0 + 1.0, y0 + 1.0));
        std::array::from_fn(|i| (a[i] * (1.0 - fx) + b[i] * fx) * (1.0 - fy) + (c[i] * (1.0 - fx) + d[i] * fx) * fy)
    }
}

fn norm(v: [f32; 3]) -> [f32; 3] {
    let l = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
    [v[0] / l, v[1] / l, v[2] / l]
}

fn dot(a: [f32; 3], b: [f32; 3]) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

/// The material on a sphere, `s` px square, premultiplied BGRA (screen y down, lit from the upper left).
fn sphere(m: &Material, kd_map: Option<&Texture>, ke_map: Option<&Texture>, s: u32) -> Vec<u8> {
    let kd = m.kd.unwrap_or([0.8; 3]);
    let ks = m.ks.unwrap_or([0.0; 3]);
    let ke = m.ke.unwrap_or([0.0; 3]);
    let ns = m.ns.unwrap_or(10.0).clamp(1.0, 1000.0);
    let opacity = m.d.unwrap_or(1.0).clamp(0.0, 1.0);
    // A key light and a dim fill; only the key makes a highlight.
    let key = norm([-0.55, -0.65, 0.75]);
    let fill = norm([0.7, 0.3, 0.6]);
    let half_vector = norm([key[0], key[1], key[2] + 1.0]);
    let mut px = vec![0u8; (s * s * 4) as usize];
    let half = s as f32 / 2.0;
    for py in 0..s {
        for qx in 0..s {
            let x = (qx as f32 + 0.5) / half - 1.0;
            let y = (py as f32 + 0.5) / half - 1.0;
            let r = (x * x + y * y).sqrt();
            let coverage = ((1.0 - r) * half + 0.5).clamp(0.0, 1.0);
            if coverage <= 0.0 {
                continue;
            }
            let n = [x, y, (1.0 - (r * r).min(1.0)).sqrt()];
            let u = 0.5 + n[0].atan2(n[2]) / std::f32::consts::TAU;
            let v = (-n[1]).clamp(-1.0, 1.0).acos() / std::f32::consts::PI;
            let (mut base, mut alpha) = (kd, opacity);
            if let Some(t) = kd_map {
                let c = t.sample(u, v);
                base = [kd[0] * c[0], kd[1] * c[1], kd[2] * c[2]];
                alpha *= c[3];
            }
            let mut glow = ke;
            if let Some(t) = ke_map {
                let c = t.sample(u, v);
                glow = if m.ke.is_some() { [ke[0] * c[0], ke[1] * c[1], ke[2] * c[2]] } else { [c[0], c[1], c[2]] };
            }
            let diffuse = 0.06 + dot(n, key).max(0.0) + 0.25 * dot(n, fill).max(0.0);
            let spec = dot(n, half_vector).max(0.0).powf(ns);
            let o = (coverage * alpha).clamp(0.0, 1.0);
            let i = ((py * s + qx) * 4) as usize;
            for (ch, &k) in [2usize, 1, 0].iter().zip(&[0usize, 1, 2]) {
                let lin = base[k] * diffuse + ks[k] * spec + glow[k];
                px[i + ch] = (to_srgb(lin) * o * 255.0 + 0.5) as u8;
            }
            px[i + 3] = (o * 255.0 + 0.5) as u8;
        }
    }
    px
}

// --- Drawing ---

thread_local! {
    static TEXT: RefCell<Option<std::rc::Rc<crate::gfx::Text>>> = const { RefCell::new(None) };
}

fn text() -> Result<std::rc::Rc<crate::gfx::Text>, String> {
    TEXT.with(|cell| {
        if let Some(t) = &*cell.borrow() {
            return Ok(t.clone());
        }
        let t = std::rc::Rc::new(crate::gfx::init_text().map_err(|e| e.message().to_string())?);
        *cell.borrow_mut() = Some(t.clone());
        Ok(t)
    })
}

fn colour(hex: u32) -> D2D1_COLOR_F {
    let c = |s: u32| ((hex >> s) & 0xFF) as f32 / 255.0;
    D2D1_COLOR_F { r: c(16), g: c(8), b: c(0), a: 1.0 }
}

fn rect(x: f32, y: f32, w: f32, h: f32) -> D2D_RECT_F {
    D2D_RECT_F { left: x, top: y, right: x + w, bottom: y + h }
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().collect()
}

pub fn decode(f: &IWICImagingFactory, path: &Path, box_w: u32, box_h: u32) -> Result<Decoded, String> {
    let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
    let mats = parse(&String::from_utf8_lossy(&bytes));
    if mats.is_empty() {
        return Err("the library defines no materials".into());
    }
    let dir = path.parent().unwrap_or(Path::new("."));
    let (cards, sw, sh) = layout(&mats);
    let (box_w, box_h) = if box_w == 0 || box_h == 0 { (VECTOR_MAX_EDGE, VECTOR_MAX_EDGE) } else { (box_w, box_h) };
    let scale = (box_w as f32 / sw).min(box_h as f32 / sh).min(VECTOR_MAX_EDGE as f32 / sw.max(sh));
    let w = ((sw * scale).round() as u32).max(1);
    let h = ((sh * scale).round() as u32).max(1);
    let sphere_px = ((SPHERE * scale).round() as u32).max(1);
    let tex_box = ((TILE * scale).max(sphere_px as f32 * 2.0).round() as u32).clamp(64, 1024);

    // Every map once (libraries share textures between materials), decoded as plain sRGB whatever the screen.
    let mut textures: HashMap<PathBuf, Option<Texture>> = HashMap::new();
    let saved = super::TARGET.with(|t| t.replace(crate::colour::Space::Srgb));
    for m in &mats {
        for (_, name) in &m.maps {
            let Some(p) = resolve(dir, name) else { continue };
            textures.entry(p.clone()).or_insert_with(|| match super::decode(f, &p, tex_box, tex_box, true) {
                Ok(d) => Some(Texture::new(d.width, d.height, d.frames.into_iter().next()?.pixels)),
                Err(e) => {
                    crate::trace::mark(format!("mtl: map {} failed: {e}", p.display()));
                    None
                }
            });
        }
    }
    super::TARGET.with(|t| *t.borrow_mut() = saved);
    let texture = |m: &Material, key: &str| m.map(key).and_then(|n| resolve(dir, n)).and_then(|p| textures.get(&p)?.as_ref());

    let text = text()?;
    let e = |e: windows::core::Error| e.message().to_string();
    unsafe {
        let name_font = text.inter(16.0, DWRITE_FONT_WEIGHT_SEMI_BOLD).map_err(e)?;
        let body = text.inter(12.0, DWRITE_FONT_WEIGHT_NORMAL).map_err(e)?;
        let small = text.inter(11.0, DWRITE_FONT_WEIGHT_NORMAL).map_err(e)?;
        let bmp = f.CreateBitmap(w, h, &GUID_WICPixelFormat32bppPBGRA, WICBitmapCacheOnLoad).map_err(e)?;
        let props = D2D1_RENDER_TARGET_PROPERTIES {
            r#type: D2D1_RENDER_TARGET_TYPE_SOFTWARE,
            pixelFormat: D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED },
            dpiX: 96.0,
            dpiY: 96.0,
            ..Default::default()
        };
        let rt = super::svg::factory()?.CreateWicBitmapRenderTarget(&bmp, &props).map_err(e)?;
        let dc: ID2D1DeviceContext = rt.cast().map_err(e)?;
        dc.SetTextAntialiasMode(D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE);
        let brush = dc.CreateSolidColorBrush(&colour(0xFFFFFF), None).map_err(e)?;
        let bitmap = |bw: u32, bh: u32, px: &[u8]| {
            let props = D2D1_BITMAP_PROPERTIES1 {
                pixelFormat: D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED },
                dpiX: 96.0,
                dpiY: 96.0,
                bitmapOptions: D2D1_BITMAP_OPTIONS_NONE,
                colorContext: std::mem::ManuallyDrop::new(None),
            };
            dc.CreateBitmap(D2D_SIZE_U { width: bw, height: bh }, Some(px.as_ptr() as _), bw * 4, &props)
        };
        let fill = |r: D2D_RECT_F, c: u32, radius: f32| {
            brush.SetColor(&colour(c));
            if radius > 0.0 {
                dc.FillRoundedRectangle(&D2D1_ROUNDED_RECT { rect: r, radiusX: radius, radiusY: radius }, &brush);
            } else {
                dc.FillRectangle(&r, &brush);
            }
        };
        let label = |s: &str, fmt: &IDWriteTextFormat, r: D2D_RECT_F, c: u32| {
            brush.SetColor(&colour(c));
            dc.DrawText(&wide(s), fmt, &r, &brush, D2D1_DRAW_TEXT_OPTIONS_CLIP, DWRITE_MEASURING_MODE_NATURAL);
        };

        dc.BeginDraw();
        dc.Clear(Some(&colour(0x1C1C1C)));
        dc.SetTransform(&windows_numerics::Matrix3x2 { M11: scale, M12: 0.0, M21: 0.0, M22: scale, M31: 0.0, M32: 0.0 });
        for (m, &(cx, cy, ch)) in mats.iter().zip(&cards) {
            fill(rect(cx, cy, CARD_W, ch), 0x2B2B2B, 8.0);
            let (x, y) = (cx + PAD, cy + PAD);
            let px = sphere(m, texture(m, "map_Kd"), texture(m, "map_Ke"), sphere_px);
            if let Ok(b) = bitmap(sphere_px, sphere_px, &px) {
                dc.DrawBitmap(&b, Some(&rect(x, y, SPHERE, SPHERE)), 1.0, D2D1_INTERPOLATION_MODE_LINEAR, None, None);
            }
            let vx = x + SPHERE + 20.0;
            let vw = cx + CARD_W - PAD - vx;
            let name = if m.name.is_empty() { "(unnamed)" } else { &m.name };
            label(name, &name_font, rect(vx, y, vw, NAME_H - 6.0), 0xF2F2F2);
            for (i, (what, value)) in rows(m).iter().enumerate() {
                let ry = y + NAME_H + i as f32 * ROW_H;
                label(what, &body, rect(vx, ry, 92.0, ROW_H), 0x9A9A9A);
                match value {
                    Value::Colour(c) => {
                        let srgb = |v: f32| (to_srgb(v) * 255.0 + 0.5) as u32;
                        fill(rect(vx + 92.0, ry + 3.0, 14.0, 14.0), (srgb(c[0]) << 16) | (srgb(c[1]) << 8) | srgb(c[2]), 3.0);
                        let t = format!("{:.3}  {:.3}  {:.3}", c[0], c[1], c[2]);
                        label(&t, &body, rect(vx + 114.0, ry, vw - 114.0, ROW_H), 0xF2F2F2);
                    }
                    Value::Text(t) => label(t, &body, rect(vx + 92.0, ry, vw - 92.0, ROW_H), 0xF2F2F2),
                }
            }
            let top = SPHERE.max(NAME_H + rows(m).len() as f32 * ROW_H);
            for (i, (key, name)) in m.maps.iter().enumerate() {
                let tx = x + (i % TILES_PER_ROW) as f32 * (TILE + TILE_GAP);
                let ty = y + top + 16.0 + (i / TILES_PER_ROW) as f32 * (TILE + TILE_LABEL_H + TILE_GAP);
                fill(rect(tx, ty, TILE, TILE), 0x202020, 4.0);
                match resolve(dir, name).and_then(|p| textures.get(&p)?.as_ref()) {
                    Some(t) => {
                        if let Ok(b) = bitmap(t.w, t.h, &t.pixels) {
                            let k = (TILE / t.w as f32).min(TILE / t.h as f32);
                            let (dw, dh) = (t.w as f32 * k, t.h as f32 * k);
                            let dest = rect(tx + (TILE - dw) / 2.0, ty + (TILE - dh) / 2.0, dw, dh);
                            dc.DrawBitmap(&b, Some(&dest), 1.0, D2D1_INTERPOLATION_MODE_HIGH_QUALITY_CUBIC, None, None);
                        }
                    }
                    None => label("Not found", &small, rect(tx + 8.0, ty, TILE - 16.0, TILE), 0x8A8A8A),
                }
                label(map_label(key).unwrap_or(key), &small, rect(tx, ty + TILE + 2.0, TILE, 16.0), 0xD0D0D0);
                let file = Path::new(&name.replace('\\', "/")).file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
                label(&file, &small, rect(tx, ty + TILE + 17.0, TILE, 16.0), 0x8A8A8A);
            }
        }
        dc.EndDraw(None, None).map_err(e)?;
        let mut px = vec![0u8; (w * h * 4) as usize];
        bmp.CopyPixels(std::ptr::null(), w * 4, &mut px).map_err(e)?;
        let mut d = Decoded::still(w, h, px, sw.round() as u32, sh.round() as u32, None);
        d.vector = true;
        Ok(d)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "# Blender MTL File\n\
        newmtl Wood Floor\n\
        Ns 250.000000\n\
        Ka 1.0 1.0 1.0\n\
        Kd 0.8 0.6 0.4\n\
        Ks 0.5\n\
        Ni 1.45\n\
        d 1.0\n\
        illum 2\n\
        map_Kd -s 2 2 1 -o 0.5 0.5 textures/oak floor.png\n\
        map_Bump -bm 0.3 C:\\Users\\someone\\oak_n.png\n\
        \n\
        newmtl glass\n\
        Tr 0.75  # mostly clear\n\
        Kd 0.1 0.2 \\\n0.3\n";

    #[test]
    fn parses_materials_values_and_maps() {
        let m = parse(SAMPLE);
        assert_eq!(m.len(), 2);
        assert_eq!(m[0].name, "Wood Floor");
        assert_eq!(m[0].kd, Some([0.8, 0.6, 0.4]));
        assert_eq!(m[0].ks, Some([0.5; 3]));
        assert_eq!(m[0].ns, Some(250.0));
        assert_eq!(m[0].illum, Some(2));
        assert_eq!(m[0].map("map_Kd"), Some("textures/oak floor.png"));
        assert_eq!(m[0].map("map_bump"), Some(r"C:\Users\someone\oak_n.png"));
        assert_eq!(m[1].d, Some(0.25));
        assert_eq!(m[1].kd, Some([0.1, 0.2, 0.3]));
    }

    #[test]
    fn map_options_come_before_the_name() {
        assert_eq!(map_file(&["-clamp", "on", "-mm", "0", "1", "a.png"]), Some("a.png".into()));
        assert_eq!(map_file(&["-o", "1", "b", "c.png"]), Some("b c.png".into()));
        assert_eq!(map_file(&["-imfchan", "r", "-t", "1", "2", "3", "x.tga"]), Some("x.tga".into()));
        assert_eq!(map_file(&["-bm", "1"]), None);
    }

    #[test]
    fn resolves_relative_then_bare_names() {
        let dir = std::env::temp_dir().join(format!("looker-mtl-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("tex")).unwrap();
        std::fs::write(dir.join("tex").join("a.png"), b"x").unwrap();
        std::fs::write(dir.join("b.png"), b"x").unwrap();
        assert_eq!(resolve(&dir, "tex/a.png"), Some(dir.join("tex").join("a.png")));
        assert_eq!(resolve(&dir, r"D:\elsewhere\b.png"), Some(dir.join("b.png")));
        assert_eq!(resolve(&dir, r"D:\elsewhere\a.png"), Some(dir.join("tex").join("a.png")));
        assert_eq!(resolve(&dir, "missing.png"), None);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// Renders the library named by `LOOKER_DECODE` to `<it>.png` beside it, fitted to 1600 px.
    /// `LOOKER_DECODE=x.mtl cargo test --release -- --ignored --nocapture renders_a_sheet`
    #[test]
    #[ignore]
    fn renders_a_sheet() {
        use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx};
        let p = PathBuf::from(std::env::var("LOOKER_DECODE").expect("set LOOKER_DECODE"));
        let f: IWICImagingFactory = unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
            CoCreateInstance(&CLSID_WICImagingFactory2, None, CLSCTX_INPROC_SERVER).unwrap()
        };
        let t = std::time::Instant::now();
        let d = decode(&f, &p, 1600, 1600).unwrap();
        println!("{}x{} native {}x{} in {:.0} ms", d.width, d.height, d.native_width, d.native_height, t.elapsed().as_secs_f64() * 1000.0);
        let rgba: Vec<u8> = d.frames[0].pixels.chunks_exact(4).flat_map(|c| [c[2], c[1], c[0], c[3]]).collect();
        image::save_buffer(p.with_extension("png"), &rgba, d.width, d.height, image::ExtendedColorType::Rgba8).unwrap();
    }

    #[test]
    fn sheet_grows_by_rows_of_cards() {
        let one = vec![Material::default()];
        let (_, w1, _) = layout(&one);
        let many = vec![Material::default(); 7];
        let (cards, w7, h7) = layout(&many);
        assert_eq!(cards.len(), 7);
        assert!(w7 > w1);
        assert!(h7 > cards[6].1);
    }
}
