//! The WIC path: decode to a target size with the decoder's own downscaling, EXIF orientation and sRGB
//! colour management, ending in premultiplied BGRA. Also the shared tail every other decoder ends in
//! ([`from_rgba`]): full-size RGBA in, scaled premultiplied BGRA out.
//!
//! Decoded pixels carry no DPI: the viewer does its own DPI math and always draws into an explicit
//! destination rect, so a file's print-resolution metadata can never reach Direct2D (the C# app's worst
//! pixelation bug).

use std::path::Path;

use windows::Win32::Foundation::{FILETIME, GENERIC_READ};
use windows::Win32::Graphics::Imaging::*;
use windows::Win32::System::Com::StructuredStorage::{PROPVARIANT, PropVariantClear};
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance};
use windows::Win32::System::Variant::{VT_FILETIME, VT_UI2};
use windows::core::{GUID, HSTRING, Interface, PCWSTR, Result, w};

use super::Decoded;

/// EXIF orientation (1–8) → the WIC transform that undoes it, and whether it swaps width/height.
fn orientation_transform(o: u16) -> (WICBitmapTransformOptions, bool) {
    match o {
        2 => (WICBitmapTransformFlipHorizontal, false),
        3 => (WICBitmapTransformRotate180, false),
        4 => (WICBitmapTransformFlipVertical, false),
        5 => (WICBitmapTransformOptions(WICBitmapTransformRotate90.0 | WICBitmapTransformFlipHorizontal.0), true),
        6 => (WICBitmapTransformRotate90, true),
        7 => (WICBitmapTransformOptions(WICBitmapTransformRotate270.0 | WICBitmapTransformFlipHorizontal.0), true),
        8 => (WICBitmapTransformRotate270, true),
        _ => (WICBitmapTransformRotate0, false),
    }
}

unsafe fn query(reader: &IWICMetadataQueryReader, name: PCWSTR) -> Option<PROPVARIANT> {
    let mut v = PROPVARIANT::default();
    unsafe { reader.GetMetadataByName(name, &mut v).ok().map(|_| v) }
}

unsafe fn read_orientation(reader: &IWICMetadataQueryReader) -> u16 {
    for name in [w!("System.Photo.Orientation"), w!("/app1/ifd/{ushort=274}"), w!("/ifd/{ushort=274}")] {
        if let Some(mut v) = unsafe { query(reader, name) } {
            let o = unsafe {
                let inner = &v.Anonymous.Anonymous;
                if inner.vt == VT_UI2 { inner.Anonymous.uiVal } else { 0 }
            };
            unsafe {
                let _ = PropVariantClear(&mut v);
            }
            if (1..=8).contains(&o) {
                return o;
            }
        }
    }
    1
}

unsafe fn read_taken(reader: &IWICMetadataQueryReader) -> Option<FILETIME> {
    let mut v = unsafe { query(reader, w!("System.Photo.DateTaken")) }?;
    let ft = unsafe {
        let inner = &v.Anonymous.Anonymous;
        if inner.vt == VT_FILETIME { Some(inner.Anonymous.filetime) } else { None }
    };
    unsafe {
        let _ = PropVariantClear(&mut v);
    }
    ft
}

/// The frame's embedded colour profile, unless it has none or it says sRGB (nothing to convert then).
unsafe fn color_profile(f: &IWICImagingFactory, frame: &IWICBitmapFrameDecode) -> Option<IWICColorContext> {
    unsafe {
        let mut count = 0u32;
        frame.GetColorContexts(&mut [], &mut count).ok()?;
        if count == 0 {
            return None;
        }
        let mut contexts: Vec<Option<IWICColorContext>> = (0..count).map(|_| f.CreateColorContext().ok()).collect();
        frame.GetColorContexts(&mut contexts, &mut count).ok()?;
        let ctx = contexts.into_iter().flatten().next()?;
        if ctx.GetType().ok()? == WICColorContextExifColorSpace && ctx.GetExifColorSpace().ok()? == 1 {
            return None;
        }
        Some(ctx)
    }
}

/// Whether the decoder's native pixel format can carry transparency.
unsafe fn has_alpha(f: &IWICImagingFactory, frame: &IWICBitmapFrameDecode) -> bool {
    unsafe {
        let Ok(fmt) = frame.GetPixelFormat() else { return true };
        let Ok(info) = f.CreateComponentInfo(&fmt) else { return true };
        match info.cast::<IWICPixelFormatInfo2>() {
            Ok(pf) => pf.SupportsTransparency().map_or(true, |b| b.as_bool()),
            Err(_) => true,
        }
    }
}

unsafe fn convert(f: &IWICImagingFactory, src: &IWICBitmapSource, to: &GUID) -> Result<IWICBitmapSource> {
    unsafe {
        let c = f.CreateFormatConverter()?;
        c.Initialize(src, to, WICBitmapDitherTypeNone, None, 0.0, WICBitmapPaletteTypeCustom)?;
        Ok(c.into())
    }
}

unsafe fn copy_all(src: &IWICBitmapSource) -> Result<(u32, u32, Vec<u8>)> {
    unsafe {
        let (mut w, mut h) = (0u32, 0u32);
        src.GetSize(&mut w, &mut h)?;
        let mut px = vec![0u8; (w * 4 * h) as usize];
        src.CopyPixels(std::ptr::null(), w * 4, &mut px)?;
        Ok((w, h, px))
    }
}

/// Straight BGRA in, premultiplied BGRA out, converted to sRGB when the file carries a profile.
///
/// WIC's colour transformer drops alpha (it comes out 0, which a premultiplied draw adds onto whatever is
/// behind: the checkerboard showed through every tagged JPEG). Opaque images therefore go through it as BGR,
/// and images with transparency get their alpha copied back from the untransformed pixels.
unsafe fn finish(f: &IWICImagingFactory, frame: &IWICBitmapFrameDecode, straight: IWICBitmapSource) -> Result<(u32, u32, Vec<u8>)> {
    unsafe {
        let Some(profile) = color_profile(f, frame) else {
            return copy_all(&convert(f, &straight, &GUID_WICPixelFormat32bppPBGRA)?);
        };
        let srgb = f.CreateColorContext()?;
        srgb.InitializeFromExifColorSpace(1)?;
        let t = f.CreateColorTransformer()?;
        if !has_alpha(f, frame) {
            if t.Initialize(&straight, &profile, &srgb, &GUID_WICPixelFormat32bppBGR).is_ok() {
                if let Ok(out) = convert(f, &t.into(), &GUID_WICPixelFormat32bppPBGRA).and_then(|s| copy_all(&s)) {
                    return Ok(out);
                }
            }
            return copy_all(&convert(f, &straight, &GUID_WICPixelFormat32bppPBGRA)?);
        }
        let cached: IWICBitmapSource = f.CreateBitmapFromSource(&straight, WICBitmapCacheOnLoad)?.into();
        let (w, h, alpha) = copy_all(&cached)?;
        let mut px = match t.Initialize(&cached, &profile, &srgb, &GUID_WICPixelFormat32bppBGRA) {
            Ok(()) => copy_all(&t.into()).map(|(_, _, p)| p).unwrap_or_else(|_| alpha.clone()),
            Err(_) => alpha.clone(),
        };
        for (p, a) in px.chunks_exact_mut(4).zip(alpha.chunks_exact(4)) {
            let a = a[3] as u32;
            p[0] = ((p[0] as u32 * a + 127) / 255) as u8;
            p[1] = ((p[1] as u32 * a + 127) / 255) as u8;
            p[2] = ((p[2] as u32 * a + 127) / 255) as u8;
            p[3] = a as u8;
        }
        Ok((w, h, px))
    }
}

/// The decoder's own reduced-size decode (JPEG's DCT scaling: 1/2, 1/4, 1/8) to the smallest size that is
/// still at least `sw × sh`, so a 24 MP JPEG shown at 1000 px wide never decodes all 24 MP. `None` when the
/// codec can't, in which case the scaler works from the full-size frame.
unsafe fn prescale(f: &IWICImagingFactory, frame: &IWICBitmapFrameDecode, w: u32, h: u32, sw: u32, sh: u32) -> Option<IWICBitmapSource> {
    unsafe {
        let st = frame.cast::<IWICBitmapSourceTransform>().ok()?;
        let (mut cw, mut ch) = (sw, sh);
        st.GetClosestSize(&mut cw, &mut ch).ok()?;
        if cw < sw || ch < sh || cw >= w || ch >= h {
            return None;
        }
        let mut fmt = GUID_WICPixelFormat32bppBGRA;
        st.GetClosestPixelFormat(&mut fmt).ok()?;
        let info = f.CreateComponentInfo(&fmt).ok()?.cast::<IWICPixelFormatInfo>().ok()?;
        let bpp = info.GetBitsPerPixel().ok()?;
        let stride = (cw * bpp).div_ceil(32) * 4;
        let mut buf = vec![0u8; (stride * ch) as usize];
        st.CopyPixels(std::ptr::null(), cw, ch, &fmt, WICBitmapTransformRotate0, stride, &mut buf).ok()?;
        let bmp = f.CreateBitmapFromMemory(cw, ch, &fmt, stride, &buf).ok()?;
        Some(bmp.into())
    }
}

/// Where WIC reads from, and with which decoder.
pub enum Source<'a> {
    /// WIC picks the decoder from the content.
    File(&'a Path),
    /// This decoder (CLSID) and no other: camera RAW, where an inbox codec that only returns the embedded
    /// preview is registered ahead of the real one.
    FileWith(&'a Path, GUID),
    /// Bytes already in memory (patched cursors).
    Memory(&'a [u8]),
}

/// CLSID of the Microsoft Raw Image Extension decoder (Store, libraw-based). Windows also ships an inbox
/// "DNG Decoder" registered ahead of it for .dng that returns only the embedded preview (960x720 for a
/// 12 MP DJI file), so camera RAW asks for this codec by id.
pub const RAW_IMAGE_DECODER: GUID = GUID::from_u128(0x41945702_8302_44a6_9445_ac98e8afa086);

unsafe fn open(f: &IWICImagingFactory, src: &Source) -> Result<IWICBitmapDecoder> {
    unsafe {
        match src {
            Source::File(path) => {
                f.CreateDecoderFromFilename(&HSTRING::from(path.as_os_str()), None, GENERIC_READ, WICDecodeMetadataCacheOnDemand)
            }
            Source::FileWith(path, clsid) => {
                let dec: IWICBitmapDecoder = CoCreateInstance(clsid, None, CLSCTX_INPROC_SERVER)?;
                let stream = f.CreateStream()?;
                stream.InitializeFromFilename(&HSTRING::from(path.as_os_str()), GENERIC_READ.0)?;
                dec.Initialize(&stream, WICDecodeMetadataCacheOnDemand)?;
                Ok(dec)
            }
            Source::Memory(bytes) => {
                let stream = f.CreateStream()?;
                stream.InitializeFromMemory(bytes)?;
                f.CreateDecoderFromStream(&stream, std::ptr::null(), WICDecodeMetadataCacheOnDemand)
            }
        }
    }
}

/// The decoder's embedded preview, if it has the frame's aspect ratio and at least the target size.
unsafe fn usable_preview(dec: &IWICBitmapDecoder, w: u32, h: u32, sw: u32, sh: u32) -> Option<IWICBitmapSource> {
    unsafe {
        let p = dec.GetPreview().ok()?;
        let (mut pw, mut ph) = (0u32, 0u32);
        p.GetSize(&mut pw, &mut ph).ok()?;
        let same_aspect = ((pw as f64 / ph as f64) - (w as f64 / h as f64)).abs() < 0.02;
        crate::trace::mark(format!("RAW preview {pw}x{ph} for {sw}x{sh} (sensor {w}x{h})"));
        (same_aspect && pw >= sw && ph >= sh).then_some(p)
    }
}

/// Scale factor that fits `w × h` inside the box without upscaling; `(0, 0)` = full size.
pub fn fit_scale(w: u32, h: u32, box_w: u32, box_h: u32) -> f64 {
    if box_w == 0 || box_h == 0 {
        1.0
    } else {
        (box_w as f64 / w as f64).min(box_h as f64 / h as f64).min(1.0)
    }
}

pub fn decode(f: &IWICImagingFactory, source: Source, box_w: u32, box_h: u32) -> Result<Decoded> {
    let (d, from_preview) = decode_with(f, &source, box_w, box_h, true)?;
    // Some cameras embed a linear, un-rendered thumbnail as the "preview" (a Mamiya ZD's averages 12/255
    // where the real image averages 115). A near-black preview is never what the photo looks like.
    if from_preview && mean_luma(&d.frames[0].pixels) < 26 {
        crate::trace::mark("RAW preview is near-black; decoding the sensor data instead");
        return decode_with(f, &source, box_w, box_h, false).map(|(d, _)| d);
    }
    Ok(d)
}

fn mean_luma(px: &[u8]) -> u64 {
    let n = (px.len() / 4).max(1) as u64;
    px.chunks_exact(4).map(|c| (c[0] as u64 + c[1] as u64 + c[2] as u64) / 3).sum::<u64>() / n
}

fn decode_with(f: &IWICImagingFactory, source: &Source, box_w: u32, box_h: u32, allow_preview: bool) -> Result<(Decoded, bool)> {
    unsafe {
        let mut from_preview = false;
        let dec = open(f, source)?;
        let frame = dec.GetFrame(0)?;
        let (mut w, mut h) = (0u32, 0u32);
        frame.GetSize(&mut w, &mut h)?;

        let reader = frame.GetMetadataQueryReader().ok();
        let orientation = reader.as_ref().map_or(1, |r| read_orientation(r));
        let taken = reader.as_ref().and_then(|r| read_taken(r));
        let (transform, swaps) = orientation_transform(orientation);
        let (ow, oh) = if swaps { (h, w) } else { (w, h) };

        // Scale factor from the oriented size; WIC scales before it rotates, so apply it to the raw size.
        let scale = fit_scale(ow, oh, box_w, box_h);
        let sw = ((w as f64 * scale).round() as u32).max(1);
        let sh = ((h as f64 * scale).round() as u32).max(1);

        let mut src: IWICBitmapSource = frame.clone().into();
        // Camera RAW: a full sensor decode can take seconds (8.7 s for a 16 MP Hasselblad 3FR through the Raw
        // Image Extension). Cameras embed a full-size JPEG preview; when it is big enough for the box, it is
        // what a fit view shows. Zooming past it asks for full resolution (box 0), which skips this.
        if matches!(source, Source::FileWith(_, id) if *id == RAW_IMAGE_DECODER) {
            match usable_preview(&dec, w, h, sw, sh).filter(|_| box_w > 0 && allow_preview) {
                Some(p) => {
                    src = p;
                    from_preview = true;
                }
                // The RAW codec re-runs its decode for every CopyPixels strip the scaler asks for (8 s for a
                // 16 MP Hasselblad file); pull the frame once into memory and scale from that.
                None => src = f.CreateBitmapFromSource(&src, WICBitmapCacheOnLoad)?.into(),
            }
        }
        if sw != w || sh != h {
            if let Some(pre) = prescale(f, &frame, w, h, sw, sh) {
                src = pre;
            }
            let (mut pw, mut ph) = (0u32, 0u32);
            src.GetSize(&mut pw, &mut ph)?;
            if pw != sw || ph != sh {
                let scaler = f.CreateBitmapScaler()?;
                scaler.Initialize(&src, sw, sh, WICBitmapInterpolationModeHighQualityCubic)?;
                src = scaler.into();
            }
        }
        if transform != WICBitmapTransformRotate0 {
            // The flip-rotator reads its source in columns (or bottom-up), and each of those small reads re-runs
            // the scaler and the JPEG decode beneath it: a 2.5 MP portrait phone photo took 6.6 s. Pull the
            // scaled pixels into memory once and turn those.
            src = f.CreateBitmapFromSource(&src, WICBitmapCacheOnLoad)?.into();
            let rot = f.CreateBitmapFlipRotator()?;
            rot.Initialize(&src, transform)?;
            src = rot.into();
        }
        let straight = convert(f, &src, &GUID_WICPixelFormat32bppBGRA)?;
        let (dw, dh, pixels) = finish(f, &frame, straight)?;
        Ok((Decoded::still(dw, dh, pixels, ow, oh, taken), from_preview))
    }
}

/// The file's EXIF orientation (1 = upright, also when it has none).
pub fn orientation(f: &IWICImagingFactory, path: &Path) -> u16 {
    unsafe {
        let Ok(dec) = open(f, &Source::File(path)) else { return 1 };
        let Ok(frame) = dec.GetFrame(0) else { return 1 };
        frame.GetMetadataQueryReader().ok().map_or(1, |r| read_orientation(&r))
    }
}

/// Writes premultiplied BGRA pixels as a PNG (straight alpha in the file).
pub fn save_png(f: &IWICImagingFactory, path: &Path, w: u32, h: u32, pbgra: &[u8]) -> Result<()> {
    unsafe {
        let bmp = f.CreateBitmapFromMemory(w, h, &GUID_WICPixelFormat32bppPBGRA, w * 4, pbgra)?;
        let straight = convert(f, &bmp.into(), &GUID_WICPixelFormat32bppBGRA)?;
        let stream = f.CreateStream()?;
        stream.InitializeFromFilename(&HSTRING::from(path.as_os_str()), windows::Win32::Foundation::GENERIC_WRITE.0)?;
        let enc = f.CreateEncoder(&GUID_ContainerFormatPng, std::ptr::null())?;
        enc.Initialize(&stream, WICBitmapEncoderNoCache)?;
        let mut frame = None;
        let mut options = None;
        enc.CreateNewFrame(&mut frame, &mut options)?;
        let frame = frame.ok_or_else(|| windows::core::Error::from_hresult(windows::Win32::Foundation::E_FAIL))?;
        frame.Initialize(options.as_ref())?;
        frame.SetSize(w, h)?;
        let mut fmt = GUID_WICPixelFormat32bppBGRA;
        frame.SetPixelFormat(&mut fmt)?;
        frame.WriteSource(&straight, std::ptr::null())?;
        frame.Commit()?;
        enc.Commit()
    }
}

/// The tail every non-WIC decoder ends in: full-size straight RGBA in, the box-fitted premultiplied BGRA out
/// (WIC's high-quality cubic scaler, the same filter as the WIC path).
pub fn from_rgba(f: &IWICImagingFactory, rgba: &[u8], w: u32, h: u32, box_w: u32, box_h: u32) -> Result<(u32, u32, Vec<u8>)> {
    unsafe {
        let bmp = f.CreateBitmapFromMemory(w, h, &GUID_WICPixelFormat32bppRGBA, w * 4, rgba)?;
        let mut src: IWICBitmapSource = bmp.into();
        let scale = fit_scale(w, h, box_w, box_h);
        let sw = ((w as f64 * scale).round() as u32).max(1);
        let sh = ((h as f64 * scale).round() as u32).max(1);
        if sw != w || sh != h {
            let scaler = f.CreateBitmapScaler()?;
            scaler.Initialize(&src, sw, sh, WICBitmapInterpolationModeHighQualityCubic)?;
            src = scaler.into();
        }
        copy_all(&convert(f, &src, &GUID_WICPixelFormat32bppPBGRA)?)
    }
}

/// Decodes a small embedded asset (the app icon) at roughly `px` pixels: the ICO frame closest above it.
pub fn decode_icon(bytes: &'static [u8], px: u32) -> Result<Decoded> {
    unsafe {
        let f: IWICImagingFactory = CoCreateInstance(&CLSID_WICImagingFactory2, None, CLSCTX_INPROC_SERVER)?;
        let stream = f.CreateStream()?;
        stream.InitializeFromMemory(bytes)?;
        let dec = f.CreateDecoderFromStream(&stream, std::ptr::null(), WICDecodeMetadataCacheOnDemand)?;
        let mut best: Option<(IWICBitmapFrameDecode, u32)> = None;
        for i in 0..dec.GetFrameCount()? {
            let fr = dec.GetFrame(i)?;
            let (mut w, mut h) = (0, 0);
            fr.GetSize(&mut w, &mut h)?;
            let better = match &best {
                None => true,
                Some((_, bw)) => (w >= px && (*bw < px || w < *bw)) || (*bw < px && w > *bw),
            };
            if better {
                best = Some((fr, w));
            }
        }
        let (frame, w) = best.ok_or_else(|| windows::core::Error::from_hresult(windows::Win32::Foundation::E_FAIL))?;
        let mut src: IWICBitmapSource = frame.into();
        if w != px {
            let scaler = f.CreateBitmapScaler()?;
            scaler.Initialize(&src, px, px, WICBitmapInterpolationModeHighQualityCubic)?;
            src = scaler.into();
        }
        let pre = f.CreateFormatConverter()?;
        pre.Initialize(&src, &GUID_WICPixelFormat32bppPBGRA, WICBitmapDitherTypeNone, None, 0.0, WICBitmapPaletteTypeCustom)?;
        let mut pixels = vec![0u8; (px * px * 4) as usize];
        pre.CopyPixels(std::ptr::null(), px * 4, &mut pixels)?;
        Ok(Decoded::still(px, px, pixels, px, px, None))
    }
}
