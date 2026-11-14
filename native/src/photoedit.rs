//! Basic editing's pixels (the edit mode is app/edit.rs): a photo turned by quarter turns, straightened by a
//! small angle about its middle, cropped to a rectangle upright on screen, and its exposure moved, at full size,
//! then written as a copy beside it or over it (the original to the Recycle Bin first).
//!
//! The tone and colour adjustments (round 2, Faye 2026-11-14) work on the photo's sRGB values, per channel, as one
//! curve each (`tone`: white balance and exposure gains, then shadows, highlights and contrast), then a saturation
//! matrix: exactly what the edit mode's preview does with Direct2D (a table transfer and a colour matrix), so the
//! saved file looks as the preview did. Exposure is a gain in linear light under a 2.2 gamma: 2^(ev / 2.2) on
//! sRGB values.
//!
//! Round 3 (Faye 2026-11-14): vibrance (HSV saturation pushed most where it's lowest: `vibrance`, the preview's
//! RGB-to-hue, table and hue-to-RGB effects doing the same), a point curve per channel (RGB, then red, green and
//! blue: `Curve`, monotone cubic through the points, the last step of `tone`), and a photo with its own colour
//! profile (Display P3, Adobe RGB as an ICC profile) edited and saved in its own space with the profile embedded
//! (JPEG and PNG; `embed_icc`) instead of being converted down to sRGB.

use std::path::{Path, PathBuf};

use windows::Foundation::{PropertyType, PropertyValue};
use windows::Graphics::Imaging::{BitmapAlphaMode, BitmapDecoder, BitmapEncoder, BitmapPixelFormat, BitmapPropertySet, BitmapTypedValue};
use windows::Storage::FileAccessMode;
use windows::core::HSTRING;

/// What an edit does, in the space it's seen in: the image mirrored left to right (`flip`), then turned by `turns`
/// quarter turns (clockwise), `w` x `h` pixels after them; turned `angle` degrees clockwise about its middle;
/// cropped to `crop` (left, top, right, bottom in pixels from the image's middle, upright on screen); exposure `ev`
/// stops; contrast, highlights, shadows, temperature, tint, vibrance and saturation each -100..100, 0 leaving it
/// be; the tone curves (RGB, red, green, blue). A crop of all zeros is the whole image (a pasted edit's).
#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub struct Params {
    pub turns: u8,
    pub flip: bool,
    pub angle: f32,
    pub crop: [f32; 4],
    pub ev: f32,
    pub contrast: f32,
    pub highlights: f32,
    pub shadows: f32,
    pub temp: f32,
    pub tint: f32,
    pub vibrance: f32,
    pub saturation: f32,
    pub curves: [Curve; 4],
}

/// The most points a curve holds (its two ends included).
pub const CURVE_POINTS: usize = 16;
/// The least gap between two points' inputs.
pub const CURVE_GAP: f32 = 0.02;

/// A tone curve: points (input, output, each 0..1) by input, joined by a monotone cubic (Fritsch-Carlson: no
/// overshoot between points), flat beyond the ends.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Curve {
    pub n: u8,
    pub pts: [[f32; 2]; CURVE_POINTS],
}

impl Default for Curve {
    fn default() -> Curve {
        let mut pts = [[0.0; 2]; CURVE_POINTS];
        pts[1] = [1.0, 1.0];
        Curve { n: 2, pts }
    }
}

impl Curve {
    pub fn points(&self) -> &[[f32; 2]] {
        &self.pts[..self.n as usize]
    }

    /// Leaves every value as it is: all its points on the diagonal, its ends at the corners.
    pub fn identity(&self) -> bool {
        let p = self.points();
        p.iter().all(|q| (q[0] - q[1]).abs() < 0.002) && p.first().is_some_and(|q| q[0] < 0.002) && p.last().is_some_and(|q| q[0] > 0.998)
    }

    /// Adds a point at input `x` on the curve as it is; its place, or None (too close to one, or full).
    pub fn insert(&mut self, x: f32) -> Option<usize> {
        let n = self.n as usize;
        if n >= CURVE_POINTS {
            return None;
        }
        let p = self.points();
        let i = p.iter().position(|q| q[0] > x)?;
        if i == 0 || x - p[i - 1][0] < CURVE_GAP || p[i][0] - x < CURVE_GAP {
            return None;
        }
        let y = self.eval(x);
        self.pts.copy_within(i..n, i + 1);
        self.pts[i] = [x, y];
        self.n += 1;
        Some(i)
    }

    /// Takes point `i` out (never an end).
    pub fn remove(&mut self, i: usize) {
        let n = self.n as usize;
        if i == 0 || i + 1 >= n {
            return;
        }
        self.pts.copy_within(i + 1..n, i);
        self.n -= 1;
    }

    /// Moves point `i` towards `(x, y)`: between its neighbours (`CURVE_GAP` from each), on the square.
    pub fn set(&mut self, i: usize, x: f32, y: f32) {
        let n = self.n as usize;
        if i >= n {
            return;
        }
        let lo = if i == 0 { 0.0 } else { self.pts[i - 1][0] + CURVE_GAP };
        let hi = if i + 1 == n { 1.0 } else { self.pts[i + 1][0] - CURVE_GAP };
        self.pts[i] = [x.clamp(lo, hi.max(lo)), y.clamp(0.0, 1.0)];
    }

    pub fn eval(&self, x: f32) -> f32 {
        let p = self.points();
        let n = p.len();
        if n == 0 {
            return x;
        }
        if x <= p[0][0] {
            return p[0][1];
        }
        if x >= p[n - 1][0] {
            return p[n - 1][1];
        }
        // Secants, then tangents: their mean inside (0 at a turn), the secant at the ends, limited so each piece
        // stays monotone.
        let mut d = [0f32; CURVE_POINTS];
        let mut m = [0f32; CURVE_POINTS];
        for k in 0..n - 1 {
            d[k] = (p[k + 1][1] - p[k][1]) / (p[k + 1][0] - p[k][0]).max(1e-6);
        }
        m[0] = d[0];
        m[n - 1] = d[n - 2];
        for k in 1..n - 1 {
            m[k] = if d[k - 1] * d[k] <= 0.0 { 0.0 } else { (d[k - 1] + d[k]) / 2.0 };
        }
        for k in 0..n - 1 {
            if d[k].abs() < 1e-9 {
                m[k] = 0.0;
                m[k + 1] = 0.0;
            } else {
                let (a, b) = (m[k] / d[k], m[k + 1] / d[k]);
                let s = a * a + b * b;
                if s > 9.0 {
                    let t = 3.0 / s.sqrt();
                    m[k] = t * a * d[k];
                    m[k + 1] = t * b * d[k];
                }
            }
        }
        let k = (0..n - 1).find(|&k| x < p[k + 1][0]).unwrap_or(n - 2);
        let h = p[k + 1][0] - p[k][0];
        let t = (x - p[k][0]) / h;
        let (t2, t3) = (t * t, t * t * t);
        let v = (2.0 * t3 - 3.0 * t2 + 1.0) * p[k][1] + (t3 - 2.0 * t2 + t) * h * m[k] + (-2.0 * t3 + 3.0 * t2) * p[k + 1][1] + (t3 - t2) * h * m[k + 1];
        v.clamp(0.0, 1.0)
    }
}

impl Params {
    /// The tone and colour left as they are (the geometry may still change).
    pub fn neutral(&self) -> bool {
        [self.ev, self.contrast, self.highlights, self.shadows, self.temp, self.tint, self.vibrance, self.saturation].iter().all(|v| v.abs() < 0.005)
            && self.curves.iter().all(Curve::identity)
    }

    /// The tone and colour alone (what Copy edits keeps): no turn, flip, straightening or crop.
    pub fn tone_only(&self) -> Params {
        Params { turns: 0, flip: false, angle: 0.0, crop: [0.0; 4], ..*self }
    }

    /// This geometry with `t`'s tone and colour (Paste edits).
    pub fn with_tone(&self, t: &Params) -> Params {
        Params { turns: self.turns, flip: self.flip, angle: self.angle, crop: self.crop, ..*t }
    }

    /// The same geometry with the tone and colour as they were (edit mode's before view).
    pub fn geometry_only(&self) -> Params {
        Params { turns: self.turns, flip: self.flip, angle: self.angle, crop: self.crop, ..Params::default() }
    }
}

/// White balance's gains on R, G and B: warmer (`temp` > 0) lifts red and lowers blue, `tint` > 0 lowers green
/// (towards magenta); scaled so a mid grey keeps its brightness.
pub fn wb_gains(p: &Params) -> [f32; 3] {
    let (t, m) = (p.temp / 100.0, p.tint / 100.0);
    let g = [1.0 + 0.22 * t, 1.0 - 0.16 * m, 1.0 - 0.22 * t];
    let luma = 0.2126 * g[0] + 0.7152 * g[1] + 0.0722 * g[2];
    g.map(|v| v / luma)
}

/// A soft bump: 0 outside `lo..hi`, 1 in the middle.
fn bump(v: f32, lo: f32, hi: f32) -> f32 {
    let x = ((v - lo) / (hi - lo)).clamp(0.0, 1.0);
    4.0 * x * (1.0 - x)
}

/// Channel `c` (0 R, 1 G, 2 B) of sRGB value `v` (0..1) through the per-channel adjustments: the white balance and
/// exposure gains (clipped), shadows and highlights (a bump over the lower and upper tones, black and white held), then contrast (an S
/// curve about the middle, flattening below 0).
pub fn tone(p: &Params, wb: &[f32; 3], c: usize, v: f32) -> f32 {
    let mut v = (v * wb[c] * gain(p.ev)).clamp(0.0, 1.0);
    v += p.shadows / 100.0 * 0.17 * bump(v, 0.0, 0.7);
    v += p.highlights / 100.0 * 0.17 * bump(v, 0.3, 1.0);
    v = v.clamp(0.0, 1.0);
    if p.contrast.abs() > 0.005 {
        let k = 2f32.powf(p.contrast / 100.0 * 1.5);
        v = if v < 0.5 { 0.5 * (2.0 * v).powf(k) } else { 1.0 - 0.5 * (2.0 * (1.0 - v)).powf(k) };
    }
    let v = p.curves[0].eval(v.clamp(0.0, 1.0));
    p.curves[c + 1].eval(v)
}

/// Vibrance's curve on HSV saturation `s` (0..1), `vibrance` -100..100: pushed most where it's low and not at all
/// at grey or full, monotone either way (up: s + 1.5 a s (1 - s)^2; down: s (1 + a (1 - s)^2)).
pub fn vibrance_curve(vibrance: f32, s: f32) -> f32 {
    let a = vibrance / 100.0;
    let k = if a > 0.0 { 1.5 } else { 1.0 };
    (s * (1.0 + a * k * (1.0 - s) * (1.0 - s))).clamp(0.0, 1.0)
}

/// `rgb` (0..1) with its HSV saturation through `vibrance_curve`, hue and value kept: each channel's distance
/// below the brightest scaled.
pub fn vibrance(vibrance: f32, rgb: [f32; 3]) -> [f32; 3] {
    let mx = rgb[0].max(rgb[1]).max(rgb[2]);
    let mn = rgb[0].min(rgb[1]).min(rgb[2]);
    if mx <= 1e-6 || mx - mn <= 1e-6 {
        return rgb;
    }
    let s = (mx - mn) / mx;
    let f = vibrance_curve(vibrance, s) / s;
    rgb.map(|c| (mx - (mx - c) * f).clamp(0.0, 1.0))
}

/// Saturation's matrix on sRGB values (`m[i][j]`: input channel i's share of output j), about Rec. 709 luma.
pub fn saturation_matrix(p: &Params) -> [[f32; 3]; 3] {
    let s = 1.0 + p.saturation / 100.0;
    let w = [0.2126f32, 0.7152, 0.0722];
    std::array::from_fn(|i| std::array::from_fn(|j| (1.0 - s) * w[i] + if i == j { s } else { 0.0 }))
}

/// Mirrors premultiplied BGRA `src` (`w` x `h`) left to right.
fn mirror(src: &[u8], w: u32, h: u32) -> Vec<u8> {
    let mut out = vec![0u8; src.len()];
    let row = w as usize * 4;
    for y in 0..h as usize {
        for x in 0..w as usize {
            let (a, b) = (y * row + x * 4, y * row + (w as usize - 1 - x) * 4);
            out[b..b + 4].copy_from_slice(&src[a..a + 4]);
        }
    }
    out
}

/// Exposure's gain on gamma-encoded values.
pub fn gain(ev: f32) -> f32 {
    2f32.powf(ev / 2.2)
}

/// Whether the point `(x, y)` (pixels from the middle, upright on screen) is on the image turned by `angle`.
pub fn on_image(w: f32, h: f32, angle: f32, x: f32, y: f32) -> bool {
    let (s, c) = angle.to_radians().sin_cos();
    let (qx, qy) = (c * x + s * y, -s * x + c * y);
    qx.abs() <= w / 2.0 + 0.01 && qy.abs() <= h / 2.0 + 0.01
}

/// Whether all of `crop` lies on the image.
pub fn crop_fits(w: f32, h: f32, angle: f32, crop: [f32; 4]) -> bool {
    let [l, t, r, b] = crop;
    r > l && b > t && [(l, t), (r, t), (r, b), (l, b)].iter().all(|&(x, y)| on_image(w, h, angle, x, y))
}

/// The biggest crop of aspect `a` (width over height) centred on the image turned by `angle`.
pub fn largest(w: f32, h: f32, angle: f32, a: f32) -> [f32; 4] {
    let (s, c) = angle.to_radians().sin_cos();
    let (s, c) = (s.abs(), c.abs());
    let ch = (w / (a * c + s)).min(h / (a * s + c));
    let cw = a * ch;
    [-cw / 2.0, -ch / 2.0, cw / 2.0, ch / 2.0]
}

/// `p` changes nothing.
pub fn is_identity(p: &Params, w: f32, h: f32) -> bool {
    let full = [-w / 2.0, -h / 2.0, w / 2.0, h / 2.0];
    p.turns == 0 && !p.flip && p.angle.abs() < 0.005 && p.neutral() && p.crop.iter().zip(full).all(|(a, b)| (a - b).abs() < 0.5)
}

/// Applies `p` to premultiplied BGRA `src` (`sw` x `sh`, upright: before `p.flip` and `p.turns`): the result and
/// its size.
pub fn apply(src: &[u8], sw: u32, sh: u32, p: &Params) -> (Vec<u8>, u32, u32) {
    let mirrored;
    let src = if p.flip {
        mirrored = mirror(src, sw, sh);
        &mirrored[..]
    } else {
        src
    };
    let turned;
    let (src, w, h) = if p.turns % 4 != 0 {
        turned = crate::fileops::rotate_bgra(src, sw, sh, (p.turns % 4) as i32 * 90);
        (&turned.0[..], turned.1, turned.2)
    } else {
        (src, sw, sh)
    };
    let [l, t, r, b] = p.crop;
    let ow = ((r - l).round() as u32).max(1);
    let oh = ((b - t).round() as u32).max(1);
    let (sn, cs) = p.angle.to_radians().sin_cos();
    let (hw, hh) = (w as f32 / 2.0, h as f32 / 2.0);
    // The tone curves as tables over the 8-bit values (R, G, B), then the saturation matrix.
    let neutral = p.neutral();
    let wb = wb_gains(p);
    let lut: [[f32; 256]; 3] = std::array::from_fn(|c| std::array::from_fn(|i| tone(p, &wb, c, i as f32 / 255.0)));
    let sat = saturation_matrix(p);
    let saturate = p.saturation.abs() > 0.005;
    let vibrant = p.vibrance.abs() > 0.005;
    let mut out = vec![0u8; ow as usize * oh as usize * 4];
    let threads = std::thread::available_parallelism().map_or(4, |n| n.get()).clamp(1, 16);
    let rows_per = (oh as usize).div_ceil(threads);
    std::thread::scope(|scope| {
        for (ci, chunk) in out.chunks_mut(rows_per * ow as usize * 4).enumerate() {
            let src = &src;
            scope.spawn(move || {
                let y0 = ci * rows_per;
                for (ry, row) in chunk.chunks_mut(ow as usize * 4).enumerate() {
                    let sy = t + (y0 + ry) as f32 + 0.5;
                    for ox in 0..ow as usize {
                        let sx = l + ox as f32 + 0.5;
                        // Upright on screen -> the image's own pixels (turned back by the angle).
                        let qx = cs * sx + sn * sy + hw - 0.5;
                        let qy = -sn * sx + cs * sy + hh - 0.5;
                        let px = sample(src, w, h, qx, qy);
                        let d = &mut row[ox * 4..ox * 4 + 4];
                        let a = px[3];
                        if neutral || a < 0.5 {
                            for k in 0..3 {
                                d[k] = px[k].min(a).round().clamp(0.0, 255.0) as u8;
                            }
                        } else {
                            // Straight colour (BGRA -> R, G, B), through the curves (interpolated between the
                            // table's steps) and the matrix, premultiplied again.
                            let un = 255.0 / a;
                            let rgb: [f32; 3] = std::array::from_fn(|c| {
                                let v = (px[2 - c] * un).clamp(0.0, 255.0);
                                let i = (v as usize).min(254);
                                let f = v - i as f32;
                                lut[c][i] + (lut[c][i + 1] - lut[c][i]) * f
                            });
                            let rgb = if saturate { std::array::from_fn(|j| (0..3).map(|i| rgb[i] * sat[i][j]).sum::<f32>().clamp(0.0, 1.0)) } else { rgb };
                            let rgb = if vibrant { vibrance(p.vibrance, rgb) } else { rgb };
                            for c in 0..3 {
                                d[2 - c] = (rgb[c] * a).round().clamp(0.0, 255.0) as u8;
                            }
                        }
                        d[3] = a.round().clamp(0.0, 255.0) as u8;
                    }
                }
            });
        }
    });
    (out, ow, oh)
}

/// Bilinear sample of premultiplied BGRA at pixel centre coordinates `(x, y)`; outside is transparent.
fn sample(src: &[u8], w: u32, h: u32, x: f32, y: f32) -> [f32; 4] {
    let (w, h) = (w as i32, h as i32);
    let (x0, y0) = (x.floor() as i32, y.floor() as i32);
    let (fx, fy) = (x - x0 as f32, y - y0 as f32);
    let at = |xx: i32, yy: i32| -> [f32; 4] {
        // Edges are held (a crop that touches the image's border reads its last pixel, not black).
        let (xx, yy) = (xx.clamp(0, w - 1), yy.clamp(0, h - 1));
        let i = (yy as usize * w as usize + xx as usize) * 4;
        [src[i] as f32, src[i + 1] as f32, src[i + 2] as f32, src[i + 3] as f32]
    };
    if x < -1.0 || y < -1.0 || x > w as f32 || y > h as f32 {
        return [0.0; 4];
    }
    let (a, b, c, d) = (at(x0, y0), at(x0 + 1, y0), at(x0, y0 + 1), at(x0 + 1, y0 + 1));
    let mut o = [0f32; 4];
    for k in 0..4 {
        let top = a[k] + (b[k] - a[k]) * fx;
        let bot = c[k] + (d[k] - c[k]) * fx;
        o[k] = top + (bot - top) * fy;
    }
    o
}

/// The extensions an edit can be written back into (WIC has their encoders).
pub fn writable(path: &Path) -> bool {
    encoder_for(path).is_some()
}

fn encoder_for(path: &Path) -> Option<windows::core::GUID> {
    let ext = path.extension()?.to_string_lossy().to_ascii_lowercase();
    match ext.as_str() {
        "jpg" | "jpeg" | "jpe" | "jfif" => BitmapEncoder::JpegEncoderId().ok(),
        "png" => BitmapEncoder::PngEncoderId().ok(),
        "tif" | "tiff" => BitmapEncoder::TiffEncoderId().ok(),
        "bmp" | "dib" => BitmapEncoder::BmpEncoderId().ok(),
        _ => None,
    }
}

/// Where a copy of `path` goes: "name (edited).ext" beside it, numbered when taken; a format that can't be
/// written goes to JPEG (PNG with `alpha`).
pub fn copy_path(path: &Path, alpha: bool) -> PathBuf {
    let stem = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "photo".into());
    let ext = if writable(path) {
        path.extension().map(|e| e.to_string_lossy().into_owned()).unwrap_or_else(|| "jpg".into())
    } else if alpha {
        "png".into()
    } else {
        "jpg".into()
    };
    let dir = path.parent().map(Path::to_path_buf).unwrap_or_default();
    let mut n = 1;
    loop {
        let name = if n == 1 { format!("{stem} (edited).{ext}") } else { format!("{stem} (edited {n}).{ext}") };
        let p = dir.join(name);
        if !p.exists() {
            return p;
        }
        n += 1;
    }
}

fn err(e: windows::core::Error) -> String {
    let m = e.message();
    if m.is_empty() { format!("{:#010x}", e.code().0) } else { m.to_string() }
}

/// What's kept from the original's metadata (its orientation isn't: the pixels are upright now).
const KEPT: [&str; 20] = [
    "System.Photo.DateTaken",
    "System.Photo.CameraManufacturer",
    "System.Photo.CameraModel",
    "System.Photo.LensModel",
    "System.Photo.LensManufacturer",
    "System.Photo.FNumber",
    "System.Photo.ExposureTime",
    "System.Photo.ISOSpeed",
    "System.Photo.FocalLength",
    "System.Photo.FocalLengthInFilm",
    "System.Photo.Flash",
    "System.Photo.ExposureBias",
    "System.Photo.WhiteBalance",
    "System.Title",
    "System.Comment",
    "System.Keywords",
    "System.SimpleRating",
    "System.Author",
    "System.Copyright",
    "System.ApplicationName",
];

/// Decodes `path` at full size, applies `p`, and writes the result: to a copy beside it (`copy`), or over it,
/// the original moved to the Recycle Bin first (`owner` owns the shell's question for a drive without one).
/// Returns the file written.
pub fn save(path: &Path, p: &Params, copy: bool, owner: isize) -> Result<PathBuf, String> {
    if !copy && !writable(path) {
        return Err("this format can't be written: save a copy".into());
    }
    // In the photo's own space when it has a profile and the file written can carry it (round 3: a P3 photo
    // stays P3), else in sRGB.
    let (mut img, mut icc) = crate::fileops::full_image_own_space(path).ok_or("couldn't read the photo")?;
    let alpha = img.pixels.chunks_exact(4).any(|px| px[3] < 255);
    let out = if copy { copy_path(path, alpha) } else { path.to_path_buf() };
    if icc.is_some() && !carries_icc(&out) {
        img = crate::fileops::full_image(path, 0).ok_or("couldn't read the photo")?;
        icc = None;
    }
    let mut p = *p;
    if p.crop == [0.0; 4] {
        // A pasted edit: the whole photo (as turned).
        let (w, h) = if p.turns & 1 == 1 { (img.height, img.width) } else { (img.width, img.height) };
        p.crop = [-(w as f32) / 2.0, -(h as f32) / 2.0, w as f32 / 2.0, h as f32 / 2.0];
    }
    let (pixels, w, h) = apply(&img.pixels, img.width, img.height, &p);
    drop(img);
    let encoder_id = match encoder_for(&out) {
        Some(e) => e,
        None => return Err("no encoder".into()),
    };
    // Written to a sibling temp first: the original is only touched once the new file is whole.
    let name = out.file_name().ok_or("no file name")?.to_string_lossy().into_owned();
    let temp = out.with_file_name(format!(".{name}.looker.tmp"));
    let r = (|| {
        std::fs::File::create(&temp).map_err(|e| e.to_string())?;
        encode(path, &temp, encoder_id, &pixels, w, h)?;
        if let Some(icc) = &icc {
            let bytes = std::fs::read(&temp).map_err(|e| e.to_string())?;
            let with = embed_icc(&bytes, icc).ok_or("couldn't embed the colour profile")?;
            std::fs::write(&temp, with).map_err(|e| e.to_string())?;
        }
        if copy {
            std::fs::rename(&temp, &out).map_err(|e| e.to_string())
        } else {
            // The shell's file operation may ask (a drive without a Recycle Bin): an STA of its own for it.
            let p = path.to_path_buf();
            let recycled = std::thread::spawn(move || unsafe {
                let _ = windows::Win32::System::Com::CoInitializeEx(None, windows::Win32::System::Com::COINIT_APARTMENTTHREADED);
                crate::fileops::recycle(&p, owner)
            })
            .join()
            .unwrap_or(false);
            if !recycled {
                return Err("couldn't move the original to the Recycle Bin".into());
            }
            std::fs::rename(&temp, &out).map_err(|e| e.to_string())
        }
    })();
    let _ = std::fs::remove_file(&temp);
    r.map(|()| out)
}

/// Whether the file written to `path` can carry an ICC profile (`embed_icc`): JPEG and PNG.
fn carries_icc(path: &Path) -> bool {
    let ext = path.extension().map(|e| e.to_string_lossy().to_ascii_lowercase()).unwrap_or_default();
    matches!(ext.as_str(), "jpg" | "jpeg" | "jpe" | "jfif" | "png")
}

/// `file` (a JPEG or PNG WIC wrote) with ICC profile `icc` in it: a JPEG's APP2 "ICC_PROFILE" segments after its
/// APP0/APP1 ones, a PNG's iCCP chunk after IHDR (its sRGB, gAMA and cHRM chunks, which would contradict it,
/// left out). None for anything else.
pub fn embed_icc(file: &[u8], icc: &[u8]) -> Option<Vec<u8>> {
    const PNG: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
    if file.starts_with(&[0xFF, 0xD8]) {
        // After SOI and any APP0/APP1 (JFIF, EXIF, XMP), dropping an APP2 profile already there.
        let mut out = file[..2].to_vec();
        let mut i = 2;
        let mut inserted = false;
        while i + 4 <= file.len() && file[i] == 0xFF {
            let marker = file[i + 1];
            if marker == 0xDA || marker == 0xD9 {
                break;
            }
            let len = u16::from_be_bytes([file[i + 2], file[i + 3]]) as usize;
            let seg = file.get(i..i + 2 + len)?;
            let leading = matches!(marker, 0xE0 | 0xE1);
            if !leading && !inserted {
                push_icc_segments(&mut out, icc)?;
                inserted = true;
            }
            if !(marker == 0xE2 && seg.get(4..16) == Some(b"ICC_PROFILE\0")) {
                out.extend_from_slice(seg);
            }
            i += 2 + len;
        }
        if !inserted {
            push_icc_segments(&mut out, icc)?;
        }
        out.extend_from_slice(file.get(i..)?);
        return Some(out);
    }
    if file.starts_with(&PNG) {
        let mut out = PNG.to_vec();
        let mut i = 8;
        while i + 12 <= file.len() {
            let len = u32::from_be_bytes(file[i..i + 4].try_into().ok()?) as usize;
            let kind = file.get(i + 4..i + 8)?;
            let chunk = file.get(i..i + 12 + len)?;
            if !matches!(kind, b"sRGB" | b"gAMA" | b"cHRM" | b"iCCP") {
                out.extend_from_slice(chunk);
            }
            if kind == b"IHDR" {
                let mut data = b"ICC Profile\0\0".to_vec();
                let mut z = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::best());
                std::io::Write::write_all(&mut z, icc).ok()?;
                data.extend(z.finish().ok()?);
                out.extend((data.len() as u32).to_be_bytes());
                let mut crc = flate2::Crc::new();
                crc.update(b"iCCP");
                crc.update(&data);
                out.extend_from_slice(b"iCCP");
                out.extend(&data);
                out.extend(crc.sum().to_be_bytes());
            }
            i += 12 + len;
        }
        return Some(out);
    }
    None
}

/// `icc` as JPEG APP2 segments of at most 65519 bytes each, numbered from 1.
fn push_icc_segments(out: &mut Vec<u8>, icc: &[u8]) -> Option<()> {
    let parts: Vec<&[u8]> = icc.chunks(65519).collect();
    if parts.is_empty() || parts.len() > 255 {
        return None;
    }
    for (k, part) in parts.iter().enumerate() {
        out.extend_from_slice(&[0xFF, 0xE2]);
        out.extend(((part.len() + 16) as u16).to_be_bytes());
        out.extend_from_slice(b"ICC_PROFILE\0");
        out.extend_from_slice(&[k as u8 + 1, parts.len() as u8]);
        out.extend_from_slice(part);
    }
    Some(())
}

/// The JPEG quality (0..1) `path` was written at, read from its luminance quantisation table against the IJG
/// standard one (libjpeg's scaling); None for anything else.
pub fn jpeg_quality(path: &Path) -> Option<f32> {
    const STD: [u32; 64] = [
        16, 11, 10, 16, 24, 40, 51, 61, 12, 12, 14, 19, 26, 58, 60, 55, 14, 13, 16, 24, 40, 57, 69, 56, 14, 17, 22, 29, 51, 87, 80, 62, 18, 22, 37, 56, 68,
        109, 103, 77, 24, 35, 55, 64, 81, 104, 113, 92, 49, 64, 78, 87, 103, 121, 120, 101, 72, 92, 95, 98, 112, 100, 103, 99,
    ];
    let mut f = std::fs::File::open(path).ok()?;
    let mut b = vec![0u8; 256 * 1024];
    let n = std::io::Read::read(&mut f, &mut b).ok()?;
    let b = &b[..n];
    if b.len() < 4 || b[0] != 0xFF || b[1] != 0xD8 {
        return None;
    }
    let mut i = 2;
    while i + 4 <= b.len() {
        if b[i] != 0xFF {
            return None;
        }
        let marker = b[i + 1];
        if marker == 0xD8 || (0xD0..=0xD7).contains(&marker) || marker == 0x01 {
            i += 2;
            continue;
        }
        if marker == 0xDA || marker == 0xD9 {
            return None;
        }
        let len = u16::from_be_bytes([b[i + 2], b[i + 3]]) as usize;
        if marker == 0xDB {
            let mut j = i + 4;
            while j < (i + 2 + len).min(b.len()) {
                let (pq, tq) = (b[j] >> 4, b[j] & 15);
                let size = if pq == 0 { 64 } else { 128 };
                if j + 1 + size > b.len() {
                    return None;
                }
                if tq == 0 {
                    let sum: u32 = (0..64).map(|k| if pq == 0 { b[j + 1 + k] as u32 } else { u16::from_be_bytes([b[j + 1 + 2 * k], b[j + 2 + 2 * k]]) as u32 }).sum();
                    let scale = sum as f32 * 100.0 / STD.iter().sum::<u32>() as f32;
                    let q = if scale <= 100.0 { (200.0 - scale) / 2.0 } else { 5000.0 / scale };
                    return Some((q / 100.0).clamp(0.01, 1.0));
                }
                j += 1 + size;
            }
        }
        i += 2 + len;
    }
    None
}

fn encode(original: &Path, dst_path: &Path, encoder_id: windows::core::GUID, pixels: &[u8], w: u32, h: u32) -> Result<(), String> {
    let dst = crate::fileops::open_stream(dst_path, FileAccessMode::ReadWrite)?;
    let encoder = if encoder_id == BitmapEncoder::JpegEncoderId().map_err(err)? {
        let options = BitmapPropertySet::new().map_err(err)?;
        // As the original was written (Faye, 2026-11-14: round 2), kept from 85 to 98 %: not worse than a fair
        // JPEG, never the huge files 100 % makes; 95 % when the original isn't a JPEG.
        let q = jpeg_quality(original).map_or(0.95, |q| q.clamp(0.85, 0.98));
        let quality = BitmapTypedValue::Create(&PropertyValue::CreateSingle(q).map_err(err)?, PropertyType::Single).map_err(err)?;
        options.Insert(&HSTRING::from("ImageQuality"), &quality).map_err(err)?;
        BitmapEncoder::CreateWithEncodingOptionsAsync(encoder_id, &dst, &options).and_then(|op| op.join()).map_err(err)?
    } else {
        BitmapEncoder::CreateAsync(encoder_id, &dst).and_then(|op| op.join()).map_err(err)?
    };
    let alpha = if pixels.chunks_exact(4).any(|px| px[3] < 255) { BitmapAlphaMode::Premultiplied } else { BitmapAlphaMode::Ignore };
    encoder.SetPixelData(BitmapPixelFormat::Bgra8, alpha, w, h, 96.0, 96.0, pixels).map_err(err)?;
    copy_metadata(original, &encoder);
    encoder.FlushAsync().and_then(|op| op.join()).map_err(err)?;
    Ok(())
}

/// Carries the original's date taken, camera, exposure and words over, each as far as the new format takes it.
fn copy_metadata(original: &Path, encoder: &BitmapEncoder) {
    let r = (|| -> Result<(), String> {
        let src = crate::fileops::open_stream(original, FileAccessMode::Read)?;
        let decoder = BitmapDecoder::CreateAsync(&src).and_then(|op| op.join()).map_err(err)?;
        let props = decoder.BitmapProperties().map_err(err)?;
        let out = encoder.BitmapProperties().map_err(err)?;
        for name in KEPT {
            let list = windows_collections::IIterable::<HSTRING>::from(vec![HSTRING::from(name)]);
            let Ok(found) = props.GetPropertiesAsync(&list).and_then(|op| op.join()) else { continue };
            if found.Size().unwrap_or(0) == 0 {
                continue;
            }
            // One at a time: a property the format can't hold fails alone.
            let _ = out.SetPropertiesAsync(&found).and_then(|op| op.join());
        }
        Ok(())
    })();
    if let Err(e) = r {
        crate::trace::mark(format!("edit: metadata not copied: {e}"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn largest_crops_fit_and_touch() {
        for angle in [0.0f32, 3.0, -12.5, 45.0] {
            for a in [1.0f32, 1.5, 0.75, 16.0 / 9.0] {
                let c = largest(4000.0, 3000.0, angle, a);
                assert!(crop_fits(4000.0, 3000.0, angle, c), "{angle} {a}");
                let bigger = [c[0] * 1.01, c[1] * 1.01, c[2] * 1.01, c[3] * 1.01];
                assert!(!crop_fits(4000.0, 3000.0, angle, bigger), "{angle} {a} not the largest");
                assert!(((c[2] - c[0]) / (c[3] - c[1]) - a).abs() < 1e-3);
            }
        }
        assert_eq!(largest(400.0, 300.0, 0.0, 4.0 / 3.0), [-200.0, -150.0, 200.0, 150.0]);
    }

    #[test]
    fn applies_crop_turn_and_exposure() {
        // 4 x 2: left half red, right half blue (BGRA, opaque).
        let mut src = Vec::new();
        for _y in 0..2 {
            for x in 0..4 {
                src.extend_from_slice(if x < 2 { &[0, 0, 200, 255] } else { &[200, 0, 0, 255] });
            }
        }
        // The right half only.
        let p = Params { crop: [0.0, -1.0, 2.0, 1.0], ..Params::default() };
        let (out, w, h) = apply(&src, 4, 2, &p);
        assert_eq!((w, h), (2, 2));
        assert!(out.chunks_exact(4).all(|px| px == [200, 0, 0, 255]));
        // A quarter turn clockwise: 2 x 4, red on top.
        let p = Params { turns: 1, crop: [-1.0, -2.0, 1.0, 2.0], ..Params::default() };
        let (out, w, h) = apply(&src, 4, 2, &p);
        assert_eq!((w, h), (2, 4));
        assert_eq!(&out[0..4], &[0, 0, 200, 255]);
        assert_eq!(&out[out.len() - 4..], &[200, 0, 0, 255]);
        // One stop up: x 2^(1/2.2).
        let p = Params { crop: [-2.0, -1.0, 2.0, 1.0], ev: 1.0, ..Params::default() };
        let (out, _, _) = apply(&src, 4, 2, &p);
        let want = (100.0 * gain(1.0)).round() as u8;
        let mut half = src.clone();
        half.iter_mut().enumerate().for_each(|(i, v)| if i % 4 != 3 && *v > 0 { *v = 100 });
        let (out2, _, _) = apply(&half, 4, 2, &p);
        assert_eq!(out2[2], want);
        assert_eq!(out[2], 255); // 200 x 1.37 clips
    }

    #[test]
    fn flips_and_adjusts() {
        // 2 x 1: red, blue.
        let src = [0u8, 0, 200, 255, 200, 0, 0, 255];
        let p = Params { flip: true, crop: [-1.0, -0.5, 1.0, 0.5], ..Params::default() };
        let (out, _, _) = apply(&src, 2, 1, &p);
        assert_eq!(out, [200, 0, 0, 255, 0, 0, 200, 255]);
        // The curves leave black and white where they are and are monotonic; neutral is the identity.
        for p in [
            Params { contrast: 100.0, ..Params::default() },
            Params { contrast: -100.0, ..Params::default() },
            Params { highlights: -100.0, shadows: 100.0, ..Params::default() },
            Params { highlights: 100.0, shadows: -100.0, ..Params::default() },
        ] {
            let wb = wb_gains(&p);
            let mut last = -1.0;
            for i in 0..=255 {
                let v = tone(&p, &wb, 1, i as f32 / 255.0);
                assert!(v >= last - 1e-6, "{p:?} at {i}");
                last = v;
            }
            assert!(tone(&p, &wb, 1, 0.0) < 1e-4 && tone(&p, &wb, 1, 1.0) > 1.0 - 1e-4, "{p:?} ends");
        }
        let n = Params::default();
        let wb = wb_gains(&n);
        assert!((0..=255).all(|i| (tone(&n, &wb, 0, i as f32 / 255.0) - i as f32 / 255.0).abs() < 1e-6));
        // No saturation: grey.
        let p = Params { saturation: -100.0, crop: [-1.0, -0.5, 1.0, 0.5], ..Params::default() };
        let (out, _, _) = apply(&src, 2, 1, &p);
        assert!(out[0] == out[1] && out[1] == out[2]);
        // Warmer: more red than blue in a grey.
        let grey = [128u8, 128, 128, 255];
        let p = Params { temp: 50.0, crop: [-0.5, -0.5, 0.5, 0.5], ..Params::default() };
        let (out, _, _) = apply(&grey, 1, 1, &p);
        assert!(out[2] > out[0], "{out:?}");
    }

    #[test]
    fn reads_jpeg_quality() {
        let dir = std::env::temp_dir().join("looker-edit-quality-test");
        let _ = std::fs::create_dir_all(&dir);
        // A minimal JPEG head: SOI, DQT table 0 at the standard table scaled to quality 75 (x 0.5), then SOS.
        let std_t: [u8; 64] = [
            16, 11, 10, 16, 24, 40, 51, 61, 12, 12, 14, 19, 26, 58, 60, 55, 14, 13, 16, 24, 40, 57, 69, 56, 14, 17, 22, 29, 51, 87, 80, 62, 18, 22, 37, 56, 68,
            109, 103, 77, 24, 35, 55, 64, 81, 104, 113, 92, 49, 64, 78, 87, 103, 121, 120, 101, 72, 92, 95, 98, 112, 100, 103, 99,
        ];
        let mut b = vec![0xFF, 0xD8, 0xFF, 0xDB, 0x00, 67, 0x00];
        b.extend(std_t.iter().map(|&v| ((v as u32 * 50 + 50) / 100).max(1) as u8));
        b.extend([0xFF, 0xDA, 0, 2]);
        let f = dir.join("q75.jpg");
        std::fs::write(&f, &b).unwrap();
        let q = jpeg_quality(&f).unwrap();
        assert!((q - 0.75).abs() < 0.02, "{q}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn copies_are_numbered() {
        let dir = std::env::temp_dir().join("looker-edit-copy-test");
        let _ = std::fs::create_dir_all(&dir);
        let orig = dir.join("a.jpg");
        let first = copy_path(&orig, false);
        assert_eq!(first.file_name().unwrap(), "a (edited).jpg");
        std::fs::write(&first, b"x").unwrap();
        assert_eq!(copy_path(&orig, false).file_name().unwrap(), "a (edited 2).jpg");
        assert_eq!(copy_path(&dir.join("b.heic"), false).file_name().unwrap(), "b (edited).jpg");
        assert_eq!(copy_path(&dir.join("b.webp"), true).file_name().unwrap(), "b (edited).png");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn curves_pass_through_their_points_without_overshoot() {
        let mut c = Curve::default();
        assert!(c.identity());
        for i in 0..=20 {
            let x = i as f32 / 20.0;
            assert!((c.eval(x) - x).abs() < 1e-5);
        }
        // An S: darker shadows, brighter highlights.
        let a = c.insert(0.25).unwrap();
        c.set(a, 0.25, 0.15);
        let b = c.insert(0.75).unwrap();
        c.set(b, 0.75, 0.85);
        assert!(!c.identity());
        assert!((c.eval(0.25) - 0.15).abs() < 1e-5 && (c.eval(0.75) - 0.85).abs() < 1e-5);
        let mut last = 0.0;
        for i in 0..=200 {
            let v = c.eval(i as f32 / 200.0);
            assert!(v >= last - 1e-6, "monotone at {i}");
            last = v;
        }
        // Too close to a point, or past the ends: no new point; ends can't be removed.
        assert!(c.insert(0.26).is_none());
        c.remove(0);
        assert_eq!(c.n, 4);
        c.remove(1);
        assert_eq!(c.n, 3);
        // A point is kept between its neighbours.
        c.set(1, 0.0, 2.0);
        assert!((c.pts[1][0] - CURVE_GAP).abs() < 1e-6 && c.pts[1][1] == 1.0);
        // The curves join the tone: a curve that lifts everything shows in every channel.
        let mut p = Params::default();
        p.curves[0].set(0, 0.0, 0.2);
        assert!(!p.neutral());
        let wb = wb_gains(&p);
        assert!((tone(&p, &wb, 1, 0.0) - 0.2).abs() < 1e-5);
        // A channel's own curve only touches it.
        let mut p = Params::default();
        p.curves[3].set(1, 1.0, 0.5);
        let wb = wb_gains(&p);
        assert!((tone(&p, &wb, 2, 1.0) - 0.5).abs() < 1e-5 && (tone(&p, &wb, 0, 1.0) - 1.0).abs() < 1e-5);
    }

    #[test]
    fn vibrance_lifts_muted_colours_most() {
        // Grey and fully saturated colours stay; hue and value are kept.
        assert_eq!(vibrance(100.0, [0.5, 0.5, 0.5]), [0.5, 0.5, 0.5]);
        assert_eq!(vibrance(100.0, [1.0, 0.0, 0.0]), [1.0, 0.0, 0.0]);
        let muted = [0.6, 0.5, 0.45];
        let up = vibrance(100.0, muted);
        assert_eq!(up[0], 0.6);
        assert!(up[2] < muted[2] && up[1] < muted[1]);
        // More push at low saturation than high.
        let gain = |s: f32| vibrance_curve(100.0, s) - s;
        assert!(gain(0.2) > gain(0.8));
        // Monotone both ways, and never past 0..1.
        for v in [-100.0f32, -50.0, 50.0, 100.0] {
            let mut last = 0.0;
            for i in 0..=100 {
                let s = vibrance_curve(v, i as f32 / 100.0);
                assert!(s >= last - 1e-6 && (0.0..=1.0).contains(&s), "{v} at {i}");
                last = s;
            }
        }
        // Down: towards grey.
        let down = vibrance(-100.0, muted);
        assert!(down[2] > muted[2]);
    }

    #[test]
    fn tone_only_and_with_tone_keep_the_geometry_apart() {
        let a = Params { turns: 1, flip: true, angle: 3.0, crop: [-1.0, -2.0, 1.0, 2.0], ev: 0.5, vibrance: 20.0, ..Params::default() };
        let t = a.tone_only();
        assert_eq!((t.turns, t.flip, t.angle, t.crop), (0, false, 0.0, [0.0; 4]));
        assert_eq!((t.ev, t.vibrance), (0.5, 20.0));
        let b = Params { turns: 2, crop: [-5.0, -5.0, 5.0, 5.0], ..Params::default() }.with_tone(&t);
        assert_eq!((b.turns, b.crop, b.ev, b.vibrance), (2, [-5.0, -5.0, 5.0, 5.0], 0.5, 20.0));
    }

    #[test]
    fn embeds_icc_profiles_in_jpeg_and_png() {
        let icc: Vec<u8> = (0..70000u32).map(|i| (i % 251) as u8).collect();
        // A JPEG skeleton: SOI, APP0, APP1, DQT, SOS, data, EOI.
        let mut jpeg = vec![0xFF, 0xD8];
        jpeg.extend([0xFF, 0xE0, 0, 4, 1, 2]);
        jpeg.extend([0xFF, 0xE1, 0, 4, 3, 4]);
        jpeg.extend([0xFF, 0xDB, 0, 4, 5, 6]);
        jpeg.extend([0xFF, 0xDA, 0, 2, 9, 9, 9, 0xFF, 0xD9]);
        let out = embed_icc(&jpeg, &icc).unwrap();
        // The profile comes after APP0/APP1, in two parts, before DQT; the rest unchanged.
        assert_eq!(&out[..14], &jpeg[..14]);
        assert_eq!(&out[14..16], &[0xFF, 0xE2]);
        assert_eq!(&out[18..30], b"ICC_PROFILE\0");
        assert_eq!(&out[30..32], &[1, 2]);
        let mut got = Vec::new();
        let mut i = 14;
        while out[i + 1] == 0xE2 {
            let len = u16::from_be_bytes([out[i + 2], out[i + 3]]) as usize;
            got.extend_from_slice(&out[i + 18..i + 2 + len]);
            i += 2 + len;
        }
        assert_eq!(got, icc);
        assert_eq!(&out[i..], &jpeg[14..]);
        // Embedding again replaces it rather than adding a second.
        assert_eq!(embed_icc(&out, &icc).unwrap(), out);

        // A PNG: signature, IHDR, sRGB, IDAT, IEND.
        let chunk = |kind: &[u8], data: &[u8]| {
            let mut c = (data.len() as u32).to_be_bytes().to_vec();
            c.extend_from_slice(kind);
            c.extend_from_slice(data);
            let mut crc = flate2::Crc::new();
            crc.update(kind);
            crc.update(data);
            c.extend(crc.sum().to_be_bytes());
            c
        };
        let mut png = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
        png.extend(chunk(b"IHDR", &[0; 13]));
        png.extend(chunk(b"sRGB", &[0]));
        png.extend(chunk(b"IDAT", &[1, 2, 3]));
        png.extend(chunk(b"IEND", &[]));
        let out = embed_icc(&png, &icc).unwrap();
        let mut kinds = Vec::new();
        let mut i = 8;
        while i + 12 <= out.len() {
            let len = u32::from_be_bytes(out[i..i + 4].try_into().unwrap()) as usize;
            let kind = out[i + 4..i + 8].to_vec();
            if kind == b"iCCP" {
                let data = &out[i + 8..i + 8 + len];
                assert!(data.starts_with(b"ICC Profile\0\0"));
                let mut z = flate2::read::ZlibDecoder::new(&data[13..]);
                let mut back = Vec::new();
                std::io::Read::read_to_end(&mut z, &mut back).unwrap();
                assert_eq!(back, icc);
                let mut crc = flate2::Crc::new();
                crc.update(b"iCCP");
                crc.update(data);
                assert_eq!(&out[i + 8 + len..i + 12 + len], &crc.sum().to_be_bytes());
            }
            kinds.push(String::from_utf8(kind).unwrap());
            i += 12 + len;
        }
        assert_eq!(kinds, ["IHDR", "iCCP", "IDAT", "IEND"]);
        assert!(embed_icc(b"GIF89a", &icc).is_none());
    }
}
