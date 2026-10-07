//! Animated AVIF (an AV1 image sequence, brand `avis`) through libavif with dav1d. Neither WIC (the AV1 Video
//! Extension rejects sequences) nor libheif (no primary item) plays them. A still AVIF is declined, before the
//! DLL is even loaded, and WIC or libheif take it on their usual path. Like heif.rs, `codecs\avif.dll` is loaded
//! on first use and its functions looked up by name. Frames are scaled to the box as in animated.rs.
//!
//! The structs below are the leading fields of libavif 1.4's `avifDecoder` and its `avifRGBImage` / `avifImageTiming`
//! (include/avif/avif.h); the decoder is only ever read through libavif's own pointer, so a prefix is enough.

use std::ffi::{CStr, c_char, c_int, c_void};
use std::sync::OnceLock;

use windows::Win32::Graphics::Imaging::IWICImagingFactory;
use windows::Win32::System::LibraryLoader::{GetProcAddress, LOAD_LIBRARY_SEARCH_DEFAULT_DIRS, LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR, LoadLibraryExW};
use windows::core::{HSTRING, PCSTR};

use super::{Decoded, Frame, wic};
use crate::format::Format;

/// Frames past this much memory are dropped (the animation plays the part that fits), as in animated.rs.
const BUDGET_BYTES: usize = 384 << 20;

const RESULT_OK: c_int = 0;
const RGB_FORMAT_RGBA: c_int = 1;

type Image = c_void;

/// The leading fields of libavif 1.4's `avifImage`, up to its colour description.
#[repr(C)]
struct ImagePrefix {
    width: u32,
    height: u32,
    depth: u32,
    yuv_format: c_int,
    yuv_range: c_int,
    yuv_chroma_sample_position: c_int,
    yuv_planes: [*mut u8; 3],
    yuv_row_bytes: [u32; 3],
    image_owns_yuv_planes: c_int,
    alpha_plane: *mut u8,
    alpha_row_bytes: u32,
    image_owns_alpha_plane: c_int,
    alpha_premultiplied: c_int,
    icc_data: *mut u8,
    icc_size: usize,
    color_primaries: u16,
    transfer_characteristics: u16,
    matrix_coefficients: u16,
}

const PRIMARIES_BT2020: u16 = 9;
const PRIMARIES_P3: u16 = 12;
const TRANSFER_PQ: u16 = 16;
const TRANSFER_HLG: u16 = 18;

#[repr(C)]
struct DecoderPrefix {
    codec_choice: c_int,
    max_threads: c_int,
    requested_source: c_int,
    allow_progressive: c_int,
    allow_incremental: c_int,
    ignore_exif: c_int,
    ignore_xmp: c_int,
    image_size_limit: u32,
    image_dimension_limit: u32,
    image_count_limit: u32,
    strict_flags: u32,
    image: *mut Image,
    image_index: c_int,
    image_count: c_int,
}

#[repr(C)]
struct RgbImage {
    width: u32,
    height: u32,
    depth: u32,
    format: c_int,
    chroma_upsampling: c_int,
    chroma_downsampling: c_int,
    avoid_libyuv: c_int,
    ignore_alpha: c_int,
    alpha_premultiplied: c_int,
    is_float: c_int,
    max_threads: c_int,
    pixels: *mut u8,
    row_bytes: u32,
}

#[repr(C)]
#[derive(Default)]
struct Timing {
    timescale: u64,
    pts: f64,
    pts_in_timescales: u64,
    duration: f64,
    duration_in_timescales: u64,
}

struct Lib {
    decoder_create: unsafe extern "C" fn() -> *mut DecoderPrefix,
    decoder_destroy: unsafe extern "C" fn(*mut DecoderPrefix),
    decoder_set_io_memory: unsafe extern "C" fn(*mut DecoderPrefix, *const u8, usize) -> c_int,
    decoder_parse: unsafe extern "C" fn(*mut DecoderPrefix) -> c_int,
    decoder_next_image: unsafe extern "C" fn(*mut DecoderPrefix) -> c_int,
    decoder_nth_image_timing: unsafe extern "C" fn(*const DecoderPrefix, u32, *mut Timing) -> c_int,
    rgb_set_defaults: unsafe extern "C" fn(*mut RgbImage, *const Image),
    rgb_allocate: unsafe extern "C" fn(*mut RgbImage) -> c_int,
    rgb_free: unsafe extern "C" fn(*mut RgbImage),
    yuv_to_rgb: unsafe extern "C" fn(*const Image, *mut RgbImage) -> c_int,
    result_to_string: unsafe extern "C" fn(c_int) -> *const c_char,
}

static LIB: OnceLock<Result<Lib, String>> = OnceLock::new();

fn load() -> Result<Lib, String> {
    let dll = super::heif::codecs_dir().ok_or("no exe path")?.join("avif.dll");
    unsafe {
        // DLL_LOAD_DIR: libavif's import of dav1d resolves from the same folder.
        let m = LoadLibraryExW(&HSTRING::from(dll.as_os_str()), None, LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR | LOAD_LIBRARY_SEARCH_DEFAULT_DIRS)
            .map_err(|e| format!("{}: {}", dll.display(), e.message()))?;
        macro_rules! sym {
            ($name:literal) => {
                std::mem::transmute(GetProcAddress(m, PCSTR(concat!($name, "\0").as_ptr())).ok_or(concat!("libavif lacks ", $name))?)
            };
        }
        Ok(Lib {
            decoder_create: sym!("avifDecoderCreate"),
            decoder_destroy: sym!("avifDecoderDestroy"),
            decoder_set_io_memory: sym!("avifDecoderSetIOMemory"),
            decoder_parse: sym!("avifDecoderParse"),
            decoder_next_image: sym!("avifDecoderNextImage"),
            decoder_nth_image_timing: sym!("avifDecoderNthImageTiming"),
            rgb_set_defaults: sym!("avifRGBImageSetDefaults"),
            rgb_allocate: sym!("avifRGBImageAllocatePixels"),
            rgb_free: sym!("avifRGBImageFreePixels"),
            yuv_to_rgb: sym!("avifImageYUVToRGB"),
            result_to_string: sym!("avifResultToString"),
        })
    }
}

fn lib() -> Result<&'static Lib, String> {
    LIB.get_or_init(|| {
        let r = load();
        if let Err(e) = &r {
            crate::trace::mark(format!("libavif unavailable: {e}"));
        }
        r
    })
    .as_ref()
    .map_err(|e| e.clone())
}

/// Whether the file's `ftyp` box lists the image-sequence brand (as its major brand or a compatible one).
fn is_sequence(bytes: &[u8]) -> bool {
    if bytes.len() < 16 || &bytes[4..8] != b"ftyp" {
        return false;
    }
    let size = (u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]) as usize).clamp(16, bytes.len());
    // major brand at 8, minor version at 12, compatible brands from 16
    &bytes[8..12] == b"avis" || bytes[16..size].chunks_exact(4).any(|b| b == b"avis")
}

/// Browsers play 0–10 ms frames at 100 ms (as animated.rs does for GIFs).
fn delay_ms(seconds: f64) -> u32 {
    let ms = (seconds * 1000.0).round() as u32;
    if ms <= 10 { 100 } else { ms }
}

/// `Ok(None)` for a still AVIF, told from the first bytes alone, unless the display is HDR and the still is an
/// HDR one (PQ or HLG), which only libavif hands over undiminished. `still` keeps the first frame only (the
/// placeholder tier).
pub fn decode(f: &IWICImagingFactory, path: &std::path::Path, box_w: u32, box_h: u32, still: bool) -> Result<Option<Decoded>, String> {
    use std::io::Read;
    let mut head = [0u8; 256];
    let n = std::fs::File::open(path).and_then(|mut file| file.read(&mut head)).map_err(|e| e.to_string())?;
    if !is_sequence(&head[..n]) {
        return match super::target() {
            crate::colour::Space::Scrgb { hdr: true, .. } => hdr_still(path, box_w, box_h),
            _ => Ok(None),
        };
    }
    let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
    let l = lib()?;
    let err = |what: &str, r: c_int| {
        let s = unsafe { (l.result_to_string)(r) };
        let s = if s.is_null() { String::new() } else { unsafe { CStr::from_ptr(s) }.to_string_lossy().into_owned() };
        format!("libavif {what}: {s}")
    };
    unsafe {
        let d = (l.decoder_create)();
        if d.is_null() {
            return Err("libavif: out of memory".into());
        }
        struct Guard<'a>(&'a Lib, *mut DecoderPrefix);
        impl Drop for Guard<'_> {
            fn drop(&mut self) {
                unsafe { (self.0.decoder_destroy)(self.1) }
            }
        }
        let _g = Guard(l, d);
        (*d).max_threads = std::thread::available_parallelism().map_or(2, |n| n.get().min(8)) as c_int;
        let r = (l.decoder_set_io_memory)(d, bytes.as_ptr(), bytes.len());
        if r != RESULT_OK {
            return Err(err("io", r));
        }
        let r = (l.decoder_parse)(d);
        if r != RESULT_OK {
            return Err(err("parse", r));
        }
        let count = (*d).image_count.max(0) as u32;
        if count <= 1 {
            return Ok(None); // a sequence of one: the still path has it
        }
        // Animations stay at the fit size even when asked for full resolution.
        let (box_w, box_h) = if box_w == 0 || box_h == 0 { (2048, 2048) } else { (box_w, box_h) };
        let mut out: Vec<Frame> = Vec::new();
        let (mut w, mut h, mut nw, mut nh) = (0, 0, 0, 0);
        let mut used = 0usize;
        for i in 0..count {
            let r = (l.decoder_next_image)(d);
            if r != RESULT_OK {
                if out.is_empty() {
                    return Err(err("frame", r));
                }
                crate::trace::mark(format!("animated AVIF stopped at frame {i}: {}", err("frame", r)));
                break;
            }
            let mut rgb: RgbImage = std::mem::zeroed();
            (l.rgb_set_defaults)(&mut rgb, (*d).image);
            rgb.depth = 8;
            rgb.format = RGB_FORMAT_RGBA;
            rgb.alpha_premultiplied = 0;
            let r = (l.rgb_allocate)(&mut rgb);
            if r != RESULT_OK {
                return Err(err("alloc", r));
            }
            let r = (l.yuv_to_rgb)((*d).image, &mut rgb);
            if r != RESULT_OK {
                (l.rgb_free)(&mut rgb);
                return Err(err("yuv to rgb", r));
            }
            let (fw, fh, stride) = (rgb.width, rgb.height, rgb.row_bytes as usize);
            let mut rgba = Vec::with_capacity(fw as usize * fh as usize * 4);
            for y in 0..fh as usize {
                rgba.extend_from_slice(std::slice::from_raw_parts(rgb.pixels.add(y * stride), fw as usize * 4));
            }
            (l.rgb_free)(&mut rgb);
            let mut t = Timing::default();
            let delay = if (l.decoder_nth_image_timing)(d, i, &mut t) == RESULT_OK { delay_ms(t.duration) } else { 100 };
            let (dw, dh, px) = wic::from_rgba(f, &rgba, fw, fh, box_w, box_h).map_err(|e| e.message().to_string())?;
            if out.is_empty() {
                (w, h, nw, nh) = (dw, dh, fw, fh);
            } else if (dw, dh) != (w, h) {
                continue; // every frame of a sequence has the track's size; anything else is malformed
            }
            used += px.len();
            out.push(Frame { pixels: px, delay_ms: if still { 0 } else { delay } });
            if still {
                break;
            }
            if used > BUDGET_BYTES {
                crate::trace::mark(format!("animation truncated at {} frames (memory budget)", out.len()));
                break;
            }
        }
        if out.is_empty() {
            return Err("libavif: no frames".into());
        }
        Ok(Some(Decoded { width: w, height: h, frames: out, native_width: nw, native_height: nh, format: Format::Unknown, taken: None, vector: false, pages: 0, page_sizes: Vec::new(), page_scale: 1.0, histogram: None, colour: super::Colour::Srgb, generation: 0 }))
    }
}

/// A PQ or HLG still as an HDR decode (see hdr.rs): 16-bit RGB from libavif, to nits, to SDR-white units
/// (BT.2408's 203 nits), to Rec. 709 primaries. `Ok(None)` for an SDR still.
fn hdr_still(path: &std::path::Path, box_w: u32, box_h: u32) -> Result<Option<Decoded>, String> {
    use super::hdr;
    let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
    let l = lib()?;
    let err = |what: &str, r: c_int| {
        let s = unsafe { (l.result_to_string)(r) };
        let s = if s.is_null() { String::new() } else { unsafe { CStr::from_ptr(s) }.to_string_lossy().into_owned() };
        format!("libavif {what}: {s}")
    };
    unsafe {
        let d = (l.decoder_create)();
        if d.is_null() {
            return Err("libavif: out of memory".into());
        }
        struct Guard<'a>(&'a Lib, *mut DecoderPrefix);
        impl Drop for Guard<'_> {
            fn drop(&mut self) {
                unsafe { (self.0.decoder_destroy)(self.1) }
            }
        }
        let _g = Guard(l, d);
        (*d).max_threads = std::thread::available_parallelism().map_or(2, |n| n.get().min(8)) as c_int;
        let r = (l.decoder_set_io_memory)(d, bytes.as_ptr(), bytes.len());
        if r != RESULT_OK {
            return Err(err("io", r));
        }
        let r = (l.decoder_parse)(d);
        if r != RESULT_OK {
            return Err(err("parse", r));
        }
        let img = (*d).image as *const ImagePrefix;
        let transfer = (*img).transfer_characteristics;
        if transfer != TRANSFER_PQ && transfer != TRANSFER_HLG {
            return Ok(None);
        }
        let primaries = (*img).color_primaries;
        let r = (l.decoder_next_image)(d);
        if r != RESULT_OK {
            return Err(err("image", r));
        }
        let mut rgb: RgbImage = std::mem::zeroed();
        (l.rgb_set_defaults)(&mut rgb, (*d).image);
        rgb.depth = 16;
        rgb.format = RGB_FORMAT_RGBA;
        rgb.alpha_premultiplied = 0;
        let r = (l.rgb_allocate)(&mut rgb);
        if r != RESULT_OK {
            return Err(err("alloc", r));
        }
        let r = (l.yuv_to_rgb)((*d).image, &mut rgb);
        if r != RESULT_OK {
            (l.rgb_free)(&mut rgb);
            return Err(err("yuv to rgb", r));
        }
        let (w, h) = (rgb.width, rgb.height);
        let to_709 = match primaries {
            PRIMARIES_BT2020 => hdr::BT2020_TO_709,
            PRIMARIES_P3 => hdr::P3_TO_709,
            _ => hdr::IDENTITY,
        };
        // The transfer curve once per 16-bit value, not three times per pixel (a 1.8 MP PQ still: 180 ms to 55).
        let pq = transfer == TRANSFER_PQ;
        let curve: Vec<f32> = (0..=u16::MAX).map(|v| v as f32 / 65535.0).map(|e| if pq { hdr::pq_to_nits(e) } else { hdr::hlg_to_scene(e) }).collect();
        let mut lin = Vec::with_capacity((w * h) as usize);
        let mut alpha = Vec::with_capacity((w * h) as usize);
        for y in 0..h as usize {
            let row = std::slice::from_raw_parts(rgb.pixels.add(y * rgb.row_bytes as usize) as *const u16, w as usize * 4);
            for p in row.chunks_exact(4) {
                let e = [p[0], p[1], p[2]].map(|v| curve[v as usize]);
                let nits = if pq { e } else { hdr::hlg_to_nits(e) };
                lin.push(hdr::apply(&to_709, nits.map(|v| v / hdr::REFERENCE_WHITE)));
                alpha.push(p[3] as f32 / 65535.0);
            }
        }
        (l.rgb_free)(&mut rgb);
        let opaque = alpha.iter().all(|&a| a >= 1.0);
        let (dw, dh, lin, alpha) = hdr::fit(lin, (!opaque).then_some(alpha), w, h, box_w, box_h);
        let peak = hdr::peak(&lin);
        crate::trace::mark(format!("HDR AVIF ({}): peak {peak:.2}x SDR white", if transfer == TRANSFER_PQ { "PQ" } else { "HLG" }));
        let mut out = Decoded::still(dw, dh, hdr::to_half(&lin, alpha.as_deref()), w, h, None);
        out.histogram = Some(Box::new(crate::metadata::Histogram::of(&hdr::sdr_bgra(&lin))));
        out.colour = super::Colour::Linear { absolute: false, peak };
        Ok(Some(out))
    }
}

#[cfg(test)]
mod tests {
    use super::is_sequence;

    fn ftyp(major: &[u8; 4], compatible: &[&[u8; 4]]) -> Vec<u8> {
        let size = 16 + 4 * compatible.len();
        let mut v = (size as u32).to_be_bytes().to_vec();
        v.extend_from_slice(b"ftyp");
        v.extend_from_slice(major);
        v.extend_from_slice(&[0, 0, 0, 0]);
        for b in compatible {
            v.extend_from_slice(*b);
        }
        v.extend_from_slice(b"\0\0\0\x08meta"); // the next box is not read as brands
        v
    }

    #[test]
    fn sequences_are_told_by_their_brands() {
        assert!(is_sequence(&ftyp(b"avis", &[b"avif", b"mif1"])));
        assert!(is_sequence(&ftyp(b"avif", &[b"mif1", b"avis"])));
        assert!(!is_sequence(&ftyp(b"avif", &[b"mif1", b"miaf"])));
        assert!(!is_sequence(&ftyp(b"avif", &[])));
        assert!(!is_sequence(b"not an avif"));
    }
}
