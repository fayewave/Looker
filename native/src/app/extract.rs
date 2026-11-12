//! Extracting archives from the explorer (its row menu), without opening them: "Extract to “photos”" unpacks
//! each archive picked into a new folder beside it named after it (one click), "Extract to…" into a folder picked
//! (several archives: each into its own folder there). It runs on a file-op thread while a pill at the bottom
//! left (rip.rs's, stacked above a rip's) shows how far, with Cancel. An archive holding just one folder of its
//! own name is unpacked without it, so "photos.zip" doesn't become "photos\photos".

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use super::*;
use crate::fileops::{self, Done};
use crate::imaging::archive as arc;

pub(super) const TIMER_EXTRACT: usize = 15;

pub(super) struct Extracting {
    /// The archive being read ("photos.zip", "photos.zip (2 of 3)").
    label: Arc<Mutex<String>>,
    cancel: Arc<AtomicBool>,
    done: Arc<AtomicU64>,
    total: Arc<AtomicU64>,
}

/// What a finished extraction did: the folders written into, files, encrypted left out, failed; or why not.
pub struct ExtractResult {
    pub dests: Vec<PathBuf>,
    pub result: Result<(usize, usize, usize), String>,
}

/// Whether every path is an archive (the row menu's Extract items).
pub(super) fn all_archives(paths: &[PathBuf]) -> bool {
    !paths.is_empty() && paths.iter().all(|p| p.is_file() && format::of_path(p) == format::Format::Archive)
}

/// The prefix to take off an archive's paths when all of it is in one folder named `name` (the folder it's
/// unpacked into already has that name), else "".
fn own_folder(list: &[arc::Entry], name: &str) -> String {
    let top = |p: &str| p.trim_start_matches('/').split('/').next().unwrap_or("").to_string();
    let Some(first) = list.first().map(|e| top(&e.path)) else { return String::new() };
    let one = list.iter().all(|e| top(&e.path) == first) && list.iter().any(|e| e.path.trim_start_matches('/').contains('/'));
    if one && first.eq_ignore_ascii_case(name) { first } else { String::new() }
}

impl App {
    /// The row menu's Extract: beside each archive into a folder named after it, or (`pick`) into a folder picked.
    pub(super) fn extract_archives(&mut self, archives: Vec<PathBuf>, pick: bool) {
        if self.extracting.is_some() || archives.is_empty() {
            return;
        }
        let jobs: Vec<(PathBuf, PathBuf)> = if pick {
            let Some(start) = archives[0].parent().map(Path::to_path_buf) else { return };
            let Some(dest) = (unsafe { self.pick_folder(&start) }) else { return };
            if archives.len() == 1 {
                vec![(archives[0].clone(), dest)]
            } else {
                // Each its own folder there; names taken by an earlier one of these count as taken.
                let mut out: Vec<(PathBuf, PathBuf)> = Vec::new();
                for a in archives {
                    let name = archive::base_name(&a);
                    let mut d = archive::unique_folder(&dest, &name);
                    let mut n = 2;
                    while out.iter().any(|(_, o)| o == &d) {
                        d = dest.join(format!("{name} ({n})"));
                        n += 1;
                    }
                    out.push((a, d));
                }
                out
            }
        } else {
            let mut out: Vec<(PathBuf, PathBuf)> = Vec::new();
            for a in archives {
                let Some(parent) = a.parent() else { continue };
                let name = archive::base_name(&a);
                let mut d = archive::unique_folder(parent, &name);
                let mut n = 2;
                while out.iter().any(|(_, o)| o == &d) {
                    d = parent.join(format!("{name} ({n})"));
                    n += 1;
                }
                out.push((a, d));
            }
            out
        };
        let ex = Extracting {
            label: Arc::new(Mutex::new(String::new())),
            cancel: Arc::new(AtomicBool::new(false)),
            done: Arc::new(AtomicU64::new(0)),
            total: Arc::new(AtomicU64::new(0)),
        };
        let (label, cancel, done, total) = (ex.label.clone(), ex.cancel.clone(), ex.done.clone(), ex.total.clone());
        fileops::spawn(self.hwnd, false, move || {
            let n = jobs.len();
            let dests: Vec<PathBuf> = jobs.iter().map(|(_, d)| d.clone()).collect();
            // Listed first: the total for the bar, and whether each is all in one folder of its own name.
            let mut lists = Vec::new();
            for (a, _) in &jobs {
                if let Ok(mut l) = label.lock() {
                    *l = file_name(a);
                }
                match arc::list(a) {
                    Ok(list) => {
                        total.fetch_add(list.iter().filter(|e| !e.dir).map(|e| e.size.max(0) as u64).sum(), Ordering::Relaxed);
                        lists.push(list);
                    }
                    Err(e) => return Done::Extracted(ExtractResult { dests, result: Err(format!("{}: {e}", file_name(a))) }),
                }
            }
            let (mut files, mut enc, mut failed) = (0, 0, 0);
            let mut base = 0u64;
            for (i, ((a, d), list)) in jobs.iter().zip(&lists).enumerate() {
                if let Ok(mut l) = label.lock() {
                    *l = if n > 1 { format!("{} ({} of {n})", file_name(a), i + 1) } else { file_name(a) };
                }
                let strip = own_folder(list, &archive::base_name(a));
                let r = arc::extract(a, d, &strip, &|_| true, &mut |written, _| {
                    done.store(base + written, Ordering::Relaxed);
                    !cancel.load(Ordering::Relaxed)
                });
                base += list.iter().filter(|e| !e.dir).map(|e| e.size.max(0) as u64).sum::<u64>();
                match r {
                    Ok(x) => {
                        files += x.files;
                        enc += x.skipped_encrypted;
                        failed += x.failed;
                    }
                    Err(e) => return Done::Extracted(ExtractResult { dests, result: Err(format!("{}: {e}", file_name(a))) }),
                }
                if cancel.load(Ordering::Relaxed) {
                    return Done::Extracted(ExtractResult { dests, result: Err("cancelled".into()) });
                }
            }
            Done::Extracted(ExtractResult { dests, result: Ok((files, enc, failed)) })
        });
        self.extracting = Some(ex);
        unsafe {
            SetTimer(Some(self.hwnd), TIMER_EXTRACT, 250, None);
        }
        self.invalidate();
    }

    pub(super) fn on_extracted(&mut self, r: ExtractResult) {
        self.extracting = None;
        unsafe {
            let _ = KillTimer(Some(self.hwnd), TIMER_EXTRACT);
        }
        let to = match r.dests.as_slice() {
            [one] => format!("\u{201c}{}\u{201d}", file_name(one)),
            more => format!("{} folders", more.len()),
        };
        match r.result {
            Ok((files, enc, failed)) => {
                let mut msg = format!("Extracted {files} file{} to {to}", if files == 1 { "" } else { "s" });
                if enc > 0 {
                    msg.push_str(&format!(", {enc} encrypted left out"));
                }
                if failed > 0 {
                    msg.push_str(&format!(", {failed} couldn't be written"));
                }
                self.show_toast(msg, false);
            }
            Err(e) if e == "cancelled" => self.show_toast("Extraction cancelled", false),
            Err(e) => self.show_toast(format!("Couldn't extract: {e}"), false),
        }
        self.invalidate();
    }

    pub(super) fn cancel_extract(&mut self) {
        if let Some(x) = &self.extracting {
            x.cancel.store(true, Ordering::Relaxed);
            self.invalidate();
        }
    }

    /// The window is closing: stop (what's half written stays, as a cancel leaves it).
    pub(super) fn stop_extract_for_close(&mut self) {
        if let Some(x) = &self.extracting {
            x.cancel.store(true, Ordering::Relaxed);
        }
    }

    pub(super) fn draw_extract(&mut self, g: &Gfx) {
        let Some(x) = &self.extracting else { return };
        let total = x.total.load(Ordering::Relaxed);
        let frac = if total == 0 { 0.0 } else { (x.done.load(Ordering::Relaxed) as f64 / total as f64).clamp(0.0, 1.0) };
        let stopping = x.cancel.load(Ordering::Relaxed);
        let label = x.label.lock().map(|l| l.clone()).unwrap_or_default();
        let text = if stopping { "Stopping\u{2026}".to_string() } else { format!("Extracting {label} \u{00B7} {}%", (frac * 100.0).floor() as u32) };
        let bottom = self.pill_stack_bottom();
        self.draw_progress_pill(g, bottom, &text, frac, Hit::ExtractPill, Hit::ExtractCancel, stopping);
    }
}
