//! Subtitles over the video (native/PLAN.md N6e). subs.rs finds the tracks (the video's own streams, sidecar files
//! beside it) and reads the one chosen on a thread of its own; this draws what shows at the picture on screen,
//! minus the delay, in the UI layer (8-bit, SDR white under HDR), placed and scaled with the video's rect on
//! screen (zoom, pan, fit):
//!
//! - plain text (SubRip, WebVTT, mov_text...): laid out here with DirectWrite in the subtitle style in use
//!   (`substyle.rs`: font, colour, outline, shadow, a box behind; Looker's own look is white Inter on a dark
//!   outline and soft shadow), its italics, bold and colours kept (`Gfx::styled_text`), lifted above the
//!   controls bar while it's up;
//! - ASS/SSA: libass's picture of the moment, at the video rect's size in pixels, with the file's own fonts;
//! - bitmap subtitles (PGS, VobSub, DVB): their pictures, placed on their canvas stretched over the video.
//!
//! The pick: what was chosen for this file before (`subs.txt` in the data dir, with its delay), else a sidecar,
//! else a track the file marks forced or default (`subs::pick_default`), else none. Keys (VLC's): G and H move the
//! subtitles 10 ms earlier or later, V goes through the tracks.

use std::cell::RefCell;
use std::collections::HashMap;

use windows::Win32::Graphics::Direct2D::Common::D2D1_COLOR_F;
use windows::Win32::Graphics::DirectWrite::*;

use super::*;
use crate::imaging::video::{SUB_ASS, SUB_BITMAP, Track as StreamTrack};
use crate::subs::{self, Choice, Sidecar, Source};
use crate::substyle::{Backdrop, SubStyle};

/// G and H move the subtitles by this much.
const DELAY_STEP_US: i64 = 10_000;
/// The text's height at 100 %: this much of the video's height (or of its width's 16:9 height, if narrower).
const TEXT_SCALE: f32 = 0.052;
/// libass draws at most this many pixels across (zoomed far in, its picture is scaled up beyond).
const ASS_MAX_PX: f32 = 3840.0;
const MEMORY_CAP: usize = 500;

/// A text group's picture: the events it shows, at what size, for which alignment.
struct Sprite {
    key: (Vec<u64>, u32, u32, u32, u64),
    bitmap: ID2D1Bitmap1,
    /// Size in DIPs and the margin the outline takes (the layout's origin in it).
    w: f32,
    h: f32,
    pad: f32,
}

pub(super) struct SubView {
    /// What shows; None until the player has opened and Looker picked.
    pub choice: Option<Choice>,
    track: Option<subs::Track>,
    pub sidecars: Vec<Sidecar>,
    /// Subtitles show this much later (negative: earlier).
    pub delay_us: i64,
    /// Text pictures by ASS alignment (1-9).
    sprites: HashMap<u8, Sprite>,
    /// libass's last picture: the bitmap, where it sits in the frame (pixels) and the frame's size.
    ass: Option<(Option<ID2D1Bitmap1>, (i32, i32, u32, u32), (u32, u32))>,
    /// Bitmap events' pictures, by event id.
    pics: HashMap<u64, Vec<ID2D1Bitmap1>>,
    /// What to draw this frame: bitmaps and where, in DIPs.
    shown: Vec<(ID2D1Bitmap1, D2D_RECT_F)>,
    /// The reader hasn't caught up with the playhead yet: draw again next vsync (paused, nothing else would).
    pub busy: bool,
}

impl Default for SubView {
    fn default() -> SubView {
        SubView { choice: None, track: None, sidecars: Vec::new(), delay_us: 0, sprites: HashMap::new(), ass: None, pics: HashMap::new(), shown: Vec::new(), busy: false }
    }
}

impl SubView {
    /// Bitmaps die with the device.
    pub(super) fn device_lost(&mut self) {
        self.sprites.clear();
        self.ass = None;
        self.pics.clear();
        self.shown.clear();
    }

    fn clear_pictures(&mut self) {
        self.device_lost();
    }
}

// --- What was chosen per file: `subs.txt`, `<delay µs>\t<choice>\t<video path>` a line, newest last ---

thread_local! {
    static MEMORY: RefCell<Option<Vec<(PathBuf, i64, Choice)>>> = const { RefCell::new(None) };
}

fn memory_file() -> Option<PathBuf> {
    settings::data_dir().map(|d| d.join("subs.txt"))
}

fn with_memory<R>(f: impl FnOnce(&mut Vec<(PathBuf, i64, Choice)>) -> R) -> R {
    MEMORY.with(|m| {
        let mut m = m.borrow_mut();
        let list = m.get_or_insert_with(|| {
            let text = memory_file().and_then(|p| std::fs::read_to_string(p).ok()).unwrap_or_default();
            text.lines()
                .filter_map(|l| {
                    let mut it = l.splitn(3, '\t');
                    let delay = it.next()?.parse().ok()?;
                    let choice = Choice::from_text(it.next()?)?;
                    Some((PathBuf::from(it.next()?), delay, choice))
                })
                .collect()
        });
        f(list)
    })
}

fn remembered(video: &Path) -> Option<(i64, Choice)> {
    with_memory(|l| l.iter().rev().find(|(p, _, _)| crate::explorer::same_path(p, video)).map(|(_, d, c)| (*d, c.clone())))
}

fn remember(video: &Path, delay: i64, choice: &Choice) {
    with_memory(|l| {
        l.retain(|(p, _, _)| !crate::explorer::same_path(p, video));
        l.push((video.to_path_buf(), delay, choice.clone()));
        if l.len() > MEMORY_CAP {
            l.drain(..l.len() - MEMORY_CAP);
        }
        let Some(file) = memory_file() else { return };
        let text: String = l.iter().map(|(p, d, c)| format!("{d}\t{}\t{}\n", c.to_text(), p.display())).collect();
        let _ = std::fs::write(file, text);
    });
}

/// The text size for a video rect `w × h` DIPs at `percent`.
pub(super) fn text_size(w: f32, h: f32, percent: u32) -> f32 {
    let base = h.min(w * 9.0 / 16.0 * 1.333);
    (base * TEXT_SCALE * percent as f32 / 100.0).clamp(11.0, 300.0)
}

impl App {
    /// The video's rect on screen this frame, as `draw_viewport` places the picture.
    fn video_dest(&mut self) -> D2D_RECT_F {
        let r = self.view.frame();
        let a = self.image_area();
        D2D_RECT_F { left: a.left + r.x as f32, top: a.top + r.y as f32, right: a.left + (r.x + r.w) as f32, bottom: a.top + (r.y + r.h) as f32 }
    }

    /// Once the player is open: the sidecars are looked for and the track picked (what was chosen for this file
    /// before, else `subs::pick_default`).
    fn pick_subtitles(&mut self) {
        let ui = subs::ui_language();
        let Some(v) = self.video.as_mut() else { return };
        let Some(p) = v.player.as_ref().filter(|p| p.info().is_some()) else { return };
        v.subs.sidecars = subs::find_sidecars(&v.path);
        let streams: Vec<subs::StreamFacts> = p.sub_tracks().iter().map(|t| (t.stream, t.lang(), t.is_default != 0, t.flags & crate::imaging::video::TRACK_FORCED != 0)).collect();
        let audio = p.audio_tracks().iter().find(|t| t.playing != 0).map(|t| t.lang()).unwrap_or_default();
        let choice = match remembered(&v.path) {
            Some((delay, c)) => {
                v.subs.delay_us = delay;
                // A file chosen then gone, or a stream the file no longer has: the default instead.
                let valid = match &c {
                    Choice::Off => true,
                    Choice::Stream(s) => streams.iter().any(|t| t.0 == *s),
                    Choice::File(f) => f.is_file(),
                };
                if valid { c } else { subs::pick_default(&streams, &v.subs.sidecars, &audio, &ui) }
            }
            None => subs::pick_default(&streams, &v.subs.sidecars, &audio, &ui),
        };
        if let Choice::File(f) = &choice {
            if !v.subs.sidecars.iter().any(|s| crate::explorer::same_path(&s.path, f)) {
                v.subs.sidecars.push(sidecar_for(f));
            }
        }
        v.subs.choice = Some(choice);
    }

    /// The reader for the choice (opened, or dropped for Off), if it isn't already.
    fn sync_subtitle_track(&mut self) {
        let Some(v) = self.video.as_mut() else { return };
        let Some(p) = v.player.as_ref().filter(|p| p.info().is_some()) else { return };
        let want = match &v.subs.choice {
            None | Some(Choice::Off) => None,
            Some(Choice::Stream(s)) => Some(Source::Stream(*s)),
            Some(Choice::File(f)) => Some(Source::File(f.clone())),
        };
        if v.subs.track.as_ref().map(|t| &t.source) == want.as_ref() {
            return;
        }
        v.subs.clear_pictures();
        let fps = p.info().and_then(|i| i.fps()).unwrap_or(0.0);
        v.subs.track = want.map(|src| {
            let zero = if matches!(src, Source::Stream(_)) { p.start_us() } else { 0 };
            subs::Track::open(&v.path, src, zero, fps)
        });
    }

    /// Before the frame begins (Direct2D isn't mid-draw): what shows now, made into bitmaps placed for this frame.
    pub(super) fn prepare_subtitles(&mut self, g: &Gfx) {
        if !self.video_shown() || self.video_bitmap().is_none() {
            if let Some(v) = self.video.as_mut() {
                v.subs.shown.clear();
                v.subs.busy = false;
            }
            return;
        }
        if self.video.as_ref().is_some_and(|v| v.subs.choice.is_none() && v.player.as_ref().is_some_and(|p| p.info().is_some())) {
            self.pick_subtitles();
        }
        self.sync_subtitle_track();
        let dest = self.video_dest();
        let style = self.settings.sub_style().clone();
        let percent = style.size.round() as u32;
        let bar = self.video.as_ref().and_then(|v| v.bar_top);
        let Some(v) = self.video.as_mut() else { return };
        v.subs.shown.clear();
        v.subs.busy = false;
        let Some(p) = v.player.as_ref() else { return };
        let Some(info) = p.info() else { return };
        let (video_w, video_h) = (info.width.max(1) as u32, info.height.max(1) as u32);
        let t = p.position() - v.subs.delay_us;
        let Some(track) = v.subs.track.as_ref() else { return };
        track.set_time(t);
        v.subs.busy = !track.settled(t);
        let Some(Ok(sinfo)) = track.info() else { return };
        let (dw, dh) = (dest.right - dest.left, dest.bottom - dest.top);
        if dw < 1.0 || dh < 1.0 {
            return;
        }
        let k = g.dpi / 96.0;
        match sinfo.kind {
            SUB_ASS => {
                let scale = (ASS_MAX_PX / (dw * k).max(dh * k)).min(1.0);
                let (fw, fh) = (((dw * k * scale).round() as u32).clamp(1, g.max_bitmap_size()), ((dh * k * scale).round() as u32).clamp(1, g.max_bitmap_size()));
                let active = track.any_active(t);
                if !active {
                    v.subs.ass = None;
                } else {
                    let frame_changed = v.subs.ass.as_ref().is_none_or(|a| a.2 != (fw, fh));
                    match track.render(t, fw, fh, video_w, video_h, percent as f64 / 100.0) {
                        Some(Some(img)) => {
                            let b = g.bitmap(img.w, img.h, &img.bgra).ok();
                            v.subs.ass = Some((b, (img.x, img.y, img.w, img.h), (fw, fh)));
                        }
                        Some(None) => v.subs.ass = Some((None, (0, 0, 0, 0), (fw, fh))),
                        None if frame_changed => v.subs.ass = None,
                        None => {}
                    }
                }
                if let Some((Some(b), (x, y, w, h), (fw, fh))) = &v.subs.ass {
                    let (sx, sy) = (dw / *fw as f32, dh / *fh as f32);
                    let r = D2D_RECT_F { left: dest.left + *x as f32 * sx, top: dest.top + *y as f32 * sy, right: dest.left + (*x as f32 + *w as f32) * sx, bottom: dest.top + (*y as f32 + *h as f32) * sy };
                    v.subs.shown.push((b.clone(), r));
                }
            }
            SUB_BITMAP => {
                let events = track.active(t);
                v.subs.pics.retain(|id, _| events.iter().any(|e| e.id == *id));
                for e in &events {
                    if !v.subs.pics.contains_key(&e.id) {
                        let bs: Vec<ID2D1Bitmap1> = e.rects.iter().filter_map(|r| g.bitmap(r.w, r.h, &r.bgra).ok()).collect();
                        v.subs.pics.insert(e.id, bs);
                    }
                    let (cw, ch) = match e.canvas {
                        (w, h) if w > 0 && h > 0 => (w as f32, h as f32),
                        _ if sinfo.canvas_w > 0 && sinfo.canvas_h > 0 => (sinfo.canvas_w as f32, sinfo.canvas_h as f32),
                        _ => (video_w as f32, video_h as f32),
                    };
                    let (sx, sy) = (dw / cw, dh / ch);
                    for (r, b) in e.rects.iter().zip(v.subs.pics[&e.id].iter()) {
                        let rr = D2D_RECT_F { left: dest.left + r.x as f32 * sx, top: dest.top + r.y as f32 * sy, right: dest.left + (r.x as f32 + r.w as f32) * sx, bottom: dest.top + (r.y as f32 + r.h as f32) * sy };
                        v.subs.shown.push((b.clone(), rr));
                    }
                }
            }
            _ => {
                let events = track.active(t);
                let size = text_size(dw, dh, percent);
                // The sides keep Looker's 5 %; the style sets the bottom's.
                let margin = dh * 0.05;
                let bottom = dh * style.margin / 100.0;
                let style_key = style.key();
                // (Zoomed far in, the picture's width is capped: it's a bitmap that wide.)
                let max_w = (dw * 0.9).clamp(40.0, 4000.0);
                // One picture per alignment; within one, the newest event on top (as libass stacks them).
                let mut groups: HashMap<u8, Vec<&subs::Event>> = HashMap::new();
                for e in events.iter().rev() {
                    if let Some(s) = &e.styled {
                        groups.entry(s.align).or_default().push(e);
                    }
                }
                v.subs.sprites.retain(|a, _| groups.contains_key(a));
                for (&align, evs) in &groups {
                    let key = (evs.iter().map(|e| e.id).collect::<Vec<_>>(), (size * 4.0).round() as u32, max_w.round() as u32, g.dpi.round() as u32, style_key);
                    if v.subs.sprites.get(&align).is_none_or(|s| s.key != key) {
                        let lines: Vec<Vec<subs::Run>> = evs.iter().filter_map(|e| e.styled.as_ref()).flat_map(|s| s.lines.iter().cloned()).collect();
                        match text_sprite(g, &lines, align, size, max_w, &style) {
                            Some((bitmap, w, h, pad)) => {
                                v.subs.sprites.insert(align, Sprite { key, bitmap, w, h, pad });
                            }
                            None => {
                                v.subs.sprites.remove(&align);
                            }
                        }
                    }
                    let Some(s) = v.subs.sprites.get(&align) else { continue };
                    // Placed by the layout's box (its whole width, `max_w`), the outline's margin around it.
                    let col = (align - 1) % 3;
                    let row = (align - 1) / 3; // 0 bottom, 1 middle, 2 top
                    let lx = match col {
                        0 => dest.left + margin,
                        1 => (dest.left + dest.right) / 2.0 - max_w / 2.0,
                        _ => dest.right - margin - max_w,
                    };
                    let text_h = s.h - 2.0 * s.pad;
                    let mut ly = match row {
                        0 => dest.bottom - bottom - text_h,
                        1 => (dest.top + dest.bottom) / 2.0 - text_h / 2.0,
                        _ => dest.top + margin,
                    };
                    // Clear of the controls bar while it's up (as it fades in and out, the text moves with it).
                    if row == 0 {
                        if let Some((top, alpha)) = bar {
                            let over = ly + text_h - (top - 8.0);
                            if over > 0.0 {
                                ly -= over * alpha.clamp(0.0, 1.0);
                            }
                        }
                    }
                    let (x, y) = (g.snap(lx - s.pad), g.snap(ly - s.pad));
                    v.subs.shown.push((s.bitmap.clone(), rect(x, y, s.w, s.h)));
                }
            }
        }
    }

    /// The pictures `prepare_subtitles` placed, over the video and under the controls (UI layer).
    pub(super) fn draw_subtitles(&self, g: &Gfx, clip: D2D_RECT_F) {
        let Some(v) = self.video.as_ref().filter(|v| !v.subs.shown.is_empty()) else { return };
        g.push_clip(clip);
        for (b, r) in &v.subs.shown {
            g.draw_bitmap(b, *r, 1.0);
        }
        g.pop_clip();
    }

    /// The video's subtitle tracks and sidecars for the Subtitles menu.
    pub(super) fn subtitle_entries(&self) -> Vec<ui::Entry<menus::Action>> {
        use menus::{Action, check};
        let mut e = Vec::new();
        let Some(v) = self.video.as_ref() else { return e };
        let choice = v.subs.choice.clone().unwrap_or(Choice::Off);
        e.push(check(Action::SubOff, "Off", choice == Choice::Off));
        if let Some(p) = v.player.as_ref() {
            for (n, t) in p.sub_tracks().iter().enumerate() {
                e.push(check(Action::SubStream(t.stream), &t.sub_label(n + 1), choice == Choice::Stream(t.stream)));
            }
        }
        if !v.subs.sidecars.is_empty() {
            e.push(ui::Entry::Separator);
            for (i, s) in v.subs.sidecars.iter().enumerate() {
                e.push(check(Action::SubFile(i), &s.label(), matches!(&choice, Choice::File(f) if crate::explorer::same_path(f, &s.path))));
            }
        }
        e.push(ui::Entry::Separator);
        e.push(ui::Entry::Item(ui::MenuItem { icon: None, tint: None, action: Action::SubAdd, glyph: Some(0xE8E5), label: "Add subtitle file\u{2026}".into(), accel: None, check: None, enabled: true, current: false }));
        e.push(ui::Entry::Separator);
        e.push(ui::Entry::Heading(subs::delay_text(v.subs.delay_us).replace("Subtitle delay", "Delay")));
        let item = |a: Action, label: &str, accel: Option<&'static str>, enabled: bool| ui::Entry::Item(ui::MenuItem { icon: None, tint: None, action: a, glyph: None, label: label.into(), accel, check: None, enabled, current: false });
        e.push(item(Action::SubDelay(-1), "Earlier", Some("G"), true));
        e.push(item(Action::SubDelay(1), "Later", Some("H"), true));
        e.push(item(Action::SubDelay(0), "Reset delay", None, v.subs.delay_us != 0));
        e.push(ui::Entry::Separator);
        e.push(ui::Entry::Heading("Style".into()));
        for (i, st) in self.settings.subtitle_styles.iter().enumerate() {
            e.push(check(Action::SubStyle(i), &st.name, i == self.settings.subtitle_style));
        }
        e.push(ui::Entry::Item(ui::MenuItem { icon: None, tint: None, action: Action::SubStyles, glyph: Some(0xE790), label: "Customise styles\u{2026}".into(), accel: None, check: None, enabled: true, current: false }));
        e.push(ui::Entry::Separator);
        e.push(ui::Entry::Heading("Size".into()));
        let size = self.settings.sub_style().size.round() as u32;
        for s in settings::SUBTITLE_SIZES {
            let label = if s == 100 { "Normal".to_string() } else { format!("{s}%") };
            e.push(check(Action::SubSize(s as u16), &label, size == s));
        }
        e
    }

    /// The Subtitles menu's style `i` picked: in use at once, said in a toast.
    pub(super) fn pick_subtitle_style(&mut self, i: usize) {
        if i >= self.settings.subtitle_styles.len() {
            return;
        }
        self.settings.subtitle_style = i;
        settings::save(&self.settings);
        let name = self.settings.sub_style().name.clone();
        self.show_video_toast(format!("Subtitle style: {name}"));
        self.invalidate();
    }

    /// Shows `choice` (the menu, V, a file added): remembered for this file.
    pub(super) fn set_subtitles(&mut self, choice: Choice) {
        let label = self.subtitle_label(&choice);
        let Some(v) = self.video.as_mut() else { return };
        if let Choice::File(f) = &choice {
            if !v.subs.sidecars.iter().any(|s| crate::explorer::same_path(&s.path, f)) {
                v.subs.sidecars.push(sidecar_for(f));
            }
        }
        remember(&v.path, v.subs.delay_us, &choice);
        v.subs.choice = Some(choice);
        self.show_video_toast(label);
        self.invalidate();
    }

    /// What the toast says for a choice: "Subtitles off", or the track's menu label.
    fn subtitle_label(&self, choice: &Choice) -> String {
        let Some(v) = self.video.as_ref() else { return String::new() };
        match choice {
            Choice::Off => "Subtitles off".into(),
            Choice::Stream(s) => {
                let tracks: &[StreamTrack] = v.player.as_ref().map_or(&[], |p| p.sub_tracks());
                tracks.iter().enumerate().find(|(_, t)| t.stream == *s).map(|(n, t)| t.sub_label(n + 1)).unwrap_or_else(|| "Subtitles".into())
            }
            Choice::File(f) => sidecar_for(f).label(),
        }
    }

    /// V: off, each of the file's tracks, each sidecar, and round again.
    pub(super) fn cycle_subtitles(&mut self) {
        let Some(v) = self.video.as_ref() else { return };
        let mut all = vec![Choice::Off];
        if let Some(p) = v.player.as_ref() {
            all.extend(p.sub_tracks().iter().map(|t| Choice::Stream(t.stream)));
        }
        all.extend(v.subs.sidecars.iter().map(|s| Choice::File(s.path.clone())));
        let now = v.subs.choice.clone().unwrap_or(Choice::Off);
        let i = all.iter().position(|c| *c == now).unwrap_or(0);
        let next = all[(i + 1) % all.len()].clone();
        self.set_subtitles(next);
    }

    /// G (-1), H (+1), or back to none (0): the delay, said in a toast and remembered for this file.
    pub(super) fn subtitle_delay(&mut self, step: i32) {
        let Some(v) = self.video.as_mut() else { return };
        v.subs.delay_us = if step == 0 { 0 } else { v.subs.delay_us + step as i64 * DELAY_STEP_US };
        let choice = v.subs.choice.clone().unwrap_or(Choice::Off);
        remember(&v.path, v.subs.delay_us, &choice);
        let text = subs::delay_text(v.subs.delay_us);
        self.show_video_toast(text);
        self.invalidate();
    }

    /// The size of the style in use.
    pub(super) fn subtitle_size(&mut self, percent: u32) {
        self.settings.sub_style_mut().set(crate::substyle::Num::Size, percent as f32);
        settings::save(&self.settings);
        if let Some(v) = self.video.as_mut() {
            v.subs.sprites.clear();
        }
        self.show_video_toast(if percent == 100 { "Subtitles normal size".to_string() } else { format!("Subtitles {percent}%") });
        self.invalidate();
    }

    /// "Add subtitle file…": a file picked from the video's folder, shown at once.
    pub(super) fn add_subtitle_file(&mut self) {
        use windows::Win32::UI::Shell::Common::COMDLG_FILTERSPEC;
        let Some(dir) = self.video.as_ref().and_then(|v| v.path.parent().map(Path::to_path_buf)) else { return };
        let picked = unsafe {
            let Ok(dlg) = CoCreateInstance::<_, IFileOpenDialog>(&FileOpenDialog, None, CLSCTX_INPROC_SERVER) else { return };
            let pattern = HSTRING::from(subs::SIDECAR_EXTENSIONS.iter().chain(["txt"].iter()).map(|e| format!("*.{e}")).collect::<Vec<_>>().join(";"));
            let filters = [COMDLG_FILTERSPEC { pszName: w!("Subtitles"), pszSpec: PCWSTR(pattern.as_ptr()) }, COMDLG_FILTERSPEC { pszName: w!("All files"), pszSpec: w!("*.*") }];
            let _ = dlg.SetFileTypes(&filters);
            let _ = dlg.SetTitle(w!("Add subtitle file"));
            if let Ok(folder) = SHCreateItemFromParsingName::<_, _, IShellItem>(&HSTRING::from(dir.as_os_str()), None) {
                let _ = dlg.SetFolder(&folder);
            }
            if dlg.Show(Some(self.hwnd)).is_err() {
                return;
            }
            let Ok(item) = dlg.GetResult() else { return };
            let Ok(name) = item.GetDisplayName(SIGDN_FILESYSPATH) else { return };
            let path = PathBuf::from(name.to_string().unwrap_or_default());
            CoTaskMemFree(Some(name.0 as _));
            path
        };
        self.set_subtitles(Choice::File(picked));
    }
}

/// A sidecar entry for a file picked by hand (its name may not follow the video's).
fn sidecar_for(path: &Path) -> Sidecar {
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let stem = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let ext = path.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
    // Its own stem's tags: "x.en.forced.srt" against "x".
    let base = stem.split('.').next().unwrap_or(&stem).to_string();
    let (lang, forced, sdh, _) = subs::sidecar_name(&base, &name).unwrap_or((String::new(), false, false, ext.clone()));
    Sidecar { path: path.to_path_buf(), lang, forced, sdh, ext }
}

/// What `Gfx::styled_text` needs from a style, at a text size of `size` DIPs.
pub(super) fn text_look(style: &SubStyle, size: f32) -> gfx::TextLook {
    gfx::TextLook {
        size,
        colour: style.colour,
        // Looker's look had an outline of at least 1.2 DIPs on small text.
        outline: if style.outline > 0.0 { (size * style.outline / 100.0).max(1.0) } else { 0.0 },
        outline_colour: style.outline_colour,
        shadow: style.shadow / 100.0,
        shadow_colour: style.shadow_colour,
        shadow_distance: size * style.shadow_distance / 100.0,
        shadow_blur: (size * style.shadow_blur / 100.0).max(0.5),
        backdrop: match style.backdrop {
            Backdrop::None => 0,
            Backdrop::Lines => 1,
            Backdrop::Block => 2,
        },
        backdrop_colour: style.backdrop_colour,
        backdrop_alpha: style.backdrop_opacity / 100.0,
        backdrop_round: style.backdrop_round / 100.0,
    }
}

/// A text group's picture: `lines` of styled runs in `style`, wrapped at `max_w`, `size` DIPs, aligned as ASS's
/// numpad `align` says.
pub(super) fn text_sprite(g: &Gfx, lines: &[Vec<subs::Run>], align: u8, size: f32, max_w: f32, style: &SubStyle) -> Option<(ID2D1Bitmap1, f32, f32, f32)> {
    let mut text: Vec<u16> = Vec::new();
    // (start, length, run) in UTF-16 units.
    let mut ranges: Vec<(u32, u32, subs::Run)> = Vec::new();
    for line in lines {
        if !text.is_empty() {
            text.push('\n' as u16);
        }
        for r in line {
            let w: Vec<u16> = r.text.encode_utf16().collect();
            ranges.push((text.len() as u32, w.len() as u32, r.clone()));
            text.extend(w);
        }
    }
    if text.iter().all(|&c| c == '\n' as u16 || c == ' ' as u16 || c == 0xA0) {
        return None;
    }
    unsafe {
        let format = g.text.subtitle_format_in(&style.font, style.weight, style.italic, size).ok()?;
        let layout = g.text.dwrite.CreateTextLayout(&text, &format, max_w, 100_000.0).ok()?;
        let _ = layout.SetTextAlignment(match (align - 1) % 3 {
            0 => DWRITE_TEXT_ALIGNMENT_LEADING,
            1 => DWRITE_TEXT_ALIGNMENT_CENTER,
            _ => DWRITE_TEXT_ALIGNMENT_TRAILING,
        });
        let inter = style.font == crate::substyle::INTER;
        for (start, len, r) in &ranges {
            let range = DWRITE_TEXT_RANGE { startPosition: *start, length: *len };
            if r.italic {
                let _ = layout.SetFontStyle(DWRITE_FONT_STYLE_ITALIC, range);
            }
            if r.bold {
                // Two steps heavier than the style (Inter: its SemiBold, a family of its own here).
                let w = DWRITE_FONT_WEIGHT((style.weight as i32 + 200).min(900));
                if inter {
                    let _ = layout.SetFontFamilyName(g.text.family(w), range);
                }
                let _ = layout.SetFontWeight(w, range);
            }
            if r.underline {
                let _ = layout.SetUnderline(true, range);
            }
            if r.strike {
                let _ = layout.SetStrikethrough(true, range);
            }
            if let (Some(c), true) = (r.color, style.file_colours) {
                let col: D2D1_COLOR_F = gfx::rgb(c);
                if let Ok(brush) = g.dev.dc.CreateSolidColorBrush(&col, None) {
                    let _ = layout.SetDrawingEffect(&brush, range);
                }
            }
        }
        g.styled_text(&layout, &text_look(style, size)).ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes() {
        // 1080p fitted 1920 × 1080: ~56 DIPs; a 9:16 phone video is sized by its width.
        assert!((text_size(1920.0, 1080.0, 100) - 56.16).abs() < 0.01);
        assert!(text_size(608.0, 1080.0, 100) < 30.0);
        assert_eq!(text_size(100.0, 50.0, 100), 11.0);
        assert!((text_size(1920.0, 1080.0, 150) / text_size(1920.0, 1080.0, 100) - 1.5).abs() < 0.001);
        let s = sidecar_for(Path::new(r"C:\x\Show.S01E01.en.forced.srt"));
        assert_eq!((s.lang.as_str(), s.forced, s.ext.as_str()), ("en", true, "srt"));
    }
}
