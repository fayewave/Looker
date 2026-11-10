//! The video on screen. The decode pool's still (its first picture, imaging/video.rs) shows first, like any
//! image; the player (player.rs) opens meanwhile on the window's hardware device and, once its pictures come,
//! each frame's due picture is turned into an RGB texture (yuv.rs) that is drawn where the still was, through
//! the same view (zoom, pan, fit).
//!
//! The controls bar floats at the bottom of the photo like the page bar: play/pause, the time, the seek bar (a
//! picture of where the pointer is above it, from a second decoder: `player::Preview`), volume, speed and loop.
//! It fades once the pointer has been still for a moment while the video plays, and stays while it's paused.
//!
//! Keys (agreed with Faye, native/PLAN.md N6): Space, or a click on the video, plays/pauses (a click flashes a
//! play or pause in the middle); a double-click undoes the click and resets the zoom when zoomed or panned, else
//! goes in or out of fullscreen (Faye, 2026-11-10); K pauses; L plays, then 2x, 4x; J slows back down, or at 1x jumps back
//! 5 s; `,` / `.` step a picture; Shift+Left/Right jump 5 s; M mutes; Up/Down (when the explorer isn't shown) and
//! the wheel over the video change the volume (Ctrl+wheel zooms).

use std::cell::RefCell;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use windows::core::Interface;

use super::*;
use crate::player::{Player, Preview};
use crate::yuv::{self, Converter, Look};
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_FORMAT_R16G16B16A16_FLOAT};

/// The hardware device couldn't be made: videos play from the CPU on WARP rather than wait for it.
pub(super) static HW_FAILED: AtomicBool = AtomicBool::new(false);

/// What a jump (Shift+arrow, J) moves.
const JUMP_US: i64 = 5_000_000;
/// The bar fades this long after the pointer last moved, while the video plays.
const BAR_IDLE: Duration = Duration::from_millis(2500);
/// The speed menu's choices.
pub(super) const SPEEDS: [f64; 7] = [0.25, 0.5, 0.75, 1.0, 1.25, 1.5, 2.0];

const BAR_H: f32 = 44.0;
/// What the bar takes from the bottom of the photo's area, its margin included (toasts go above it).
pub(super) const BAR_ROOM: f32 = BAR_H + 16.0;
const BAR_BTN: f32 = 32.0;
const VOLUME_W: f32 = 76.0;
/// The seek bar's and the volume slider's thickness, and how much each grows while the pointer is on it or it's
/// held (no knob on either; always the hovered thickness, no growing: Faye, 2026-11-10).
const SLIDER_H: f32 = 8.0;
const SLIDER_GROW: f32 = 0.0;
/// Their corners: a little rounded (Faye, 2026-11-10).
const SLIDER_R: f32 = 2.5;
const SPEED_W: f32 = 48.0;
/// The hover preview's box, in DIPs.
const PREVIEW_W: f32 = 160.0;
const PREVIEW_H: f32 = 90.0;
/// Where a video counts as finished (no resume) or barely started.
const RESUME_MARGIN_US: i64 = 10_000_000;
const RESUME_CAP: usize = 200;
/// The play/pause flashed over a clicked video: how long it fades, and its rounded square.
const FLASH: Duration = Duration::from_millis(450);
const FLASH_D: f32 = 84.0;
const FLASH_R: f32 = 18.0;
/// Which the flash shows (Segoe's play and pause code points, though it draws them as shapes).
const PLAY_GLYPH: u16 = 0xE768;
const PAUSE_GLYPH: u16 = 0xE769;

/// The seek bar held down: where it's held, the last seek sent while dragging (and whether it was exact), and
/// whether it was playing.
struct Scrub {
    at: i64,
    sent: Option<i64>,
    exact: bool,
    was_playing: bool,
}

/// Dragging the seek bar closer than this to where the picture is seeks to the exact picture (slowly: almost frame by
/// frame); further, to the keyframe first (at once), then exactly once that's up.
const SCRUB_EXACT_US: i64 = 2_000_000;

pub(super) struct VideoView {
    pub path: PathBuf,
    player: Option<Player>,
    /// On the current device; dropped when it changes.
    conv: Option<Converter>,
    bitmap: Option<ID2D1Bitmap1>,
    /// It couldn't be opened: the still stays.
    failed: bool,
    /// Where to reopen after a device change: the time, and whether it was playing.
    resume: Option<(i64, bool)>,
    /// Whether the loop setting makes this one loop, once the player has opened (its length decides).
    auto_loop: Option<bool>,
    scrub: Option<Scrub>,
    /// The seek bar's previews: made the first time the pointer is over it.
    preview: Option<Preview>,
    /// The newest preview: the time asked for, the picture and its size in DIPs.
    still: Option<(i64, ID2D1Bitmap1, f32, f32)>,
    /// The loop button, once pressed; until then the setting decides.
    looping: Option<bool>,
    /// The picture is SDR Rec. 709 (clamped to SDR white under scRGB like a photo); HDR and wide-gamut ones aren't.
    sdr: bool,
    /// A click's play/pause glyph in the middle, and when it was clicked.
    flash: Option<(u16, Instant)>,
    /// The pointer is hidden (fullscreen, playing, the bar gone).
    cursor_hidden: bool,
    stats: Stats,
}

/// How the UI thread kept up while it played, for the trace.
#[derive(Default)]
struct Stats {
    pictures: u32,
    /// Vsyncs that passed without a frame drawn, while playing.
    missed: u32,
    last: Option<Instant>,
    convert_ms: f64,
    convert_max_ms: f64,
}

/// The refresh rate of the monitor `hwnd` is on, in Hz (read again every couple of seconds, or when the window
/// moves to another monitor).
fn monitor_hz(hwnd: HWND) -> Option<f64> {
    use std::cell::Cell;
    use windows::Win32::Graphics::Gdi::{DEVMODEW, ENUM_CURRENT_SETTINGS, EnumDisplaySettingsW, GetMonitorInfoW, MONITOR_DEFAULTTONEAREST, MONITORINFO, MONITORINFOEXW, MonitorFromWindow};
    thread_local! {
        static HZ: Cell<Option<(isize, f64, Instant)>> = const { Cell::new(None) };
    }
    let mon = unsafe { MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST) };
    if let Some((m, hz, at)) = HZ.get() {
        if m == mon.0 as isize && at.elapsed() < Duration::from_secs(2) {
            return Some(hz);
        }
    }
    let mut mi = MONITORINFOEXW::default();
    mi.monitorInfo.cbSize = std::mem::size_of::<MONITORINFOEXW>() as u32;
    let mut dm = DEVMODEW { dmSize: std::mem::size_of::<DEVMODEW>() as u16, ..Default::default() };
    unsafe {
        if !GetMonitorInfoW(mon, &mut mi as *mut MONITORINFOEXW as *mut MONITORINFO).as_bool() {
            return None;
        }
        if !EnumDisplaySettingsW(windows::core::PCWSTR(mi.szDevice.as_ptr()), ENUM_CURRENT_SETTINGS, &mut dm).as_bool() {
            return None;
        }
    }
    let hz = dm.dmDisplayFrequency as f64;
    if hz < 20.0 {
        return None; // 0 or 1: "the hardware's default"
    }
    HZ.set(Some((mon.0 as isize, hz, Instant::now())));
    Some(hz)
}

/// When the frame being drawn now reaches the screen (the next vblank of the monitor the window is on), the
/// refresh period there, and where the vblank's timing came from. The period is that monitor's; the phase is the
/// swap chain's last vblank, else DWM's when DWM runs at this monitor's rate (DWM's timing is the primary
/// monitor's: Faye's is 360 Hz, the second one 165 Hz).
fn next_vblank(g: &Gfx, hwnd: HWND) -> (Instant, Duration, &'static str) {
    use windows::Win32::Graphics::Dwm::{DWM_TIMING_INFO, DwmGetCompositionTimingInfo};
    use windows::Win32::System::Performance::{QueryPerformanceCounter, QueryPerformanceFrequency};
    let now = Instant::now();
    let (mut qpc, mut freq) = (0i64, 0i64);
    unsafe {
        if QueryPerformanceCounter(&mut qpc).is_err() || QueryPerformanceFrequency(&mut freq).is_err() || freq <= 0 {
            return (now, Duration::from_millis(16), "none");
        }
    }
    let mut info = DWM_TIMING_INFO { cbSize: std::mem::size_of::<DWM_TIMING_INFO>() as u32, ..Default::default() };
    let dwm = unsafe { DwmGetCompositionTimingInfo(HWND::default(), &mut info) }.is_ok() && info.qpcRefreshPeriod > 0;
    let period = match monitor_hz(hwnd) {
        Some(hz) => (freq as f64 / hz) as i64,
        None if dwm => info.qpcRefreshPeriod as i64,
        None => freq / 60,
    }
    .max(1);
    let secs = |ticks: i64| Duration::from_secs_f64(ticks as f64 / freq as f64);
    let recent = |v: i64| v > 0 && v <= qpc && qpc - v < freq;
    let (vblank, source) = if let Some(v) = g.last_vblank_qpc().filter(|&v| recent(v)) {
        (v, "swap chain")
    } else if dwm && ((info.qpcRefreshPeriod as i64 - period).abs() as f64) < period as f64 * 0.02 && recent(info.qpcVBlank as i64) {
        (info.qpcVBlank as i64, "DWM")
    } else {
        return (now, secs(period), "none");
    };
    let next = vblank + ((qpc - vblank) / period + 1) * period;
    (now + secs(next - qpc), secs(period), source)
}

impl VideoView {
    /// Textures die with their device: so does the player's decoder (its surfaces are on it), reopened where it was.
    pub(super) fn device_lost(&mut self) {
        self.conv = None;
        self.bitmap = None;
        self.still = None;
        self.preview = None; // its decoder is on the old device too
        if let Some(p) = self.player.take() {
            self.resume = Some((p.position(), p.playing()));
        }
    }

    /// The time the bar shows: where the seek bar is held, else the picture on screen.
    fn position(&self) -> i64 {
        match (&self.scrub, &self.player) {
            (Some(s), _) => s.at,
            (None, Some(p)) => p.position(),
            _ => 0,
        }
    }
}

/// The bar's margin, height and button size for a photo area `area_w` wide: a small one (browse mode's preview)
/// gets a compact bar on tighter margins.
fn bar_metrics(area_w: f32) -> (f32, f32, f32) {
    if area_w < 360.0 { (8.0, 30.0, 24.0) } else { (16.0, BAR_H, BAR_BTN) }
}

/// The band at the bottom of a photo area `area_w` wide that a video keeps clear for its bar: the bar and a margin
/// above and below it.
pub(super) fn bar_band(area_w: f32) -> f32 {
    let (margin, h, _) = bar_metrics(area_w);
    h + 2.0 * margin
}

/// The bar's parts, laid out from the image area each frame.
struct BarLayout {
    bar: D2D_RECT_F,
    radius: f32,
    play: Option<D2D_RECT_F>,
    /// The two times, left and right of the seek bar.
    times: Option<(D2D_RECT_F, D2D_RECT_F)>,
    /// The seek bar's line, and the taller strip that takes its clicks. Always there.
    track: D2D_RECT_F,
    seek: D2D_RECT_F,
    mute: Option<D2D_RECT_F>,
    volume: Option<D2D_RECT_F>,
    speed: Option<D2D_RECT_F>,
    looped: Option<D2D_RECT_F>,
    /// Fullscreen, at the far right: kept as long as the speaker is.
    full: Option<D2D_RECT_F>,
}

/// The bar's timecode: minutes, seconds and the picture within the second, "01:05:12", or "1:02:03:12" for a video
/// an hour or longer (Faye, 2026-11-10: to see each picture while stepping through). `fps` 0: no pictures part.
pub(super) fn timecode(us: i64, fps: f64, hours: bool) -> String {
    let us = us.max(0);
    let s = (us / 1_000_000) as u64;
    let f = if fps > 0.0 { ((us % 1_000_000) as f64 * fps / 1e6 + 1e-3).floor() as u64 } else { 0 };
    if hours { format!("{}:{:02}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60, f) } else { format!("{:02}:{:02}:{:02}", s / 60, s % 60, f) }
}

/// "1:05", "12:40", "1:02:03" (hours when the video is that long).
pub(super) fn clock_text(us: i64, hours: bool) -> String {
    let s = (us.max(0) / 1_000_000) as u64;
    if hours { format!("{}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60) } else { format!("{}:{:02}", s / 60, s % 60) }
}

/// Thumbnails at least this tall get a video's length on them.
pub(super) const BADGE_MIN_H: f32 = 40.0;

/// A video's length in the bottom-right corner of its thumbnail `r`: white on a dark pill, as file managers do.
/// Fills a slider's rounded track from its left end to `x`: the track's corners, a square end at `x`.
fn fill_track_to(g: &Gfx, line: D2D_RECT_F, x: f32, c: windows::Win32::Graphics::Direct2D::Common::D2D1_COLOR_F) {
    if x <= line.left {
        return;
    }
    g.push_clip(D2D_RECT_F { right: x.min(line.right), ..line });
    g.fill_round(line, SLIDER_R, c);
    g.pop_clip();
}

pub(super) fn draw_duration_badge(g: &Gfx, r: D2D_RECT_F, us: i64) {
    let text = clock_text(us, us >= 3_600_000_000);
    let w = g.measure_tabular(&wide(&text), &g.fonts.caption).ceil() + 8.0;
    let h = 16.0;
    if w + 8.0 > r.right - r.left {
        return;
    }
    let b = rect(g.snap(r.right - 4.0 - w), g.snap(r.bottom - 4.0 - h), w, h);
    g.fill_round(b, 3.0, gfx::rgba(0x000000, 0.72));
    g.text_tabular(&wide(&text), &g.fonts.caption, b, gfx::rgba(0xFFFFFF, 0.95), Align::Center);
}

fn speed_text(s: f64) -> String {
    let t = format!("{s}");
    format!("{t}\u{00D7}")
}

// --- Where long videos were left: `resume.txt` in the data dir, `<µs>\t<path>` a line, newest last ---

thread_local! {
    static RESUME: RefCell<Option<Vec<(PathBuf, i64)>>> = const { RefCell::new(None) };
}

fn resume_file() -> Option<PathBuf> {
    settings::data_dir().map(|d| d.join("resume.txt"))
}

fn with_resume<R>(f: impl FnOnce(&mut Vec<(PathBuf, i64)>) -> R) -> R {
    RESUME.with(|r| {
        let mut r = r.borrow_mut();
        let list = r.get_or_insert_with(|| {
            let text = resume_file().and_then(|p| std::fs::read_to_string(p).ok()).unwrap_or_default();
            text.lines()
                .filter_map(|l| {
                    let (us, path) = l.split_once('\t')?;
                    Some((PathBuf::from(path), us.parse().ok()?))
                })
                .collect()
        });
        f(list)
    })
}

fn save_resume(list: &[(PathBuf, i64)]) {
    let Some(file) = resume_file() else { return };
    let text: String = list.iter().map(|(p, us)| format!("{us}\t{}\n", p.display())).collect();
    let _ = std::fs::write(file, text);
}

fn resume_point(path: &Path) -> Option<i64> {
    with_resume(|l| l.iter().find(|(p, _)| p == path).map(|&(_, us)| us))
}

impl App {
    /// After every navigation: a video on screen gets a player; anything else drops the last one.
    pub(super) fn sync_video(&mut self) {
        let want = self.current_path().filter(|p| format::from_extension(p.extension().and_then(|e| e.to_str())) == format::Format::Video).map(Path::to_path_buf);
        if self.video.as_ref().is_some_and(|v| Some(&v.path) == want.as_ref()) {
            return;
        }
        self.leave_video();
        let Some(path) = want else {
            self.video = None;
            return;
        };
        if self.settings.video_start_muted && !self.settings.video_muted {
            self.settings.video_muted = true;
            settings::save(&self.settings);
        }
        self.video_moved = Instant::now(); // the bar shows a moment as it opens
        self.video = Some(VideoView {
            path,
            player: None,
            conv: None,
            bitmap: None,
            failed: false,
            resume: None,
            auto_loop: None,
            scrub: None,
            preview: None,
            still: None,
            looping: None,
            sdr: true,
            flash: None,
            cursor_hidden: false,
            stats: Stats::default(),
        });
    }

    /// A file was renamed or moved: the open video keeps playing under its new path (the decoder holds a handle,
    /// not a name), and a resume point follows it.
    pub(super) fn video_renamed(&mut self, from: &Path, to: &Path) {
        if let Some(v) = self.video.as_mut().filter(|v| crate::explorer::same_path(&v.path, from)) {
            v.path = to.to_path_buf();
        }
        with_resume(|l| {
            let mut moved = false;
            for (p, _) in l.iter_mut().filter(|(p, _)| crate::explorer::same_path(p, from)) {
                *p = to.to_path_buf();
                moved = true;
            }
            if moved {
                save_resume(l);
            }
        });
    }

    /// The video going away (another file, the window closing): a long one remembers where it was left.
    pub(super) fn leave_video(&mut self) {
        let Some(v) = self.video.take() else { return };
        let s = &v.stats;
        if s.pictures > 0 {
            crate::trace::mark(format!(
                "video: UI drew {} pictures, {} vsyncs missed while playing; picture to texture {:.2} ms average, {:.2} max",
                s.pictures,
                s.missed,
                s.convert_ms / s.pictures as f64,
                s.convert_max_ms
            ));
        }
        let Some(p) = v.player.as_ref().filter(|p| p.info.is_some()) else { return };
        let dur = p.duration();
        if !self.settings.video_resume || dur < settings::RESUME_MIN_SECS as i64 * 1_000_000 {
            return;
        }
        let at = v.position();
        let keep = at > RESUME_MARGIN_US && at < dur - RESUME_MARGIN_US;
        with_resume(|l| {
            let had = l.iter().any(|(q, _)| *q == v.path);
            if !keep && !had {
                return;
            }
            l.retain(|(q, _)| *q != v.path);
            if keep {
                l.push((v.path.clone(), at));
                if l.len() > RESUME_CAP {
                    l.drain(..l.len() - RESUME_CAP);
                }
            }
            save_resume(l);
        });
    }

    /// A video is the current file (its keys and wheel apply, even before its first picture).
    pub(super) fn video_shown(&self) -> bool {
        self.placeholder.is_none() && self.video.as_ref().is_some_and(|v| !v.failed)
    }

    /// The slideshow's dwell is up but this video is still opening or playing: it waits for the end.
    pub(super) fn video_holds_slideshow(&self) -> bool {
        self.video_shown() && self.video.as_ref().and_then(|v| v.player.as_ref()).is_none_or(|p| p.info.is_none() || !p.at_end())
    }

    /// Frames are moving, the player is still coming up, or a preview is on its way: draw again next vsync.
    pub(super) fn video_animating(&self) -> bool {
        self.video.as_ref().is_some_and(|v| {
            !v.failed && (v.flash.is_some() || v.player.as_ref().is_none_or(|p| p.animating()) || v.scrub.as_ref().is_some_and(|s| s.sent != Some(s.at) || !s.exact) || v.preview.as_ref().is_some_and(|p| p.busy()))
        })
    }

    /// Before the frame begins (Direct2D isn't mid-draw): opens the player once the device is right, and turns
    /// the picture due now into the bitmap drawn this frame.
    pub(super) fn prepare_video(&mut self, g: &Gfx) {
        // A file Looker can't show covering the video (the explorer opened it) stops it, as another file would; it
        // starts again when the video's back.
        if self.placeholder.is_some() {
            if self.video.is_some() {
                self.leave_video();
            }
            return;
        }
        self.sync_video();
        let slideshow = self.slideshow.running;
        // A slideshow plays every video (to its end, without looping).
        let (volume, muted, autoplay) = (self.settings.video_volume, self.video_muted(), self.settings.video_autoplay || slideshow);
        let (loop_secs, resume_on) = (self.settings.video_loop_secs, self.settings.video_resume);
        let mut toast = None;
        let Some(v) = self.video.as_mut().filter(|v| !v.failed) else { return };
        if v.player.is_none() {
            // The first ~200 ms after launch draw on WARP: wait for the hardware device to decode on its GPU.
            if g.is_warp() && !HW_FAILED.load(Ordering::Relaxed) {
                return;
            }
            let dev = if g.is_warp() { std::ptr::null_mut() } else { g.d3d().as_raw() };
            let (at, play) = match v.resume.take() {
                Some(r) => r,
                None => match resume_point(&v.path).filter(|_| resume_on) {
                    Some(at) => {
                        toast = Some(format!("Resuming at {}", clock_text(at, at >= 3_600_000_000)));
                        (at, autoplay)
                    }
                    None => (0, autoplay),
                },
            };
            let mut p = Player::open(&v.path, gfx::Sendable(dev), play, volume, muted);
            if at > 0 {
                p.seek(at, true);
            }
            v.player = Some(p);
        }
        let p = v.player.as_mut().unwrap();
        match p.poll_open() {
            None => {}
            Some(Err(_)) => {
                v.failed = true; // the still stays
                return;
            }
            Some(Ok(())) => {
                let dur = p.duration();
                let auto = *v.auto_loop.get_or_insert(loop_secs == u32::MAX || (dur > 0 && dur < loop_secs as i64 * 1_000_000));
                let want = v.looping.unwrap_or(auto) && !slideshow;
                if p.looping != want {
                    p.set_looping(want);
                }
            }
        }
        let ended = slideshow && self.slideshow.video_wait && v.player.as_ref().is_some_and(|p| p.at_end());
        if ended {
            self.video_ended_in_slideshow();
        }
        if let Some(t) = toast {
            self.show_toast(t, false);
        }
        let Some(v) = self.video.as_mut() else { return };
        let Some(p) = v.player.as_mut() else { return };
        if p.info.is_none() {
            return;
        }
        // Scrubbing: the next seek once the last one's picture is up.
        if let Some(s) = &mut v.scrub {
            let refine = s.sent == Some(s.at) && !s.exact; // a keyframe seek landed: now the exact picture
            if (s.sent != Some(s.at) || refine) && (s.sent.is_none() || p.settled()) {
                // Exact while it's close and quick (forward, cached, a CPU decoder); else the keyframe at once and
                // the exact picture once the pointer rests.
                let exact = refine || ((s.at - p.position()).abs() < SCRUB_EXACT_US && p.exact_is_quick(s.at));
                p.seek(s.at, exact);
                s.sent = Some(s.at);
                s.exact = exact;
            }
        }
        if let Some(s) = v.preview.as_ref().and_then(|p| p.take()) {
            if let Ok(b) = g.bitmap(s.width, s.height, &s.bgra) {
                v.still = Some((s.asked, b, s.width as f32, s.height as f32));
            }
        }
        // The picture for the vsync this frame will be shown at: picked by the clock at that moment, not at
        // whenever this frame happened to start (which wanders, and at 165 Hz flipped pictures a vsync early or late).
        let (shown_at, period, timing) = next_vblank(g, self.hwnd);
        if p.playing() {
            let now = Instant::now();
            if let Some(last) = v.stats.last {
                let gap = now.duration_since(last).as_secs_f64() / period.as_secs_f64();
                if gap > 1.5 {
                    v.stats.missed += (gap - 1.0).round() as u32;
                }
            }
            v.stats.last = Some(now);
        } else {
            v.stats.last = None;
        }
        let Some(due) = p.due(shown_at) else { return };
        let Some(info) = p.info.as_ref() else { return };
        // The display size, scaled by how the picture compares with the size the headers gave (a 1080p stream can
        // be stored 1088 high): square pixels and upright, as the still was.
        let (pw, ph) = (due.picture.width.max(1) as f64, due.picture.height.max(1) as f64);
        let (cw, ch) = (info.coded_width.max(1) as f64, info.coded_height.max(1) as f64);
        let (dw, dh) = if matches!(info.rotation, 90 | 270) {
            (info.width as f64 * ph / ch, info.height as f64 * pw / cw)
        } else {
            (info.width as f64 * pw / cw, info.height as f64 * ph / ch)
        };
        let cap = g.max_bitmap_size().max(1);
        let (w, h) = ((dw.round().max(1.0) as u32).min(cap), (dh.round().max(1.0) as u32).min(cap));
        // The picture's own colour tags, else the container's (2 = unspecified).
        let tag = |own: i32, stream: i32| if own != 2 && own != 0 { own } else { stream };
        let (transfer, primaries) = (tag(due.picture.transfer, info.transfer), tag(due.picture.primaries, info.primaries));
        let space = g.space();
        let white = space.is_scrgb().then(|| space.white_scale());
        // Only HDR asks for the panel's peak (its first reading costs ~12 ms).
        let headroom = if yuv::is_hdr(transfer) { space.headroom() } else { 1.0 };
        let look = Look { white, headroom, transfer, primaries };
        v.sdr = !yuv::is_wide(transfer, primaries);
        if v.conv.is_none() {
            match Converter::new(g.d3d()) {
                Ok(c) => v.conv = Some(c),
                Err(e) => {
                    crate::trace::mark(format!("video: {e}"));
                    v.failed = true;
                    return;
                }
            }
        }
        if v.bitmap.is_none() {
            let f = &due.picture;
            crate::trace::mark(format!(
                "video: first picture {}x{} layout {} planes {} {}-bit (shift {}) chroma {}/{} matrix {} full {}{}; {look:?}",
                f.width,
                f.height,
                f.layout,
                f.nplanes,
                f.bit_depth,
                f.shift,
                f.chroma_w_shift,
                f.chroma_h_shift,
                f.matrix,
                f.full_range,
                if f.uploaded.is_some() { ", uploaded by the decode thread" } else { "" }
            ));
            crate::trace::mark(format!("video: vsyncs every {:.2} ms, timed by the {timing}", period.as_secs_f64() * 1000.0));
        }
        let conv = v.conv.as_mut().unwrap();
        let t = Instant::now();
        let converted = conv.convert(&due.picture, due.picture.uploaded.as_ref(), info.rotation, w, h, &look);
        let ms = t.elapsed().as_secs_f64() * 1000.0;
        v.stats.pictures += 1;
        v.stats.convert_ms += ms;
        v.stats.convert_max_ms = v.stats.convert_max_ms.max(ms);
        match converted {
            Ok(remade) => {
                if remade || v.bitmap.is_none() {
                    let format = if white.is_some() { DXGI_FORMAT_R16G16B16A16_FLOAT } else { DXGI_FORMAT_B8G8R8A8_UNORM };
                    v.bitmap = conv.out.as_ref().and_then(|o| g.texture_bitmap(&o.texture, format).ok());
                }
            }
            Err(e) => crate::trace::mark(format!("video: picture: {e}")),
        }
    }

    /// The current picture, to draw instead of the still, and whether it's SDR (see `VideoView::sdr`).
    pub(super) fn video_bitmap(&self) -> Option<(ID2D1Bitmap1, bool)> {
        self.video.as_ref().filter(|_| self.placeholder.is_none()).and_then(|v| Some((v.bitmap.clone()?, v.sdr)))
    }

    fn player(&mut self) -> Option<&mut Player> {
        self.video.as_mut().filter(|v| !v.failed).and_then(|v| v.player.as_mut()).filter(|p| p.info.is_some())
    }

    /// The status row's facts about the video playing (frame rate, codec...), once it has opened.
    pub(super) fn video_status(&self) -> Vec<(u8, String)> {
        self.player_ref().and_then(|p| p.info.as_ref()).map(crate::imaging::video::status_parts).unwrap_or_default()
    }

    fn player_ref(&self) -> Option<&Player> {
        self.video.as_ref().filter(|v| !v.failed).and_then(|v| v.player.as_ref()).filter(|p| p.info.is_some())
    }

    pub(super) fn video_volume_by(&mut self, delta: f32) {
        self.set_video_volume(self.settings.video_volume + delta, true);
    }

    /// Whether the sound is off. Browse mode doesn't change it (Faye, 2026-11-10).
    pub(super) fn video_muted(&self) -> bool {
        // LOOKER_MUTE=1: test runs stay silent whatever the volume setting.
        static FORCED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        self.settings.video_muted || *FORCED.get_or_init(|| std::env::var_os("LOOKER_MUTE").is_some())
    }

    fn set_video_volume(&mut self, v: f32, toast: bool) {
        self.settings.video_volume = (v.clamp(0.0, 1.0) * 100.0).round() / 100.0;
        self.settings.video_muted = false;
        self.apply_volume();
        if toast {
            self.show_toast(format!("Volume {:.0}%", self.settings.video_volume * 100.0), true);
        }
    }

    fn apply_volume(&mut self) {
        let (v, m) = (self.settings.video_volume, self.video_muted());
        if let Some(p) = self.player() {
            p.set_volume(v, m);
        }
        settings::save(&self.settings);
    }

    /// M, the speaker button.
    fn toggle_mute(&mut self) {
        self.settings.video_muted = !self.settings.video_muted;
        self.apply_volume();
    }

    /// Plays the video on screen if it's paused.
    pub(super) fn video_play(&mut self) {
        if let Some(p) = self.player().filter(|p| !p.playing()) {
            p.set_playing(true);
            self.invalidate();
        }
    }

    /// Space, a click on the video, the play button.
    pub(super) fn video_toggle_play(&mut self) {
        if let Some(p) = self.player() {
            let play = !p.playing();
            p.set_playing(play);
            self.video_moved = Instant::now();
            self.invalidate();
        }
    }

    /// A click on the video, or Space: plays or pauses, and flashes which in the middle.
    pub(super) fn video_click(&mut self) {
        self.video_toggle_play();
        let playing = self.player_ref().map(|p| p.playing());
        if let (Some(v), Some(playing)) = (self.video.as_mut(), playing) {
            v.flash = Some((if playing { PLAY_GLYPH } else { PAUSE_GLYPH }, Instant::now()));
        }
    }

    /// A double-click on the video: its first click's play/pause undone (and its flash dropped), then back to fit
    /// when zoomed or panned, else into or out of fullscreen.
    pub(super) fn video_double_click(&mut self) {
        self.video_toggle_play();
        if let Some(v) = self.video.as_mut() {
            v.flash = None;
        }
        if self.view.has_content() && !self.view.at_fit_position() {
            self.view.fit();
            self.after_zoom();
        } else {
            self.toggle_fullscreen();
        }
        self.invalidate();
    }

    /// The click's play/pause: a dark rounded square with a solid play (rounded, as Segoe's) or pause, fading out.
    pub(super) fn draw_video_flash(&mut self, g: &Gfx) {
        let area = self.image_area();
        // The middle of where the video fits (above the bar's band).
        let (_, vh) = self.view_size();
        let Some(v) = self.video.as_mut() else { return };
        let Some((glyph, at)) = v.flash else { return };
        let t = at.elapsed().as_secs_f32() / FLASH.as_secs_f32();
        if t >= 1.0 {
            v.flash = None;
            return;
        }
        let (cx, cy) = (g.snap((area.left + area.right) / 2.0), g.snap(area.top + vh as f32 / 2.0));
        let disc = rect(cx - FLASH_D / 2.0, cy - FLASH_D / 2.0, FLASH_D, FLASH_D);
        // Held, then gone: most of the fade comes at the end. Only the opacity changes (it used to grow too, and
        // the disc and glyph re-snapped to the pixels every frame: it shook).
        g.push_fade(1.0 - t * t, 1.0, cx, cy);
        g.fill_round(disc, FLASH_R, gfx::rgba(0x000000, 0.72));
        let ink = gfx::rgba(0xFFFFFF, 1.0);
        if glyph == PLAY_GLYPH {
            // Its centroid on the centre (a third of its width left of it, two thirds right), corners rounded.
            let (w, h) = (32.0, 36.0);
            g.fill_rounded_polygon(&[(cx - w / 3.0, cy - h / 2.0), (cx + w * 2.0 / 3.0, cy), (cx - w / 3.0, cy + h / 2.0)], 5.0, ink);
        } else {
            let (w, h, gap) = (10.0, 34.0, 8.0);
            g.fill_round(rect(cx - gap / 2.0 - w, cy - h / 2.0, w, h), 3.0, ink);
            g.fill_round(rect(cx + gap / 2.0, cy - h / 2.0, w, h), 3.0, ink);
        }
        g.pop_fade();
    }

    pub(super) fn set_video_speed(&mut self, s: f64) {
        if let Some(p) = self.player() {
            p.set_speed(s);
            if !p.playing() {
                p.set_playing(true);
            }
        }
        self.invalidate();
    }

    fn toggle_video_loop(&mut self) {
        let Some(v) = self.video.as_mut() else { return };
        let Some(p) = v.player.as_mut() else { return };
        let on = !p.looping;
        p.set_looping(on);
        v.looping = Some(on);
        self.show_toast(if on { "Loop on" } else { "Loop off" }, true);
    }

    /// A key for the video; false when it isn't one (or no video is up).
    pub(super) fn video_key(&mut self, vk: VIRTUAL_KEY, shift: bool, ctrl: bool) -> bool {
        if ctrl || !self.video_shown() {
            return false;
        }
        let explorer = self.explorer_shown();
        match vk {
            VK_M => {
                self.toggle_mute();
                self.show_toast(if self.video_muted() { "Muted" } else { "Sound on" }, true);
            }
            VK_UP if !explorer => self.video_volume_by(0.05),
            VK_DOWN if !explorer => self.video_volume_by(-0.05),
            VK_SPACE if self.player().is_some() => self.video_click(),
            _ => {
                let Some(p) = self.player() else {
                    // Not open yet: the keys that would act on it are still taken, not passed to the photo keys.
                    return matches!(vk, VK_SPACE | VK_K | VK_L | VK_J | VK_OEM_COMMA | VK_OEM_PERIOD) || (shift && matches!(vk, VK_LEFT | VK_RIGHT));
                };
                let mut toast = None;
                match vk {
                    VK_K => {
                        p.set_playing(false);
                        p.set_speed(1.0);
                    }
                    VK_L => {
                        if !p.playing() {
                            p.set_speed(1.0);
                            p.set_playing(true);
                        } else {
                            let s = if p.speed() < 1.0 { 1.0 } else { (p.speed() * 2.0).min(4.0) };
                            p.set_speed(s);
                            toast = Some(speed_text(s));
                        }
                    }
                    VK_J => {
                        if p.playing() && p.speed() > 1.0 {
                            let s = (p.speed() / 2.0).max(1.0);
                            p.set_speed(s);
                            toast = Some(speed_text(s));
                        } else {
                            let t = p.position() - JUMP_US;
                            p.seek(t, true);
                        }
                    }
                    // Said as the volume is (Faye, 2026-11-10).
                    VK_OEM_PERIOD => {
                        p.step_forward();
                        toast = Some("Next frame".to_string());
                    }
                    VK_OEM_COMMA => {
                        p.step_back();
                        toast = Some("Previous frame".to_string());
                    }
                    VK_LEFT | VK_RIGHT if shift => {
                        let t = p.position() + if vk == VK_RIGHT { JUMP_US } else { -JUMP_US };
                        p.seek(t, true);
                    }
                    _ => return false,
                }
                if let Some(t) = toast {
                    self.show_toast(t, true);
                }
            }
        }
        self.invalidate();
        true
    }

    // --- The controls bar ---------------------------------------------------------------------------

    /// Whether the bar should be up: always while paused; while playing, for a moment after the pointer moves,
    /// and while it's over the bar or using it.
    fn bar_wanted(&self) -> bool {
        let Some(p) = self.player_ref() else { return false };
        let using = matches!(self.pressed, Some(h) if is_bar_hit(h)) || matches!(self.menu, Some((menus::MenuKind::VideoSpeed, _)));
        let over = matches!(self.hover, Some(h) if is_bar_hit(h));
        // Out of fullscreen it stays (Faye, 2026-11-10): only fullscreen is the video alone.
        self.fullscreen.is_none() || !p.playing() || using || over || (self.tracking && self.video_moved.elapsed() < BAR_IDLE)
    }

    /// While a video plays fullscreen with its bar away, the pointer hides too.
    pub(super) fn video_hides_cursor(&self) -> bool {
        self.fullscreen.is_some() && self.video_shown() && !self.bar_wanted() && self.player_ref().is_some_and(|p| p.playing())
    }

    /// The pointer moved to (x, y): the bar comes back, and a drag on the seek bar or volume follows it.
    pub(super) fn video_pointer(&mut self, x: f32, y: f32) {
        if (x, y) != self.mouse {
            // Over the bar (or using it) every move redraws: the hover fill, the time and the preview follow the
            // pointer at the display's rate. They used to wait for something else to draw a frame while paused.
            let on_bar = self.hit(x, y).is_some_and(is_bar_hit) || self.pressed.is_some_and(is_bar_hit);
            if !self.bar_wanted() || on_bar {
                self.invalidate();
            }
            self.video_moved = Instant::now();
        }
        match self.pressed {
            Some(Hit::VideoSeek) => self.scrub_to(x),
            Some(Hit::VideoVolume) => self.volume_at(x),
            _ => {}
        }
    }

    fn bar_layout(&self, g: &Gfx) -> Option<BarLayout> {
        let a = self.image_area();
        let aw = a.right - a.left;
        let (margin, h, btn) = bar_metrics(aw);
        let pad = (h - btn) / 2.0;
        // As wide as the photo's area (Faye, 2026-11-10: the seek bar the full width of the preview).
        let w = aw - 2.0 * margin;
        if w < 40.0 || a.bottom - a.top < h + 2.0 * margin {
            return None;
        }
        let x = g.snap((a.left + a.right - w) / 2.0);
        let bar = rect(x, g.snap(a.bottom - margin - h), w, h);
        let y = bar.top + pad;
        let long = self.player_ref().is_some_and(|p| p.duration() >= 3_600_000_000);
        let tw = g.measure_tabular(&wide(if long { "0:00:00:00" } else { "00:00:00" }), &g.fonts.caption).ceil() + 2.0;
        // As it narrows the loop button goes first, then the volume slider, the speed (each while the seek bar
        // would stay 160 wide), then the times, the speaker with fullscreen, and play (while it would stay 48): the
        // seek bar stays.
        let essentials = |play: bool, times: bool, mute: bool| {
            2.0 * pad + 12.0 + if play { btn + 6.0 } else { 0.0 } + if times { 2.0 * tw + 12.0 } else { 0.0 } + if mute { 2.0 * btn + 12.0 } else { 0.0 }
        };
        let (with_play, with_times, with_mute) =
            [(true, true, true), (true, false, true), (true, false, false), (false, false, false)].into_iter().find(|&(p, t, m)| essentials(p, t, m) + 48.0 <= w).unwrap_or((false, false, false));
        let base = essentials(with_play, with_times, with_mute) + 160.0;
        let all = with_play && with_times && with_mute;
        let with_speed = all && base + SPEED_W + 4.0 <= w;
        let with_volume = with_speed && base + SPEED_W + 4.0 + VOLUME_W <= w;
        let with_loop = with_volume && base + SPEED_W + 4.0 + VOLUME_W + btn + 4.0 <= w;
        let play = with_play.then(|| rect(bar.left + pad, y, btn, btn));
        let mut right = bar.right - pad;
        let full = with_mute.then(|| {
            right -= btn;
            let r = rect(right, y, btn, btn);
            right -= 4.0;
            r
        });
        let looped = with_loop.then(|| {
            right -= btn;
            let r = rect(right, y, btn, btn);
            right -= 4.0;
            r
        });
        let speed = with_speed.then(|| {
            right -= SPEED_W;
            let r = rect(right, y, SPEED_W, btn);
            right -= 4.0;
            r
        });
        let volume = with_volume.then(|| {
            right -= VOLUME_W;
            rect(right, y, VOLUME_W, btn)
        });
        let mute = with_mute.then(|| {
            right -= btn;
            let r = rect(right, y, btn, btn);
            right -= 8.0;
            r
        });
        let mut left = play.map_or(bar.left + pad, |p| p.right + 6.0);
        let times = with_times.then(|| {
            let elapsed = rect(left, y, tw, btn);
            right -= tw;
            let total = rect(right, y, tw, btn);
            left += tw;
            (elapsed, total)
        });
        let (tl, tr) = (left + 6.0 + if times.is_some() { 6.0 } else { 0.0 }, right - 6.0 - if times.is_some() { 6.0 } else { 0.0 });
        let cy = y + btn / 2.0;
        let track = rect(tl, cy - SLIDER_H / 2.0, (tr - tl).max(1.0), SLIDER_H);
        let seek = rect(tl - 6.0, bar.top, (tr - tl).max(1.0) + 12.0, h);
        Some(BarLayout { bar, radius: (h / 2.0).min(10.0), play, times, track, seek, mute, volume, speed, looped, full })
    }

    /// The bar, over the photo (above the page bar's place: a video has no pages). Returns whether it's fading.
    pub(super) fn draw_video_bar(&mut self, g: &Gfx) {
        if !self.video_shown() || self.player_ref().is_none() {
            return;
        }
        let wanted = self.bar_wanted();
        let a = self.fades.get(Hit::VideoBar, wanted);
        // The pointer goes with the bar in fullscreen, as it fades: Windows only asks for the cursor when the mouse
        // moves, so a pointer held still stayed up.
        let hide = a <= 0.0 && self.video_hides_cursor() && self.hover.is_none_or(|h| h == Hit::Viewport || is_bar_hit(h));
        if let Some(v) = self.video.as_mut() {
            if hide != v.cursor_hidden {
                unsafe {
                    // Back with the bar at once too (the next WM_SETCURSOR would be a move later).
                    let arrow = if hide { None } else { LoadCursorW(None, IDC_ARROW).ok() };
                    SetCursor(arrow);
                }
            }
            v.cursor_hidden = hide;
        }
        if a <= 0.0 {
            return;
        }
        let Some(l) = self.bar_layout(g) else { return };
        let (dur, playing, speed, looping, ended) = {
            let p = self.player_ref().unwrap();
            (p.duration(), p.playing(), p.speed(), p.looping, p.at_end())
        };
        let pos = self.video.as_ref().map_or(0, |v| v.position());
        let t = theme::t();
        // The cards' colours (the explorer's, the info card's), not a popup's lighter greys (Faye, 2026-11-10).
        let raised = t;
        // Hits only while it's (mostly) there: a click on a fading bar is meant for the video.
        let live = a > 0.5;
        g.push_fade(a, 1.0, 0.0, 0.0);
        ui::shadow(g, l.bar, l.radius);
        g.fill_round(l.bar, l.radius, rgb(t.sidebar));
        ui::stroke(g, l.bar, l.radius, rgb(raised.divider));
        if live {
            self.hits.add(Hit::VideoBar, l.bar);
        }
        let button = |app: &mut App, id: Hit, r: D2D_RECT_F, kind: ui::Kind| {
            if live {
                app.hits.add(id, r);
            }
            let st = app.state(id, true);
            ui::quiet_frame(g, r, kind, &st)
        };
        let glyph = if ended { 0xE72C } else if playing { 0xE769 } else { 0xE768 };
        if let Some(r) = l.play {
            // The app's boxed buttons and toggles, as the toolbar's (Faye, 2026-11-10).
            let fg = button(self, Hit::VideoPlay, r, ui::Kind::Standard);
            g.text(&[glyph], &g.fonts.body_icons, r, fg, Align::Center);
        }

        let long = dur >= 3_600_000_000;
        let fps = self.player_ref().and_then(|p| p.info.as_ref()).and_then(|i| i.fps()).unwrap_or(0.0);
        if let Some((elapsed, total)) = l.times {
            g.text_tabular(&wide(&timecode(pos, fps, long)), &g.fonts.caption, elapsed, rgb(raised.text), Align::Right);
            g.text_tabular(&wide(&timecode(dur, fps, long)), &g.fonts.caption, total, rgb(raised.label), Align::Left);
        }

        // The seek bar: the line, a grey fill up to the pointer while it's on it (where a click would put the
        // red), what's played in the accent; thicker while the pointer is on it.
        if live {
            self.hits.add(Hit::VideoSeek, l.seek);
        }
        let scrubbing = self.pressed == Some(Hit::VideoSeek);
        let over = self.hover == Some(Hit::VideoSeek) || scrubbing;
        let k = self.fades.get(Hit::VideoSeek, over);
        let grow = SLIDER_GROW / 2.0 * k;
        // Slightly rounded corners, but the fills' ends stay square: where the red stops reads exactly (Faye,
        // 2026-11-10).
        let line = D2D_RECT_F { top: l.track.top - grow, bottom: l.track.bottom + grow, ..l.track };
        g.fill_round(line, SLIDER_R, rgb(raised.track));
        let frac = if dur > 0 { (pos as f64 / dur as f64).clamp(0.0, 1.0) as f32 } else { 0.0 };
        let px = line.left + (line.right - line.left) * frac;
        fill_track_to(g, line, px, rgb(t.accent));
        // Over the red too (Faye: the whole way to the pointer).
        if k > 0.0 && !scrubbing {
            let mx = self.mouse.0.clamp(line.left, line.right);
            fill_track_to(g, line, mx, gfx::rgba(raised.text, 0.28 * k));
        }

        let vol = self.settings.video_volume;
        let muted = self.video_muted();
        // The full speaker whatever the level, crossed out when muted (Faye, 2026-11-10: the slider shows the level).
        let glyph = if muted { 0xE74F } else { 0xE995 };
        if let Some(r) = l.mute {
            let fg = button(self, Hit::VideoMute, r, ui::Kind::Standard);
            // 16 px: the speaker glyphs sit smaller in their box than play's at 14 (Faye: the same size).
            g.text(&[glyph], &g.fonts.icons, r, fg, Align::Center);
        }
        if let Some(r) = l.volume {
            if live {
                self.hits.add(Hit::VideoVolume, r);
            }
            let held = self.pressed == Some(Hit::VideoVolume);
            let k = self.fades.get(Hit::VideoVolume, self.hover == Some(Hit::VideoVolume) || held);
            let cy = (r.top + r.bottom) / 2.0;
            // Rounded as the seek bar.
            let h = SLIDER_H + SLIDER_GROW * k;
            let line = rect(r.left + 6.0, cy - h / 2.0, r.right - r.left - 12.0, h);
            g.fill_round(line, SLIDER_R, rgb(raised.track));
            let shown = if muted { 0.0 } else { vol };
            let px = line.left + (line.right - line.left) * shown;
            if shown > 0.0 {
                fill_track_to(g, line, px, rgb(if muted { raised.label } else { raised.text }));
            }
            // The seek bar's light fill up to the pointer while it's on it.
            if k > 0.0 && !held {
                let mx = self.mouse.0.clamp(line.left, line.right);
                fill_track_to(g, line, mx, gfx::rgba(raised.text, 0.28 * k));
            }
        }
        if let Some(r) = l.speed {
            let open = matches!(self.menu, Some((menus::MenuKind::VideoSpeed, _)));
            let fg = button(self, Hit::VideoSpeed, r, ui::Kind::Toggle(open));
            g.text_tabular(&wide(&speed_text(speed)), &g.fonts.body, r, fg, Align::Center);
        }
        if let Some(r) = l.looped {
            let fg = button(self, Hit::VideoLoop, r, ui::Kind::Toggle(looping));
            g.text(&[0xE8EE], &g.fonts.body_icons, r, fg, Align::Center);
        }
        if let Some(r) = l.full {
            let fg = button(self, Hit::VideoFullscreen, r, ui::Kind::Standard);
            // Segoe's back-to-window in fullscreen, full screen otherwise.
            let glyph = if self.fullscreen.is_some() { 0xE73F } else { 0xE740 };
            g.text(&[glyph], &g.fonts.body_icons, r, fg, Align::Center);
        }
        g.pop_fade();

        // Over the seek bar: the time there, and a picture of it once the preview decoder has one.
        if live && over && dur > 0 {
            let mx = if scrubbing { px } else { self.mouse.0.clamp(l.track.left, l.track.right) };
            let at = if scrubbing { pos } else { (((mx - l.track.left) / (l.track.right - l.track.left).max(1.0)) as f64 * dur as f64) as i64 };
            self.draw_seek_preview(g, &l, mx, at, long);
        }
    }

    fn draw_seek_preview(&mut self, g: &Gfx, l: &BarLayout, x: f32, at: i64, long: bool) {
        let area = self.image_area();
        let Some(v) = self.video.as_mut() else { return };
        if v.preview.is_none() {
            let rotation = v.player.as_ref().and_then(|p| p.info.as_ref()).map_or(0, |i| i.rotation);
            let s = g.dpi / 96.0;
            let dev = if g.is_warp() { std::ptr::null_mut() } else { g.d3d().as_raw() };
            v.preview = Some(Preview::open(&v.path, rotation, (PREVIEW_W * s) as u32, (PREVIEW_H * s) as u32, gfx::Sendable(dev)));
        }
        // A wish a pixel's worth of time apart at most: only the newest is decoded.
        if let Some(p) = &v.preview {
            p.want(at);
        }
        let raised = theme::raised();
        let fps = v.player.as_ref().and_then(|p| p.info.as_ref()).and_then(|i| i.fps()).unwrap_or(0.0);
        let label = timecode(at, fps, long);
        let lw = g.measure_tabular(&wide(&label), &g.fonts.caption).ceil();
        let (iw, ih) = match &v.still {
            Some((_, _, w, h)) => {
                let s = g.dpi / 96.0;
                (w / s, h / s)
            }
            None => (0.0, 0.0),
        };
        let w = (iw + 8.0).max(lw + 20.0);
        let h = if iw > 0.0 { ih + 8.0 + 22.0 } else { 26.0 };
        let (wl, wr) = (area.left + 4.0, area.right - 4.0);
        let left = g.snap((x - w / 2.0).clamp(wl, (wr - w).max(wl)));
        let r = rect(left, g.snap(l.bar.top - 8.0 - h), w, h);
        ui::shadow(g, r, 8.0);
        g.fill_round(r, 8.0, rgb(raised.popup));
        ui::stroke(g, r, 8.0, rgb(raised.divider));
        if let Some((_, b, _, _)) = &v.still {
            let img = rect(r.left + (w - iw) / 2.0, r.top + 4.0, iw, ih);
            g.push_clip(img);
            g.draw_bitmap(b, img, 1.0);
            g.pop_clip();
        }
        g.text_tabular(&wide(&label), &g.fonts.caption, rect(r.left, r.bottom - 24.0, w, 22.0), rgb(raised.text), Align::Center);
    }

    /// The time under `x` on the seek bar.
    fn seek_time_at(&self, x: f32) -> Option<i64> {
        let g = self.gfx.as_ref()?;
        let l = self.bar_layout(g)?;
        let dur = self.player_ref()?.duration();
        let f = ((x - l.track.left) / (l.track.right - l.track.left).max(1.0)).clamp(0.0, 1.0);
        Some((f as f64 * dur as f64) as i64)
    }

    /// The seek bar pressed: playing stops while it's held, and the picture follows.
    pub(super) fn scrub_start(&mut self, x: f32) {
        let Some(at) = self.seek_time_at(x) else { return };
        let Some(v) = self.video.as_mut() else { return };
        let Some(p) = v.player.as_mut() else { return };
        let was_playing = p.playing();
        p.set_playing(false);
        v.scrub = Some(Scrub { at, sent: None, exact: false, was_playing });
        self.invalidate();
    }

    fn scrub_to(&mut self, x: f32) {
        let Some(at) = self.seek_time_at(x) else { return };
        if let Some(s) = self.video.as_mut().and_then(|v| v.scrub.as_mut()) {
            s.at = at;
        }
        self.invalidate();
    }

    /// The seek bar let go: the exact picture there, and playing again if it was.
    pub(super) fn scrub_end(&mut self) {
        let Some(v) = self.video.as_mut() else { return };
        let Some(s) = v.scrub.take() else { return };
        if let Some(p) = v.player.as_mut() {
            p.seek(s.at, true);
            if s.was_playing {
                p.set_playing(true);
            }
        }
        self.invalidate();
    }

    pub(super) fn volume_at(&mut self, x: f32) {
        let Some(g) = &self.gfx else { return };
        let Some(r) = self.bar_layout(g).and_then(|l| l.volume) else { return };
        let v = (x - r.left - 6.0) / (r.right - r.left - 12.0);
        self.set_video_volume(v, false);
        self.invalidate();
    }

    /// A click on one of the bar's buttons.
    pub(super) fn video_bar_click(&mut self, h: Hit) {
        match h {
            Hit::VideoPlay => self.video_toggle_play(),
            Hit::VideoMute => self.toggle_mute(),
            Hit::VideoLoop => self.toggle_video_loop(),
            Hit::VideoFullscreen => self.toggle_fullscreen(),
            Hit::VideoSpeed => self.toggle_speed_menu(),
            _ => {}
        }
        self.invalidate();
    }

    fn toggle_speed_menu(&mut self) {
        if matches!(self.menu, Some((menus::MenuKind::VideoSpeed, _))) {
            self.close_menu();
            return;
        }
        let Some(r) = self.hits.rect_of(Hit::VideoSpeed) else { return };
        let speed = self.player_ref().map_or(1.0, |p| p.speed());
        let entries = SPEEDS
            .iter()
            .enumerate()
            .map(|(i, &s)| menus::check(menus::Action::Choose(i), &if s == 1.0 { "Normal".to_string() } else { speed_text(s) }, s == speed))
            .collect();
        let Some(g) = &self.gfx else { return };
        let (w, h) = self.size_dip();
        self.menu = Some((menus::MenuKind::VideoSpeed, ui::Menu::below(g, entries, r, rect(0.0, 0.0, w, h))));
        self.hide_tooltip();
        self.invalidate();
    }
}

/// The bar's own hits: the pointer over them keeps it up, and the photo's clicks and drags don't apply.
pub(super) fn is_bar_hit(h: Hit) -> bool {
    matches!(h, Hit::VideoBar | Hit::VideoPlay | Hit::VideoSeek | Hit::VideoMute | Hit::VideoVolume | Hit::VideoSpeed | Hit::VideoLoop | Hit::VideoFullscreen)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clock_texts() {
        assert_eq!(clock_text(0, false), "0:00");
        assert_eq!(clock_text(65_400_000, false), "1:05");
        assert_eq!(clock_text(760_000_000, false), "12:40");
        assert_eq!(clock_text(3_723_000_000, true), "1:02:03");
        assert_eq!(timecode(65_400_000, 25.0, false), "01:05:10");
        assert_eq!(timecode(65_983_000, 60.0, false), "01:05:58");
        assert_eq!(timecode(3_723_040_000, 25.0, true), "1:02:03:01");
        assert_eq!(timecode(5_000_000, 0.0, false), "00:05:00");
        assert_eq!(speed_text(0.25), "0.25\u{00D7}");
        assert_eq!(speed_text(2.0), "2\u{00D7}");
    }
}
