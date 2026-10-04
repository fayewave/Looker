//! Animated GIF, APNG and animated WebP through the `image` crate, whose frame iterators hand back fully
//! composited frames (disposal and blending already applied). Frames are scaled to the box like a still
//! image; an animation never decodes past its fit size, since full resolution × N frames explodes memory.

use std::fs::File;
use std::io::BufReader;
use std::path::Path;

use image::AnimationDecoder;
use windows::Win32::Graphics::Imaging::IWICImagingFactory;

use super::{Decoded, Frame, wic};
use crate::format::{self, Format};

/// Frames past this much memory are dropped (the animation plays the part that fits).
const BUDGET_BYTES: usize = 384 << 20;

/// Browsers treat 0–10 ms frame delays as "as fast as allowed" and play them at 100 ms; GIFs in the wild rely
/// on it.
fn delay_ms(d: image::Delay) -> u32 {
    let (n, den) = d.numer_denom_ms();
    let ms = if den == 0 { 0 } else { n / den };
    if ms <= 10 { 100 } else { ms }
}

fn open(path: &Path) -> Result<BufReader<File>, String> {
    File::open(path).map(BufReader::new).map_err(|e| e.to_string())
}

/// `Ok(None)` when the file is a still image (WIC then decodes it on the fast path).
pub fn decode(f: &IWICImagingFactory, path: &Path, box_w: u32, box_h: u32) -> Result<Option<Decoded>, String> {
    let e = |e: image::ImageError| e.to_string();
    let frames: image::Frames = match format::sniff_file(path) {
        Format::Gif => image::codecs::gif::GifDecoder::new(open(path)?).map_err(e)?.into_frames(),
        Format::Png => {
            let dec = image::codecs::png::PngDecoder::new(open(path)?).map_err(e)?;
            if !dec.is_apng().map_err(e)? {
                return Ok(None);
            }
            dec.apng().map_err(e)?.into_frames()
        }
        Format::Webp => {
            let dec = image::codecs::webp::WebPDecoder::new(open(path)?).map_err(e)?;
            if !dec.has_animation() {
                return Ok(None);
            }
            dec.into_frames()
        }
        _ => return Ok(None),
    };
    // Animations stay at the fit size even when asked for full resolution.
    let (box_w, box_h) = if box_w == 0 || box_h == 0 { (2048, 2048) } else { (box_w, box_h) };
    let mut out: Vec<Frame> = Vec::new();
    let (mut w, mut h, mut nw, mut nh) = (0, 0, 0, 0);
    let mut used = 0usize;
    for frame in frames {
        let frame = frame.map_err(e)?;
        let delay = delay_ms(frame.delay());
        let buf = frame.into_buffer();
        let (fw, fh) = buf.dimensions();
        let (dw, dh, px) = wic::from_rgba(f, buf.as_raw(), fw, fh, box_w, box_h).map_err(|e| e.message().to_string())?;
        if out.is_empty() {
            (w, h, nw, nh) = (dw, dh, fw, fh);
        } else if (dw, dh) != (w, h) {
            continue; // every frame is canvas-sized; anything else is malformed
        }
        used += px.len();
        out.push(Frame { pixels: px, delay_ms: delay });
        if used > BUDGET_BYTES {
            crate::trace::mark(format!("animation truncated at {} frames (memory budget)", out.len()));
            break;
        }
    }
    if out.is_empty() {
        return Err("no frames".into());
    }
    if out.len() == 1 {
        // A one-frame GIF is a still image; it's decoded already, so keep it.
        out[0].delay_ms = 0;
    }
    Ok(Some(Decoded { width: w, height: h, frames: out, native_width: nw, native_height: nh, format: Format::Unknown, taken: None, vector: false, pages: 0, page_sizes: Vec::new(), page_scale: 1.0, histogram: None }))
}
