//! Multi-page documents (PDF), the C# `PdfLayout` / `PdfPageSet` pair: the pages laid out as one horizontal
//! strip, and a thread that renders the pages the viewport draws, one at a time.
//!
//! The strip is the "image" the zoom works on and one page is its focus (see `View::set_focus`). Only pages
//! that are actually drawn get rasterized (a 300-page file must not cost 300 bitmaps), and a bounded number
//! stay resident per decode; page 1 is the decode itself and always stays.

use std::path::PathBuf;
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::Graphics::Imaging::{CLSID_WICImagingFactory2, IWICImagingFactory};
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx};
use windows::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_APP};

use crate::engine::Key;
use crate::view::Rect;

/// Posted when rendered pages are waiting in [`Renderer::take`].
pub const WM_PAGE_RENDERED: u32 = WM_APP + 11;

/// Space between pages in page pixels (a third of an inch at 96 dpi).
pub const GAP: f64 = 32.0;

/// Resident pages per decode: enough for the visible ones plus some scroll-back, capped so a zoomed-in
/// render (up to 4096 px a page) can't hold a gigabyte of textures.
const RESIDENT_BUDGET: usize = 192 << 20;
const MIN_RESIDENT: usize = 2;
const MAX_RESIDENT: usize = 12;

#[derive(Debug, PartialEq)]
pub struct Layout {
    /// One rect per page, in strip coordinates (page pixels).
    pub pages: Vec<Rect>,
    /// Every page plus the gaps between them.
    pub width: f64,
    /// The tallest page.
    pub height: f64,
    pub max_w: f64,
    pub max_h: f64,
}

impl Layout {
    /// Pages left to right in reading order, each centred vertically on the tallest.
    pub fn compute(sizes: &[(f32, f32)]) -> Layout {
        let sane = |v: f32| if v.is_finite() && v > 0.0 { v as f64 } else { 1.0 };
        let max_w = sizes.iter().map(|s| sane(s.0)).fold(1.0, f64::max);
        let max_h = sizes.iter().map(|s| sane(s.1)).fold(1.0, f64::max);
        let mut pages = Vec::with_capacity(sizes.len());
        let mut x = 0.0;
        for (i, &(w, h)) in sizes.iter().enumerate() {
            let (w, h) = (sane(w), sane(h));
            pages.push(Rect { x, y: (max_h - h) / 2.0, w, h });
            x += w + if i + 1 < sizes.len() { GAP } else { 0.0 };
        }
        Layout { pages, width: x.max(1.0), height: max_h, max_w, max_h }
    }

    /// The page whose centre is nearest `x` (strip coordinates).
    pub fn page_at(&self, x: f64) -> usize {
        let d = |r: &Rect| (r.x + r.w / 2.0 - x).abs();
        (0..self.pages.len()).min_by(|&a, &b| d(&self.pages[a]).total_cmp(&d(&self.pages[b]))).unwrap_or(0)
    }

    /// The edge a decode bucket measures: the longest page edge.
    pub fn max_edge(&self) -> f64 {
        self.max_w.max(self.max_h)
    }
}

/// How many rendered pages a decode keeps, from the size of its largest page bitmap.
pub fn resident_limit(page_bytes: usize) -> usize {
    (RESIDENT_BUDGET / page_bytes.max(1)).clamp(MIN_RESIDENT, MAX_RESIDENT)
}

/// The resident page to drop to make room for one more, or `None` while there is room: the one furthest
/// from `near`. Page 0 is the decode itself and is never a candidate.
pub fn victim(resident: &[usize], near: usize, limit: usize) -> Option<usize> {
    let others: Vec<usize> = resident.iter().copied().filter(|&p| p != 0).collect();
    // Page 0 counts against the limit too: it is always resident.
    if others.len() + 2 <= limit {
        return None;
    }
    others.into_iter().max_by_key(|&p| p.abs_diff(near))
}

// --- The render thread ------------------------------------------------------------------------------

pub struct Request {
    /// The decode the page belongs to (its bucket decides the render scale).
    pub key: Key,
    pub page: usize,
    pub w: u32,
    pub h: u32,
}

pub struct Rendered {
    pub key: Key,
    pub page: usize,
    pub w: u32,
    pub h: u32,
    pub result: Result<Vec<u8>, String>,
}

struct Queue {
    wanted: Vec<Request>,
    /// The page being rendered now (a fresh `want` doesn't ask for it twice).
    running: Option<(Key, usize)>,
}

struct Shared {
    queue: Mutex<Queue>,
    wake: Condvar,
    results: Mutex<Vec<Rendered>>,
    hwnd: isize,
}

pub struct Renderer {
    shared: Arc<Shared>,
}

/// An idle document is closed after this long, so a PDF left behind doesn't keep its bytes in memory.
const IDLE_CLOSE: Duration = Duration::from_secs(3);

impl Renderer {
    pub fn start(hwnd: HWND) -> Renderer {
        let shared = Arc::new(Shared {
            queue: Mutex::new(Queue { wanted: Vec::new(), running: None }),
            wake: Condvar::new(),
            results: Mutex::new(Vec::new()),
            hwnd: hwnd.0 as isize,
        });
        let s = shared.clone();
        std::thread::Builder::new().name("pdf-pages".into()).spawn(move || worker(&s)).expect("spawn page renderer");
        Renderer { shared }
    }

    /// What the viewport wants now, most wanted first. Replaces the previous wish list: a page that scrolled
    /// out of view before its turn is never rendered.
    pub fn want(&self, mut wanted: Vec<Request>) {
        let mut q = self.shared.queue.lock().unwrap();
        if let Some((k, p)) = &q.running {
            wanted.retain(|r| !(r.key == *k && r.page == *p));
        }
        q.wanted = wanted;
        drop(q);
        self.shared.wake.notify_one();
    }

    pub fn take(&self) -> Vec<Rendered> {
        std::mem::take(&mut *self.shared.results.lock().unwrap())
    }
}

fn worker(s: &Shared) {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
    }
    let factory: Option<IWICImagingFactory> = unsafe { CoCreateInstance(&CLSID_WICImagingFactory2, None, CLSCTX_INPROC_SERVER).ok() };
    // The open document: consecutive pages of one file don't re-read it.
    let mut open: Option<((PathBuf, u64), windows::Data::Pdf::PdfDocument)> = None;
    loop {
        let req = {
            let mut q = s.queue.lock().unwrap();
            q.running = None;
            loop {
                if !q.wanted.is_empty() {
                    let r = q.wanted.remove(0);
                    q.running = Some((r.key.clone(), r.page));
                    break r;
                }
                let (g, t) = s.wake.wait_timeout(q, IDLE_CLOSE).unwrap();
                q = g;
                if t.timed_out() && q.wanted.is_empty() {
                    open = None;
                }
            }
        };
        let file = (req.key.path.clone(), req.key.stamp);
        if open.as_ref().is_none_or(|(f, _)| *f != file) {
            open = crate::imaging::pdf::open(&req.key.path).ok().map(|d| (file, d));
        }
        let result = match (&factory, &open) {
            (Some(f), Some((_, doc))) => crate::imaging::pdf::render(f, doc, req.page as u32, req.w, req.h),
            (None, _) => Err("WIC is unavailable".into()),
            (_, None) => Err("the document didn't open".into()),
        };
        if let Err(e) = &result {
            crate::trace::mark(format!("page {} FAIL: {e}", req.page + 1));
        }
        s.results.lock().unwrap().push(Rendered { key: req.key, page: req.page, w: req.w, h: req.h, result });
        unsafe {
            let _ = PostMessageW(Some(HWND(s.hwnd as _)), WM_PAGE_RENDERED, WPARAM(0), LPARAM(0));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pages_run_left_to_right_centred_on_the_tallest() {
        let l = Layout::compute(&[(816.0, 1056.0), (1056.0, 816.0), (400.0, 600.0)]);
        assert_eq!(l.pages[0], Rect { x: 0.0, y: 0.0, w: 816.0, h: 1056.0 });
        assert_eq!(l.pages[1], Rect { x: 848.0, y: 120.0, w: 1056.0, h: 816.0 });
        assert_eq!(l.pages[2].x, 848.0 + 1056.0 + GAP);
        assert_eq!(l.width, 848.0 + 1056.0 + GAP + 400.0);
        assert_eq!(l.height, 1056.0);
        assert_eq!(l.max_edge(), 1056.0);
    }

    #[test]
    fn a_broken_page_still_gets_a_slot() {
        let l = Layout::compute(&[(f32::NAN, 0.0), (100.0, 100.0)]);
        assert_eq!(l.pages[0].w, 1.0);
        assert_eq!(l.pages[1].x, 1.0 + GAP);
    }

    #[test]
    fn page_at_picks_the_nearest_centre() {
        let l = Layout::compute(&[(100.0, 100.0); 4]);
        assert_eq!(l.page_at(-50.0), 0);
        assert_eq!(l.page_at(50.0), 0);
        assert_eq!(l.page_at(190.0), 1);
        assert_eq!(l.page_at(1e9), 3);
    }

    #[test]
    fn eviction_keeps_page_one_and_drops_the_furthest() {
        assert_eq!(resident_limit(1), MAX_RESIDENT);
        assert_eq!(resident_limit(usize::MAX), MIN_RESIDENT);
        assert_eq!(victim(&[0, 1, 2], 2, 4), None);
        assert_eq!(victim(&[0, 1, 2, 3], 2, 4), Some(3)); // a tie goes to the later page
        assert_eq!(victim(&[0, 1, 5, 6], 6, 4), Some(1));
        assert_eq!(victim(&[0, 9], 1, 2), Some(9));
    }
}
