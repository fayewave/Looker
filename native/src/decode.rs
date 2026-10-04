//! The decode pool: a priority queue of jobs and a few worker threads that run [`crate::imaging::decode`].

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicIsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};

use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::Graphics::Imaging::{CLSID_WICImagingFactory2, IWICImagingFactory};
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx};
use windows::Win32::UI::WindowsAndMessaging::PostMessageW;

pub use crate::imaging::Decoded;

/// Posted to the window when results are waiting in [`Pool::take_results`].
pub const WM_DECODED: u32 = windows::Win32::UI::WindowsAndMessaging::WM_APP + 1;

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
                Some(f) => crate::imaging::decode(f, &job.path, job.box_w, job.box_h),
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

