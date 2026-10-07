//! JPEGs with an HDR gain map (Adobe's gain map / Android's Ultra HDR: phones, Lightroom's HDR export). The file
//! is an ordinary SDR JPEG followed by a second, small JPEG (found through the MPF index) whose pixels say how
//! much brighter each part of the photo is in its HDR rendition; XMP (`hdrgm:`) says how to read them. On an
//! HDR display the two are combined into an HDR decode ([`super::hdr`]), boosted only as far as the display can
//! go above SDR white; everywhere else the gain map is skipped and the JPEG decodes as usual.
//!
//! The XMP form is read (every writer so far also writes it); ISO 21496-1's binary metadata alone is not.

use std::path::Path;

use windows::Win32::Graphics::Imaging::*;

use super::hdr::{self, Matrix};
use super::{Colour, Decoded, wic};
use crate::colour::Space;

/// How to read a gain map, per channel (one value repeated for a single-channel map). Logs are base 2.
#[derive(Clone, Debug, PartialEq)]
pub struct Params {
    pub gain_min: [f32; 3],
    pub gain_max: [f32; 3],
    pub gamma: [f32; 3],
    pub offset_sdr: [f32; 3],
    pub offset_hdr: [f32; 3],
    pub capacity_min: f32,
    pub capacity_max: f32,
}

impl Params {
    /// How much of the full gain a display with `headroom` (peak / SDR white) gets, 0..1.
    pub fn weight(&self, headroom: f32) -> f32 {
        let h = headroom.max(1.0).log2();
        if self.capacity_max <= self.capacity_min {
            return if h >= self.capacity_max { 1.0 } else { 0.0 };
        }
        ((h - self.capacity_min) / (self.capacity_max - self.capacity_min)).clamp(0.0, 1.0)
    }

    /// The linear boost for each 8-bit gain-map value, per channel, at `weight`.
    fn tables(&self, weight: f32) -> [[f32; 256]; 3] {
        std::array::from_fn(|c| {
            std::array::from_fn(|v| {
                let g = (v as f32 / 255.0).powf(1.0 / self.gamma[c]);
                let log = self.gain_min[c] + (self.gain_max[c] - self.gain_min[c]) * g;
                (log * weight).exp2()
            })
        })
    }
}

/// The JPEG's marker segments before its scan: (marker, payload).
fn segments(jpeg: &[u8]) -> Vec<(u8, &[u8])> {
    let mut out = Vec::new();
    if jpeg.get(..2) != Some(&[0xFF, 0xD8]) {
        return out;
    }
    let mut i = 2;
    while i + 4 <= jpeg.len() && jpeg[i] == 0xFF {
        let m = jpeg[i + 1];
        if m == 0xFF {
            i += 1; // fill byte
            continue;
        }
        if m == 0xDA || m == 0xD9 {
            break;
        }
        let len = u16::from_be_bytes([jpeg[i + 2], jpeg[i + 3]]) as usize;
        let Some(payload) = jpeg.get(i + 4..i + 2 + len) else { break };
        out.push((m, payload));
        i += 2 + len;
    }
    out
}

const XMP: &[u8] = b"http://ns.adobe.com/xap/1.0/\0";

fn xmp(jpeg: &[u8]) -> Option<String> {
    segments(jpeg).into_iter().find(|(m, p)| *m == 0xE1 && p.starts_with(XMP)).map(|(_, p)| String::from_utf8_lossy(&p[XMP.len()..]).into_owned())
}

/// Byte offset and length of the second image in the MPF index (APP2 `MPF\0`).
fn mpf_second(jpeg: &[u8]) -> Option<(usize, usize)> {
    // The offsets in the index count from the TIFF header that follows `MPF\0` (past the EXIF and any
    // extended XMP: ~84 KB into a Pixel's photo).
    let (_, payload) = segments(jpeg).into_iter().find(|(m, p)| *m == 0xE2 && p.starts_with(b"MPF\0"))?;
    let base = payload.as_ptr() as usize - jpeg.as_ptr() as usize + 4;
    let t = jpeg.get(base..)?;
    let le = match t.get(..2)? {
        b"II" => true,
        b"MM" => false,
        _ => return None,
    };
    let u16_at = |i: usize| t.get(i..i + 2).map(|b| if le { u16::from_le_bytes([b[0], b[1]]) } else { u16::from_be_bytes([b[0], b[1]]) });
    let u32_at = |i: usize| t.get(i..i + 4).map(|b| if le { u32::from_le_bytes([b[0], b[1], b[2], b[3]]) } else { u32::from_be_bytes([b[0], b[1], b[2], b[3]]) });
    let ifd = u32_at(4)? as usize;
    let n = u16_at(ifd)? as usize;
    for k in 0..n.min(32) {
        let e = ifd + 2 + k * 12;
        if u16_at(e)? == 0xB002 {
            // MP entries, 16 bytes each: attributes, size, offset, two dependent-image entries.
            let count = u32_at(e + 4)? as usize;
            let at = u32_at(e + 8)? as usize;
            if count < 32 {
                return None;
            }
            let size = u32_at(at + 16 + 4)? as usize;
            let off = u32_at(at + 16 + 8)? as usize;
            return (off > 0 && size > 0).then_some((base + off, size));
        }
    }
    None
}

/// The values of `hdrgm:<name>`: an attribute, an element, or an element holding an `rdf:Seq` (per channel).
fn values(xmp: &str, name: &str) -> Option<Vec<f32>> {
    let attr = format!("hdrgm:{name}=\"");
    if let Some(i) = xmp.find(&attr) {
        let rest = &xmp[i + attr.len()..];
        return rest[..rest.find('"')?].trim().parse().ok().map(|v| vec![v]);
    }
    let open = format!("<hdrgm:{name}>");
    let i = xmp.find(&open)? + open.len();
    let body = &xmp[i..i + xmp[i..].find(&format!("</hdrgm:{name}>"))?];
    if body.contains("<rdf:li") {
        let v: Vec<f32> = body.split("<rdf:li").skip(1).filter_map(|li| li.split_once('>')?.1.split('<').next()?.trim().parse().ok()).collect();
        return (!v.is_empty()).then_some(v);
    }
    body.trim().parse().ok().map(|v| vec![v])
}

/// The gain map's parameters from its XMP; `None` without them or when the base is the HDR rendition.
pub fn params(xmp: &str) -> Option<Params> {
    if !xmp.contains("hdrgm:") {
        return None;
    }
    let base_is_hdr = xmp.contains("hdrgm:BaseRenditionIsHDR=\"True\"") || xmp.contains("<hdrgm:BaseRenditionIsHDR>True<");
    if base_is_hdr {
        return None;
    }
    let three = |name: &str, default: f32| -> [f32; 3] {
        match values(xmp, name) {
            Some(v) if v.len() >= 3 => [v[0], v[1], v[2]],
            Some(v) if !v.is_empty() => [v[0]; 3],
            _ => [default; 3],
        }
    };
    let gain_max = three("GainMapMax", f32::NAN);
    if gain_max[0].is_nan() {
        return None; // the one value without a default
    }
    let capacity_min = values(xmp, "HDRCapacityMin").map_or(0.0, |v| v[0]);
    let capacity_max = values(xmp, "HDRCapacityMax").map_or(gain_max[0], |v| v[0]);
    let gamma = three("Gamma", 1.0).map(|g| if g > 0.0 { g } else { 1.0 });
    Some(Params {
        gain_min: three("GainMapMin", 0.0),
        gain_max,
        gamma,
        offset_sdr: three("OffsetSDR", 1.0 / 64.0),
        offset_hdr: three("OffsetHDR", 1.0 / 64.0),
        capacity_min,
        capacity_max,
    })
}

/// `Ok(None)` unless the display is HDR and the JPEG has a gain map Looker can read.
pub fn decode(f: &IWICImagingFactory, path: &Path, box_w: u32, box_h: u32) -> Result<Option<Decoded>, String> {
    let headroom = match super::target() {
        s @ Space::Scrgb { hdr: true, .. } => s.headroom(),
        _ => return Ok(None),
    };
    if headroom <= 1.0 {
        return Ok(None);
    }
    let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
    let primary_xmp = xmp(&bytes).unwrap_or_default();
    if !primary_xmp.contains("hdrgm") {
        return Ok(None);
    }
    let Some((at, len)) = mpf_second(&bytes) else { return Ok(None) };
    let Some(gain_jpeg) = bytes.get(at..at + len) else { return Ok(None) };
    let Some(p) = xmp(gain_jpeg).and_then(|x| params(&x)).or_else(|| params(&primary_xmp)) else { return Ok(None) };
    let weight = p.weight(headroom);
    if weight <= 0.0 {
        return Ok(None);
    }

    let e = |e: windows::core::Error| e.message().to_string();
    let source = wic::Source::Memory(&bytes);
    let mut base = wic::decode(f, source, box_w, box_h).map_err(e)?;
    let (w, h) = (base.width, base.height);
    let orientation = wic::orientation_of(f, &wic::Source::Memory(&bytes));
    let gain = gain_pixels(f, gain_jpeg, w, h, orientation).map_err(e)?;

    let to_709: Matrix = match &base.colour {
        Colour::Icc(icc) => hdr::icc_to_709(icc).unwrap_or(hdr::IDENTITY),
        _ => hdr::IDENTITY,
    };
    let lin = hdr::srgb_table();
    let boost = p.tables(weight);
    let px = &base.frames[0].pixels;
    let rgb: Vec<[f32; 3]> = px
        .chunks_exact(4)
        .zip(gain.chunks_exact(4))
        .map(|(b, g)| {
            // BGRA, opaque: premultiplied is straight.
            let c = [2, 1, 0].map(|k| lin[b[k] as usize]);
            let gm = [g[2], g[1], g[0]];
            let hdr_c: [f32; 3] = std::array::from_fn(|i| (c[i] + p.offset_sdr[i]) * boost[i][gm[i] as usize] - p.offset_hdr[i]);
            hdr::apply(&to_709, hdr_c)
        })
        .collect();
    let peak = hdr::peak(&rgb);
    crate::trace::mark(format!("gain map: weight {weight:.2} for headroom {headroom:.2}, peak {peak:.2}x SDR white"));
    base.histogram = Some(Box::new(crate::metadata::Histogram::of(px)));
    base.frames[0].pixels = hdr::to_half(&rgb, None);
    base.colour = Colour::Linear { absolute: false, peak };
    Ok(Some(base))
}

/// The gain map at the base's decoded size `w x h` and orientation, as BGRA (a grey map in all three).
fn gain_pixels(f: &IWICImagingFactory, jpeg: &[u8], w: u32, h: u32, orientation: u16) -> windows::core::Result<Vec<u8>> {
    unsafe {
        let stream = f.CreateStream()?;
        stream.InitializeFromMemory(jpeg)?;
        let dec = f.CreateDecoderFromStream(&stream, std::ptr::null(), WICDecodeMetadataCacheOnDemand)?;
        let frame = dec.GetFrame(0)?;
        let (transform, swaps) = wic::orientation_transform(orientation);
        let (sw, sh) = if swaps { (h, w) } else { (w, h) };
        let scaler = f.CreateBitmapScaler()?;
        scaler.Initialize(&frame, sw, sh, WICBitmapInterpolationModeHighQualityCubic)?;
        let mut src: IWICBitmapSource = f.CreateBitmapFromSource(&scaler, WICBitmapCacheOnLoad)?.into();
        if transform != WICBitmapTransformRotate0 {
            let rot = f.CreateBitmapFlipRotator()?;
            rot.Initialize(&src, transform)?;
            src = rot.into();
        }
        let conv = f.CreateFormatConverter()?;
        conv.Initialize(&src, &GUID_WICPixelFormat32bppBGRA, WICBitmapDitherTypeNone, None, 0.0, WICBitmapPaletteTypeCustom)?;
        let mut px = vec![0u8; (w * h * 4) as usize];
        conv.CopyPixels(std::ptr::null(), w * 4, &mut px)?;
        Ok(px)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_attribute_and_sequence_forms() {
        let attrs = r#"<rdf:Description hdrgm:Version="1.0" hdrgm:GainMapMin="-0.5" hdrgm:GainMapMax="2.5" hdrgm:Gamma="1" hdrgm:HDRCapacityMax="2.5"/>"#;
        let p = params(attrs).unwrap();
        assert_eq!(p.gain_min, [-0.5; 3]);
        assert_eq!(p.gain_max, [2.5; 3]);
        assert_eq!(p.offset_sdr, [1.0 / 64.0; 3]);
        assert_eq!((p.capacity_min, p.capacity_max), (0.0, 2.5));
        let seq = "<hdrgm:GainMapMax><rdf:Seq><rdf:li>1</rdf:li><rdf:li>2</rdf:li><rdf:li>3</rdf:li></rdf:Seq></hdrgm:GainMapMax>";
        assert_eq!(params(seq).unwrap().gain_max, [1.0, 2.0, 3.0]);
        assert!(params(r#"hdrgm:GainMapMax="2" hdrgm:BaseRenditionIsHDR="True""#).is_none());
        assert!(params("no gain map here").is_none());
    }

    #[test]
    fn weight_follows_the_display() {
        let p = params(r#"hdrgm:GainMapMax="3" hdrgm:HDRCapacityMin="0" hdrgm:HDRCapacityMax="2""#).unwrap();
        assert_eq!(p.weight(1.0), 0.0);
        assert!((p.weight(2.0) - 0.5).abs() < 1e-6);
        assert_eq!(p.weight(4.0), 1.0);
        assert_eq!(p.weight(16.0), 1.0);
    }

    #[test]
    fn tables_span_min_to_max() {
        let p = params(r#"hdrgm:GainMapMin="0" hdrgm:GainMapMax="2""#).unwrap();
        let t = p.tables(1.0);
        assert!((t[0][0] - 1.0).abs() < 1e-6);
        assert!((t[0][255] - 4.0).abs() < 1e-4);
        assert!((p.tables(0.5)[0][255] - 2.0).abs() < 1e-4);
    }

    /// `LOOKER_DECODE=<gain-map JPEG> cargo test --release -- --ignored --nocapture gain_map_file`
    #[test]
    #[ignore]
    fn gain_map_file() {
        let bytes = std::fs::read(std::env::var("LOOKER_DECODE").unwrap()).unwrap();
        println!("primary XMP has hdrgm: {}", xmp(&bytes).is_some_and(|x| x.contains("hdrgm")));
        let (at, len) = mpf_second(&bytes).expect("MPF second image");
        println!("gain map at {at}, {len} bytes, starts {:02X?}", &bytes[at..at + 4]);
        let gx = xmp(&bytes[at..at + len]).unwrap_or_default();
        println!("{:?}", params(&gx).or_else(|| params(&xmp(&bytes).unwrap_or_default())));
    }
}
