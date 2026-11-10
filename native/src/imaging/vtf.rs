//! Valve Texture Format (.vtf), Source engine textures: opened as images (Faye, 2026-11-11), and read for an
//! .mtl's maps should one name a .vtf. The first frame and face of the smallest mip at least `edge` across (or
//! the largest), so a fit decode reads a small mip and zooming in asks for a bigger one. The common formats:
//! 8-bit RGBA orders, RGB565, greyscale, DXT1/3/5, and 16-bit integer or half-float RGBA (HDR, clipped to SDR).
//! Hands back premultiplied BGRA, as the other decoders.

use std::path::Path;

pub struct Image {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>,
    /// The texture's full size (mip 0).
    pub full: (u32, u32),
}

/// The file as an image fitted into the box (0 x 0: full size).
pub fn decode_file(path: &Path, box_w: u32, box_h: u32) -> Result<super::Decoded, String> {
    let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
    let edge = if box_w == 0 || box_h == 0 { u32::MAX } else { box_w.max(box_h) };
    let img = decode(&bytes, edge)?;
    Ok(super::Decoded::still(img.width, img.height, img.pixels, img.full.0, img.full.1, None))
}

/// IEEE half to f32.
fn half(h: u16) -> f32 {
    let s = if h & 0x8000 != 0 { -1.0 } else { 1.0 };
    let e = ((h >> 10) & 0x1F) as i32;
    let m = (h & 0x3FF) as f32;
    s * match e {
        0 => m * 2f32.powi(-24),
        31 => f32::INFINITY,
        _ => (1.0 + m / 1024.0) * 2f32.powi(e - 15),
    }
}

fn u16_at(b: &[u8], o: usize) -> Option<u16> {
    Some(u16::from_le_bytes(b.get(o..o + 2)?.try_into().ok()?))
}

fn u32_at(b: &[u8], o: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(o..o + 4)?.try_into().ok()?))
}

/// Bytes of one `w`×`h` image in `format`, or None for a format not read here.
fn size(format: u32, w: u32, h: u32) -> Option<usize> {
    let (w, h) = (w as usize, h as usize);
    let blocks = w.div_ceil(4) * h.div_ceil(4);
    Some(match format {
        0 | 1 | 11 | 12 | 16 => w * h * 4,
        24 | 25 => w * h * 8,
        2 | 3 | 9 | 10 => w * h * 3,
        4 | 17 | 18 | 19 | 6 | 7 | 22 => w * h * 2,
        23 => w * h * 4,
        5 | 8 => w * h,
        13 | 20 => blocks * 8,
        14 | 15 => blocks * 16,
        _ => return None,
    })
}

fn rgb565(c: u16) -> [u8; 3] {
    let r = ((c >> 11) & 31) as u32;
    let g = ((c >> 5) & 63) as u32;
    let b = (c & 31) as u32;
    [(r * 255 / 31) as u8, (g * 255 / 63) as u8, (b * 255 / 31) as u8]
}

/// One DXT colour block into RGBA `out` (row stride `w`), at (`bx`, `by`).
fn colour_block(src: &[u8], out: &mut [[u8; 4]], w: usize, h: usize, bx: usize, by: usize, dxt1: bool) {
    let c0 = u16::from_le_bytes([src[0], src[1]]);
    let c1 = u16::from_le_bytes([src[2], src[3]]);
    let (a, b) = (rgb565(c0), rgb565(c1));
    let mix = |p: u32, q: u32, d: u32| -> [u8; 4] {
        let f = |i: usize| ((a[i] as u32 * p + b[i] as u32 * q) / d) as u8;
        [f(0), f(1), f(2), 255]
    };
    let palette: [[u8; 4]; 4] = if c0 > c1 || !dxt1 {
        [[a[0], a[1], a[2], 255], [b[0], b[1], b[2], 255], mix(2, 1, 3), mix(1, 2, 3)]
    } else {
        [[a[0], a[1], a[2], 255], [b[0], b[1], b[2], 255], mix(1, 1, 2), [0, 0, 0, 0]]
    };
    let bits = u32::from_le_bytes([src[4], src[5], src[6], src[7]]);
    for i in 0..16 {
        let (x, y) = (bx * 4 + i % 4, by * 4 + i / 4);
        if x < w && y < h {
            out[y * w + x] = palette[((bits >> (2 * i)) & 3) as usize];
        }
    }
}

fn dxt(data: &[u8], w: usize, h: usize, format: u32) -> Vec<[u8; 4]> {
    let mut out = vec![[0u8; 4]; w * h];
    let step = if format == 13 || format == 20 { 8 } else { 16 };
    let bw = w.div_ceil(4);
    for (n, blk) in data.chunks_exact(step).enumerate().take(bw * h.div_ceil(4)) {
        let (bx, by) = (n % bw, n / bw);
        let (alpha, colour) = if step == 16 { blk.split_at(8) } else { (&blk[..0], blk) };
        colour_block(colour, &mut out, w, h, bx, by, step == 8);
        if step == 16 {
            let mut a = [255u8; 16];
            if format == 14 {
                for (i, v) in a.iter_mut().enumerate() {
                    let nib = (alpha[i / 2] >> ((i % 2) * 4)) & 15;
                    *v = nib * 17;
                }
            } else {
                let (a0, a1) = (alpha[0] as u32, alpha[1] as u32);
                let mut pal = [0u32; 8];
                pal[0] = a0;
                pal[1] = a1;
                for k in 1..7u32 {
                    if a0 > a1 {
                        pal[k as usize + 1] = ((7 - k) * a0 + k * a1) / 7;
                    } else if k < 5 {
                        pal[k as usize + 1] = ((5 - k) * a0 + k * a1) / 5;
                    }
                }
                if a0 <= a1 {
                    pal[6] = 0;
                    pal[7] = 255;
                }
                let bits = alpha[2..8].iter().rev().fold(0u64, |acc, &b| (acc << 8) | b as u64);
                for (i, v) in a.iter_mut().enumerate() {
                    *v = pal[((bits >> (3 * i)) & 7) as usize] as u8;
                }
            }
            for i in 0..16 {
                let (x, y) = (bx * 4 + i % 4, by * 4 + i / 4);
                if x < w && y < h {
                    out[y * w + x][3] = a[i];
                }
            }
        }
    }
    out
}

/// Pixels of one image as RGBA.
fn rgba(data: &[u8], w: usize, h: usize, format: u32) -> Option<Vec<[u8; 4]>> {
    let n = w * h;
    let px: Vec<[u8; 4]> = match format {
        13 | 14 | 15 | 20 => return Some(dxt(data, w, h, format)),
        0 => data.chunks_exact(4).take(n).map(|p| [p[0], p[1], p[2], p[3]]).collect(),
        1 => data.chunks_exact(4).take(n).map(|p| [p[3], p[2], p[1], p[0]]).collect(),
        11 => data.chunks_exact(4).take(n).map(|p| [p[1], p[2], p[3], p[0]]).collect(),
        12 => data.chunks_exact(4).take(n).map(|p| [p[2], p[1], p[0], p[3]]).collect(),
        16 => data.chunks_exact(4).take(n).map(|p| [p[2], p[1], p[0], 255]).collect(),
        2 | 9 => data.chunks_exact(3).take(n).map(|p| [p[0], p[1], p[2], 255]).collect(),
        3 | 10 => data.chunks_exact(3).take(n).map(|p| [p[2], p[1], p[0], 255]).collect(),
        4 => data.chunks_exact(2).take(n).map(|p| {
            let c = rgb565(u16::from_le_bytes([p[0], p[1]]));
            [c[0], c[1], c[2], 255]
        }).collect(),
        17 => data.chunks_exact(2).take(n).map(|p| {
            let c = rgb565(u16::from_le_bytes([p[0], p[1]]));
            [c[2], c[1], c[0], 255]
        }).collect(),
        5 => data.iter().take(n).map(|&v| [v, v, v, 255]).collect(),
        6 => data.chunks_exact(2).take(n).map(|p| [p[0], p[0], p[0], p[1]]).collect(),
        8 => data.iter().take(n).map(|&v| [255, 255, 255, v]).collect(),
        // Distortion maps (UV, UVWQ): their channels as colours, opaque.
        22 => data.chunks_exact(2).take(n).map(|p| [p[0], p[1], 0, 255]).collect(),
        23 => data.chunks_exact(4).take(n).map(|p| [p[0], p[1], p[2], 255]).collect(),
        // Half-float RGBA (HDR): linear light, clipped and turned to sRGB.
        24 => data
            .chunks_exact(8)
            .take(n)
            .map(|p| {
                let c = |i: usize| half(u16::from_le_bytes([p[i], p[i + 1]]));
                let srgb = |v: f32| {
                    let v = v.clamp(0.0, 1.0);
                    ((if v <= 0.0031308 { v * 12.92 } else { 1.055 * v.powf(1.0 / 2.4) - 0.055 }) * 255.0 + 0.5) as u8
                };
                [srgb(c(0)), srgb(c(2)), srgb(c(4)), (c(6).clamp(0.0, 1.0) * 255.0 + 0.5) as u8]
            })
            .collect(),
        25 => data.chunks_exact(8).take(n).map(|p| [p[1], p[3], p[5], p[7]]).collect(),
        _ => return None,
    };
    (px.len() == n).then_some(px)
}

pub fn decode(bytes: &[u8], edge: u32) -> Result<Image, String> {
    let bad = || "not a VTF texture".to_string();
    if bytes.get(..4) != Some(b"VTF\0") {
        return Err(bad());
    }
    let minor = u32_at(bytes, 8).ok_or_else(bad)?;
    let header = u32_at(bytes, 12).ok_or_else(bad)? as usize;
    let width = u16_at(bytes, 16).ok_or_else(bad)? as u32;
    let height = u16_at(bytes, 18).ok_or_else(bad)? as u32;
    let flags = u32_at(bytes, 20).ok_or_else(bad)?;
    let frames = u16_at(bytes, 24).ok_or_else(bad)?.max(1) as usize;
    let first_frame = u16_at(bytes, 26).ok_or_else(bad)?;
    let format = u32_at(bytes, 52).ok_or_else(bad)?;
    let mips = (*bytes.get(56).ok_or_else(bad)?).max(1) as u32;
    let low_format = u32_at(bytes, 57).ok_or_else(bad)?;
    let (low_w, low_h) = (*bytes.get(61).ok_or_else(bad)? as u32, *bytes.get(62).ok_or_else(bad)? as u32);
    let depth = if minor >= 2 { u16_at(bytes, 63).unwrap_or(1).max(1) as usize } else { 1 };
    if width == 0 || height == 0 {
        return Err(bad());
    }
    let faces = if flags & 0x4000 != 0 { if minor < 5 && first_frame != 0xFFFF { 7 } else { 6 } } else { 1 };
    let mip_size = |m: u32| (width >> m).max(1);
    let mip_h = |m: u32| (height >> m).max(1);
    let unsupported = || format!("VTF image format {format} isn't read");

    // Where the high-resolution mips start: a resource from 7.3 on, else after the header and the thumbnail.
    let mut start = None;
    if minor >= 3 {
        let count = u32_at(bytes, 68).unwrap_or(0) as usize;
        for r in 0..count.min(32) {
            let at = 80 + r * 8;
            if bytes.get(at..at + 3) == Some(&[0x30, 0, 0]) {
                start = u32_at(bytes, at + 4).map(|v| v as usize);
            }
        }
    }
    let start = match start {
        Some(s) => s,
        None => header + if low_w > 0 && low_h > 0 { size(low_format, low_w, low_h).unwrap_or(0) } else { 0 },
    };

    // Mips are stored smallest first; each holds every frame, face and slice.
    let pick = (0..mips).rev().find(|&m| mip_size(m).max(mip_h(m)) >= edge).unwrap_or(0);
    let mut offset = start;
    for m in (pick + 1..mips).rev() {
        offset += size(format, mip_size(m), mip_h(m)).ok_or_else(unsupported)? * frames * faces * depth;
    }
    let (w, h) = (mip_size(pick), mip_h(pick));
    let len = size(format, w, h).ok_or_else(unsupported)?;
    let data = bytes.get(offset..offset + len).ok_or("the VTF texture is cut short")?;
    let px = rgba(data, w as usize, h as usize, format).ok_or_else(unsupported)?;
    let pixels = px
        .iter()
        .flat_map(|&[r, g, b, a]| {
            let pm = |c: u8| ((c as u32 * a as u32 + 127) / 255) as u8;
            [pm(b), pm(g), pm(r), a]
        })
        .collect();
    Ok(Image { width: w, height: h, pixels, full: (width, height) })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 7.2 file: 80-byte header, no thumbnail, one 4×4 mip.
    fn file(format: u32, data: &[u8]) -> Vec<u8> {
        let mut b = vec![0u8; 80];
        b[..4].copy_from_slice(b"VTF\0");
        b[4..8].copy_from_slice(&7u32.to_le_bytes());
        b[8..12].copy_from_slice(&2u32.to_le_bytes());
        b[12..16].copy_from_slice(&80u32.to_le_bytes());
        b[16..18].copy_from_slice(&4u16.to_le_bytes());
        b[18..20].copy_from_slice(&4u16.to_le_bytes());
        b[24..26].copy_from_slice(&1u16.to_le_bytes());
        b[52..56].copy_from_slice(&format.to_le_bytes());
        b[56] = 1;
        b[57..61].copy_from_slice(&u32::MAX.to_le_bytes());
        b[63..65].copy_from_slice(&1u16.to_le_bytes());
        b.extend_from_slice(data);
        b
    }

    #[test]
    fn reads_bgra8888() {
        let px: Vec<u8> = (0..16).flat_map(|_| [10u8, 20, 30, 255]).collect();
        let img = decode(&file(12, &px), 1).unwrap();
        assert_eq!((img.width, img.height), (4, 4));
        assert_eq!(&img.pixels[..4], &[10, 20, 30, 255]);
    }

    #[test]
    fn reads_dxt1_solid_red() {
        // c0 = c1 = pure red (0xF800), every index 0.
        let block = [0x00, 0xF8, 0x00, 0xF8, 0, 0, 0, 0];
        let img = decode(&file(13, &block), 1).unwrap();
        assert_eq!(&img.pixels[..4], &[0, 0, 255, 255]);
    }

    /// Reads every .vtf under `LOOKER_VTF` and counts what fails, by reason.
    #[test]
    #[ignore]
    fn reads_a_folder() {
        let Ok(root) = std::env::var("LOOKER_VTF") else { return };
        let mut stack = vec![std::path::PathBuf::from(root)];
        let (mut ok, mut fails) = (0usize, std::collections::BTreeMap::<String, (usize, String)>::new());
        while let Some(d) = stack.pop() {
            for e in std::fs::read_dir(&d).into_iter().flatten().flatten() {
                let p = e.path();
                if p.is_dir() {
                    stack.push(p);
                } else if p.extension().is_some_and(|x| x.eq_ignore_ascii_case("vtf")) {
                    match std::fs::read(&p).map_err(|e| e.to_string()).and_then(|b| decode(&b, 256)) {
                        Ok(_) => ok += 1,
                        Err(e) => {
                            let f = fails.entry(e).or_insert((0, p.display().to_string()));
                            f.0 += 1;
                        }
                    }
                }
            }
        }
        println!("ok {ok}");
        for (e, (n, example)) in &fails {
            println!("{n:5}  {e}  e.g. {example}");
        }
    }
}
