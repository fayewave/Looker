//! Paste edits onto photos (round 3, Faye 2026-11-14): the tone and colour copied in edit mode (or the last edit
//! saved) put onto each photo picked in the explorer and saved, over it (the original to the Recycle Bin first, as
//! edit mode's Save) or as a copy beside it. It runs on a file-op thread, one photo after another, while a pill at
//! the bottom left (extract.rs's) shows how far, with Cancel. The viewer's Paste edits opens edit mode instead,
//! with the edit pasted, to be looked at before saving (`edit::App::edit_paste`).

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;

use super::*;
use crate::fileops::{self, Done};
use crate::photoedit::{self, Params};

pub(super) const TIMER_PASTE: usize = 17;

pub(super) struct Pasting {
    cancel: Arc<AtomicBool>,
    done: Arc<AtomicUsize>,
    total: usize,
}

/// What a paste did: the files written (each photo and its new file), and those that failed and why.
pub struct PasteResult {
    pub written: Vec<(PathBuf, PathBuf)>,
    pub failed: Vec<(PathBuf, String)>,
    pub copy: bool,
    pub cancelled: bool,
}

/// The photos among `paths` an edit can be pasted onto.
pub(super) fn pasteable(paths: &[PathBuf]) -> Vec<PathBuf> {
    paths.iter().filter(|p| p.is_file() && super::edit::editable_format(p)).cloned().collect()
}

impl App {
    /// Whether there's an edit to paste.
    pub(super) fn has_edit_clipboard(&self) -> bool {
        self.edit_clipboard.is_some()
    }

    /// The explorer's Paste edits: the edit copied onto each photo, saved over it or (`copy`) as a copy.
    pub(super) fn paste_edits(&mut self, paths: Vec<PathBuf>, copy: bool) {
        let Some(tone) = self.edit_clipboard else { return };
        let photos = pasteable(&paths);
        if self.pasting.is_some() || photos.is_empty() {
            return;
        }
        // Over the original only where it can be written back; anything else gets a copy.
        let p = Params { crop: [0.0; 4], ..tone.tone_only() };
        let owner = self.hwnd.0 as isize;
        let pasting = Pasting { cancel: Arc::new(AtomicBool::new(false)), done: Arc::new(AtomicUsize::new(0)), total: photos.len() };
        let (cancel, done) = (pasting.cancel.clone(), pasting.done.clone());
        fileops::spawn(self.hwnd, false, move || {
            let mut r = PasteResult { written: Vec::new(), failed: Vec::new(), copy, cancelled: false };
            for path in photos {
                if cancel.load(Ordering::Relaxed) {
                    r.cancelled = true;
                    break;
                }
                let as_copy = copy || !photoedit::writable(&path);
                match photoedit::save(&path, &p, as_copy, owner) {
                    Ok(out) => r.written.push((path, out)),
                    Err(e) => {
                        crate::trace::mark(format!("paste edits failed on {}: {e}", path.display()));
                        r.failed.push((path, e));
                    }
                }
                done.fetch_add(1, Ordering::Relaxed);
            }
            Done::EditsPasted(r)
        });
        self.pasting = Some(pasting);
        unsafe {
            SetTimer(Some(self.hwnd), TIMER_PASTE, 250, None);
        }
        self.invalidate();
    }

    pub(super) fn on_edits_pasted(&mut self, r: PasteResult) {
        self.pasting = None;
        unsafe {
            let _ = KillTimer(Some(self.hwnd), TIMER_PASTE);
        }
        // The photo showing, if one was written over, shows its new pixels.
        let current = self.current_path().map(Path::to_path_buf);
        for (path, out) in &r.written {
            self.viewer.forget(path);
            if !r.copy && current.as_ref().is_some_and(|c| crate::explorer::same_path(c, path)) && self.edit.is_none() {
                self.turns = 0;
                let stamp = self.viewer.listing.as_mut().and_then(|l| l.refresh(out)).unwrap_or_else(|| crate::engine::stamp(out));
                let shown = self.viewer.show(out.clone(), stamp, self.gfx.as_ref());
                self.apply(shown);
                self.current_changed();
            }
        }
        let n = r.written.len();
        let copies = r.written.iter().filter(|(a, b)| a != b).count();
        let mut msg = if n > 0 && copies == n {
            format!("Pasted edits into {n} cop{}", if n == 1 { "y" } else { "ies" })
        } else {
            format!("Pasted edits onto {n} photo{}", if n == 1 { "" } else { "s" })
        };
        if !r.failed.is_empty() {
            msg.push_str(&format!(", {} couldn't be saved", r.failed.len()));
        }
        if r.cancelled {
            msg.push_str(" (stopped)");
        }
        self.show_toast(msg, false);
        self.invalidate();
    }

    pub(super) fn cancel_paste(&mut self) {
        if let Some(p) = &self.pasting {
            p.cancel.store(true, Ordering::Relaxed);
            self.invalidate();
        }
    }

    /// The window is closing: stop after the photo being saved.
    pub(super) fn stop_paste_for_close(&mut self) {
        if let Some(p) = &self.pasting {
            p.cancel.store(true, Ordering::Relaxed);
        }
    }

    pub(super) fn draw_paste(&mut self, g: &Gfx) {
        let Some(p) = &self.pasting else { return };
        let done = p.done.load(Ordering::Relaxed);
        let frac = (done as f64 / p.total.max(1) as f64).clamp(0.0, 1.0);
        let stopping = p.cancel.load(Ordering::Relaxed);
        let text = if stopping { "Stopping\u{2026}".to_string() } else { format!("Pasting edits \u{00B7} {} of {}", (done + 1).min(p.total), p.total) };
        // Above an extraction's pill when there is one.
        let bottom = self.pill_stack_bottom() - if self.extracting.is_some() { super::rip::PILL_H + 8.0 } else { 0.0 };
        self.draw_progress_pill(g, bottom, &text, frac, Hit::PastePill, Hit::PasteCancel, stopping);
    }
}
