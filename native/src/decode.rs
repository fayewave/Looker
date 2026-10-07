//! The decode pool: a priority queue of jobs and a few worker threads that run [`crate::imaging::decode`].
//! Decodes can't be cancelled once running (WIC has no cancellation), so the queue is where work is
//! shed: jobs that leave the preload window are dropped before they start, and a queued neighbour that
//! becomes current is moved to the front rather than decoded twice.

use std::collections::VecDeque;
use std::path::Path;
use std::sync::atomic::{AtomicIsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};

use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::Graphics::Imaging::{CLSID_WICImagingFactory2, IWICImagingFactory};
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx};
use windows::Win32::UI::WindowsAndMessaging::PostMessageW;

use crate::engine::{self, Key};
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
    pub key: Key,
    pub priority: Priority,
}

pub struct Done {
    pub key: Key,
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
            // Behind other current-image work (the placeholder before the sharp decode), ahead of preloads.
            let at = q.iter().position(|j| j.priority != Priority::Current).unwrap_or(q.len());
            q.insert(at, job);
        } else {
            q.push_back(job);
        }
        drop(q);
        self.wake.notify_one();
    }

    /// Makes a queued job current-priority. False when it isn't queued (running already, or never submitted).
    pub fn promote(&self, key: &Key) -> bool {
        let mut q = self.queue.lock().unwrap();
        let Some(i) = q.iter().position(|j| &j.key == key) else { return false };
        let mut job = q.remove(i).unwrap();
        job.priority = Priority::Current;
        let at = q.iter().position(|j| j.priority != Priority::Current).unwrap_or(q.len());
        q.insert(at, job);
        true
    }

    /// Drops queued (not yet started) jobs the predicate rejects; returns their keys.
    pub fn retain(&self, keep: impl Fn(&Job) -> bool) -> Vec<Key> {
        let mut dropped = Vec::new();
        self.queue.lock().unwrap().retain(|j| {
            let k = keep(j);
            if !k {
                dropped.push(j.key.clone());
            }
            k
        });
        dropped
    }

    pub fn take_results(&self) -> Vec<Done> {
        std::mem::take(&mut *self.results.lock().unwrap())
    }

    /// Blocks until any result for `path` is in, or `timeout_ms` passes. Startup only.
    pub fn wait_for(&self, path: &Path, timeout_ms: u64) -> bool {
        let deadline = std::time::Instant::now() + std::time::Duration::from_millis(timeout_ms);
        loop {
            if self.results.lock().unwrap().iter().any(|d| d.key.path == path) {
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
            let name = job.key.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            crate::trace::mark(format!("decode start {name} bucket={} {:?}", job.key.bucket, job.priority));
            let (bw, bh) = engine::box_for(job.key.bucket);
            // The placeholder tier is a still first frame, even for an animation.
            let still = job.key.bucket == engine::LOW;
            let result = match &factory {
                Some(f) => crate::imaging::decode_for_screen(f, &job.key.path, bw, bh, still, &crate::colour::current()),
                None => Err("WIC is unavailable".into()),
            }
            .map(|mut d| {
                // ~1 ms for a screen-size decode; the info card then has it the moment the image lands.
                d.histogram = d.frames.first().map(|f| Box::new(crate::metadata::Histogram::of(&f.pixels)));
                d
            });
            match &result {
                Ok(d) => crate::trace::mark(format!(
                    "decode done  {name} {}x{} (native {}x{})",
                    d.width, d.height, d.native_width, d.native_height
                )),
                Err(e) => crate::trace::mark(format!("decode FAIL  {name}: {e}")),
            }
            self.results.lock().unwrap().push(Done { key: job.key, result });
            self.notify();
        }
    }
}
