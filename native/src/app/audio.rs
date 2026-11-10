//! An audio file on screen, after Transient's waveform tab: the cover large (or a placeholder), the title and who
//! made it under it, and the waveform below across the photo's area, above the controls bar (app/video.rs, which
//! plays it through `Engine::Audio`).
//!
//! The waveform (waveform.rs fills it in as it reads): one lane per channel, each the min/max envelope mirrored about
//! its centre with the RMS inside it, what's played in the accent; a ruler of times above, the playhead, a ghost
//! line where the pointer is. A click seeks; a drag marks a region, which then loops; Ctrl+wheel zooms about the
//! pointer, Shift+wheel pans, a scrollbar shows where the view is while zoomed in, and the view follows the playhead.
//! The Spectrogram toggle swaps the lanes for the spectrogram (log frequency, the channels mixed).

use std::sync::Mutex;

use windows::Win32::Graphics::Direct2D::Common::D2D_RECT_U;
use windows::Win32::Graphics::Direct2D::{D2D1_INTERPOLATION_MODE_LINEAR, ID2D1Bitmap};

use super::video::Engine;
use super::*;
use crate::waveform::{BINS, FFT, Scan};

/// Off the photo area's edges.
const PAD: f32 = 24.0;
/// The row above the ruler (the region, the zoom, the mode toggle), the ruler, and the scrollbar under the lanes.
const ROW_H: f32 = 26.0;
const RULER_H: f32 = 20.0;
const SCROLL_H: f32 = 5.0;
const SCROLL_GAP: f32 = 6.0;
/// The gap between lanes.
const LANE_GAP: f32 = 6.0;
/// The spectrogram's height in rows (log frequency from `SPEC_MIN_HZ` to half the rate).
const SPEC_ROWS: u32 = 256;
const SPEC_MIN_HZ: f64 = 40.0;
/// The furthest in: this many frames across a pixel.
const MIN_FRAMES_PER_PX: f64 = 0.125;
/// A region shorter than this is a click.
const MIN_REGION_SECS: f64 = 0.01;
/// The ruler's steps, in seconds; the first that keeps its labels this far apart is used.
const RULER_STEPS: [f64; 19] = [0.01, 0.02, 0.05, 0.1, 0.2, 0.5, 1.0, 2.0, 5.0, 10.0, 15.0, 30.0, 60.0, 120.0, 300.0, 600.0, 900.0, 1800.0, 3600.0];
const RULER_MIN_PX: f32 = 90.0;

#[derive(Clone, Copy, PartialEq)]
pub(super) enum Mode {
    Wave,
    Spectrum,
}

enum Drag {
    /// Pressed on the lanes: dragging scrubs, seeking as it goes (Transient's; Faye, 2026-11-11).
    Press { x: f32, frame: f64 },
    /// Marking a region, from and to (frames). Off for now (Faye, 2026-11-11: a drag seeks instead); kept for
    /// when it comes back with a modifier.
    #[allow(dead_code)]
    Select { from: f64, to: f64 },
    /// The scrollbar's thumb held, this many frames from the view's start.
    Scroll { grab: f64 },
}

type ArtJob = Arc<Mutex<Option<Result<(u32, u32, Vec<u8>), String>>>>;

pub(super) struct AudioView {
    path: PathBuf,
    scan: Scan,
    /// The cover on the GPU, and its size in pixels; its pixels are kept to make it again on a new device.
    art: Option<(ID2D1Bitmap1, f32, f32)>,
    art_px: Option<(u32, u32, Vec<u8>)>,
    art_job: Option<ArtJob>,
    /// The player's cover bytes were looked at (taken, or there were none).
    art_asked: bool,
    mode: Mode,
    /// The view: its first frame and how many frames across. None: the whole file.
    span: Option<(f64, f64)>,
    drag: Option<Drag>,
    /// The spectrogram as a bitmap (columns across, `SPEC_ROWS` down), the columns put in it so far, and the
    /// accent it was coloured with.
    spec: Option<(ID2D1Bitmap1, usize, usize, u32)>,
    /// The envelope per pixel column, kept while the view, the size and the data are the same.
    cache: Option<WaveCache>,
    /// The file's length went to the player once the scan counted it.
    total_sent: bool,
}

struct WaveCache {
    key: (u64, u64, u64, i32, usize),
    /// Per lane, per column: (min, max, rms); the columns stop where the data does.
    lanes: Vec<Vec<(f32, f32, f32)>>,
}

/// Where everything goes.
struct Layout {
    /// The cover's square, and the text block under it (title, byline, format line).
    art: D2D_RECT_F,
    title: D2D_RECT_F,
    row: D2D_RECT_F,
    ruler: D2D_RECT_F,
    lanes: D2D_RECT_F,
    scroll: D2D_RECT_F,
}

/// "1:05", "1:05.4", "1:05.42" (to the step's precision).
fn ruler_label(secs: f64, step: f64) -> String {
    let decimals = if step < 0.1 { 2 } else if step < 1.0 { 1 } else { 0 };
    let m = (secs / 60.0).floor();
    let s = secs - m * 60.0;
    let s = match decimals {
        0 => format!("{:02}", s.round() as i64),
        d => format!("{:0w$.d$}", s, w = 3 + d, d = d),
    };
    format!("{}:{}", m as i64, s)
}

/// "1:05.42": a position to the hundredth.
/// The audio bar's clock: "3:19.400".
pub(super) fn precise_us(us: i64) -> String {
    precise(us.max(0) as f64 / 1e6)
}

fn precise(secs: f64) -> String {
    let secs = secs.max(0.0);
    let h = (secs / 3600.0).floor() as i64;
    let m = ((secs / 60.0).floor() as i64) % 60;
    let s = secs % 60.0;
    if h > 0 { format!("{h}:{m:02}:{s:05.2}") } else { format!("{m}:{s:05.2}") }
}

/// The spectrogram's colours: black through a deep red to the accent, then to a warm white.
fn spec_colour(v: u8, accent: u32) -> [u8; 3] {
    let (ar, ag, ab) = (((accent >> 16) & 0xFF) as f32, ((accent >> 8) & 0xFF) as f32, (accent & 0xFF) as f32);
    let stops: [(f32, [f32; 3]); 6] = [
        (0.0, [0.0, 0.0, 0.0]),
        (0.38, [ar * 0.16, ag * 0.08, ab * 0.12]),
        (0.6, [ar * 0.55, ag * 0.3, ab * 0.35]),
        (0.8, [ar, ag, ab]),
        (0.92, [255.0, 170.0, 110.0]),
        (1.0, [255.0, 246.0, 225.0]),
    ];
    let t = v as f32 / 255.0;
    let i = stops.iter().rposition(|s| s.0 <= t).unwrap_or(0).min(stops.len() - 2);
    let (a, b) = (stops[i], stops[i + 1]);
    let k = ((t - a.0) / (b.0 - a.0)).clamp(0.0, 1.0);
    let c = |n: usize| (a.1[n] + (b.1[n] - a.1[n]) * k).round().clamp(0.0, 255.0) as u8;
    [c(2), c(1), c(0)] // BGR
}

/// Which spectrogram row (0 at the top) frequency `hz` falls on.
fn row_of(hz: f64, rate: f64) -> f64 {
    let top = rate / 2.0;
    (1.0 - (hz / SPEC_MIN_HZ).ln() / (top / SPEC_MIN_HZ).ln()) * SPEC_ROWS as f64
}

impl AudioView {
    /// Bitmaps die with their device: the cover is made again from its pixels, the spectrogram from the scan.
    pub(super) fn device_lost(&mut self) {
        self.art = None;
        self.spec = None;
    }
}

impl App {
    /// After every navigation and before each frame: an audio file on screen gets its view (and scan).
    pub(super) fn prepare_audio(&mut self, g: &Gfx) {
        let want = self.video.as_ref().filter(|v| v.audio && self.placeholder.is_none()).map(|v| v.path.clone());
        if self.audio_view.as_ref().map(|a| &a.path) != want.as_ref() {
            self.audio_view = want.map(|path| AudioView {
                scan: Scan::start(&path, None),
                path,
                art: None,
                art_px: None,
                art_job: None,
                art_asked: false,
                mode: if self.settings.audio_spectrum { Mode::Spectrum } else { Mode::Wave },
                span: None,
                drag: None,
                spec: None,
                cache: None,
                total_sent: false,
            });
        }
        if self.audio_view.is_none() {
            return;
        }
        // The cover: its bytes come with the player; decoded on a thread of its own.
        let cover = if self.audio_view.as_ref().is_some_and(|a| !a.art_asked) {
            match self.player().and_then(Engine::audio) {
                Some(p) if p.info.is_some() => Some(p.cover.take()),
                _ => None,
            }
        } else {
            None
        };
        let (position, playing, total_frames) = match self.player_ref().and_then(Engine::audio_ref) {
            Some(p) => (Some(p.position_frame() as f64), p.playing(), p.total_frames()),
            None => (None, false, 0),
        };
        let max = g.max_bitmap_size();
        let accent = theme::t().accent;
        let Some(a) = self.audio_view.as_mut() else { return };
        if let Some(bytes) = cover {
            a.art_asked = true;
            if let Some(bytes) = bytes {
                let job: ArtJob = Arc::new(Mutex::new(None));
                let j = job.clone();
                std::thread::Builder::new()
                    .name("audio-cover".into())
                    .spawn(move || {
                        let r = (|| {
                            unsafe {
                                let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
                            }
                            let f: windows::Win32::Graphics::Imaging::IWICImagingFactory =
                                unsafe { CoCreateInstance(&windows::Win32::Graphics::Imaging::CLSID_WICImagingFactory2, None, CLSCTX_INPROC_SERVER) }.map_err(|e| e.message().to_string())?;
                            let d = crate::imaging::audio::decode_cover(&f, &bytes, 1600, 1600)?;
                            let px = d.frames.into_iter().next().ok_or("no picture")?.pixels;
                            Ok((d.width, d.height, px))
                        })();
                        if let Ok(mut s) = j.lock() {
                            *s = Some(r);
                        }
                    })
                    .ok();
                a.art_job = Some(job);
            }
        }
        if let Some(job) = &a.art_job {
            let got = job.lock().ok().and_then(|mut s| s.take());
            if let Some(r) = got {
                a.art_job = None;
                match r {
                    Ok(px) => a.art_px = Some(px),
                    Err(e) => crate::trace::mark(format!("audio cover: {e}")),
                }
            }
        }
        // On the GPU (again, after a device change).
        if a.art.is_none() {
            if let Some((w, h, px)) = &a.art_px {
                match g.photo_bitmap(*w, *h, px, &crate::imaging::Colour::Srgb) {
                    Ok(b) => a.art = Some((b, *w as f32, *h as f32)),
                    Err(e) => {
                        crate::trace::mark(format!("audio cover: {}", e.message()));
                        a.art_px = None;
                    }
                }
            }
        }
        // The spectrogram's new columns into its bitmap.
        {
            let d = a.scan.data();
            let cols = d.columns();
            if a.mode == Mode::Spectrum && cols > 0 {
                let want_w = {
                    let est = if d.hop > 0 { ((d.total().max(0) as usize).saturating_sub(FFT) / d.hop + 2).max(cols) } else { cols };
                    est.min(max as usize).max(1)
                };
                // Made as wide as the file will need; again (from the start) if the file turns out longer, or the
                // accent changes.
                let need = cols.min(max as usize);
                if a.spec.as_ref().is_none_or(|s| s.3 != accent || need > s.1) {
                    let w = want_w.max(need);
                    let blank = vec![0u8; w * SPEC_ROWS as usize * 4];
                    a.spec = g.bitmap(w as u32, SPEC_ROWS, &blank).ok().map(|b| (b, w, 0, accent));
                }
                if let Some((bmp, w, done, _)) = a.spec.as_mut() {
                    let upto = cols.min(*w);
                    if upto > *done {
                        let n = upto - *done;
                        let rate = d.rate as f64;
                        // Each row: the loudest bin between its edges.
                        let bins: Vec<(usize, usize)> = (0..SPEC_ROWS)
                            .map(|r| {
                                let hz = |row: f64| SPEC_MIN_HZ * ((rate / 2.0) / SPEC_MIN_HZ).powf(1.0 - row / SPEC_ROWS as f64);
                                let to_bin = |f: f64| ((f / (rate / FFT as f64)).round() as usize).min(BINS - 1);
                                let (lo, hi) = (to_bin(hz(r as f64 + 1.0)), to_bin(hz(r as f64)));
                                (lo.min(hi), hi.max(lo))
                            })
                            .collect();
                        let mut px = vec![0u8; n * SPEC_ROWS as usize * 4];
                        for c in 0..n {
                            let col = &d.spec[(*done + c) * BINS..(*done + c + 1) * BINS];
                            for (r, &(lo, hi)) in bins.iter().enumerate() {
                                let v = col[lo..=hi].iter().copied().max().unwrap_or(0);
                                let [b, gg, rr] = spec_colour(v, accent);
                                let o = (r * n + c) * 4;
                                px[o..o + 4].copy_from_slice(&[b, gg, rr, 255]);
                            }
                        }
                        let dst = D2D_RECT_U { left: *done as u32, top: 0, right: upto as u32, bottom: SPEC_ROWS };
                        let b: &ID2D1Bitmap = bmp;
                        unsafe {
                            let _ = b.CopyFromMemory(Some(&dst), px.as_ptr() as _, (n * 4) as u32);
                        }
                        *done = upto;
                    }
                }
            }
        }
        // The length, counted: the seek bar and the times use it.
        let counted = {
            let d = a.scan.data();
            (d.done && d.error.is_none() && !a.total_sent).then_some(d.frames)
        };
        let total = a.scan.data().total().max(total_frames);
        // Zoomed in and playing: the view follows the playhead (a page on when it runs off the right, back to it when
        // it jumps away), except while it's being dragged.
        if let (Some((start, span)), Some(pos), true, None) = (a.span, position, playing, &a.drag) {
            let start = if pos >= start + span || pos < start { if pos >= start + span && pos < start + span * 1.5 { start + span } else { pos - span * 0.1 } } else { start };
            a.span = Some((start.clamp(0.0, (total as f64 - span).max(0.0)), span));
        }
        if let Some(frames) = counted {
            a.total_sent = true;
            if let Some(p) = self.player().and_then(Engine::audio) {
                p.set_total_frames(frames);
            }
        }
    }

    /// The scan is still reading, or the cover is on its way: draw again next vsync.
    pub(super) fn audio_animating(&self) -> bool {
        self.audio_view.as_ref().is_some_and(|a| a.art_job.is_some() || !a.scan.data().done || a.drag.is_some())
    }

    /// An audio file is up (its view draws instead of the still).
    pub(super) fn audio_shown(&self) -> bool {
        self.audio_view.is_some() && self.placeholder.is_none()
    }

    fn audio_layout(&self) -> Option<Layout> {
        let a = self.audio_view.as_ref()?;
        let mut area = self.image_area();
        let w = area.right - area.left;
        // Under the bar across the top (Transient's).
        area.top += video::AUDIO_BAR_H + 12.0;
        let h = area.bottom - area.top;
        if w < 120.0 || h < 120.0 {
            return None;
        }
        let has_art = a.art_px.is_some() || a.art_job.is_some();
        let lanes_share = if has_art { 0.34 } else { 0.5 };
        let wave_h = (h * lanes_share).clamp(110.0, if has_art { 300.0 } else { 380.0 }).min(h - 40.0);
        let left = area.left + PAD;
        let right = area.right - PAD;
        let bottom = area.bottom - 4.0;
        let scroll = rect(left, bottom - SCROLL_H, right - left, SCROLL_H);
        let top = bottom - wave_h;
        let row = rect(left, top, right - left, ROW_H);
        let ruler = rect(left, row.bottom + 2.0, right - left, RULER_H);
        let lanes = D2D_RECT_F { left, top: ruler.bottom, right, bottom: scroll.top - SCROLL_GAP };
        // The cover and the text under it, centred in what's left.
        let text_h = 32.0;
        let room = top - 16.0 - (area.top + 12.0);
        let side = if has_art { (room - text_h).min(right - left).min(560.0).max(0.0) } else { (room - text_h).clamp(0.0, 132.0) };
        let block = side + text_h;
        let y0 = area.top + 12.0 + ((room - block) / 2.0).max(0.0);
        let cx = (area.left + area.right) / 2.0;
        let art = rect(cx - side / 2.0, y0, side, side);
        let title = rect(left, art.bottom + 12.0, right - left, text_h - 12.0);
        Some(Layout { art, title, row, ruler, lanes, scroll })
    }

    /// The view's first frame and frames across, clamped to the file.
    fn audio_span(&self) -> (f64, f64) {
        let total = self.audio_total().max(1) as f64;
        match self.audio_view.as_ref().and_then(|a| a.span) {
            Some((s, n)) => (s.clamp(0.0, (total - n).max(0.0)), n.min(total)),
            None => (0.0, total),
        }
    }

    fn audio_total(&self) -> i64 {
        let scanned = self.audio_view.as_ref().map_or(0, |a| a.scan.data().total());
        let player = self.player_ref().and_then(Engine::audio_ref).map_or(0, |p| p.total_frames());
        scanned.max(player)
    }

    fn audio_rate(&self) -> f64 {
        self.player_ref().and_then(Engine::audio_ref).map(|p| p.rate()).or_else(|| self.audio_view.as_ref().map(|a| a.scan.data().rate as f64)).unwrap_or(48000.0).max(1.0)
    }

    /// The cover, in the photo layer (it's a picture: under scRGB it's clamped to SDR white as photos are).
    /// Returns whether the audio view is up (the still isn't drawn then).
    pub(super) fn draw_audio_photo(&mut self, g: &Gfx) -> bool {
        if !self.audio_shown() {
            return false;
        }
        let Some(l) = self.audio_layout() else { return true };
        if let Some((b, w, h)) = self.audio_view.as_ref().and_then(|a| a.art.clone()) {
            // Fitted into its square, centred.
            let s = (l.art.right - l.art.left) / w.max(h);
            let (dw, dh) = (w * s, h * s);
            let (cx, cy) = ((l.art.left + l.art.right) / 2.0, (l.art.top + l.art.bottom) / 2.0);
            let dest = rect(g.snap(cx - dw / 2.0), g.snap(cy - dh / 2.0), g.snap(dw), g.snap(dh));
            if dw > 1.0 {
                g.draw_photo(&b, dest, 1.0, true);
            }
        }
        true
    }

    /// The audio bar's two lines: the title (the tag, else the file's name), and who made it with the format
    /// and the loudness.
    pub(super) fn audio_title_lines(&self) -> (String, String) {
        let stem = self.current_path().and_then(|p| p.file_stem()).map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
        let mut parts = Vec::new();
        let title = match self.player_ref().and_then(Engine::audio_ref) {
            Some(p) => {
                if let Some(f) = p.facts.as_ref() {
                    parts.push(crate::imaging::audio::format_line(f));
                }
                if p.tags.title.is_empty() { stem } else { p.tags.title.clone() }
            }
            None => stem,
        };
        if let Some(loud) = self.audio_view.as_ref().and_then(|a| a.scan.data().loudness) {
            if let Some(i) = loud.integrated {
                parts.push(format!("{:.1} LUFS", i).replace('-', "\u{2212}"));
            }
            parts.push(format!("Peak {:.1} dB", loud.peak).replace('-', "\u{2212}"));
        }
        (title, parts.join("  \u{b7}  "))
    }

    /// Everything else, in the UI layer.
    pub(super) fn draw_audio(&mut self, g: &Gfx) {
        if !self.audio_shown() {
            return;
        }
        let Some(l) = self.audio_layout() else { return };
        let t = theme::t();
        let rate = self.audio_rate();
        let (start, span) = self.audio_span();
        let total = self.audio_total();
        let lw = (l.lanes.right - l.lanes.left).max(1.0);
        let x_of = |f: f64| l.lanes.left + ((f - start) / span.max(1e-9)) as f32 * lw;

        // The cover's edge, or the placeholder; then the title, who made it, and the format.
        let has_art = self.audio_view.as_ref().is_some_and(|a| a.art.is_some());
        let side = l.art.right - l.art.left;
        if side > 8.0 {
            if has_art {
                if let Some((_, w, h)) = self.audio_view.as_ref().and_then(|a| a.art.clone()) {
                    let s = side / w.max(h);
                    let (dw, dh) = (w * s, h * s);
                    let (cx, cy) = ((l.art.left + l.art.right) / 2.0, (l.art.top + l.art.bottom) / 2.0);
                    let r = rect(g.snap(cx - dw / 2.0), g.snap(cy - dh / 2.0), g.snap(dw), g.snap(dh));
                    ui::stroke(g, r, 0.0, gfx::rgba(t.text, 0.12));
                }
            } else if self.audio_view.as_ref().is_some_and(|a| a.art_job.is_none()) {
                g.fill_round(l.art, 10.0, gfx::rgba(t.text, 0.06));
                ui::stroke(g, l.art, 10.0, gfx::rgba(t.text, 0.10));
                g.text(&[0xEC4F], &g.fonts.hero_icons, l.art, rgb(t.placeholder), Align::Center);
            }
        }
        let (_title, byline, mut _line) = match self.player_ref().and_then(Engine::audio_ref) {
            Some(p) => {
                let stem = self.current_path().and_then(|p| p.file_stem()).map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
                let title = if p.tags.title.is_empty() { stem } else { p.tags.title.clone() };
                (title, p.tags.byline(), p.facts.as_ref().map(crate::imaging::audio::format_line).unwrap_or_default())
            }
            None => (self.current_path().and_then(|p| p.file_stem()).map(|s| s.to_string_lossy().into_owned()).unwrap_or_default(), String::new(), String::new()),
        };
        let tr = l.title;
        if !byline.is_empty() {
            g.text(&wide(&byline), &g.fonts.body, rect(tr.left, tr.top, tr.right - tr.left, 18.0), rgb(t.label), Align::Center);
        }

        // The row: the region (with a clear button) or the zoom on the left, the mode on the right.
        let (mode, region_drag) = match self.audio_view.as_ref() {
            Some(a) => (a.mode, match a.drag {
                Some(Drag::Select { from, to }) => Some((from.min(to), from.max(to))),
                _ => None,
            }),
            None => return,
        };
        let region = region_drag.or_else(|| self.player_ref().and_then(Engine::audio_ref).and_then(|p| p.region()).map(|(a, b)| (a as f64, b as f64)));
        let labels = ["Waveform", "Spectrogram"];
        let mut right = l.row.right;
        for (i, label) in labels.iter().enumerate().rev() {
            let bw = (g.measure(&wide(label), &g.fonts.caption) + 20.0).ceil();
            right -= bw;
            let r = rect(right, l.row.top + 1.0, bw, ROW_H - 2.0);
            right -= 4.0;
            let id = Hit::AudioMode(i as u8);
            self.hits.add(id, r);
            let on = (i == 0) == (mode == Mode::Wave);
            let st = self.state(id, true);
            let fg = ui::button_frame(g, r, ui::Kind::Toggle(on), &st);
            g.text(&wide(label), &g.fonts.caption, r, fg, Align::Center);
        }
        let left_text = if let Some((a, b)) = region {
            let text = format!("Loop  {} \u{2013} {}  ({:.2} s)", precise(a / rate), precise(b / rate), (b - a) / rate);
            let tw = g.measure_tabular(&wide(&text), &g.fonts.caption).ceil();
            g.text_tabular(&wide(&text), &g.fonts.caption, rect(l.row.left, l.row.top, tw + 2.0, ROW_H), rgb(t.text), Align::Left);
            if region_drag.is_none() {
                let r = rect(l.row.left + tw + 8.0, l.row.top + 1.0, ROW_H - 2.0, ROW_H - 2.0);
                self.hits.add(Hit::AudioClear, r);
                let st = self.state(Hit::AudioClear, true);
                let fg = ui::quiet_frame(g, r, ui::Kind::Standard, &st);
                g.text(&[0xE711], &g.fonts.small_icons, r, fg, Align::Center);
            }
            None
        } else if span < total as f64 * 0.999 {
            Some(format!("Zoom {:.1}\u{00d7}", total as f64 / span.max(1.0)))
        } else {
            None
        };
        if let Some(s) = left_text {
            g.text(&wide(&s), &g.fonts.caption, rect(l.row.left, l.row.top, (right - l.row.left - 8.0).max(0.0), ROW_H), rgb(t.placeholder), Align::Left);
        }

        // The ruler.
        self.hits.add(Hit::AudioWave, D2D_RECT_F { top: l.ruler.top, ..l.lanes });
        let px_per_sec = lw as f64 * rate / span.max(1.0);
        let step = RULER_STEPS.iter().copied().find(|s| s * px_per_sec >= RULER_MIN_PX as f64).unwrap_or(3600.0);
        let first = (start / rate / step).floor() as i64;
        let last = ((start + span) / rate / step).ceil() as i64;
        g.push_clip(D2D_RECT_F { left: l.ruler.left - 1.0, right: l.ruler.right + 1.0, ..l.ruler });
        for k in first..=last {
            for m in 0..4 {
                let secs = (k as f64 + m as f64 / 4.0) * step;
                let x = g.snap(x_of(secs * rate));
                if x < l.ruler.left - 0.5 || x > l.ruler.right + 0.5 {
                    continue;
                }
                if m == 0 {
                    g.fill(rect(x, l.ruler.bottom - 9.0, g.px(), 9.0), gfx::rgba(t.text, 0.4));
                    g.text_tabular(&wide(&ruler_label(secs, step)), &g.fonts.caption, rect(x + 4.0, l.ruler.top - 2.0, 80.0, RULER_H - 4.0), rgb(t.label), Align::Left);
                } else {
                    g.fill(rect(x, l.ruler.bottom - 3.0, g.px(), 3.0), gfx::rgba(t.text, 0.22));
                }
            }
        }
        g.pop_clip();

        // Where it's heard (or where the seek bar's held).
        let pos = self.video.as_ref().map(|v| v.position()).zip(self.player_ref()).map(|(us, p)| if p.at_end() { total as f64 } else { us as f64 * rate / 1e6 });
        let played_x = pos.map_or(l.lanes.left, |p| x_of(p).clamp(l.lanes.left, l.lanes.right));

        let lanes_r = l.lanes;
        g.push_clip(lanes_r);
        match mode {
            Mode::Wave => self.draw_lanes(g, lanes_r, start, span, played_x),
            Mode::Spectrum => self.draw_spectrogram(g, lanes_r, start, span, rate),
        }
        // The region: a wash across the lanes, edged in the accent.
        if let Some((a, b)) = region {
            let (xa, xb) = (x_of(a), x_of(b));
            g.fill(D2D_RECT_F { left: xa, right: xb, ..lanes_r }, gfx::rgba(t.accent, 0.12));
            g.fill(rect(g.snap(xa), lanes_r.top, g.px(), lanes_r.bottom - lanes_r.top), rgb(t.accent));
            g.fill(rect(g.snap(xb), lanes_r.top, g.px(), lanes_r.bottom - lanes_r.top), rgb(t.accent));
        }
        g.pop_clip();
        // The pointer's ghost line and its time; the playhead over everything, a little past the lanes.
        let over = matches!(self.hover, Some(Hit::AudioWave)) && self.audio_view.as_ref().is_some_and(|a| !matches!(a.drag, Some(Drag::Scroll { .. })));
        let k = self.fades.get(Hit::AudioWave, over);
        if k > 0.0 {
            let mx = self.mouse.0.clamp(lanes_r.left, lanes_r.right);
            g.fill(rect(g.snap(mx), lanes_r.top, g.px(), lanes_r.bottom - lanes_r.top), gfx::rgba(t.text, 0.5 * k));
            let at = start + ((mx - lanes_r.left) / lw) as f64 * span;
            let label = precise(at / rate);
            let tw = g.measure_tabular(&wide(&label), &g.fonts.caption).ceil() + 14.0;
            let pill = rect(g.snap((mx - tw / 2.0).clamp(lanes_r.left, lanes_r.right - tw)), lanes_r.top + 4.0, tw, 20.0);
            let raised = theme::raised();
            g.push_fade(k, 1.0, 0.0, 0.0);
            g.fill_round(pill, 5.0, rgb(raised.popup));
            ui::stroke(g, pill, 5.0, rgb(raised.divider));
            g.text_tabular(&wide(&label), &g.fonts.caption, pill, rgb(raised.text), Align::Center);
            g.pop_fade();
        }
        if pos.is_some() && played_x >= lanes_r.left && played_x <= lanes_r.right {
            g.fill(rect(g.snap(played_x - 0.75), lanes_r.top - 4.0, 1.5, lanes_r.bottom - lanes_r.top + 8.0), rgb(t.text));
        }

        // The scrollbar, while zoomed in.
        if span < total as f64 * 0.999 && total > 0 {
            let track = l.scroll;
            self.hits.add(Hit::AudioScroll, D2D_RECT_F { top: track.top - 4.0, bottom: track.bottom + 4.0, ..track });
            g.fill_round(track, 2.5, rgb(t.track));
            let tw = (track.right - track.left) * (span / total as f64) as f32;
            let tw = tw.max(16.0);
            let tx = track.left + ((track.right - track.left - tw) as f64 * (start / (total as f64 - span).max(1.0))) as f32;
            let k = self.fades.get(Hit::AudioScroll, matches!(self.hover, Some(Hit::AudioScroll)) || self.audio_view.as_ref().is_some_and(|a| matches!(a.drag, Some(Drag::Scroll { .. }))));
            g.fill_round(rect(tx, track.top, tw, SCROLL_H), 2.5, gfx::rgba(t.text, 0.35 + 0.25 * k));
        }
    }

    /// The lanes: per channel, a faint wash (darker on the played side), the centre line and guides at half and
    /// full scale, the envelope and the RMS inside it, played in the accent.
    fn draw_lanes(&mut self, g: &Gfx, r: D2D_RECT_F, start: f64, span: f64, played_x: f32) {
        let t = theme::t();
        let px = g.px();
        let cols = (((r.right - r.left) / px).floor() as usize).max(1);
        let Some(a) = self.audio_view.as_mut() else { return };
        let d = a.scan.data();
        let lanes = d.lanes.max(1);
        let key = (d.generation, start.to_bits(), span.to_bits(), (r.right - r.left) as i32 * 4 + (px * 4.0) as i32, cols);
        let fresh = a.cache.as_ref().is_none_or(|c| c.key != key);
        if fresh {
            let per = span / cols as f64;
            let mut out = vec![Vec::with_capacity(cols); lanes];
            for (lane, v) in out.iter_mut().enumerate() {
                for c in 0..cols {
                    let f0 = start + c as f64 * per;
                    match d.range(lane, f0, f0 + per) {
                        Some(x) => v.push(x),
                        None => break,
                    }
                }
            }
            a.cache = Some(WaveCache { key, lanes: out });
        }
        drop(d);
        let cache = a.cache.as_ref().unwrap();
        let gap = if lanes <= 2 { LANE_GAP } else { 3.0 };
        let lane_h = ((r.bottom - r.top) - gap * (lanes - 1) as f32) / lanes as f32;
        if lane_h < 4.0 {
            return;
        }
        let (env, body, env_played) = (gfx::rgba(t.text, 0.34), gfx::rgba(t.text, 0.58), gfx::rgba(t.accent, 0.72));
        for (lane, cols_v) in cache.lanes.iter().enumerate() {
            let top = r.top + lane as f32 * (lane_h + gap);
            let lr = rect(r.left, top, r.right - r.left, lane_h);
            let mid = top + lane_h / 2.0;
            let half = lane_h / 2.0 * 0.96;
            g.fill(lr, gfx::rgba(t.text, 0.025));
            g.fill(D2D_RECT_F { right: played_x, ..lr }, gfx::rgba(t.text, 0.03));
            // Dashed guides at half and full scale.
            for k in [-1.0f32, -0.5, 0.5, 1.0] {
                let y = g.snap(mid - k * half);
                let mut x = r.left;
                while x < r.right {
                    g.fill(rect(x, y, 5.0f32.min(r.right - x), px), gfx::rgba(t.text, 0.08));
                    x += 9.0;
                }
            }
            g.fill(rect(r.left, g.snap(mid), r.right - r.left, px), rgb(t.divider));
            if cols_v.is_empty() {
                continue;
            }
            let n = cols_v.len();
            let xs = |c: usize| r.left + (c as f32 + 0.5) * px;
            let mut envelope = Vec::with_capacity(n * 2 + 2);
            let mut rms = Vec::with_capacity(n * 2 + 2);
            // The envelope's top edge left to right, then its bottom edge back; at least a pixel thick, so quiet
            // passages still show.
            let edges = |&(lo, hi, _): &(f32, f32, f32)| {
                let (y_hi, y_lo) = (mid - hi.clamp(-1.0, 1.0) * half, mid - lo.clamp(-1.0, 1.0) * half);
                let m = (y_hi + y_lo) / 2.0;
                (y_hi.min(m - px / 2.0), y_lo.max(m + px / 2.0))
            };
            for (c, v) in cols_v.iter().enumerate() {
                envelope.push((xs(c), edges(v).0));
            }
            for (c, v) in cols_v.iter().enumerate().rev() {
                envelope.push((xs(c), edges(v).1));
            }
            for (c, &(_, hi, v)) in cols_v.iter().enumerate() {
                rms.push((xs(c), mid - v.min(hi.max(0.0)) * half));
            }
            for (c, &(lo, _, v)) in cols_v.iter().enumerate().rev() {
                rms.push((xs(c), mid + v.min((-lo).max(0.0)) * half));
            }
            let split = played_x.clamp(r.left, r.right);
            g.push_clip(D2D_RECT_F { left: split, ..lr });
            g.fill_polygon(&envelope, env);
            g.fill_polygon(&rms, body);
            g.pop_clip();
            if split > r.left {
                g.push_clip(D2D_RECT_F { right: split, ..lr });
                g.fill_polygon(&envelope, env_played);
                g.fill_polygon(&rms, rgb(t.accent));
                g.pop_clip();
            }
        }
    }

    fn draw_spectrogram(&mut self, g: &Gfx, r: D2D_RECT_F, start: f64, span: f64, rate: f64) {
        let t = theme::t();
        g.fill(r, gfx::rgba(0x000000, if t.dark { 0.35 } else { 0.85 }));
        let Some(a) = self.audio_view.as_ref() else { return };
        let (hop, cols) = {
            let d = a.scan.data();
            (d.hop as f64, d.columns())
        };
        if let Some((bmp, w, done, _)) = &a.spec {
            if hop > 0.0 && *done > 0 {
                // Column c is centred on frame c * hop + FFT / 2.
                let c0 = (start - FFT as f64 / 2.0) / hop;
                let c1 = (start + span - FFT as f64 / 2.0) / hop;
                let have = (*done).min(*w).min(cols) as f64;
                let (s0, s1) = (c0.max(0.0), c1.min(have));
                if s1 > s0 {
                    let lw = (r.right - r.left) as f64;
                    let dl = r.left as f64 + (s0 - c0) / (c1 - c0) * lw;
                    let dr = r.left as f64 + (s1 - c0) / (c1 - c0) * lw;
                    let dest = D2D_RECT_F { left: dl as f32, right: dr as f32, ..r };
                    let src = D2D_RECT_F { left: s0 as f32, top: 0.0, right: s1 as f32, bottom: SPEC_ROWS as f32 };
                    let b: &ID2D1Bitmap = bmp;
                    unsafe {
                        g.dev.dc.DrawBitmap(b, Some(&dest), 1.0, D2D1_INTERPOLATION_MODE_LINEAR, Some(&src), None);
                    }
                }
            }
        }
        // A few frequencies up the left.
        for (hz, label) in [(100.0, "100"), (1000.0, "1k"), (10000.0, "10k")] {
            if hz >= rate / 2.0 {
                continue;
            }
            let y = r.top + (row_of(hz, rate) / SPEC_ROWS as f64) as f32 * (r.bottom - r.top);
            g.fill(rect(r.left, g.snap(y), 6.0, g.px()), gfx::rgba(0xFFFFFF, 0.5));
            g.text(&wide(label), &g.fonts.caption, rect(r.left + 9.0, y - 9.0, 40.0, 18.0), gfx::rgba(0xFFFFFF, 0.7), Align::Left);
        }
    }

    // --- Input ---------------------------------------------------------------------------------------

    fn audio_frame_at(&self, x: f32) -> Option<f64> {
        let l = self.audio_layout()?;
        let (start, span) = self.audio_span();
        let f = ((x - l.lanes.left) / (l.lanes.right - l.lanes.left).max(1.0)).clamp(0.0, 1.0) as f64;
        Some(start + f * span)
    }

    /// The lanes or ruler pressed: a click seeks, a drag marks a region.
    pub(super) fn audio_press(&mut self, x: f32) {
        let Some(frame) = self.audio_frame_at(x) else { return };
        if let Some(a) = self.audio_view.as_mut() {
            a.drag = Some(Drag::Press { x, frame });
        }
        self.audio_seek_to(frame);
        self.invalidate();
    }

    /// Seeks to `frame`, dropping a loop region it's outside.
    fn audio_seek_to(&mut self, frame: f64) {
        let Some(p) = self.player().and_then(Engine::audio) else { return };
        if p.region().is_some_and(|(a, b)| (frame as i64) < a || frame as i64 >= b) {
            p.set_region(None);
        }
        p.seek_frame(frame as i64);
    }

    pub(super) fn audio_scroll_press(&mut self, x: f32) {
        let Some(l) = self.audio_layout() else { return };
        let (start, span) = self.audio_span();
        let total = self.audio_total() as f64;
        let f = ((x - l.scroll.left) / (l.scroll.right - l.scroll.left).max(1.0)).clamp(0.0, 1.0) as f64 * total;
        // On the thumb it's held where it was taken; elsewhere the thumb comes to the pointer first.
        let grab = if f >= start && f <= start + span { f - start } else { span / 2.0 };
        if let Some(a) = self.audio_view.as_mut() {
            a.drag = Some(Drag::Scroll { grab });
        }
        self.audio_pointer(x);
    }

    /// The pointer moved with the button held.
    pub(super) fn audio_pointer(&mut self, x: f32) {
        let Some(drag) = self.audio_view.as_ref().and_then(|a| a.drag.as_ref()) else { return };
        match *drag {
            Drag::Press { x: x0, frame } => {
                // Scrubbing: the playhead follows the pointer.
                let to = self.audio_frame_at(x).unwrap_or(frame);
                if to != frame {
                    if let Some(a) = self.audio_view.as_mut() {
                        a.drag = Some(Drag::Press { x: x0, frame: to });
                    }
                    self.audio_seek_to(to);
                }
            }
            Drag::Select { from, .. } => {
                let to = self.audio_frame_at(x).unwrap_or(from);
                if let Some(a) = self.audio_view.as_mut() {
                    a.drag = Some(Drag::Select { from, to });
                }
            }
            Drag::Scroll { grab } => {
                let Some(l) = self.audio_layout() else { return };
                let (_, span) = self.audio_span();
                let total = self.audio_total() as f64;
                let f = ((x - l.scroll.left) / (l.scroll.right - l.scroll.left).max(1.0)).clamp(0.0, 1.0) as f64 * total;
                if let Some(a) = self.audio_view.as_mut() {
                    a.span = Some(((f - grab).clamp(0.0, (total - span).max(0.0)), span));
                }
            }
        }
        self.invalidate();
    }

    /// Let go: a click or scrub has already sought; a marked region (off for now) loops.
    pub(super) fn audio_release(&mut self) {
        let Some(drag) = self.audio_view.as_mut().and_then(|a| a.drag.take()) else { return };
        let rate = self.audio_rate();
        let Some(p) = self.player().and_then(Engine::audio) else { return };
        match drag {
            // Already where the scrub left it.
            Drag::Press { .. } => {}
            Drag::Select { from, to } => {
                let (a, b) = (from.min(to), from.max(to));
                if (b - a) / rate >= MIN_REGION_SECS {
                    p.set_region(Some((a as i64, b as i64)));
                    if !p.playing() {
                        p.seek_frame(a as i64);
                    }
                }
            }
            Drag::Scroll { .. } => {}
        }
        self.invalidate();
    }

    /// A double-click on the lanes: back to the whole file when zoomed in, else the region goes.
    pub(super) fn audio_double_click(&mut self) {
        let zoomed = self.audio_view.as_ref().is_some_and(|a| a.span.is_some());
        if zoomed {
            self.audio_zoom_fit();
        } else {
            self.audio_clear_region();
        }
    }

    pub(super) fn audio_zoom_fit(&mut self) {
        if let Some(a) = self.audio_view.as_mut() {
            a.span = None;
        }
        self.invalidate();
    }

    pub(super) fn audio_clear_region(&mut self) -> bool {
        let had = self.player_ref().and_then(Engine::audio_ref).is_some_and(|p| p.region().is_some());
        if had {
            if let Some(p) = self.player().and_then(Engine::audio) {
                p.set_region(None);
            }
            self.show_video_toast("Loop cleared");
            self.invalidate();
        }
        had
    }

    /// Zooms by `factor` (under 1: in) keeping frame `at` under the same x.
    fn audio_zoom(&mut self, factor: f64, at: f64) {
        let Some(l) = self.audio_layout() else { return };
        let total = self.audio_total().max(1) as f64;
        let (start, span) = self.audio_span();
        let min = (l.lanes.right - l.lanes.left) as f64 * MIN_FRAMES_PER_PX;
        let new = (span * factor).clamp(min.min(total), total);
        let k = (at - start) / span.max(1e-9);
        let s = (at - k * new).clamp(0.0, (total - new).max(0.0));
        if let Some(a) = self.audio_view.as_mut() {
            a.span = if new >= total * 0.999 { None } else { Some((s, new)) };
        }
        self.invalidate();
    }

    /// The wheel over the lanes or the scrollbar: Ctrl zooms about the pointer, Shift pans, else the volume.
    pub(super) fn audio_wheel(&mut self, delta: f32, x: f32, ctrl: bool, shift: bool) {
        let notches = delta as f64 / 120.0;
        if ctrl {
            let at = self.audio_frame_at(x).unwrap_or(0.0);
            self.audio_zoom(0.8f64.powf(notches), at);
        } else if shift {
            self.audio_pan(-notches * 0.15);
        } else {
            self.video_volume_by(delta / 120.0 * 0.05);
        }
    }

    /// Pans by `k` views (negative: back).
    fn audio_pan(&mut self, k: f64) {
        let total = self.audio_total() as f64;
        let (start, span) = self.audio_span();
        if let Some(a) = self.audio_view.as_mut().filter(|a| a.span.is_some()) {
            a.span = Some(((start + k * span).clamp(0.0, (total - span).max(0.0)), span));
        }
        self.invalidate();
    }

    pub(super) fn set_audio_mode(&mut self, i: u8) {
        let spectrum = i == 1;
        if let Some(a) = self.audio_view.as_mut() {
            a.mode = if spectrum { Mode::Spectrum } else { Mode::Wave };
        }
        if self.settings.audio_spectrum != spectrum {
            self.settings.audio_spectrum = spectrum;
            settings::save(&self.settings);
        }
        self.invalidate();
    }

    /// The waveform's keys: Escape drops the region; Ctrl+plus/minus zoom about the playhead, Ctrl+0 shows it all.
    pub(super) fn audio_key(&mut self, vk: VIRTUAL_KEY, ctrl: bool) -> bool {
        if !self.audio_shown() {
            return false;
        }
        match vk {
            VK_ESCAPE if self.player_ref().and_then(Engine::audio_ref).is_some_and(|p| p.region().is_some()) => {
                self.audio_clear_region();
            }
            VK_OEM_PLUS | VK_ADD | VK_OEM_MINUS | VK_SUBTRACT if ctrl => {
                let at = self.player_ref().and_then(Engine::audio_ref).map_or(0.0, |p| p.position_frame() as f64);
                self.audio_zoom(if matches!(vk, VK_OEM_PLUS | VK_ADD) { 0.5 } else { 2.0 }, at);
            }
            VK_0 if ctrl => self.audio_zoom_fit(),
            _ => return false,
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels() {
        assert_eq!(ruler_label(65.0, 5.0), "1:05");
        assert_eq!(ruler_label(65.4, 0.5), "1:05.4");
        assert_eq!(ruler_label(5.25, 0.05), "0:05.25");
        assert_eq!(precise(65.42), "1:05.42");
        assert_eq!(precise(3725.5), "1:02:05.50");
        assert!(row_of(30.0, 48000.0) > 255.0 && row_of(24000.0, 48000.0).abs() < 1e-6);
    }
}
