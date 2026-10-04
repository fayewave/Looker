//! WIC decoding to a target size, on a small pool of worker threads.
//!
//! Decoded pixels carry no DPI: the viewer does its own DPI math and always draws into an explicit
//! destination rect, so a file's print-resolution metadata can never reach Direct2D (the C# app's worst
//! pixelation bug).

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicIsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};

use windows::Win32::Foundation::{FILETIME, GENERIC_READ, HWND, LPARAM, WPARAM};
use windows::Win32::Graphics::Imaging::*;
use windows::Win32::System::Com::StructuredStorage::{PROPVARIANT, PropVariantClear};
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx};
use windows::Win32::System::Variant::{VT_FILETIME, VT_UI2};
use windows::Win32::UI::WindowsAndMessaging::PostMessageW;
use windows::core::{GUID, HSTRING, Interface, PCWSTR, Result, w};

/// Posted to the window when results are waiting in [`Pool::take_results`].
pub const WM_DECODED: u32 = windows::Win32::UI::WindowsAndMessaging::WM_APP + 1;

pub struct Decoded {
    pub width: u32,
    pub height: u32,
    /// Premultiplied BGRA, `width * 4` bytes per row.
    pub pixels: Vec<u8>,
    /// Oriented size of the full-resolution image.
    pub native_width: u32,
    pub native_height: u32,
    pub format: &'static str,
    pub taken: Option<FILETIME>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Priority {
    /// The image on screen: jumps the queue.
    Current,
    Preload,
}

pub struct Job {
    pub path: PathBuf,
    /// Decode to fit inside this box (device pixels), never upscaling. `(0, 0)` = full resolution.
    pub box_w: u32,
    pub box_h: u32,
    pub priority: Priority,
}

pub struct Done {
    pub path: PathBuf,
    pub box_w: u32,
    pub box_h: u32,
    pub result: std::result::Result<Decoded, String>,
}

pub struct Pool {
    queue: Mutex<VecDeque<Job>>,
    wake: Condvar,
    results: Mutex<Vec<Done>>,
    hwnd: AtomicIsize,
}

impl Pool {
    pub fn start(workers: usize) -> Arc<Pool> {
        let pool = Arc::new(Pool {
            queue: Mutex::new(VecDeque::new()),
            wake: Condvar::new(),
            results: Mutex::new(Vec::new()),
            hwnd: AtomicIsize::new(0),
        });
        for i in 0..workers {
            let p = pool.clone();
            std::thread::Builder::new()
                .name(format!("decode{i}"))
                .spawn(move || p.worker())
                .expect("spawn decode worker");
        }
        pool
    }

    /// Where finished decodes are announced. Results that finish earlier wait in the list.
    pub fn set_window(&self, hwnd: HWND) {
        self.hwnd.store(hwnd.0 as isize, Ordering::Release);
        if !self.results.lock().unwrap().is_empty() {
            self.notify();
        }
    }

    pub fn submit(&self, job: Job) {
        let mut q = self.queue.lock().unwrap();
        if job.priority == Priority::Current {
            q.push_front(job);
        } else {
            q.push_back(job);
        }
        drop(q);
        self.wake.notify_one();
    }

    /// Drops queued (not yet started) jobs the predicate rejects; returns their paths.
    pub fn retain(&self, keep: impl Fn(&Job) -> bool) -> Vec<PathBuf> {
        let mut dropped = Vec::new();
        self.queue.lock().unwrap().retain(|j| {
            let k = keep(j);
            if !k {
                dropped.push(j.path.clone());
            }
            k
        });
        dropped
    }

    pub fn take_results(&self) -> Vec<Done> {
        std::mem::take(&mut *self.results.lock().unwrap())
    }

    /// Blocks until a result for `path` is in, or `timeout_ms` passes. Startup only.
    pub fn wait_for(&self, path: &Path, timeout_ms: u64) -> bool {
        let deadline = std::time::Instant::now() + std::time::Duration::from_millis(timeout_ms);
        loop {
            if self.results.lock().unwrap().iter().any(|d| d.path == path) {
                return true;
            }
            if std::time::Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
    }

    fn notify(&self) {
        let h = self.hwnd.load(Ordering::Acquire);
        if h != 0 {
            unsafe {
                let _ = PostMessageW(Some(HWND(h as _)), WM_DECODED, WPARAM(0), LPARAM(0));
            }
        }
    }

    fn worker(&self) {
        unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        }
        let factory: Option<IWICImagingFactory> =
            unsafe { CoCreateInstance(&CLSID_WICImagingFactory2, None, CLSCTX_INPROC_SERVER).ok() };
        loop {
            let job = {
                let mut q = self.queue.lock().unwrap();
                loop {
                    if let Some(j) = q.pop_front() {
                        break j;
                    }
                    q = self.wake.wait(q).unwrap();
                }
            };
            let name = job.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            crate::trace::mark(format!("decode start {name} box={}x{} {:?}", job.box_w, job.box_h, job.priority));
            let result = match &factory {
                Some(f) => decode(f, &job.path, job.box_w, job.box_h).map_err(|e| e.message().to_string()),
                None => Err("WIC is unavailable".into()),
            };
            match &result {
                Ok(d) => crate::trace::mark(format!(
                    "decode done  {name} {}x{} (native {}x{})",
                    d.width, d.height, d.native_width, d.native_height
                )),
                Err(e) => crate::trace::mark(format!("decode FAIL  {name}: {e}")),
            }
            self.results.lock().unwrap().push(Done { path: job.path, box_w: job.box_w, box_h: job.box_h, result });
            self.notify();
        }
    }
}

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

fn format_name(container: GUID) -> &'static str {
    match container {
        g if g == GUID_ContainerFormatJpeg => "JPEG",
        g if g == GUID_ContainerFormatPng => "PNG",
        g if g == GUID_ContainerFormatGif => "GIF",
        g if g == GUID_ContainerFormatBmp => "BMP",
        g if g == GUID_ContainerFormatTiff => "TIFF",
        g if g == GUID_ContainerFormatIco => "ICO",
        g if g == GUID_ContainerFormatWmp => "JPEG XR",
        g if g == GUID_ContainerFormatHeif => "HEIF",
        g if g == GUID_ContainerFormatWebp => "WebP",
        g if g == GUID_ContainerFormatDds => "DDS",
        g if g == GUID_ContainerFormatAdng => "DNG",
        g if g == GUID::from_u128(0xfec14e3f_427a_4736_aae6_27ed84f69322) => "JPEG XL",
        g if g == GUID_ContainerFormatRaw => "RAW",
        _ => "Image",
    }
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

pub fn decode(f: &IWICImagingFactory, path: &Path, box_w: u32, box_h: u32) -> Result<Decoded> {
    unsafe {
        let wide = HSTRING::from(path.as_os_str());
        let dec = f.CreateDecoderFromFilename(&wide, None, GENERIC_READ, WICDecodeMetadataCacheOnDemand)?;
        let format = format_name(dec.GetContainerFormat()?);
        let frame = dec.GetFrame(0)?;
        let (mut w, mut h) = (0u32, 0u32);
        frame.GetSize(&mut w, &mut h)?;

        let reader = frame.GetMetadataQueryReader().ok();
        let orientation = reader.as_ref().map_or(1, |r| read_orientation(r));
        let taken = reader.as_ref().and_then(|r| read_taken(r));
        let (transform, swaps) = orientation_transform(orientation);
        let (ow, oh) = if swaps { (h, w) } else { (w, h) };

        // Scale factor from the oriented size; WIC scales before it rotates, so apply it to the raw size.
        let scale = if box_w == 0 || box_h == 0 {
            1.0
        } else {
            (box_w as f64 / ow as f64).min(box_h as f64 / oh as f64).min(1.0)
        };
        let sw = ((w as f64 * scale).round() as u32).max(1);
        let sh = ((h as f64 * scale).round() as u32).max(1);

        let mut src: IWICBitmapSource = frame.clone().into();
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
            let rot = f.CreateBitmapFlipRotator()?;
            rot.Initialize(&src, transform)?;
            src = rot.into();
        }
        let straight = convert(f, &src, &GUID_WICPixelFormat32bppBGRA)?;
        let (dw, dh, pixels) = finish(f, &frame, straight)?;
        Ok(Decoded { width: dw, height: dh, pixels, native_width: ow, native_height: oh, format, taken })
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
        Ok(Decoded { width: px, height: px, pixels, native_width: px, native_height: px, format: "ICO", taken: None })
    }
}
