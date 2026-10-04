//! Thumbnails for the strip (port of `ThumbnailLoader`): the Windows shell's thumbnail cache first (a cached
//! hit is a millisecond, and it covers RAW/HEIC and anything with a shell provider), then the shell's full
//! extraction, then Looker's own decoder for what the shell can't do (JPEG XL, PSD, TGA, DDS, …). SVG and
//! PDF skip the shell: our renderer is deterministic, and the shell's depends on whichever app is installed.
//! Online-only cloud files never fall back to our decoder, which would download them.
//!
//! A few STA workers (shell thumbnail providers expect an apartment) pull from a queue the strip keeps
//! trimmed to the cells on screen, so flinging past a thousand files never queues a thousand extractions.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicIsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};

use windows::Win32::Foundation::{HWND, LPARAM, SIZE, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BI_RGB, BITMAP, BITMAPINFO, BITMAPINFOHEADER, DIB_RGB_COLORS, DeleteObject, GetDC, GetDIBits, GetObjectW, HBITMAP, HGDIOBJ, ReleaseDC,
};
use windows::Win32::Graphics::Imaging::{CLSID_WICImagingFactory2, IWICImagingFactory};
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE, CoCreateInstance, CoInitializeEx};
use windows::Win32::UI::Shell::{IShellItemImageFactory, SHCreateItemFromParsingName, SIIGBF, SIIGBF_INCACHEONLY, SIIGBF_THUMBNAILONLY};
use windows::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_APP};
use windows::core::HSTRING;

use crate::format::{self, Format};

pub const WM_THUMBS: u32 = WM_APP + 6;
const WORKERS: usize = 3;

#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct Job {
    pub path: PathBuf,
    pub stamp: u64,
    /// Longest edge wanted, device pixels.
    pub size: u32,
    pub cloud: bool,
}

pub struct Done {
    pub job: Job,
    /// Premultiplied BGRA, or None when nothing could make one.
    pub image: Option<(u32, u32, Vec<u8>)>,
}

pub struct Pool {
    queue: Mutex<VecDeque<Job>>,
    wake: Condvar,
    results: Mutex<Vec<Done>>,
    hwnd: AtomicIsize,
}

impl Pool {
    pub fn start(hwnd: HWND) -> Arc<Pool> {
        let pool = Arc::new(Pool {
            queue: Mutex::new(VecDeque::new()),
            wake: Condvar::new(),
            results: Mutex::new(Vec::new()),
            hwnd: AtomicIsize::new(hwnd.0 as isize),
        });
        for i in 0..WORKERS {
            let p = pool.clone();
            std::thread::Builder::new().name(format!("thumb{i}")).spawn(move || p.worker()).ok();
        }
        pool
    }

    pub fn submit(&self, job: Job) {
        self.queue.lock().unwrap().push_back(job);
        self.wake.notify_one();
    }

    /// Drops queued jobs the predicate rejects (cells that scrolled away); returns them.
    pub fn retain(&self, keep: impl Fn(&Job) -> bool) -> Vec<Job> {
        let mut dropped = Vec::new();
        self.queue.lock().unwrap().retain(|j| {
            let k = keep(j);
            if !k {
                dropped.push(j.clone());
            }
            k
        });
        dropped
    }

    pub fn take_results(&self) -> Vec<Done> {
        std::mem::take(&mut *self.results.lock().unwrap())
    }

    fn worker(&self) {
        unsafe {
            let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE);
        }
        let wic: Option<IWICImagingFactory> = unsafe { CoCreateInstance(&CLSID_WICImagingFactory2, None, CLSCTX_INPROC_SERVER).ok() };
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
            let image = load(wic.as_ref(), &job);
            self.results.lock().unwrap().push(Done { job, image });
            let h = self.hwnd.load(Ordering::Acquire);
            unsafe {
                let _ = PostMessageW(Some(HWND(h as _)), WM_THUMBS, WPARAM(0), LPARAM(0));
            }
        }
    }
}

fn load(wic: Option<&IWICImagingFactory>, job: &Job) -> Option<(u32, u32, Vec<u8>)> {
    let ours = || -> Option<(u32, u32, Vec<u8>)> {
        let d = crate::imaging::decode(wic?, &job.path, job.size, job.size, true).ok()?;
        Some((d.width, d.height, d.frames.into_iter().next()?.pixels))
    };
    if matches!(format::sniff_file(&job.path), Format::Svg | Format::Pdf) {
        return ours();
    }
    if let Some(t) = shell(&job.path, job.size, true).or_else(|| shell(&job.path, job.size, false)) {
        return Some(t);
    }
    if job.cloud { None } else { ours() }
}

/// The shell's thumbnail, fitted inside `size` × `size` and never cropped. `cached`: only if it is already in
/// the thumbnail cache. Fails rather than handing back the file-type icon.
fn shell(path: &Path, size: u32, cached: bool) -> Option<(u32, u32, Vec<u8>)> {
    unsafe {
        let factory: IShellItemImageFactory = SHCreateItemFromParsingName(&HSTRING::from(path.as_os_str()), None).ok()?;
        let flags = SIIGBF(SIIGBF_THUMBNAILONLY.0 | if cached { SIIGBF_INCACHEONLY.0 } else { 0 });
        let hbmp = factory.GetImage(SIZE { cx: size as i32, cy: size as i32 }, flags).ok()?;
        let px = hbitmap_pixels(hbmp);
        let _ = DeleteObject(HGDIOBJ(hbmp.0));
        px
    }
}

/// An HBITMAP's pixels as top-down 32-bit BGRA.
unsafe fn hbitmap_pixels(hbmp: HBITMAP) -> Option<(u32, u32, Vec<u8>)> {
    unsafe {
        let mut bm = BITMAP::default();
        if GetObjectW(HGDIOBJ(hbmp.0), size_of::<BITMAP>() as i32, Some(&mut bm as *mut _ as _)) == 0 {
            return None;
        }
        let (w, h) = (bm.bmWidth, bm.bmHeight.abs());
        if w <= 0 || h <= 0 {
            return None;
        }
        let mut bmi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: w,
                biHeight: -h, // top-down
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut px = vec![0u8; (w * h * 4) as usize];
        let dc = GetDC(None);
        let lines = GetDIBits(dc, hbmp, 0, h as u32, Some(px.as_mut_ptr() as _), &mut bmi, DIB_RGB_COLORS);
        ReleaseDC(None, dc);
        if lines == 0 {
            return None;
        }
        fix_alpha(&mut px);
        Some((w as u32, h as u32, px))
    }
}

/// Shell bitmaps carry premultiplied alpha, except that some providers leave alpha 0 on every pixel of an
/// opaque image (a fully transparent thumbnail is never meant): that is made opaque. Colour is clamped to
/// alpha so a sloppy provider can't produce invalid premultiplied pixels.
pub fn fix_alpha(px: &mut [u8]) {
    if px.chunks_exact(4).all(|p| p[3] == 0) {
        for p in px.chunks_exact_mut(4) {
            p[3] = 255;
        }
        return;
    }
    for p in px.chunks_exact_mut(4) {
        let a = p[3];
        p[0] = p[0].min(a);
        p[1] = p[1].min(a);
        p[2] = p[2].min(a);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_zero_alpha_is_opaque() {
        let mut px = vec![10, 20, 30, 0, 40, 50, 60, 0];
        fix_alpha(&mut px);
        assert_eq!(px, [10, 20, 30, 255, 40, 50, 60, 255]);
    }

    #[test]
    fn real_alpha_is_kept_and_clamped() {
        let mut px = vec![200, 20, 30, 100, 0, 0, 0, 0];
        fix_alpha(&mut px);
        assert_eq!(px, [100, 20, 30, 100, 0, 0, 0, 0]);
    }
}
