//! Turning a file into display pixels: sniff the format, then walk that format's decoder chain (port of
//! `DecoderChain.cs` / `DecoderRouter.cs`). WIC goes first wherever Windows can decode the format, because it
//! is fastest and honours the Store codecs; pure-Rust decoders catch the rest. Every failure in a chain is
//! written to the trace so a fallback is never silent (the C# router's swallowed exceptions cost days).

mod animated;
mod avif;
mod fallback;
mod gainmap;
mod hdr;
pub mod heif;
pub mod pdf;
mod raster;
pub mod raw;
pub mod svg;
pub mod wic;

use std::path::Path;

use windows::Win32::Foundation::FILETIME;
use windows::Win32::Graphics::Imaging::IWICImagingFactory;

use crate::colour::{Output, Space};
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
    /// A document's page sizes in 96-dpi pixels (empty for an image), and the render pixels per page pixel
    /// every page of this decode uses (`frames[0]` is page 1 at that scale).
    pub page_sizes: Vec<(f32, f32)>,
    pub page_scale: f64,
    /// Of the first frame, filled in by the decode pool (the info card's histogram).
    pub histogram: Option<Box<crate::metadata::Histogram>>,
    /// What the pixels' values mean.
    pub colour: Colour,
    /// The colour generation (colour.rs) this was decoded for; 0 for anything not decoded for the screen.
    pub generation: u32,
}

/// The colour space a decode's pixels are in.
#[derive(Clone, Debug, PartialEq)]
pub enum Colour {
    /// sRGB (also every file without a profile).
    Srgb,
    /// The file's embedded ICC profile (kept for the GPU under scRGB, see colour.rs).
    Icc(std::sync::Arc<Vec<u8>>),
    /// Adobe RGB by EXIF tag, without a profile.
    AdobeRgb,
    /// Already converted to the monitor's profile.
    Display,
    /// HDR: half-float linear scRGB-primaries pixels (see hdr.rs), 1.0 being SDR white or, when `absolute`,
    /// 80 nits; `peak` is the brightest value in that unit.
    Linear { absolute: bool, peak: f32 },
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
            page_sizes: Vec::new(),
            page_scale: 1.0,
            histogram: None,
            colour: Colour::Srgb,
            generation: 0,
        }
    }
}

thread_local! {
    /// The space the decode running on this thread is for (see `decode_for_screen`).
    static TARGET: std::cell::RefCell<Space> = const { std::cell::RefCell::new(Space::Srgb) };
}

/// The space the decode running on this thread is for.
pub fn target() -> Space {
    TARGET.with(|t| t.borrow().clone())
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
    /// libheif (bundled), when Windows lacks the HEVC or AV1 extension.
    Heif,
    /// libavif (bundled): animated AVIF, which nothing else plays; declines a still one.
    Avif,
    /// LibRaw (bundled), when Windows lacks the Raw Image Extension.
    LibRaw,
    /// A JPEG's HDR gain map, on an HDR display; declines otherwise.
    GainMap,
}

/// Decoders to try, in order. Pure function of the format (unit tested).
fn plan(f: Format) -> &'static [Step] {
    use Step::*;
    match f {
        // A JPEG with an HDR gain map, on an HDR display, has it applied; any other JPEG is declined at once.
        Format::Jpeg => &[GainMap, Wic],
        Format::Bmp | Format::Tiff | Format::Ico | Format::JpegXr => &[Wic],
        // Animated first: it declines a still file, which WIC then takes on the fast path.
        Format::Gif | Format::Png => &[Animated, Wic, Image],
        Format::Webp => &[Animated, Wic, Image],
        // The Raw Image Extension, else the bundled LibRaw; WIC's own pick last (the inbox DNG decoder: only the
        // embedded preview).
        Format::Raw => &[WicRaw, LibRaw, Wic],
        // WIC needs the HEVC (paid) or AV1 Video Extension; the bundled libheif catches the rest. An animated AVIF
        // goes to libavif, which reads only the first bytes of a still one before declining it.
        Format::Heif => &[Wic, Heif],
        Format::Avif => &[Avif, Wic, Heif],
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

fn run(f: &IWICImagingFactory, step: Step, path: &Path, box_w: u32, box_h: u32, still: bool) -> Result<Option<Decoded>, String> {
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
        Step::Heif => fallback::heif(f, path, box_w, box_h).map(Some),
        Step::Avif => avif::decode(f, path, box_w, box_h, still),
        Step::LibRaw => fallback::raw(f, path, box_w, box_h).map(Some),
        Step::GainMap => gainmap::decode(f, path, box_w, box_h),
    }
}

/// `decode` for the screen in `out`'s space: converted to the monitor's profile on an SDR monitor that has one,
/// left in the file's own space under scRGB (the GPU converts it, see gfx.rs).
pub fn decode_for_screen(f: &IWICImagingFactory, path: &Path, box_w: u32, box_h: u32, still: bool, out: &Output) -> Result<Decoded, String> {
    TARGET.with(|t| *t.borrow_mut() = out.space.clone());
    let r = decode(f, path, box_w, box_h, still);
    TARGET.with(|t| *t.borrow_mut() = Space::Srgb);
    let mut d = r?;
    if let Space::Icc(monitor) = &out.space {
        if d.colour != Colour::Display {
            for fr in &mut d.frames {
                if let Err(e) = wic::to_profile(f, &mut fr.pixels, d.width, d.height, &d.colour, monitor) {
                    crate::trace::mark(format!("monitor profile conversion failed: {}", e.message()));
                    break;
                }
            }
            d.colour = Colour::Display;
        }
    }
    d.generation = out.generation;
    Ok(d)
}

/// Decodes `path` to fit `box_w × box_h` device pixels (`(0, 0)` = full size), in sRGB unless called through
/// `decode_for_screen`. `still` asks for the first frame only (the placeholder tier), skipping the animation
/// decoders.
pub fn decode(f: &IWICImagingFactory, path: &Path, box_w: u32, box_h: u32, still: bool) -> Result<Decoded, String> {
    let fmt = format::sniff_file(path);
    // LOOKER_SKIP_WIC=1 takes the bundled decoders even where Windows has the codec (testing the fallbacks).
    static SKIP_WIC: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    let skip_wic = *SKIP_WIC.get_or_init(|| std::env::var_os("LOOKER_SKIP_WIC").is_some());
    let steps: Vec<Step> = plan(fmt)
        .iter()
        .copied()
        .filter(|s| !(still && *s == Step::Animated) && !(skip_wic && matches!(s, Step::Wic | Step::WicRaw) && plan(fmt).len() > 1))
        .collect();
    if steps.is_empty() {
        return Err(format!("no decoder for {fmt:?}"));
    }
    let mut errors = Vec::new();
    for &step in &steps {
        match run(f, step, path, box_w, box_h, still) {
            Ok(Some(mut d)) => {
                d.format = fmt;
                if !errors.is_empty() {
                    crate::trace::mark(format!("decoded by {step:?} after: {}", errors.join("; ")));
                }
                return Ok(d);
            }
            Ok(None) => {} // declined (a still image offered to an animation decoder)
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

    /// Decodes `LOOKER_DECODE` for each screen space and prints the colour and the first pixel.
    /// `LOOKER_DECODE=x.png cargo test --release -- --ignored --nocapture colour_paths`
    #[test]
    #[ignore]
    fn colour_paths() {
        use windows::Win32::Graphics::Imaging::CLSID_WICImagingFactory2;
        use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx};
        let p = std::path::PathBuf::from(std::env::var("LOOKER_DECODE").expect("set LOOKER_DECODE"));
        let f: IWICImagingFactory = unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
            CoCreateInstance(&CLSID_WICImagingFactory2, None, CLSCTX_INPROC_SERVER).unwrap()
        };
        let pro = std::fs::read(r"C:\Windows\System32\spool\drivers\color\ProPhoto.icm").unwrap();
        for space in [Space::Srgb, Space::Icc(std::sync::Arc::new(pro)), Space::Scrgb { white_nits: 80.0, hdr: false }] {
            let out = Output { space: space.clone(), generation: 7 };
            let d = decode_for_screen(&f, &p, 400, 400, true, &out).unwrap();
            let c = match &d.colour {
                Colour::Icc(b) => format!("Icc({} bytes)", b.len()),
                c => format!("{c:?}"),
            };
            println!("{} -> {c}, first pixel BGRA {:?}", crate::colour::describe(&space), &d.frames[0].pixels[..4]);
        }
    }

    #[test]
    fn raw_asks_for_the_raw_extension_first() {
        assert_eq!(plan(Format::Raw)[0], Step::WicRaw);
    }

    #[test]
    fn raw_tries_libraw_before_the_inbox_preview() {
        assert_eq!(plan(Format::Raw), &[Step::WicRaw, Step::LibRaw, Step::Wic]);
    }

    #[test]
    fn animatable_formats_try_animation_first() {
        for f in [Format::Gif, Format::Png, Format::Webp] {
            assert_eq!(plan(f)[0], Step::Animated);
        }
    }
}
