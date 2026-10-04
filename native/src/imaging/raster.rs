//! Small formats decoded by hand: PCX, and the merged composite of PSD/PSB (the flattened image Photoshop
//! stores after the layers). Pure functions over bytes, unit tested, producing straight RGBA.

fn be16(b: &[u8], o: usize) -> Option<u16> {
    Some(u16::from_be_bytes(b.get(o..o + 2)?.try_into().ok()?))
}
fn be32(b: &[u8], o: usize) -> Option<u32> {
    Some(u32::from_be_bytes(b.get(o..o + 4)?.try_into().ok()?))
}
fn be64(b: &[u8], o: usize) -> Option<u64> {
    Some(u64::from_be_bytes(b.get(o..o + 8)?.try_into().ok()?))
}
fn le16(b: &[u8], o: usize) -> Option<u16> {
    Some(u16::from_le_bytes(b.get(o..o + 2)?.try_into().ok()?))
}

/// PCX: 1/4/8-bit paletted, EGA 4-plane, 24-bit (3 planes) and 32-bit (4 planes), RLE or raw.
pub fn pcx(b: &[u8]) -> Result<(u32, u32, Vec<u8>), String> {
    let bad = || "not a PCX file".to_string();
    if b.len() < 128 || b[0] != 0x0A {
        return Err(bad());
    }
    let rle = b[2] == 1;
    let bpp = b[3] as usize;
    let w = (le16(b, 8).ok_or_else(bad)? as i32 - le16(b, 4).ok_or_else(bad)? as i32 + 1).max(0) as usize;
    let h = (le16(b, 10).ok_or_else(bad)? as i32 - le16(b, 6).ok_or_else(bad)? as i32 + 1).max(0) as usize;
    let planes = b[65] as usize;
    let bpl = le16(b, 66).ok_or_else(bad)? as usize;
    if w == 0 || h == 0 || planes == 0 || bpl == 0 || w > 65535 || h > 65535 {
        return Err(bad());
    }
    let line = planes * bpl;
    // Decode the whole RLE stream: runs may cross plane and line boundaries.
    let mut data = Vec::with_capacity(line * h);
    let mut i = 128;
    while data.len() < line * h && i < b.len() {
        let v = b[i];
        i += 1;
        if rle && v & 0xC0 == 0xC0 {
            let n = (v & 0x3F) as usize;
            let Some(&c) = b.get(i) else { break };
            i += 1;
            data.extend(std::iter::repeat_n(c, n));
        } else {
            data.push(v);
        }
    }
    data.resize(line * h, 0);
    let ega: Vec<[u8; 3]> = (0..16).map(|k| [b[16 + k * 3], b[17 + k * 3], b[18 + k * 3]]).collect();
    let vga: Option<Vec<[u8; 3]>> = (b.len() >= 769 && b[b.len() - 769] == 0x0C)
        .then(|| (0..256).map(|k| { let o = b.len() - 768 + k * 3; [b[o], b[o + 1], b[o + 2]] }).collect());
    let mut out = vec![0u8; w * h * 4];
    for y in 0..h {
        let row = &data[y * line..(y + 1) * line];
        for x in 0..w {
            let rgba: [u8; 4] = match (bpp, planes) {
                (8, 1) => {
                    let v = row[x];
                    match &vga {
                        Some(p) => [p[v as usize][0], p[v as usize][1], p[v as usize][2], 255],
                        None => [v, v, v, 255],
                    }
                }
                (8, 3) => [row[x], row[bpl + x], row[2 * bpl + x], 255],
                (8, 4) => [row[x], row[bpl + x], row[2 * bpl + x], row[3 * bpl + x]],
                (4, 1) => {
                    let v = (row[x / 2] >> if x % 2 == 0 { 4 } else { 0 }) & 0x0F;
                    let c = ega[v as usize];
                    [c[0], c[1], c[2], 255]
                }
                (1, n) if n <= 4 => {
                    let mut idx = 0usize;
                    for p in 0..n {
                        idx |= (((row[p * bpl + x / 8] >> (7 - x % 8)) & 1) as usize) << p;
                    }
                    if n == 1 {
                        let v = if idx == 1 { 255 } else { 0 };
                        [v, v, v, 255]
                    } else {
                        let c = ega[idx];
                        [c[0], c[1], c[2], 255]
                    }
                }
                _ => return Err(format!("PCX with {bpp} bits x {planes} planes is not supported")),
            };
            out[(y * w + x) * 4..][..4].copy_from_slice(&rgba);
        }
    }
    Ok((w as u32, h as u32, out))
}

/// PackBits rows (PSD RLE) into `out`, which must be exactly the unpacked size.
fn packbits(src: &[u8], out: &mut [u8]) {
    let (mut i, mut o) = (0, 0);
    while i < src.len() && o < out.len() {
        let n = src[i] as i8;
        i += 1;
        if n >= 0 {
            let len = (n as usize + 1).min(out.len() - o).min(src.len().saturating_sub(i));
            out[o..o + len].copy_from_slice(&src[i..i + len]);
            i += n as usize + 1;
            o += len;
        } else if n != -128 {
            let len = ((1 - n as isize) as usize).min(out.len() - o);
            let Some(&v) = src.get(i) else { break };
            out[o..o + len].fill(v);
            i += 1;
            o += len;
        }
    }
}

/// The merged composite of a PSD (version 1) or PSB (version 2): bitmap, grayscale, indexed, RGB and CMYK at
/// 1/8/16/32 bits, raw or RLE, with the first extra channel as transparency.
pub fn psd(b: &[u8]) -> Result<(u32, u32, Vec<u8>), String> {
    let bad = || "not a PSD/PSB file".to_string();
    if b.len() < 26 || &b[..4] != b"8BPS" {
        return Err(bad());
    }
    let psb = match be16(b, 4) {
        Some(1) => false,
        Some(2) => true,
        _ => return Err(bad()),
    };
    let channels = be16(b, 12).ok_or_else(bad)? as usize;
    let h = be32(b, 14).ok_or_else(bad)? as usize;
    let w = be32(b, 18).ok_or_else(bad)? as usize;
    let depth = be16(b, 22).ok_or_else(bad)? as usize;
    let mode = be16(b, 24).ok_or_else(bad)?;
    if w == 0 || h == 0 || channels == 0 || w * h > 1 << 30 {
        return Err(bad());
    }
    let mut o = 26;
    let cm_len = be32(b, o).ok_or_else(bad)? as usize;
    let palette = b.get(o + 4..o + 4 + cm_len).unwrap_or(&[]).to_vec();
    o += 4 + cm_len;
    o += 4 + be32(b, o).ok_or_else(bad)? as usize; // image resources
    o += if psb { 8 + be64(b, o).ok_or_else(bad)? as usize } else { 4 + be32(b, o).ok_or_else(bad)? as usize };
    let compression = be16(b, o).ok_or_else(bad)?;
    o += 2;

    let row_bytes = (w * depth).div_ceil(8);
    let mut planes = vec![vec![0u8; row_bytes * h]; channels];
    match compression {
        0 => {
            for (c, plane) in planes.iter_mut().enumerate() {
                let start = o + c * row_bytes * h;
                let src = b.get(start..start + row_bytes * h).ok_or_else(bad)?;
                plane.copy_from_slice(src);
            }
        }
        1 => {
            let count_size = if psb { 4 } else { 2 };
            let counts_at = o;
            let mut data = o + channels * h * count_size;
            for (c, plane) in planes.iter_mut().enumerate() {
                for y in 0..h {
                    let k = counts_at + (c * h + y) * count_size;
                    let n = if psb { be32(b, k) } else { be16(b, k).map(u32::from) }.ok_or_else(bad)? as usize;
                    let src = b.get(data..data + n).ok_or_else(bad)?;
                    packbits(src, &mut plane[y * row_bytes..(y + 1) * row_bytes]);
                    data += n;
                }
            }
        }
        _ => return Err("ZIP-compressed PSD composite is not supported".into()),
    }

    // One 0..255 sample per pixel per channel.
    let sample = |plane: &[u8], i: usize| -> u8 {
        match depth {
            1 => if (plane[(i % w) / 8 + (i / w) * row_bytes] >> (7 - (i % w) % 8)) & 1 == 1 { 0 } else { 255 },
            8 => plane[i],
            16 => plane[i * 2],
            32 => {
                let v = f32::from_be_bytes(plane[i * 4..i * 4 + 4].try_into().unwrap()).clamp(0.0, 1.0);
                // 32-bit documents are linear light.
                let s = if v <= 0.003_130_8 { v * 12.92 } else { 1.055 * v.powf(1.0 / 2.4) - 0.055 };
                (s * 255.0).round() as u8
            }
            _ => 0,
        }
    };
    if !matches!(depth, 1 | 8 | 16 | 32) {
        return Err(format!("{depth}-bit PSD is not supported"));
    }
    let color = match mode {
        0 | 1 | 2 | 8 => 1,
        3 => 3,
        4 => 4,
        _ => return Err(format!("PSD colour mode {mode} is not supported")),
    };
    if channels < color {
        return Err(bad());
    }
    let has_alpha = channels > color;
    let mut out = vec![0u8; w * h * 4];
    for i in 0..w * h {
        let s = |c: usize| sample(&planes[c], i);
        let rgb = match mode {
            2 => {
                let v = s(0) as usize;
                [palette.get(v).copied().unwrap_or(0), palette.get(256 + v).copied().unwrap_or(0), palette.get(512 + v).copied().unwrap_or(0)]
            }
            3 => [s(0), s(1), s(2)],
            // Stored inverted: 255 = no ink.
            4 => {
                let k = s(3) as u32;
                let m = |v: u8| (v as u32 * k / 255) as u8;
                [m(s(0)), m(s(1)), m(s(2))]
            }
            _ => [s(0); 3],
        };
        out[i * 4..i * 4 + 3].copy_from_slice(&rgb);
        out[i * 4 + 3] = if has_alpha { s(color) } else { 255 };
    }
    Ok((w as u32, h as u32, out))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn packbits_literal_and_run() {
        let mut out = [0u8; 6];
        packbits(&[1, 10, 20, (-2i8) as u8, 7, 0, 9], &mut out);
        assert_eq!(out, [10, 20, 7, 7, 7, 9]);
    }

    fn pcx_header(w: u16, h: u16, bpp: u8, planes: u8, bpl: u16) -> Vec<u8> {
        let mut b = vec![0u8; 128];
        b[0] = 0x0A;
        b[1] = 5;
        b[2] = 1;
        b[3] = bpp;
        b[8..10].copy_from_slice(&(w - 1).to_le_bytes());
        b[10..12].copy_from_slice(&(h - 1).to_le_bytes());
        b[65] = planes;
        b[66..68].copy_from_slice(&bpl.to_le_bytes());
        b
    }

    #[test]
    fn pcx_24_bit_rle() {
        // 2x1, three planes of 2 bytes: R=(9,9) as a run, G=(1,2), B=(3,4) literal.
        let mut b = pcx_header(2, 1, 8, 3, 2);
        b.extend([0xC2, 9, 1, 2, 3, 4]);
        let (w, h, px) = pcx(&b).unwrap();
        assert_eq!((w, h), (2, 1));
        assert_eq!(px, [9, 1, 3, 255, 9, 2, 4, 255]);
    }

    #[test]
    fn pcx_8_bit_palette() {
        let mut b = pcx_header(1, 1, 8, 1, 2);
        b.extend([5, 0]);
        b.push(0x0C);
        let mut pal = vec![0u8; 768];
        pal[15..18].copy_from_slice(&[10, 20, 30]);
        b.extend(pal);
        assert_eq!(pcx(&b).unwrap().2, [10, 20, 30, 255]);
    }

    #[test]
    fn psd_raw_rgb_with_alpha() {
        let mut b = b"8BPS".to_vec();
        b.extend(1u16.to_be_bytes());
        b.extend([0u8; 6]);
        b.extend(4u16.to_be_bytes()); // channels
        b.extend(1u32.to_be_bytes()); // height
        b.extend(2u32.to_be_bytes()); // width
        b.extend(8u16.to_be_bytes());
        b.extend(3u16.to_be_bytes()); // RGB
        b.extend(0u32.to_be_bytes()); // colour mode data
        b.extend(0u32.to_be_bytes()); // resources
        b.extend(0u32.to_be_bytes()); // layers
        b.extend(0u16.to_be_bytes()); // raw
        b.extend([1, 2, 3, 4, 5, 6, 255, 128]); // R R G G B B A A
        let (w, h, px) = psd(&b).unwrap();
        assert_eq!((w, h), (2, 1));
        assert_eq!(px, [1, 3, 5, 255, 2, 4, 6, 128]);
    }
}
