//! Rip DVD (native/PLAN.md N8), VLC's Convert/Save for a disc: the explorer's menu on a DVD drive or a folder
//! holding VIDEO_TS offers it. The disc is read on a file-op thread (`dvd::open`, plus the copy-protection check:
//! Looker rips only unprotected discs), then a dialog picks the title, the format (MKV with every track copied,
//! or MP4 with H.264 + AAC) and the file name; the rip runs on its own thread into the Videos folder while a pill
//! at the bottom left shows its progress, with Cancel.

use std::sync::Arc;
use std::time::{Duration, Instant};
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU64, Ordering};

use super::*;
use crate::dvd::{self, Disc};
use crate::fileops::{self, Done};
use crate::textedit::TextEdit;
use crate::ui::{Choices, Dialog, DialogButton, Focus, TextField};
use super::actions::{Choice, DialogKind};

pub(super) const TIMER_RIP: usize = 13;
/// The dialog lists at most this many titles: the longest ones, in disc order (menus' loops under 10 s left out).
const MAX_TITLES: usize = 6;

pub(super) struct RipSetup {
    disc: Disc,
    /// The titles offered, as indices into `disc.titles`.
    titles: Vec<usize>,
}

pub(super) struct Ripping {
    name: String,
    cancel: Arc<AtomicI32>,
    done: Arc<AtomicU64>,
    finished: Arc<AtomicBool>,
    total: u64,
    started: Instant,
}

const PILL_H: f32 = 44.0;

impl App {
    /// "Rip DVD…" on `path` (a drive, a folder holding VIDEO_TS, or VIDEO_TS itself).
    pub(super) fn open_dvd(&mut self, path: PathBuf) {
        if self.ripping.is_some() {
            self.show_toast("A DVD is already being ripped", false);
            return;
        }
        self.show_toast("Reading the DVD\u{2026}", false);
        fileops::spawn(self.hwnd, false, move || {
            let r = dvd::open(&path).map_err(|e| e.to_string()).and_then(|d| {
                let main = d.titles.iter().max_by_key(|t| t.duration_ms).expect("open returns titles");
                match dvd::scrambled(&d, main) {
                    Ok(false) => Ok(Box::new(d)),
                    Ok(true) => Err(dvd::PROTECTED.to_string()),
                    Err(e) => Err(format!("Couldn't read the DVD: {e}")),
                }
            });
            Done::DvdOpened(r)
        });
    }

    pub(super) fn on_dvd_opened(&mut self, r: Result<Box<Disc>, String>) {
        let disc = match r {
            Ok(d) => *d,
            Err(e) => {
                self.notice("Can't rip this DVD", e);
                return;
            }
        };
        let longest = disc.titles.iter().enumerate().max_by_key(|(_, t)| t.duration_ms).map_or(0, |(i, _)| i);
        let mut titles: Vec<usize> = (0..disc.titles.len()).filter(|&i| disc.titles[i].duration_ms >= 10_000).collect();
        if titles.is_empty() {
            titles = (0..disc.titles.len()).collect();
        }
        titles.sort_by_key(|&i| std::cmp::Reverse(disc.titles[i].duration_ms));
        titles.truncate(MAX_TITLES);
        titles.sort_unstable();
        let rows = titles
            .iter()
            .map(|&i| {
                let t = &disc.titles[i];
                let name = if i == longest { format!("Title {} (main title)", t.number) } else { format!("Title {}", t.number) };
                (name, t.summary())
            })
            .collect();
        let main = titles.iter().position(|&i| i == longest).unwrap_or(0);
        let choices = vec![
            Choices { label: "Title".into(), rows, selected: main },
            Choices {
                label: "Format".into(),
                rows: vec![
                    ("MKV: every track copied".into(), "Lossless and quick: the DVD's own video, every audio and subtitle track, chapters".into()),
                    ("MP4: H.264 and AAC".into(), "Smaller and plays anywhere: re-encoded by Windows' video encoder, no subtitles".into()),
                ],
                selected: 0,
            },
        ];
        let mut edit = TextEdit::new(&disc.name);
        edit.select_all();
        let buttons = vec![DialogButton::new("Rip", Choice::Confirm), DialogButton::new("Cancel", Choice::Cancel)];
        let mut d = Dialog::new(format!("Rip \u{201C}{}\u{201D}", disc.name), "Saves to your Videos folder.".into(), buttons, 0, false)
            .with_field(TextField::new(edit))
            .with_choices(choices);
        d.field_label = Some("File name".into());
        d.focus = Focus::Choices(0);
        self.open_dialog_ui(DialogKind::Rip(Box::new(RipSetup { disc, titles })), d);
    }

    /// The dialog's Rip: title pick, format pick (1 = MP4), the file name typed.
    pub(super) fn start_rip(&mut self, setup: RipSetup, title: usize, mp4: bool, name: String) {
        let Some(&ti) = setup.titles.get(title) else { return };
        let Some(folder) = videos_folder() else {
            self.notice("Couldn't rip the DVD", "Windows didn't say where your Videos folder is.".into());
            return;
        };
        let clean: String = name.trim().chars().map(|c| if "<>:\"/\\|?*".contains(c) || c.is_control() { '_' } else { c }).collect();
        let base = if clean.trim_matches(['.', ' ']).is_empty() { dvd::default_name(&setup.disc, &setup.disc.titles[ti]) } else { clean };
        let out = dvd::unique_path(&folder, &base, if mp4 { "mp4" } else { "mkv" });
        let ripping = Ripping {
            name: file_name(&out),
            cancel: Arc::new(AtomicI32::new(0)),
            done: Arc::new(AtomicU64::new(0)),
            finished: Arc::new(AtomicBool::new(false)),
            total: setup.disc.titles[ti].bytes().max(1),
            started: Instant::now(),
        };
        let (cancel, done, finished) = (ripping.cancel.clone(), ripping.done.clone(), ripping.finished.clone());
        let disc = setup.disc;
        fileops::spawn(self.hwnd, false, move || {
            let r = dvd::rip(&disc, &disc.titles[ti], &out, mp4, &cancel, &done);
            finished.store(true, Ordering::Release);
            Done::Ripped { out, result: r }
        });
        self.ripping = Some(ripping);
        unsafe {
            SetTimer(Some(self.hwnd), TIMER_RIP, 500, None);
        }
        self.invalidate();
    }

    pub(super) fn on_ripped(&mut self, out: PathBuf, result: Result<(), String>) {
        self.ripping = None;
        unsafe {
            let _ = KillTimer(Some(self.hwnd), TIMER_RIP);
        }
        self.invalidate();
        match result {
            Ok(()) => self.show_toast(format!("Ripped to Videos\\{}", file_name(&out)), false),
            Err(e) if e == "Cancelled" => self.show_toast("Rip cancelled", false),
            Err(e) => self.notice("Couldn't rip the DVD", e),
        }
    }

    pub(super) fn cancel_rip(&mut self) {
        if let Some(r) = &self.ripping {
            r.cancel.store(1, Ordering::Relaxed);
            self.invalidate();
        }
    }

    /// The window is closing: stop the rip and give it a moment to remove its unfinished file.
    pub(super) fn stop_rip_for_close(&mut self) {
        let Some(r) = &self.ripping else { return };
        r.cancel.store(1, Ordering::Relaxed);
        let t = Instant::now();
        while !r.finished.load(Ordering::Acquire) && t.elapsed() < Duration::from_secs(3) {
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// A message with OK.
    pub(super) fn notice(&mut self, title: &str, body: String) {
        let d = Dialog::new(title.into(), body, vec![DialogButton::new("OK", Choice::Cancel)], 0, false);
        self.open_dialog_ui(DialogKind::Notice, d);
    }

    /// The rip's pill at the bottom left: what is ripping, how far, the time left, a bar and Cancel.
    pub(super) fn draw_rip(&mut self, g: &Gfx) {
        let Some(r) = &self.ripping else { return };
        let frac = (r.done.load(Ordering::Relaxed) as f64 / r.total as f64).clamp(0.0, 1.0);
        let stopping = r.cancel.load(Ordering::Relaxed) != 0;
        let text = if stopping {
            "Stopping the rip\u{2026}".to_string()
        } else {
            let secs = r.started.elapsed().as_secs_f64();
            let left = if secs > 3.0 && frac > 0.01 { secs * (1.0 - frac) / frac } else { -1.0 };
            let eta = match left {
                l if l < 0.0 => String::new(),
                l if l < 60.0 => " \u{00B7} under a minute left".into(),
                l => format!(" \u{00B7} about {} min left", (l / 60.0).round() as u64),
            };
            format!("Ripping {} \u{00B7} {}%{eta}", r.name, (frac * 100.0).floor() as u32)
        };
        let (_, h) = self.size_dip();
        let raised = theme::raised();
        let tw = g.measure(&wide(&text), &g.fonts.body).ceil().min(420.0);
        let bw = 72.0;
        let w = 16.0 + tw + 12.0 + bw + 8.0;
        // Above the status row.
        let pill = rect(16.0, g.snap(h - STATUS_H - 12.0 - PILL_H), w, PILL_H);
        self.hits.add(Hit::RipPill, pill);
        ui::shadow(g, pill, 8.0);
        g.fill_round(pill, 8.0, rgb(raised.popup));
        ui::stroke(g, pill, 8.0, rgb(raised.divider));
        ui::label(g, &text, &g.fonts.body, rect(pill.left + 16.0, pill.top, tw, PILL_H - 2.0), rgb(raised.text), Align::Left);
        // The bar along the bottom edge, inside the corners.
        let track = rect(pill.left + 12.0, pill.bottom - 7.0, w - 24.0, 3.0);
        g.fill_round(track, 1.5, rgb(raised.divider));
        g.fill_round(rect(track.left, track.top, ((track.right - track.left) as f64 * frac) as f32, 3.0), 1.5, rgb(theme::t().accent));
        let b = rect(pill.right - 8.0 - bw, pill.top + 8.0, bw, PILL_H - 18.0);
        self.hits.add(Hit::RipCancel, b);
        let st = self.state(Hit::RipCancel, !stopping);
        let fg = ui::button_frame(g, b, ui::Kind::Standard, &st);
        ui::label(g, "Cancel", &g.fonts.body, b, fg, Align::Center);
    }
}

fn videos_folder() -> Option<PathBuf> {
    use windows::Win32::UI::Shell::{FOLDERID_Videos, KF_FLAG_DEFAULT, SHGetKnownFolderPath};
    unsafe {
        let p = SHGetKnownFolderPath(&FOLDERID_Videos, KF_FLAG_DEFAULT, None).ok()?;
        let s = p.to_string().unwrap_or_default();
        windows::Win32::System::Com::CoTaskMemFree(Some(p.0 as _));
        (!s.is_empty()).then(|| PathBuf::from(s))
    }
}
