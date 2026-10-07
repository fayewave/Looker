//! What the HDR decoders share: transfer functions, primaries, and the half-float pixels they hand on.
//!
//! An HDR decode ([`super::Colour::Linear`]) is premultiplied RGBA in IEEE half floats, linear, with Rec. 709
//! primaries (scRGB's), 8 bytes a pixel. Under `relative` 1.0 is SDR white (the GPU lifts it to the display's
//! SDR white level, like any SDR photo); otherwise 1.0 is 80 nits (scRGB as it is, a Windows HDR screenshot).
//! They are only made for an HDR display (`super::target`); everywhere else the same files decode to their SDR
//! rendition.

use crate::colour::{f16, srgb_to_linear};

/// BT.2408 reference white: where PQ and HLG put SDR white (a sheet of paper), in nits.
pub const REFERENCE_WHITE: f32 = 203.0;

/// Linear RGB (straight, any range) to the premultiplied half-float pixels of an HDR decode.
pub fn to_half(rgb: &[[f32; 3]], alpha: Option<&[f32]>) -> Vec<u8> {
    let mut out = Vec::with_capacity(rgb.len() * 8);
    for (i, c) in rgb.iter().enumerate() {
        let a = alpha.map_or(1.0, |a| a[i]);
        for v in [c[0] * a, c[1] * a, c[2] * a, a] {
            out.extend_from_slice(&f16(v).to_le_bytes());
        }
    }
    out
}

/// `rgb` clipped to SDR and sRGB-encoded as opaque BGRA (the info card's histogram of an HDR decode).
pub fn sdr_bgra(rgb: &[[f32; 3]]) -> Vec<u8> {
    let enc = |v: f32| {
        let v = v.clamp(0.0, 1.0);
        let e = if v <= 0.003_130_8 { v * 12.92 } else { 1.055 * v.powf(1.0 / 2.4) - 0.055 };
        (e * 255.0).round() as u8
    };
    rgb.iter().flat_map(|c| [enc(c[2]), enc(c[1]), enc(c[0]), 255]).collect()
}

/// The brightest channel value of `rgb`.
pub fn peak(rgb: &[[f32; 3]]) -> f32 {
    rgb.iter().fold(0.0f32, |m, c| m.max(c[0]).max(c[1]).max(c[2]))
}

/// SMPTE ST 2084 (PQ): a 0..1 signal to nits.
pub fn pq_to_nits(e: f32) -> f32 {
    const M1: f32 = 2610.0 / 16384.0;
    const M2: f32 = 2523.0 / 4096.0 * 128.0;
    const C1: f32 = 3424.0 / 4096.0;
    const C2: f32 = 2413.0 / 4096.0 * 32.0;
    const C3: f32 = 2392.0 / 4096.0 * 32.0;
    let p = e.clamp(0.0, 1.0).powf(1.0 / M2);
    10000.0 * ((p - C1).max(0.0) / (C2 - C3 * p)).powf(1.0 / M1)
}

/// BT.2100 HLG: a 0..1 signal to scene light 0..1 (the inverse OETF).
pub fn hlg_to_scene(e: f32) -> f32 {
    const A: f32 = 0.178_832_77;
    const B: f32 = 0.284_668_92;
    const C: f32 = 0.559_910_7;
    let e = e.clamp(0.0, 1.0);
    if e <= 0.5 { e * e / 3.0 } else { (((e - C) / A).exp() + B) / 12.0 }
}

/// HLG scene light (Rec. 2020 RGB) to display nits on BT.2100's nominal 1000-nit display (system gamma 1.2).
pub fn hlg_to_nits(rgb: [f32; 3]) -> [f32; 3] {
    let y = 0.2627 * rgb[0] + 0.6780 * rgb[1] + 0.0593 * rgb[2];
    let k = 1000.0 * y.max(0.0).powf(0.2);
    [rgb[0] * k, rgb[1] * k, rgb[2] * k]
}

pub type Matrix = [[f32; 3]; 3];

pub const IDENTITY: Matrix = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];

/// Linear Rec. 2020 to linear Rec. 709 (both D65).
pub const BT2020_TO_709: Matrix = [[1.6605, -0.5876, -0.0728], [-0.1246, 1.1329, -0.0083], [-0.0182, -0.1006, 1.1187]];

/// Linear Display P3 to linear Rec. 709 (both D65).
pub const P3_TO_709: Matrix = [[1.2249, -0.2247, 0.0], [-0.0420, 1.0419, 0.0], [-0.0197, -0.0786, 1.0979]];

pub fn apply(m: &Matrix, c: [f32; 3]) -> [f32; 3] {
    [
        m[0][0] * c[0] + m[0][1] * c[1] + m[0][2] * c[2],
        m[1][0] * c[0] + m[1][1] * c[1] + m[1][2] * c[2],
        m[2][0] * c[0] + m[2][1] * c[1] + m[2][2] * c[2],
    ]
}

fn mul(a: &Matrix, b: &Matrix) -> Matrix {
    let mut m = [[0.0; 3]; 3];
    for (i, row) in m.iter_mut().enumerate() {
        for (j, v) in row.iter_mut().enumerate() {
            *v = (0..3).map(|k| a[i][k] * b[k][j]).sum();
        }
    }
    m
}

fn invert(m: &Matrix) -> Option<Matrix> {
    let det = m[0][0] * (m[1][1] * m[2][2] - m[1][2] * m[2][1]) - m[0][1] * (m[1][0] * m[2][2] - m[1][2] * m[2][0])
        + m[0][2] * (m[1][0] * m[2][1] - m[1][1] * m[2][0]);
    if det.abs() < 1e-9 {
        return None;
    }
    let d = 1.0 / det;
    Some([
        [(m[1][1] * m[2][2] - m[1][2] * m[2][1]) * d, (m[0][2] * m[2][1] - m[0][1] * m[2][2]) * d, (m[0][1] * m[1][2] - m[0][2] * m[1][1]) * d],
        [(m[1][2] * m[2][0] - m[1][0] * m[2][2]) * d, (m[0][0] * m[2][2] - m[0][2] * m[2][0]) * d, (m[0][2] * m[1][0] - m[0][0] * m[1][2]) * d],
        [(m[1][0] * m[2][1] - m[1][1] * m[2][0]) * d, (m[0][1] * m[2][0] - m[0][0] * m[2][1]) * d, (m[0][0] * m[1][1] - m[0][1] * m[1][0]) * d],
    ])
}

/// sRGB's colorants as an ICC profile states them (D50-adapted), columns R, G, B.
const SRGB_D50: Matrix = [[0.436_074_7, 0.385_064_9, 0.143_080_4], [0.222_504_5, 0.716_878_6, 0.060_616_9], [0.013_932_2, 0.097_104_5, 0.714_173_3]];

/// Linear RGB in an RGB ICC profile's primaries to linear Rec. 709, from its `rXYZ`/`gXYZ`/`bXYZ` colorants
/// (a matrix/TRC profile; anything else gets `None`). The profile's transfer curve is not read: gain-map
/// bases are sRGB-encoded whatever their primaries.
pub fn icc_to_709(icc: &[u8]) -> Option<Matrix> {
    let be32 = |i: usize| icc.get(i..i + 4).map(|b| u32::from_be_bytes([b[0], b[1], b[2], b[3]]));
    let count = be32(128)? as usize;
    let mut cols = [[0.0f32; 3]; 3];
    let mut found = 0;
    for t in 0..count.min(64) {
        let at = 132 + t * 12;
        let sig = icc.get(at..at + 4)?;
        let col = match sig {
            b"rXYZ" => 0,
            b"gXYZ" => 1,
            b"bXYZ" => 2,
            _ => continue,
        };
        let off = be32(at + 4)? as usize;
        if icc.get(off..off + 4)? != b"XYZ " {
            return None;
        }
        for (k, v) in cols[col].iter_mut().enumerate() {
            *v = be32(off + 8 + k * 4)? as i32 as f32 / 65536.0;
        }
        found += 1;
    }
    if found != 3 {
        return None;
    }
    let base = [[cols[0][0], cols[1][0], cols[2][0]], [cols[0][1], cols[1][1], cols[2][1]], [cols[0][2], cols[1][2], cols[2][2]]];
    Some(mul(&invert(&SRGB_D50)?, &base))
}

/// The sRGB transfer curve as a table over 8-bit values.
pub fn srgb_table() -> [f32; 256] {
    std::array::from_fn(|i| srgb_to_linear(i as f32 / 255.0))
}

/// Area-average downscale of straight RGB (+ alpha) from `w x h` to fit `box_w x box_h` (no upscaling; `(0, 0)`
/// keeps the size). Returns the new size.
pub fn fit(rgb: Vec<[f32; 3]>, alpha: Option<Vec<f32>>, w: u32, h: u32, box_w: u32, box_h: u32) -> (u32, u32, Vec<[f32; 3]>, Option<Vec<f32>>) {
    let s = super::wic::fit_scale(w, h, box_w, box_h);
    if s >= 1.0 {
        return (w, h, rgb, alpha);
    }
    let dw = ((w as f64 * s).round() as u32).max(1);
    let dh = ((h as f64 * s).round() as u32).max(1);
    let mut out = vec![[0.0f32; 3]; (dw * dh) as usize];
    let mut out_a = alpha.as_ref().map(|_| vec![0.0f32; (dw * dh) as usize]);
    let (fx, fy) = (w as f64 / dw as f64, h as f64 / dh as f64);
    for oy in 0..dh {
        let (y0, y1) = ((oy as f64 * fy) as u32, (((oy + 1) as f64 * fy).ceil() as u32).min(h));
        for ox in 0..dw {
            let (x0, x1) = ((ox as f64 * fx) as u32, (((ox + 1) as f64 * fx).ceil() as u32).min(w));
            let mut acc = [0.0f32; 4];
            for y in y0..y1 {
                for x in x0..x1 {
                    let i = (y * w + x) as usize;
                    // Weighted by alpha, so transparent pixels don't darken the edges.
                    let a = alpha.as_ref().map_or(1.0, |a| a[i]);
                    acc[0] += rgb[i][0] * a;
                    acc[1] += rgb[i][1] * a;
                    acc[2] += rgb[i][2] * a;
                    acc[3] += a;
                }
            }
            let n = ((y1 - y0) * (x1 - x0)).max(1) as f32;
            let o = (oy * dw + ox) as usize;
            if acc[3] > 0.0 {
                out[o] = [acc[0] / acc[3], acc[1] / acc[3], acc[2] / acc[3]];
            }
            if let Some(a) = &mut out_a {
                a[o] = acc[3] / n;
            }
        }
    }
    (dw, dh, out, out_a)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pq_reference_points() {
        assert!(pq_to_nits(0.0) < 0.001);
        assert!((pq_to_nits(1.0) - 10000.0).abs() < 1.0);
        // 0.58 of the PQ signal is ~203 nits (BT.2408's reference white).
        assert!((pq_to_nits(0.5807) - 203.0).abs() < 2.0);
    }

    #[test]
    fn hlg_reference_points() {
        assert!((hlg_to_scene(0.5) - 1.0 / 12.0).abs() < 1e-5);
        assert!((hlg_to_scene(1.0) - 1.0).abs() < 1e-3);
        // HLG 75 % is BT.2408's reference white: ~203 nits on the 1000-nit display.
        let e = hlg_to_scene(0.75);
        assert!((hlg_to_nits([e, e, e])[0] - 203.0).abs() < 3.0);
    }

    #[test]
    fn srgb_profile_colorants_are_identity() {
        let icc = std::fs::read(r"C:\Windows\System32\spool\drivers\color\sRGB Color Space Profile.icm").unwrap();
        let m = icc_to_709(&icc).unwrap();
        for i in 0..3 {
            for j in 0..3 {
                assert!((m[i][j] - IDENTITY[i][j]).abs() < 0.01, "{m:?}");
            }
        }
    }

    #[test]
    fn white_stays_white_through_the_matrices() {
        for m in [BT2020_TO_709, P3_TO_709] {
            let w = apply(&m, [1.0, 1.0, 1.0]);
            assert!(w.iter().all(|v| (v - 1.0).abs() < 0.01), "{w:?}");
        }
    }

    #[test]
    fn fit_averages_and_keeps_size_when_small() {
        let rgb = vec![[0.0, 0.0, 0.0], [2.0, 2.0, 2.0], [0.0, 0.0, 0.0], [2.0, 2.0, 2.0]];
        let (w, h, out, _) = fit(rgb.clone(), None, 2, 2, 1, 1);
        assert_eq!((w, h), (1, 1));
        assert!((out[0][0] - 1.0).abs() < 1e-6);
        let (w, h, _, _) = fit(rgb, None, 2, 2, 0, 0);
        assert_eq!((w, h), (2, 2));
    }
}
