//! The video on screen. The decode pool's still (its first picture, imaging/video.rs) shows first, like any
//! image; the player (player.rs) opens meanwhile on the window's hardware device and, once its pictures come,
//! each frame's due picture is turned into an RGB texture (yuv.rs) that is drawn where the still was, through
//! the same view (zoom, pan, fit).
//!
//! The controls bar floats at the bottom of the photo like the page bar: play/pause, the time, the seek bar (a
//! picture of where the pointer is above it, from a second decoder: `player::Preview`), volume, and a Settings menu of speed and loop.
//! It fades once the pointer has been still for a moment while the video plays, and stays while it's paused.
//!
//! Keys (agreed with Faye, native/PLAN.md N6): Space, or a click on the video, plays/pauses (a click flashes a
//! play or pause in the middle); a double-click undoes the click and resets the zoom when zoomed or panned, else
//! goes in or out of fullscreen (Faye, 2026-11-10); K pauses; L plays, then 2x, 4x; J slows back down, then plays
//! backwards (2x, 4x again; L slows that, then plays forwards); `,` / `.` step a picture; Shift+Left/Right jump 5 s; M mutes; Up/Down (when the explorer isn't shown) and
//! the wheel over the video change the volume (Ctrl+wheel zooms).

use std::cell::RefCell;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use windows::core::Interface;

use super::*;
use crate::audio::AudioPlayer;
use crate::imaging::video::{Chapter, Info, Track};
use crate::player::{Due, Player, Preview};
use crate::yuv::{self, Converter, Look};
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_FORMAT_R16G16B16A16_FLOAT};

/// The hardware device couldn't be made: videos play from the CPU on WARP rather than wait for it.
pub(super) static HW_FAILED: AtomicBool = AtomicBool::new(false);

/// What a jump (Shift+arrow; J and L for sound) moves.
const JUMP_US: i64 = 5_000_000;
/// The bar fades this long after the pointer last moved, while the video plays.
const BAR_IDLE: Duration = Duration::from_millis(2500);
/// The speed menu's choices.
pub(super) const SPEEDS: [f64; 7] = [0.25, 0.5, 0.75, 1.0, 1.25, 1.5, 2.0];

const BAR_H: f32 = 44.0;
const BAR_BTN: f32 = 32.0;
const VOLUME_W: f32 = 76.0;
/// The seek bar's and the volume slider's thickness, and how much each grows while the pointer is on it or it's
/// held (no knob on either; always the hovered thickness, no growing: Faye, 2026-11-10).
const SLIDER_H: f32 = 8.0;
const SLIDER_GROW: f32 = 0.0;
/// Their corners: a little rounded (Faye, 2026-11-10).
const SLIDER_R: f32 = 2.5;
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
    /// An audio file (app/audio.rs draws it): its engine plays sound only.
    pub audio: bool,
    pub(super) player: Option<Engine>,
    /// On the current device; dropped when it changes.
    conv: Option<Converter>,
    bitmap: Option<ID2D1Bitmap1>,
    /// It couldn't be opened: the still stays.
    failed: bool,
    /// Where to reopen after a device change: the time, and whether it was playing.
    resume: Option<(i64, bool)>,
    /// The audio stream picked from the context menu (a `Track::stream`), None: the file's best.
    audio_track: Option<i32>,
    /// The video stream picked from the context menu, None: the file's best.
    video_track: Option<i32>,
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
    /// Its subtitles (subtitles.rs).
    pub(super) subs: super::subtitles::SubView,
    /// The controls bar's top and how far it has faded in, as last drawn (subtitles keep clear of it).
    pub(super) bar_top: Option<(f32, f32)>,
    /// A motion photo's video (app/motionphoto.rs): it plays over the still, with no bar, keys, loop or resume point.
    pub(super) motion: bool,
}

impl VideoView {
    /// A view of `path` playing a motion photo's video: held (no loop, it ends) or in video mode (`open`: as any
    /// short video, it loops by the setting).
    pub(super) fn for_motion(path: PathBuf, open: bool) -> VideoView {
        VideoView {
            audio: false,
            path,
            player: None,
            conv: None,
            bitmap: None,
            failed: false,
            resume: Some((0, true)),
            audio_track: None,
            video_track: None,
            auto_loop: if open { None } else { Some(false) },
            scrub: None,
            preview: None,
            still: None,
            looping: if open { None } else { Some(false) },
            sdr: true,
            flash: None,
            cursor_hidden: false,
            stats: Stats::default(),
            subs: Default::default(),
            bar_top: None,
            motion: true,
        }
    }
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
        self.subs.device_lost();
        if let Some(p) = self.player.take() {
            self.resume = Some((p.position(), p.playing()));
        }
    }

    /// The time the bar shows: where the seek bar is held, else the picture on screen.
    pub(super) fn position(&self) -> i64 {
        match (&self.scrub, &self.player) {
            (Some(s), _) => s.at,
            (None, Some(p)) => p.position(),
            _ => 0,
        }
    }
}

/// The bar's margin, height and button size for a photo area `area_w` wide: a small one (browse mode's preview)
/// gets a compact bar on tighter margins. 12: the cards' margin, so the bar's bottom lines up with theirs (Faye,
/// 2026-11-10).
fn bar_metrics(area_w: f32) -> (f32, f32, f32) {
    if area_w < 360.0 { (12.0, 30.0, 24.0) } else { (12.0, BAR_H, BAR_BTN) }
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
    /// Elapsed, the slash, and the total, side by side after the seek bar.
    times: Option<(D2D_RECT_F, D2D_RECT_F, D2D_RECT_F)>,
    /// The seek bar's line, and the taller strip that takes its clicks. Always there.
    track: D2D_RECT_F,
    seek: D2D_RECT_F,
    mute: Option<D2D_RECT_F>,
    volume: Option<D2D_RECT_F>,
    settings: Option<D2D_RECT_F>,
    /// Fullscreen, at the far right: kept as long as the speaker is.
    full: Option<D2D_RECT_F>,
    /// Stop, after play.
    stop: Option<D2D_RECT_F>,
    /// A motion photo's Motion toggle, at the far left (back to photo mode).
    motion: Option<D2D_RECT_F>,
    /// An audio file's bar only (Transient's, at the top): previous and next file, Loop, the title and format
    /// lines on the left, and the time on the right.
    prev: Option<D2D_RECT_F>,
    next: Option<D2D_RECT_F>,
    looping: Option<D2D_RECT_F>,
    title: Option<D2D_RECT_F>,
    clock: Option<D2D_RECT_F>,
}

/// The audio bar's height: the title and format lines, the seek bar, the buttons.
pub(super) const AUDIO_BAR_H: f32 = 12.0 + 40.0 + 6.0 + 18.0 + 4.0 + BAR_BTN + 10.0;

/// The bar's timecode: minutes, seconds and the picture within the second, "01:05:12", or "1:02:03:12" for a video
/// an hour or longer (Faye, 2026-11-10: to see each picture while stepping through). `fps` 0: no pictures part.
pub(super) fn timecode(us: i64, fps: f64, hours: bool) -> String {
    let us = us.max(0);
    let mut s = (us / 1_000_000) as u64;
    // The nearest picture, not the one before: screen recordings (NVIDIA's) stamp pictures 7.5 to 9 ms apart at
    // 120 fps, and rounding down gave two pictures one number and skipped the next (Faye, 2026-11-10: a step
    // that didn't move the timecode, then one that moved it two). Rounding up to a whole second carries.
    let mut f = if fps > 0.0 { ((us % 1_000_000) as f64 * fps / 1e6).round() as u64 } else { 0 };
    if fps > 0.0 && f >= fps.ceil() as u64 {
        s += 1;
        f = 0;
    }
    // Always as many picture digits as the rate needs: "00:00:091" at 120 fps, never "91" then "100" (Faye).
    let d = frame_digits(fps);
    if hours { format!("{}:{:02}:{:02}:{:0d$}", s / 3600, s / 60 % 60, s % 60, f) } else { format!("{:02}:{:02}:{:0d$}", s / 60, s % 60, f) }
}

/// The picture number's digits at `fps`: two, three past 100 fps.
fn frame_digits(fps: f64) -> usize {
    if fps > 0.0 { ((fps.ceil() as u64).saturating_sub(1)).max(1).to_string().len().max(2) } else { 2 }
}

/// "1:05", "12:40", "1:02:03" (hours when the video is that long).
pub(super) fn clock_text(us: i64, hours: bool) -> String {
    let s = (us.max(0) / 1_000_000) as u64;
    if hours { format!("{}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60) } else { format!("{}:{:02}", s / 60, s % 60) }
}

/// Thumbnails at least this tall get a video's length on them.
pub(super) const BADGE_MIN_H: f32 = 40.0;

/// The widest `timecode` can be for a video at `fps`: all zeros, with as many picture digits as the rate needs
/// (three past 100 fps: Faye, 2026-11-10, a 120 fps video's times were cut off).
fn timecode_template(fps: f64, hours: bool) -> String {
    format!("{}:{}", if hours { "0:00:00" } else { "00:00" }, "0".repeat(frame_digits(fps)))
}

/// Fills a slider's rounded track from its left end to `x`: the track's corners, a square end at `x`.
fn fill_track_to(g: &Gfx, line: D2D_RECT_F, x: f32, c: windows::Win32::Graphics::Direct2D::Common::D2D1_COLOR_F) {
    if x <= line.left {
        return;
    }
    g.push_clip(D2D_RECT_F { right: x.min(line.right), ..line });
    g.fill_round(line, SLIDER_R, c);
    g.pop_clip();
}

/// A video's length in the bottom-right corner of its thumbnail `r`: white on a dark pill, as file managers do.

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

/// A play badge in the middle of a video's or GIF's thumbnail `r` (Faye, 2026-11-10): a dark disc with a white
/// triangle, sized to the thumbnail, the triangle nudged right as the click flash's is.
pub(super) fn draw_play_badge(g: &Gfx, r: D2D_RECT_F) {
    // Most of the thumbnail's height (it was tiny: Faye, 2026-11-11).
    let d = ((r.bottom - r.top).min(r.right - r.left) * 0.8).clamp(15.0, 48.0);
    let (cx, cy) = ((r.left + r.right) / 2.0, (r.top + r.bottom) / 2.0);
    let disc = rect(cx - d / 2.0, cy - d / 2.0, d, d);
    g.fill_round(disc, d / 2.0, gfx::rgba(0x000000, 0.6));
    let (w, h) = (d * 0.4, d * 0.46);
    let left = cx - w * 0.42;
    g.fill_rounded_polygon(&[(left, cy - h / 2.0), (left + w, cy), (left, cy + h / 2.0)], (d * 0.06).max(0.5), gfx::rgba(0xFFFFFF, 0.95));
}

fn speed_text(s: f64) -> String {
    let t = format!("{s}");
    format!("{t}\u{00D7}")
}

/// Said as J takes it backwards: "Backwards", "Backwards 2×".
fn reverse_text(s: f64) -> String {
    if s == 1.0 { "Backwards".to_string() } else { format!("Backwards {}", speed_text(s)) }
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
        // A motion photo's video comes and goes with its photo (sync_motion).
        if self.video.as_ref().is_some_and(|v| v.motion) {
            return;
        }
        let kind = |p: &Path| format::of_path(p);
        let want = self.current_path().filter(|p| matches!(kind(p), format::Format::Video | format::Format::Audio)).map(Path::to_path_buf);
        if self.video.as_ref().is_some_and(|v| Some(&v.path) == want.as_ref()) {
            return;
        }
        self.leave_video();
        let Some(path) = want else {
            self.video = None;
            return;
        };
        if self.settings.video_start_muted && !self.settings.video_muted && kind(&path) == format::Format::Video {
            self.settings.video_muted = true;
            settings::save(&self.settings);
        }
        self.video_moved = Instant::now(); // the bar shows a moment as it opens
        let audio = kind(&path) == format::Format::Audio;
        self.video = Some(VideoView {
            audio,
            path,
            player: None,
            conv: None,
            bitmap: None,
            failed: false,
            resume: None,
            audio_track: None,
            video_track: None,
            auto_loop: None,
            scrub: None,
            preview: None,
            still: None,
            looping: None,
            sdr: true,
            flash: None,
            cursor_hidden: false,
            stats: Stats::default(),
            subs: Default::default(),
            bar_top: None,
            motion: false,
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
        if v.motion {
            return;
        }
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
        let Some(p) = v.player.as_ref().filter(|p| p.info().is_some()) else { return };
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
        self.placeholder.is_none() && self.video.as_ref().is_some_and(|v| !v.failed && (!v.motion || !self.motion_held))
    }

    /// The slideshow's dwell is up but this video is still opening or playing: it waits for the end.
    pub(super) fn video_holds_slideshow(&self) -> bool {
        self.video_shown() && self.video.as_ref().and_then(|v| v.player.as_ref()).is_none_or(|p| p.info().is_none() || !p.at_end())
    }

    /// Frames are moving, the player is still coming up, or a preview is on its way: draw again next vsync.
    pub(super) fn video_animating(&self) -> bool {
        self.video.as_ref().is_some_and(|v| {
            !v.failed && (v.flash.is_some() || v.subs.busy || v.player.as_ref().is_none_or(|p| p.animating()) || v.scrub.as_ref().is_some_and(|s| s.sent != Some(s.at) || !s.exact) || v.preview.as_ref().is_some_and(|p| p.busy()))
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
        self.sync_motion();
        self.sync_video();
        let slideshow = self.slideshow.running;
        // A slideshow plays every video (to its end, without looping).
        let (volume, muted, autoplay) = (self.settings.video_volume, self.sound_off(), self.settings.video_autoplay || slideshow);
        let (loop_secs, resume_on) = (self.settings.video_loop_secs, self.settings.video_resume);
        let mut toast = None;
        let Some(v) = self.video.as_mut().filter(|v| !v.failed) else { return };
        if v.player.is_none() {
            // The first ~200 ms after launch draw on WARP: wait for the hardware device to decode on its GPU.
            if !v.audio && g.is_warp() && !HW_FAILED.load(Ordering::Relaxed) {
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
            let mut p = if v.audio {
                Engine::Audio(AudioPlayer::open(&v.path, v.audio_track, play, volume, muted))
            } else {
                Engine::Video(Player::open_track(&v.path, gfx::Sendable(dev), play, volume, muted, v.video_track, v.audio_track, self.settings.video_passthrough))
            };
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
                // Short videos loop like animations; sounds don't (the loop button still loops one).
                let auto = *v.auto_loop.get_or_insert(!v.audio && (loop_secs == u32::MAX || (dur > 0 && dur < loop_secs as i64 * 1_000_000)));
                let want = v.looping.unwrap_or(auto) && !slideshow;
                if p.looping() != want {
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
        if p.info().is_none() {
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
        let Some(info) = p.info() else { return };
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
        // A motion video going back to photo mode: the still, at once.
        let shows = |v: &&VideoView| !v.motion || self.motion_open || self.motion_held;
        self.video.as_ref().filter(|_| self.placeholder.is_none()).filter(shows).and_then(|v| Some((v.bitmap.clone()?, v.sdr)))
    }

    pub(super) fn player(&mut self) -> Option<&mut Engine> {
        let held = self.motion_held;
        self.video.as_mut().filter(|v| !v.failed && (!v.motion || !held)).and_then(|v| v.player.as_mut()).filter(|p| p.info().is_some())
    }

    /// The status row's facts about the video playing (frame rate, codec...), once it has opened.
    pub(super) fn video_status(&self) -> Vec<(u8, String)> {
        match self.player_ref() {
            Some(Engine::Audio(a)) => a.facts.as_ref().map(|f| vec![(1, crate::imaging::audio::format_line(f)), (2, crate::imaging::video::duration_text(a.duration()))]).unwrap_or_default(),
            p => p.and_then(|p| p.info()).map(crate::imaging::video::status_parts).unwrap_or_default(),
        }
    }

    pub(super) fn player_ref(&self) -> Option<&Engine> {
        self.video.as_ref().filter(|v| !v.failed && (!v.motion || !self.motion_held)).and_then(|v| v.player.as_ref()).filter(|p| p.info().is_some())
    }

    pub(super) fn video_volume_by(&mut self, delta: f32) {
        self.set_video_volume(self.settings.video_volume + delta, true);
    }

    /// Whether the sound is off, as the speaker button shows it. Browse mode doesn't change it (Faye, 2026-11-10).
    pub(super) fn video_muted(&self) -> bool {
        self.settings.video_muted
    }

    /// What the players are told: the setting, or always off under LOOKER_MUTE=1, so test runs stay silent
    /// whatever the volume setting while the controls still look as they would (screenshots).
    fn sound_off(&self) -> bool {
        static FORCED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        self.video_muted() || *FORCED.get_or_init(|| std::env::var_os("LOOKER_MUTE").is_some())
    }

    fn set_video_volume(&mut self, v: f32, toast: bool) {
        self.settings.video_volume = (v.clamp(0.0, 1.0) * 100.0).round() / 100.0;
        self.settings.video_muted = false;
        self.apply_volume();
        if toast {
            self.show_video_toast(format!("Volume {:.0}%", self.settings.video_volume * 100.0));
        }
    }

    fn apply_volume(&mut self) {
        let (v, m) = (self.settings.video_volume, self.sound_off());
        if let Some(p) = self.player() {
            p.set_volume(v, m);
        }
        settings::save(&self.settings);
    }

    /// M, the speaker button.
    fn toggle_mute(&mut self) {
        self.settings.video_muted = !self.settings.video_muted;
        self.apply_volume();
        // Said as the volume keys say theirs (Faye, 2026-11-14).
        let text = if self.settings.video_muted { "Muted".to_string() } else { format!("Unmuted, volume {:.0}%", self.settings.video_volume * 100.0) };
        self.show_video_toast(text);
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
        // A slight white edge (Faye, 2026-11-10).
        ui::stroke(g, disc, FLASH_R, gfx::rgba(0xFFFFFF, 0.22));
        let ink = gfx::rgba(0xFFFFFF, 1.0);
        if glyph == PLAY_GLYPH {
            // Optically centred: halfway between its ink box centred (which looked left: Faye, 2026-11-10) and its
            // centroid centred (which looked right). The rounded tip ends short of the point: the curve's middle is
            // half the corner's run back along the axis. A tad larger in the square (was 32 x 36).
            let (w, h, r) = (38.0f32, 43.0f32, 6.0f32);
            let tip_cut = r / 2.0 * (w / (w * w + h * h / 4.0).sqrt());
            let (by_box, by_centroid) = (cx - (w - tip_cut) / 2.0, cx - w / 3.0);
            let left = g.snap((by_box + by_centroid) / 2.0);
            g.fill_rounded_polygon(&[(left, cy - h / 2.0), (left + w, cy), (left, cy + h / 2.0)], r, ink);
        } else {
            let (w, h, gap) = (12.0, 40.0, 9.0);
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
            // Said, as J and L say it (Faye, 2026-11-10).
            self.show_video_toast(speed_text(s));
        }
        self.invalidate();
    }

    pub(super) fn toggle_video_loop(&mut self) {
        let Some(v) = self.video.as_mut() else { return };
        let Some(p) = v.player.as_mut() else { return };
        let on = !p.looping();
        p.set_looping(on);
        v.looping = Some(on);
        self.show_video_toast(if on { "Loop on" } else { "Loop off" });
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
                self.show_video_toast(if self.video_muted() { "Muted" } else { "Sound on" });
            }
            VK_UP if !explorer => self.video_volume_by(0.05),
            VK_DOWN if !explorer => self.video_volume_by(-0.05),
            VK_SPACE if self.player().is_some() => self.video_click(),
            // VLC's: G earlier, H later, V the next subtitle track.
            VK_G if self.player().is_some() => self.subtitle_delay(-1),
            VK_H if self.player().is_some() => self.subtitle_delay(1),
            VK_V if self.player().is_some() => self.cycle_subtitles(),
            // VLC's: Shift+N the next chapter, Shift+P the previous.
            VK_N if shift && self.player_ref().is_some_and(|p| !p.chapters().is_empty()) => self.step_chapter(false),
            VK_P if shift && self.player_ref().is_some_and(|p| !p.chapters().is_empty()) => self.step_chapter(true),
            _ => {
                let Some(p) = self.player() else {
                    // Not open yet: the keys that would act on it are still taken, not passed to the photo keys.
                    return matches!(vk, VK_SPACE | VK_K | VK_L | VK_J | VK_OEM_COMMA | VK_OEM_PERIOD) || (shift && matches!(vk, VK_LEFT | VK_RIGHT));
                };
                let mut toast = None;
                // Sound only: no speeds or pictures to step; J and L jump back and forward.
                if p.is_audio() {
                    match vk {
                        VK_K => p.set_playing(false),
                        VK_L if !p.playing() => p.set_playing(true),
                        VK_L | VK_J => {
                            let t = p.position() + if vk == VK_L { JUMP_US } else { -JUMP_US };
                            p.seek(t, true);
                        }
                        VK_LEFT | VK_RIGHT if shift => {
                            let t = p.position() + if vk == VK_RIGHT { JUMP_US } else { -JUMP_US };
                            p.seek(t, true);
                        }
                        _ => return false,
                    }
                    self.invalidate();
                    return true;
                }
                match vk {
                    VK_K => {
                        p.set_playing(false);
                        p.set_speed(1.0);
                    }
                    // Backwards, L slows down, then plays forwards at 1x.
                    VK_L if p.reverse() => {
                        if p.speed() > 1.0 {
                            let s = p.speed() / 2.0;
                            p.play_reverse(s);
                            toast = Some(reverse_text(s));
                        } else {
                            p.set_speed(1.0);
                            p.set_playing(true);
                        }
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
                    // J plays backwards (again: 2x, 4x); played forwards faster than 1x, it slows down first.
                    VK_J => {
                        if p.reverse() {
                            let s = (p.speed() * 2.0).min(4.0);
                            p.play_reverse(s);
                            toast = Some(reverse_text(s));
                        } else if p.playing() && p.speed() > 1.0 {
                            let s = (p.speed() / 2.0).max(1.0);
                            p.set_speed(s);
                            toast = Some(speed_text(s));
                        } else {
                            p.play_reverse(1.0);
                            toast = Some(reverse_text(1.0));
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
                    self.show_video_toast(t);
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
        if self.audio_shown() {
            return true;
        }
        let using = matches!(self.pressed, Some(h) if is_bar_hit(h)) || matches!(self.menu, Some((menus::MenuKind::VideoSettings | menus::MenuKind::VideoTime, _)));
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

    /// Where the player's bar is (or would be, while it's faded out): its readouts sit above it.
    pub(super) fn seek_bar_rect(&self, g: &Gfx) -> Option<D2D_RECT_F> {
        self.bar_layout(g).map(|l| l.bar)
    }

    /// Where the video bar goes, and its corner radius (a motion photo's pill grows into it).
    pub(super) fn video_bar_rect(&self, g: &Gfx) -> Option<(D2D_RECT_F, f32)> {
        self.bar_layout(g).map(|l| (l.bar, l.radius))
    }

    fn bar_layout(&self, g: &Gfx) -> Option<BarLayout> {
        if self.audio_shown() {
            return self.audio_bar_layout(g);
        }
        // Its own margin off the status row, not the photo's pad as well (its band spaces the video off it).
        let mut a = self.image_area();
        a.bottom += self.preview_pad();
        let aw = a.right - a.left;
        let (margin, h, btn) = bar_metrics(aw);
        let pad = (h - btn) / 2.0;
        // As wide as the photo's room: beside a card, out to its edge (already the cards' margin off the card,
        // as the video is: Faye, 2026-11-10); against the window's edge, its margin off that.
        let (ml, mr) = ((margin - self.left_inset()).max(0.0), (margin - self.right_inset()).max(0.0));
        let w = aw - ml - mr;
        if w < 40.0 || a.bottom - a.top < h + 2.0 * margin {
            return None;
        }
        let x = g.snap(a.left + ml);
        let bar = rect(x, g.snap(a.bottom - margin - h), g.snap(a.left + aw - mr) - x, h);
        let y = bar.top + pad;
        let long = self.player_ref().is_some_and(|p| p.duration() >= 3_600_000_000);
        let _ = long;
        let tw = g.measure_tabular(&wide(&self.time_template()), &g.fonts.caption).ceil() + 2.0;
        let sw = g.measure(&wide("/"), &g.fonts.caption).ceil() + 4.0;
        // As it narrows the volume slider goes first, then Settings (each while the seek bar would stay 160 wide),
        // then the times, the speaker with fullscreen, and play (while it would stay 48): the seek bar stays.
        let with_motion = self.motion_ready() || self.video.as_ref().is_some_and(|v| v.motion);
        let motion_w = if with_motion { btn + 4.0 } else { 0.0 };
        let essentials = |play: bool, times: bool, mute: bool| {
            2.0 * pad + 12.0 + motion_w + if play { 2.0 * btn + 10.0 } else { 0.0 } + if times { 2.0 * tw + sw + 12.0 } else { 0.0 } + if mute { 2.0 * btn + 12.0 } else { 0.0 }
        };
        let (with_play, with_times, with_mute) =
            [(true, true, true), (true, false, true), (true, false, false), (false, false, false)].into_iter().find(|&(p, t, m)| essentials(p, t, m) + 48.0 <= w).unwrap_or((false, false, false));
        let base = essentials(with_play, with_times, with_mute) + 160.0;
        let all = with_play && with_times && with_mute;
        let with_settings = all && base + btn + 4.0 <= w;
        let with_volume = with_settings && base + btn + 4.0 + VOLUME_W <= w;
        let motion = with_motion.then(|| rect(bar.left + pad, y, btn, btn));
        let first = bar.left + pad + motion_w;
        let play = with_play.then(|| rect(first, y, btn, btn));
        let stop = play.map(|p| rect(p.right + 4.0, y, btn, btn));
        let mut right = bar.right - pad;
        let full = with_mute.then(|| {
            right -= btn;
            let r = rect(right, y, btn, btn);
            right -= 4.0;
            r
        });
        // Speed and loop live in its menu (Faye, 2026-11-10).
        let settings = with_settings.then(|| {
            right -= btn;
            let r = rect(right, y, btn, btn);
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
        let left = stop.map_or(first, |p| p.right + 6.0);
        // Both times after the seek bar, "00:01:00/00:02:00" (Faye, 2026-11-10): the elapsed time against the
        // slash, the total after it, so the slash stays put.
        let times = with_times.then(|| {
            right -= tw;
            let total = rect(right, y, tw, btn);
            right -= sw;
            let slash = rect(right, y, sw, btn);
            right -= tw;
            let elapsed = rect(right, y, tw, btn);
            (elapsed, slash, total)
        });
        let (tl, tr) = (left + 6.0, right - 6.0 - if times.is_some() { 6.0 } else { 0.0 });
        let cy = y + btn / 2.0;
        let track = rect(tl, cy - SLIDER_H / 2.0, (tr - tl).max(1.0), SLIDER_H);
        let seek = rect(tl - 6.0, bar.top, (tr - tl).max(1.0) + 12.0, h);
        Some(BarLayout { bar, radius: (h / 2.0).min(10.0), play, times, track, seek, mute, volume, settings, full, stop, motion, prev: None, next: None, looping: None, title: None, clock: None })
    }

    /// An audio file's bar, as Transient's: across the top of the photo area, the title and format top left and
    /// the time top right, the seek bar the whole width under them, then previous, play, stop, next and Loop on
    /// the left and the speaker with the volume on the right (Faye, 2026-11-11).
    fn audio_bar_layout(&self, g: &Gfx) -> Option<BarLayout> {
        let a = self.image_area();
        let margin = 12.0;
        let (ml, mr) = ((margin - self.left_inset()).max(0.0), (margin - self.right_inset()).max(0.0));
        let x = g.snap(a.left + ml);
        let w = g.snap(a.right - mr) - x;
        if w < 200.0 || a.bottom - a.top < AUDIO_BAR_H + 40.0 {
            return None;
        }
        let bar = rect(x, g.snap(a.top), w, AUDIO_BAR_H);
        let pad = 14.0;
        let btn = BAR_BTN;
        let (l, r) = (bar.left + pad, bar.right - pad);
        let clock_w = 150.0;
        let clock = rect(r - clock_w, bar.top + 12.0, clock_w, 40.0);
        let title = rect(l, bar.top + 12.0, (clock.left - 16.0 - l).max(0.0), 40.0);
        let cy = clock.bottom + 6.0 + 9.0;
        let track = rect(l, cy - SLIDER_H / 2.0, r - l, SLIDER_H);
        let seek = rect(l - 6.0, cy - 9.0, r - l + 12.0, 18.0);
        let y = cy + 9.0 + 4.0;
        let mut at = l;
        let mut next_btn = |gap: f32| {
            let b = rect(at, y, btn, btn);
            at += btn + gap;
            b
        };
        let prev = next_btn(4.0);
        let play = next_btn(4.0);
        let stop = next_btn(4.0);
        let next = next_btn(12.0);
        let looping = next_btn(0.0);
        let volume = rect(r - VOLUME_W - 40.0, y, VOLUME_W + 40.0, btn);
        let mute = rect(volume.left - 4.0 - btn, y, btn, btn);
        let fits = mute.left > looping.right + 8.0;
        Some(BarLayout {
            bar,
            radius: 10.0,
            play: Some(play),
            times: None,
            track,
            seek,
            mute: fits.then_some(mute),
            volume: fits.then_some(volume),
            settings: None,
            full: None,
            stop: Some(stop),
            prev: Some(prev),
            next: Some(next),
            looping: Some(looping),
            title: Some(title),
            clock: Some(clock),
            motion: None,
        })
    }

    /// Stop: paused, back at the start.
    pub(super) fn video_stop(&mut self) {
        if let Some(p) = self.player() {
            p.set_playing(false);
            let start = p.start_us();
            p.seek(start, true);
            self.video_moved = Instant::now();
            self.invalidate();
        }
    }

    /// The bar, over the photo (above the page bar's place: a video has no pages). Returns whether it's fading.
    pub(super) fn draw_video_bar(&mut self, g: &Gfx) {
        if !self.video_shown() || self.player_ref().is_none() {
            return;
        }
        // A motion photo's bar while it grows out of (or back into) its pill is drawn by draw_motion_bar.
        if self.video.as_ref().is_some_and(|v| v.motion) && (!self.motion_open || self.motion_k() < 1.0) {
            return;
        }
        self.draw_video_bar_with(g, None);
    }

    /// The bar; with `morph` (a motion photo's pill growing into it): its contents only, faded by the alpha given,
    /// clipped to the rect given (the caller draws that box), and not clickable.
    pub(super) fn draw_video_bar_with(&mut self, g: &Gfx, morph: Option<(D2D_RECT_F, f32)>) {
        let wanted = self.bar_wanted();
        let a = match morph {
            Some((_, alpha)) => alpha,
            None => self.fades.get(Hit::VideoBar, wanted),
        };
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
        let layout = if a > 0.0 { self.bar_layout(g) } else { None };
        let audio = self.audio_shown();
        if let Some(v) = self.video.as_mut() {
            v.bar_top = layout.as_ref().filter(|_| !audio).map(|l| (l.bar.top, a));
        }
        let Some(l) = layout else { return };
        let (dur, playing, ended) = {
            let p = self.player_ref().unwrap();
            (p.duration(), p.playing(), p.at_end())
        };
        let pos = self.video.as_ref().map_or(0, |v| v.position());
        let t = theme::t();
        // The cards' colours (the explorer's, the info card's), not a popup's lighter greys (Faye, 2026-11-10).
        let raised = t;
        // Hits only while it's (mostly) there: a click on a fading bar is meant for the video.
        let live = a > 0.5 && morph.is_none();
        if let Some((c, _)) = morph {
            g.push_clip(c);
        }
        g.push_fade(a, 1.0, 0.0, 0.0);
        if morph.is_none() {
            ui::shadow(g, l.bar, l.radius);
            g.fill_round(l.bar, l.radius, rgb(t.sidebar));
            ui::stroke(g, l.bar, l.radius, rgb(raised.divider));
        }
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
        if let Some(r) = l.motion {
            let fg = button(self, Hit::MotionBadge, r, ui::Kind::Toggle(true));
            super::motionphoto::draw_mark(g, (r.left + r.right) / 2.0, (r.top + r.bottom) / 2.0, fg);
        }
        let glyph = if ended { 0xE72C } else if playing { 0xE769 } else { 0xE768 };
        if let Some(r) = l.play {
            // The app's boxed buttons and toggles, as the toolbar's (Faye, 2026-11-10).
            let fg = button(self, Hit::VideoPlay, r, ui::Kind::Standard);
            g.text(&[glyph], &g.fonts.icons, r, fg, Align::Center);
        }
        if let Some(r) = l.stop {
            let fg = button(self, Hit::VideoStop, r, ui::Kind::Standard);
            g.text(&[0xE71A], &g.fonts.icons, r, fg, Align::Center);
        }
        if let Some(r) = l.prev {
            let fg = button(self, Hit::AudioPrev, r, ui::Kind::Standard);
            g.text(&[0xE892], &g.fonts.icons, r, fg, Align::Center);
        }
        if let Some(r) = l.next {
            let fg = button(self, Hit::AudioNext, r, ui::Kind::Standard);
            g.text(&[0xE893], &g.fonts.icons, r, fg, Align::Center);
        }
        if let Some(r) = l.looping {
            let on = self.player_ref().is_some_and(|p| p.looping());
            let fg = button(self, Hit::AudioLoop, r, ui::Kind::Toggle(on));
            g.text(&[0xE8EE], &g.fonts.icons, r, fg, Align::Center);
        }
        if let Some(r) = l.title {
            let (title, line) = self.audio_title_lines();
            g.text(&wide(&title), &g.fonts.title, rect(r.left, r.top, r.right - r.left, 22.0), rgb(raised.text), Align::Left);
            g.text_tabular(&wide(&line), &g.fonts.caption, rect(r.left, r.top + 22.0, r.right - r.left, 16.0), rgb(raised.label), Align::Left);
        }
        if let Some(r) = l.clock {
            let now = if ended { dur } else { pos };
            g.text_tabular(&wide(&super::audio::precise_us(now)), &g.fonts.title, rect(r.left, r.top, r.right - r.left, 22.0), rgb(raised.text), Align::Right);
            g.text_tabular(&wide(&format!("of {}", super::audio::precise_us(dur))), &g.fonts.caption, rect(r.left, r.top + 22.0, r.right - r.left, 16.0), rgb(raised.label), Align::Right);
        }

        let long = dur >= 3_600_000_000;
        if let Some((elapsed, slash, total)) = l.times {
            // One clickable group (Timecode or Frames): the bar's quiet box on hover, as its buttons, from the
            // elapsed time's box to the end of the total's text.
            let total_text = self.total_text();
            let total_w = g.measure_tabular(&wide(&total_text), &g.fonts.caption).ceil();
            let group = D2D_RECT_F { left: elapsed.left - 5.0, right: (total.left + total_w + 5.0).min(total.right + 5.0), ..elapsed };
            if live {
                self.hits.add(Hit::VideoTime, group);
            }
            let open = matches!(self.menu, Some((menus::MenuKind::VideoTime, _)));
            let st = self.state(Hit::VideoTime, true);
            ui::quiet_frame(g, group, ui::Kind::Toggle(open), &st);
            g.text_tabular(&wide(&self.time_text(pos)), &g.fonts.caption, elapsed, rgb(raised.text), Align::Right);
            g.text(&wide("/"), &g.fonts.caption, slash, rgb(raised.label), Align::Center);
            g.text_tabular(&wide(&total_text), &g.fonts.caption, total, rgb(raised.label), Align::Left);
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
        // At the end the position is the last picture's start, a frame short of the duration: full then (Faye,
        // 2026-11-10: the red stopped before the end).
        let frac = if ended && self.video.as_ref().is_none_or(|v| v.scrub.is_none()) {
            1.0
        } else if dur > 0 {
            (pos as f64 / dur as f64).clamp(0.0, 1.0) as f32
        } else {
            0.0
        };
        let px = line.left + (line.right - line.left) * frac;
        fill_track_to(g, line, px, rgb(t.accent));
        // Over the red too (Faye: the whole way to the pointer).
        if k > 0.0 && !scrubbing {
            let mx = self.mouse.0.clamp(line.left, line.right);
            fill_track_to(g, line, mx, gfx::rgba(raised.text, 0.28 * k));
        }
        // Chapters: a clean cut through the line at each one's start, in the bar's own colour, a whole number of
        // device pixels (2 at least) wide so it never blurs into a smudge.
        if dur > 0 {
            if let Some(p) = self.player_ref() {
                let px_w = (1.5 * g.dpi / 96.0).round().max(2.0);
                let gap = px_w * 96.0 / g.dpi;
                for c in p.chapters().iter().filter(|c| c.start_us > 0 && c.start_us < dur) {
                    let x = g.snap(line.left + (line.right - line.left) * (c.start_us as f64 / dur as f64) as f32 - gap / 2.0);
                    g.fill(D2D_RECT_F { left: x, right: x + gap, ..line }, rgb(t.sidebar));
                }
            }
        }

        let vol = self.settings.video_volume;
        let muted = self.video_muted();
        // The full speaker whatever the level, crossed out when muted (Faye, 2026-11-10: the slider shows the level).
        let glyph = if muted { 0xE74F } else { 0xE995 };
        if let Some(r) = l.mute {
            let fg = button(self, Hit::VideoMute, r, ui::Kind::Standard);
            // 18 px: the speaker glyphs sit smaller in their box than play's at 16 (Faye: the same size; all a tad
            // larger, 2026-11-10).
            g.text(&[glyph], &g.fonts.large_icons, r, fg, Align::Center);
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
        if let Some(r) = l.settings {
            let open = matches!(self.menu, Some((menus::MenuKind::VideoSettings, _)));
            let fg = button(self, Hit::VideoSettings, r, ui::Kind::Toggle(open));
            g.text(&[0xE713], &g.fonts.icons, r, fg, Align::Center);
        }
        if let Some(r) = l.full {
            let fg = button(self, Hit::VideoFullscreen, r, ui::Kind::Standard);
            // Segoe's back-to-window in fullscreen, full screen otherwise.
            let glyph = if self.fullscreen.is_some() { 0xE73F } else { 0xE740 };
            g.text(&[glyph], &g.fonts.icons, r, fg, Align::Center);
        }
        g.pop_fade();
        if morph.is_some() {
            g.pop_clip();
        }

        // Over the seek bar: the time there, and a picture of it once the preview decoder has one.
        if live && over && dur > 0 && !audio {
            let mx = if scrubbing { px } else { self.mouse.0.clamp(l.track.left, l.track.right) };
            let at = if scrubbing { pos } else { (((mx - l.track.left) / (l.track.right - l.track.left).max(1.0)) as f64 * dur as f64) as i64 };
            self.draw_seek_preview(g, &l, mx, at, long);
        }
    }

    fn draw_seek_preview(&mut self, g: &Gfx, l: &BarLayout, x: f32, at: i64, _long: bool) {
        let area = self.image_area();
        // The chapter there by name, then the time.
        let chapter = self.chapter_at(at).and_then(|i| self.player_ref().map(|p| p.chapters()[i].title.clone())).filter(|t| !t.is_empty());
        let label = match chapter {
            Some(t) => format!("{t} \u{b7} {}", self.time_text(at)),
            None => self.time_text(at),
        };
        let Some(v) = self.video.as_mut() else { return };
        if v.preview.is_none() && !v.audio {
            let rotation = v.player.as_ref().and_then(|p| p.info()).map_or(0, |i| i.rotation);
            let s = g.dpi / 96.0;
            let dev = if g.is_warp() { std::ptr::null_mut() } else { g.d3d().as_raw() };
            v.preview = Some(Preview::open(&v.path, rotation, (PREVIEW_W * s) as u32, (PREVIEW_H * s) as u32, gfx::Sendable(dev)));
        }
        // A wish a pixel's worth of time apart at most: only the newest is decoded.
        if let Some(p) = &v.preview {
            p.want(at);
        }
        let raised = theme::raised();
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

    /// The playing video's audio tracks for the context menu: (stream, label, playing). Empty without sound.
    pub(super) fn video_audio_tracks(&self) -> Vec<(i32, String, bool)> {
        let Some(p) = self.player_ref().filter(|_| self.video_shown()) else { return Vec::new() };
        let t = p.audio_tracks();
        t.iter().enumerate().map(|(n, tr)| (tr.stream, tr.label(n + 1), tr.playing != 0)).collect()
    }

    /// Whether the playing video plays every audio track at once (`ALL_AUDIO`: each one is marked playing).
    pub(super) fn video_audio_mixed(&self) -> bool {
        let t = self.video_audio_tracks();
        t.len() > 1 && t.iter().all(|t| t.2)
    }

    /// Plays the video's audio stream `stream` (Faye, 2026-11-10): the player opens again on it, where it was and
    /// playing if it was.
    /// `stream` may be `ALL_AUDIO`: every track at once, mixed (Faye, 2026-11-14: game captures with the game and
    /// the mic on tracks of their own).
    pub(super) fn set_audio_track(&mut self, stream: i32) {
        let mixed = self.video_audio_mixed();
        let label = if stream == crate::imaging::video::ALL_AUDIO {
            Some("All audio tracks".to_string())
        } else {
            self.video_audio_tracks().into_iter().find(|t| t.0 == stream).map(|t| t.1)
        };
        let Some(v) = self.video.as_mut() else { return };
        let Some(p) = v.player.as_ref() else { return };
        let already = if stream == crate::imaging::video::ALL_AUDIO {
            mixed
        } else {
            !mixed && p.audio_tracks().iter().any(|t| t.stream == stream && t.playing != 0)
        };
        if already {
            return;
        }
        let at = v.position();
        let playing = p.playing();
        v.audio_track = Some(stream);
        v.resume = Some((at, playing));
        v.player = None;
        if let Some(l) = label {
            self.show_video_toast(l);
        }
        self.invalidate();
    }

    /// The video's picture streams for the context menu: (stream, label, showing). Cover art isn't one.
    pub(super) fn video_picture_tracks(&self) -> Vec<(i32, String, bool)> {
        let Some(p) = self.player_ref().filter(|_| self.video_shown()) else { return Vec::new() };
        p.video_tracks().iter().enumerate().map(|(n, tr)| (tr.stream, tr.video_label(n + 1), tr.playing != 0)).collect()
    }

    /// Shows the video stream `stream` (Faye, 2026-11-10: some files have several): the player opens again on
    /// it, where it was and playing if it was, keeping the audio track.
    pub(super) fn set_video_track(&mut self, stream: i32) {
        let label = self.video_picture_tracks().into_iter().find(|t| t.0 == stream).map(|t| t.1);
        let Some(v) = self.video.as_mut() else { return };
        let Some(p) = v.player.as_ref() else { return };
        if p.video_tracks().iter().any(|t| t.stream == stream && t.playing != 0) {
            return;
        }
        let at = v.position();
        let playing = p.playing();
        if v.audio_track.is_none() {
            v.audio_track = p.audio_tracks().iter().find(|t| t.playing != 0).map(|t| t.stream);
        }
        v.video_track = Some(stream);
        v.resume = Some((at, playing));
        v.player = None;
        if let Some(l) = label {
            self.show_video_toast(l);
        }
        self.invalidate();
    }

    /// Each frame: the video playing (or nothing) for the taskbar's progress and Windows' media controls.
    pub(super) fn update_media(&mut self) {
        let now = if self.video_shown() {
            self.player_ref().map(|p| crate::media::Playing {
                title: self.current_path().and_then(|p| p.file_stem()).map(|s| s.to_string_lossy().into_owned()).unwrap_or_default(),
                playing: p.playing(),
                position_us: if p.at_end() { p.duration() } else { self.video.as_ref().map_or(0, |v| v.position()) },
                duration_us: p.duration(),
            })
        } else {
            None
        };
        self.media.update(self.hwnd, now.as_ref());
    }

    /// A media control pressed (the keyboard's play/pause key, the media flyout): `button` is media.rs's.
    pub(super) fn media_button(&mut self, button: usize) {
        let Some(playing) = self.player_ref().map(|p| p.playing()) else { return };
        let want = match button {
            crate::media::BUTTON_PLAY => true,
            crate::media::BUTTON_PAUSE | crate::media::BUTTON_STOP => false,
            _ => return,
        };
        if want != playing {
            self.video_toggle_play();
        }
        self.invalidate();
    }

    /// A click on one of the bar's buttons.
    pub(super) fn video_bar_click(&mut self, h: Hit) {
        match h {
            Hit::VideoPlay => self.video_toggle_play(),
            Hit::VideoStop => self.video_stop(),
            Hit::AudioPrev => self.step(-1),
            Hit::AudioNext => self.step(1),
            Hit::AudioLoop => self.toggle_video_loop(),
            Hit::VideoMute => self.toggle_mute(),
            Hit::VideoFullscreen => self.toggle_fullscreen(),
            Hit::VideoSettings => self.toggle_settings_menu(),
            Hit::VideoTime => self.toggle_time_menu(),
            _ => {}
        }
        self.invalidate();
    }

    /// The Settings menu's rows: the Speed submenu, then Loop, each ticked as the player is now.
    pub(super) fn video_settings_entries(&self) -> Vec<ui::Entry<menus::Action>> {
        let (speed, looping) = self.player_ref().map_or((1.0, false), |p| (p.speed(), p.looping()));
        let audio = self.player_ref().is_some_and(Engine::is_audio);
        let mut entries = Vec::new();
        if !audio {
            // The speeds in a menu of their own beside it (Faye, 2026-11-14).
            let name = |s: f64| if s == 1.0 { "Normal".to_string() } else { speed_text(s) };
            let rows = SPEEDS.iter().enumerate().map(|(i, &s)| menus::check(menus::Action::Choose(i), &name(s), s == speed)).collect();
            entries.push(ui::Entry::Sub(Some(0xEC4A), format!("Speed: {}", name(speed)), rows));
        }
        entries.push(menus::check(menus::Action::ToggleVideoLoop, "Loop", looping));
        // The tracks and subtitles, as the right-click menu has them (Faye, 2026-11-13).
        let tracks = self.track_entries();
        if !tracks.is_empty() {
            entries.push(ui::Entry::Separator);
            entries.extend(tracks);
        }
        if !audio {
            entries.push(ui::Entry::Separator);
            entries.push(ui::Entry::Heading("Time".into()));
            entries.extend(self.time_display_entries());
            entries.push(ui::Entry::Separator);
            entries.push(ui::Entry::Heading("Sound".into()));
            entries.push(menus::check(menus::Action::TogglePassthrough, "Passthrough to receiver", self.settings.video_passthrough));
        }
        // The chapters, the one playing ticked: a click goes to its start.
        if let Some(p) = self.player_ref().filter(|p| !p.chapters().is_empty()) {
            let now = self.chapter_at(p.position());
            entries.push(ui::Entry::Separator);
            entries.push(ui::Entry::Heading("Chapters".into()));
            for (i, c) in p.chapters().iter().enumerate() {
                entries.push(menus::check(menus::Action::Chapter(i), &chapter_label(c, i, &self.time_text(c.start_us)), now == Some(i)));
            }
        }
        entries
    }

    /// The chapter playing at `us`: the last that starts at or before it.
    fn chapter_at(&self, us: i64) -> Option<usize> {
        let cs = self.player_ref()?.chapters();
        let n = cs.partition_point(|c| c.start_us <= us);
        (n > 0).then(|| n - 1)
    }

    /// Dolby and DTS sound to a receiver undecoded, or not: the video opens again where it was (the output is
    /// opened differently), playing if it was.
    pub(super) fn toggle_passthrough(&mut self) {
        self.settings.video_passthrough = !self.settings.video_passthrough;
        settings::save(&self.settings);
        let on = self.settings.video_passthrough;
        if let Some(v) = self.video.as_mut() {
            if let Some(p) = v.player.as_ref().filter(|p| !p.is_audio()) {
                v.resume = Some((v.position(), p.playing()));
                v.player = None;
            }
        }
        self.show_video_toast(if on { "Passthrough on" } else { "Passthrough off" });
        self.invalidate();
    }

    /// Goes to chapter `i`'s start, saying which.
    pub(super) fn go_to_chapter(&mut self, i: usize) {
        let Some(c) = self.player_ref().and_then(|p| p.chapters().get(i)).cloned() else { return };
        let label = chapter_label(&c, i, &self.time_text(c.start_us));
        if let Some(p) = self.player() {
            p.seek(c.start_us, true);
        }
        self.show_video_toast(label);
        self.invalidate();
    }

    /// The next chapter, or (`back`) the start of this one, or the one before when this one has only just begun
    /// (as a CD player's previous track does).
    fn step_chapter(&mut self, back: bool) {
        let Some(p) = self.player_ref() else { return };
        let (pos, n) = (p.position(), p.chapters().len());
        if n == 0 {
            return;
        }
        let at = self.chapter_at(pos);
        let to = if back {
            match at {
                Some(i) if pos - p.chapters()[i].start_us > 3_000_000 => i,
                Some(i) => i.saturating_sub(1),
                None => 0,
            }
        } else {
            match at {
                Some(i) if i + 1 < n => i + 1,
                Some(_) => return,
                None => 0,
            }
        };
        self.go_to_chapter(to);
    }

    /// Timecode or Frames, ticked as set: the time menu's rows, and the Settings menu's last.
    fn time_display_entries(&self) -> Vec<ui::Entry<menus::Action>> {
        let frames = self.settings.video_frames;
        vec![menus::check(menus::Action::VideoFrames(false), "Timecode", !frames), menus::check(menus::Action::VideoFrames(true), "Frames", frames)]
    }

    /// The bar's times: timecode, or the picture's number counted from 1 (Faye, 2026-11-10).
    pub(super) fn set_video_frames(&mut self, on: bool) {
        if self.settings.video_frames != on {
            self.settings.video_frames = on;
            settings::save(&self.settings);
        }
        self.show_video_toast(if on { "Frames" } else { "Timecode" });
        self.invalidate();
    }

    /// The bar's time at `us`: timecode, or in frames mode the picture's number from the container's list of
    /// pictures (exact on a variable frame rate), else worked out from the frame rate.
    fn time_text(&self, us: i64) -> String {
        let Some(p) = self.player_ref() else { return String::new() };
        // Sound: minutes and seconds, as a music player shows them.
        if p.is_audio() {
            return clock_text(us, p.duration() >= 3_600_000_000);
        }
        let fps = p.info().and_then(|i| i.fps()).unwrap_or(0.0);
        if self.settings.video_frames {
            if let Some((n, _)) = p.frame_number(us) {
                return n.to_string();
            }
            if fps > 0.0 {
                return ((us.max(0) as f64 * fps / 1e6).round() as u64 + 1).to_string();
            }
        }
        timecode(us, fps, p.duration() >= 3_600_000_000)
    }

    /// The bar's total: the duration's timecode, or how many pictures there are.
    fn total_text(&self) -> String {
        let Some(p) = self.player_ref() else { return String::new() };
        if p.is_audio() {
            return clock_text(p.duration(), p.duration() >= 3_600_000_000);
        }
        let fps = p.info().and_then(|i| i.fps()).unwrap_or(0.0);
        if self.settings.video_frames {
            if let Some((_, count)) = p.frame_number(0) {
                return count.to_string();
            }
            if fps > 0.0 {
                return ((p.duration() as f64 * fps / 1e6).round() as u64).max(1).to_string();
            }
        }
        timecode(p.duration(), fps, p.duration() >= 3_600_000_000)
    }

    /// The widest the bar's times get, for their boxes.
    fn time_template(&self) -> String {
        let Some(p) = self.player_ref() else { return String::new() };
        if p.is_audio() {
            return clock_text(p.duration(), p.duration() >= 3_600_000_000).chars().map(|c| if c.is_ascii_digit() { '0' } else { c }).collect();
        }
        let fps = p.info().and_then(|i| i.fps()).unwrap_or(0.0);
        if self.settings.video_frames && (fps > 0.0 || p.frame_number(0).is_some()) {
            return "0".repeat(self.total_text().len());
        }
        timecode_template(fps, p.duration() >= 3_600_000_000)
    }

    /// A click on the bar's times: Timecode or Frames, in a menu off them.
    fn toggle_time_menu(&mut self) {
        if self.player_ref().is_some_and(Engine::is_audio) {
            return;
        }
        if matches!(self.menu, Some((menus::MenuKind::VideoTime, _))) {
            self.close_menu();
            return;
        }
        let Some(r) = self.hits.rect_of(Hit::VideoTime) else { return };
        let entries = self.time_display_entries();
        let Some(g) = &self.gfx else { return };
        let (w, h) = self.size_dip();
        self.menu = Some((menus::MenuKind::VideoTime, ui::Menu::below(g, entries, r, rect(0.0, 0.0, w, h))));
        self.hide_tooltip();
        self.invalidate();
    }

    /// The bar's Settings button: a menu of the speeds and Loop.
    fn toggle_settings_menu(&mut self) {
        if matches!(self.menu, Some((menus::MenuKind::VideoSettings, _))) {
            self.close_menu();
            return;
        }
        let Some(r) = self.hits.rect_of(Hit::VideoSettings) else { return };
        let entries = self.video_settings_entries();
        let Some(g) = &self.gfx else { return };
        let (w, h) = self.size_dip();
        self.menu = Some((menus::MenuKind::VideoSettings, ui::Menu::below(g, entries, r, rect(0.0, 0.0, w, h))));
        self.hide_tooltip();
        self.invalidate();
    }
}

/// What plays: a video's player (pictures and sound), or an audio file's (sound only, app/audio.rs). The bar,
/// keys, resume points and media controls drive either through this.
pub(super) enum Engine {
    Video(Player),
    Audio(AudioPlayer),
}

macro_rules! both {
    ($s:expr, $p:ident => $e:expr) => {
        match $s {
            Engine::Video($p) => $e,
            Engine::Audio($p) => $e,
        }
    };
}

impl Engine {
    pub(super) fn is_audio(&self) -> bool {
        matches!(self, Engine::Audio(_))
    }
    pub(super) fn audio(&mut self) -> Option<&mut AudioPlayer> {
        if let Engine::Audio(a) = self { Some(a) } else { None }
    }
    pub(super) fn audio_ref(&self) -> Option<&AudioPlayer> {
        if let Engine::Audio(a) = self { Some(a) } else { None }
    }
    pub(super) fn info(&self) -> Option<&Info> {
        both!(self, p => p.info.as_ref())
    }
    fn poll_open(&mut self) -> Option<Result<(), String>> {
        both!(self, p => p.poll_open())
    }
    fn due(&mut self, now: Instant) -> Option<Due> {
        if let Engine::Video(p) = self { p.due(now) } else { None }
    }
    pub(super) fn animating(&self) -> bool {
        both!(self, p => p.animating())
    }
    pub(super) fn playing(&self) -> bool {
        both!(self, p => p.playing())
    }
    pub(super) fn set_playing(&mut self, play: bool) {
        both!(self, p => p.set_playing(play))
    }
    pub(super) fn position(&self) -> i64 {
        both!(self, p => p.position())
    }
    pub(super) fn duration(&self) -> i64 {
        both!(self, p => p.duration())
    }
    pub(super) fn seek(&mut self, us: i64, exact: bool) {
        both!(self, p => p.seek(us, exact))
    }
    pub(super) fn at_end(&self) -> bool {
        both!(self, p => p.at_end())
    }
    pub(super) fn looping(&self) -> bool {
        both!(self, p => p.looping)
    }
    pub(super) fn set_looping(&mut self, on: bool) {
        both!(self, p => p.set_looping(on))
    }
    pub(super) fn set_volume(&mut self, volume: f32, muted: bool) {
        both!(self, p => p.set_volume(volume, muted))
    }
    pub(super) fn frame_number(&self, us: i64) -> Option<(usize, usize)> {
        if let Engine::Video(p) = self { p.frame_number(us) } else { None }
    }
    pub(super) fn audio_tracks(&self) -> &[Track] {
        if let Engine::Video(p) = self { p.audio_tracks() } else { &[] }
    }
    pub(super) fn video_tracks(&self) -> &[Track] {
        if let Engine::Video(p) = self { p.video_tracks() } else { &[] }
    }
    /// The video's subtitle streams (none for an audio file).
    pub(super) fn sub_tracks(&self) -> &[Track] {
        if let Engine::Video(p) = self { p.sub_tracks() } else { &[] }
    }
    /// The video's chapters (none for an audio file).
    pub(super) fn chapters(&self) -> &[Chapter] {
        if let Engine::Video(p) = self { p.chapters() } else { &[] }
    }
    /// The video stream's start, which the player's clock counts from.
    pub(super) fn start_us(&self) -> i64 {
        if let Engine::Video(p) = self { p.start_us() } else { 0 }
    }
    fn step_forward(&mut self) {
        if let Engine::Video(p) = self {
            p.step_forward()
        }
    }
    fn step_back(&mut self) {
        if let Engine::Video(p) = self {
            p.step_back()
        }
    }
    pub(super) fn speed(&self) -> f64 {
        if let Engine::Video(p) = self { p.speed() } else { 1.0 }
    }
    fn set_speed(&mut self, s: f64) {
        if let Engine::Video(p) = self {
            p.set_speed(s)
        }
    }
    fn reverse(&self) -> bool {
        if let Engine::Video(p) = self { p.reverse() } else { false }
    }
    fn play_reverse(&mut self, s: f64) {
        if let Engine::Video(p) = self {
            p.play_reverse(s)
        }
    }
    fn exact_is_quick(&self, us: i64) -> bool {
        if let Engine::Video(p) = self { p.exact_is_quick(us) } else { true }
    }
    fn settled(&self) -> bool {
        if let Engine::Video(p) = self { p.settled() } else { true }
    }
}

/// How a chapter is named in the Settings menu and the toast: its title, else "Chapter n", and where it starts.
fn chapter_label(c: &Chapter, i: usize, at: &str) -> String {
    let t = c.title.trim();
    if t.is_empty() { format!("Chapter {} \u{b7} {at}", i + 1) } else { format!("{t} \u{b7} {at}") }
}

/// The bar's own hits: the pointer over them keeps it up, and the photo's clicks and drags don't apply.
pub(super) fn is_bar_hit(h: Hit) -> bool {
    matches!(h, Hit::VideoBar | Hit::VideoPlay | Hit::VideoStop | Hit::AudioPrev | Hit::AudioNext | Hit::AudioLoop | Hit::VideoSeek | Hit::VideoMute | Hit::VideoVolume | Hit::VideoSettings | Hit::VideoTime | Hit::VideoFullscreen)
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
        assert_eq!(timecode(65_983_000, 60.0, false), "01:05:59"); // picture 59 starts at .98333, stored as .983
        assert_eq!(timecode(3_723_040_000, 25.0, true), "1:02:03:01");
        assert_eq!(timecode(5_000_000, 0.0, false), "00:05:00");
        assert_eq!(timecode(760_000, 120.0, false), "00:00:091");
        assert_eq!(timecode(1_833_334, 120.0, false), "00:01:100");
        assert_eq!(timecode(1_000_000, 119.88, false), "00:01:000");
        // Late in a second, nearest is the next second's first picture.
        assert_eq!(timecode(1_999_000, 120.0, false), "00:02:000");
        // An NVIDIA capture's pictures (7.5 to 9 ms apart at 120 fps), stepped through: each one number on.
        let pts = [3_091_178, 3_100_222, 3_108_244, 3_115_778, 3_124_800, 3_132_500, 3_141_011, 3_150_044, 3_157_856];
        let got: Vec<String> = pts.iter().map(|&t| timecode(t, 120.008, false)).collect();
        let want: Vec<String> = (11..=19).map(|f| format!("00:03:{f:03}")).collect();
        assert_eq!(got, want);
        assert_eq!(timecode_template(30.0, false), "00:00:00");
        assert_eq!(timecode_template(100.0, false), "00:00:00");
        assert_eq!(timecode_template(120.0, true), "0:00:00:000");
        assert_eq!(timecode_template(0.0, false), "00:00:00");
        assert_eq!(speed_text(0.25), "0.25\u{00D7}");
        assert_eq!(speed_text(2.0), "2\u{00D7}");
    }
}
