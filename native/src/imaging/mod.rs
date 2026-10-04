//! Turning a file into display pixels: sniff the format, then walk that format's decoder chain (port of
//! `DecoderChain.cs` / `DecoderRouter.cs`). WIC goes first wherever Windows can decode the format, because it
//! is fastest and honours the Store codecs; pure-Rust decoders catch the rest. Every failure in a chain is
//! written to the trace so a fallback is never silent (the C# router's swallowed exceptions cost days).

mod animated;
mod fallback;
mod pdf;
mod raster;
pub mod svg;
pub mod wic;

use std::path::Path;

use windows::Win32::Foundation::FILETIME;
use windows::Win32::Graphics::Imaging::IWICImagingFactory;

use crate::format::{self, Format};

/// Largest bitmap edge we ever make (also Direct2D's usual maximum texture size).
pub const MAX_EDGE: u32 = 16384;
/// Vectors (SVG) re-rasterize sharp on zoom, up to this edge per bitmap.
pub const VECTOR_MAX_EDGE: u32 = 4096;

pub struct Frame {
    /// Premultiplied BGRA, `width * 4` bytes per row.
    pub pixels: Vec<u8>,
    pub delay_ms: u32,
}

pub struct Decoded {
    pub width: u32,
    pub height: u32,
    /// One frame for a still image; every frame of an animation, at the same size.
    pub frames: Vec<Frame>,
    /// Oriented size of the full-resolution image (an SVG's intrinsic size).
    pub native_width: u32,
    pub native_height: u32,
    pub format: Format,
    pub taken: Option<FILETIME>,
    /// Re-rasterizes at any size (SVG, PDF): zoom caps at `VECTOR_MAX_EDGE`, not the native size.
    pub vector: bool,
    /// Page count of a document (PDF); 0 for an image.
    pub pages: u32,
    /// Of the first frame, filled in by the decode pool (the info card's histogram).
    pub histogram: Option<Box<crate::metadata::Histogram>>,
}

impl Decoded {
    pub fn still(width: u32, height: u32, pixels: Vec<u8>, native_width: u32, native_height: u32, taken: Option<FILETIME>) -> Decoded {
        Decoded {
            width,
            height,
            frames: vec![Frame { pixels, delay_ms: 0 }],
            native_width,
            native_height,
            format: Format::Unknown,
            taken,
            vector: false,
            pages: 0,
            histogram: None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Step {
    /// WIC, letting it pick the codec.
    Wic,
    /// WIC with the Raw Image Extension by CLSID.
    WicRaw,
    /// WIC's ICO decoder on a cursor whose header says "icon".
    WicCursor,
    Animated,
    Svg,
    Image,
    Psd,
    JpegXl,
    Jpeg2000,
    Pcx,
    Xcf,
    Pdf,
}

/// Decoders to try, in order. Pure function of the format (unit tested).
fn plan(f: Format) -> &'static [Step] {
    use Step::*;
    match f {
        Format::Jpeg | Format::Bmp | Format::Tiff | Format::Ico | Format::JpegXr => &[Wic],
        // Animated first: it declines a still file, which WIC then takes on the fast path.
        Format::Gif | Format::Png => &[Animated, Wic, Image],
        Format::Webp => &[Animated, Wic, Image],
        Format::Raw => &[WicRaw, Wic],
        Format::Heif | Format::Avif => &[Wic],
        Format::Cur => &[WicCursor],
        Format::Svg => &[Svg],
        Format::Psd => &[Wic, Psd],
        Format::JpegXl => &[Wic, JpegXl],
        Format::Jpeg2000 => &[Wic, Jpeg2000],
        Format::Tga | Format::Qoi | Format::Pnm => &[Image],
        Format::Dds => &[Wic, Image],
        Format::Pcx => &[Pcx],
        Format::Xcf => &[Xcf],
        // Magick can't read PDF without Ghostscript; the inbox renderer is the only option.
        Format::Pdf => &[Pdf],
        Format::Unknown => &[],
    }
}

fn run(f: &IWICImagingFactory, step: Step, path: &Path, box_w: u32, box_h: u32) -> Result<Option<Decoded>, String> {
    let e = |e: windows::core::Error| e.message().to_string();
    match step {
        Step::Wic => wic::decode(f, wic::Source::File(path), box_w, box_h).map(Some).map_err(e),
        Step::WicRaw => wic::decode(f, wic::Source::FileWith(path, wic::RAW_IMAGE_DECODER), box_w, box_h).map(Some).map_err(e),
        Step::WicCursor => {
            let mut bytes = std::fs::read(path).map_err(|e| e.to_string())?;
            if bytes.len() < 6 {
                return Err("truncated".into());
            }
            // CUR and ICO share the directory layout; only the type word differs (hotspots sit in the
            // planes/bit-count fields, which the embedded DIB/PNG headers make redundant).
            bytes[2] = 1;
            wic::decode(f, wic::Source::Memory(&bytes), box_w, box_h).map(Some).map_err(e)
        }
        Step::Animated => animated::decode(f, path, box_w, box_h),
        Step::Svg => svg::decode(f, path, box_w, box_h).map(Some),
        Step::Image => fallback::image(f, path, box_w, box_h).map(Some),
        Step::Psd => fallback::psd(f, path, box_w, box_h).map(Some),
        Step::JpegXl => fallback::jpeg_xl(f, path, box_w, box_h).map(Some),
        Step::Jpeg2000 => fallback::jpeg_2000(f, path, box_w, box_h).map(Some),
        Step::Pcx => fallback::pcx(f, path, box_w, box_h).map(Some),
        Step::Xcf => fallback::xcf(f, path, box_w, box_h).map(Some),
        Step::Pdf => pdf::decode(f, path, box_w, box_h).map(Some),
    }
}

/// Decodes `path` to fit `box_w × box_h` device pixels (`(0, 0)` = full size). `still` asks for the first
/// frame only (the placeholder tier), skipping the animation decoders.
pub fn decode(f: &IWICImagingFactory, path: &Path, box_w: u32, box_h: u32, still: bool) -> Result<Decoded, String> {
    let fmt = format::sniff_file(path);
    let steps: Vec<Step> = plan(fmt).iter().copied().filter(|s| !(still && *s == Step::Animated)).collect();
    if steps.is_empty() {
        return Err(format!("no decoder for {fmt:?}"));
    }
    let mut errors = Vec::new();
    for &step in &steps {
        match run(f, step, path, box_w, box_h) {
            Ok(Some(mut d)) => {
                d.format = fmt;
                if !errors.is_empty() {
                    crate::trace::mark(format!("decoded by {step:?} after: {}", errors.join("; ")));
                }
                return Ok(d);
            }
            Ok(None) => {} // declined (a still image offered to the animated decoder)
            Err(e) => errors.push(format!("{step:?}: {e}")),
        }
    }
    Err(errors.join("; "))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_supported_format_has_a_decoder() {
        for f in format::ALL {
            assert!(!plan(*f).is_empty(), "{f:?}");
        }
    }

    /// Decodes every file in `Pictures\Looker Test Photos` (written by scripts/New-TestImages.ps1 plus the
    /// real RAW/HEIC samples) and prints what each became. `cargo test --release -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn decodes_the_test_photos() {
        use windows::Win32::Graphics::Imaging::CLSID_WICImagingFactory2;
        use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx};
        let dir = std::path::PathBuf::from(std::env::var("USERPROFILE").unwrap()).join("Pictures").join("Looker Test Photos");
        let f: IWICImagingFactory = unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
            CoCreateInstance(&CLSID_WICImagingFactory2, None, CLSCTX_INPROC_SERVER).unwrap()
        };
        let mut paths: Vec<_> = std::fs::read_dir(&dir).unwrap().flatten().map(|e| e.path()).filter(|p| format::is_supported(p)).collect();
        paths.sort();
        let mut failed = Vec::new();
        for p in &paths {
            let t = std::time::Instant::now();
            let name = p.file_name().unwrap().to_string_lossy().into_owned();
            match decode(&f, p, 1200, 800, false) {
                Ok(d) => println!(
                    "ok   {name:<44} {:?} {}x{} native {}x{} frames {} {:.0} ms",
                    d.format,
                    d.width,
                    d.height,
                    d.native_width,
                    d.native_height,
                    d.frames.len(),
                    t.elapsed().as_secs_f64() * 1000.0
                ),
                Err(e) => {
                    println!("FAIL {name:<44} {e}");
                    failed.push(name);
                }
            }
        }
        println!("{} of {} decoded; failed: {failed:?}", paths.len() - failed.len(), paths.len());
    }

    /// Decodes the one file named by `LOOKER_DECODE` at a fit box and at full size, printing timings.
    #[test]
    #[ignore]
    fn decodes_one_file() {
        use windows::Win32::Graphics::Imaging::CLSID_WICImagingFactory2;
        use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx};
        let p = std::path::PathBuf::from(std::env::var("LOOKER_DECODE").expect("set LOOKER_DECODE"));
        let f: IWICImagingFactory = unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
            CoCreateInstance(&CLSID_WICImagingFactory2, None, CLSCTX_INPROC_SERVER).unwrap()
        };
        for (bw, bh) in [(1280, 1280), (0, 0)] {
            let t = std::time::Instant::now();
            match decode(&f, &p, bw, bh, false) {
                Ok(d) => println!("box {bw}x{bh}: {}x{} native {}x{} in {:.0} ms", d.width, d.height, d.native_width, d.native_height, t.elapsed().as_secs_f64() * 1000.0),
                Err(e) => println!("box {bw}x{bh}: FAIL {e}"),
            }
        }
    }

    #[test]
    #[ignore]
    fn raw_preview_vs_full_brightness() {
        use windows::Win32::Graphics::Imaging::CLSID_WICImagingFactory2;
        use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx};
        let dir = std::path::PathBuf::from(std::env::var("USERPROFILE").unwrap()).join("Pictures").join("Looker Test Photos");
        let f: IWICImagingFactory = unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
            CoCreateInstance(&CLSID_WICImagingFactory2, None, CLSCTX_INPROC_SERVER).unwrap()
        };
        let mean = |d: &Decoded| {
            let p = &d.frames[0].pixels;
            p.chunks_exact(4).map(|c| (c[0] as u64 + c[1] as u64 + c[2] as u64) / 3).sum::<u64>() / (p.len() as u64 / 4)
        };
        for name in ["74 RAW MEF Mamiya ZD.mef", "62 RAW SRW Samsung EX1.srw", "45 DNG DJI drone.dng"] {
            let p = dir.join(name);
            let fit = decode(&f, &p, 1200, 800, false).unwrap();
            let full = decode(&f, &p, 0, 0, false).unwrap();
            println!("{name}: fit {}x{} mean {}  full {}x{} mean {}", fit.width, fit.height, mean(&fit), full.width, full.height, mean(&full));
        }
    }

    #[test]
    fn raw_asks_for_the_raw_extension_first() {
        assert_eq!(plan(Format::Raw)[0], Step::WicRaw);
    }

    #[test]
    fn animatable_formats_try_animation_first() {
        for f in [Format::Gif, Format::Png, Format::Webp] {
            assert_eq!(plan(f)[0], Step::Animated);
        }
    }
}
