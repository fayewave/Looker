//! The video on screen. The decode pool's still (its first picture, imaging/video.rs) shows first, like any
//! image; the player (player.rs) opens meanwhile on the window's hardware device and, once its pictures come,
//! each frame's due picture is turned into an RGB texture (yuv.rs) that is drawn where the still was, through
//! the same view (zoom, pan, fit).
//!
//! Keys (agreed with Faye, native/PLAN.md N6): Space plays/pauses; K pauses; L plays, then 2x, 4x (silent); J
//! slows back down, or at 1x jumps back 5 s; `,` / `.` step a picture; Shift+Left/Right jump 5 s; M mutes;
//! Up/Down (when the explorer isn't shown) and the wheel over the video change the volume (Ctrl+wheel zooms).

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use windows::core::Interface;

use super::*;
use crate::player::Player;
use crate::yuv::Converter;
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_FORMAT_R16G16B16A16_FLOAT};

/// The hardware device couldn't be made: videos play from the CPU on WARP rather than wait for it.
pub(super) static HW_FAILED: AtomicBool = AtomicBool::new(false);

/// What a jump (Shift+arrow, J) moves.
const JUMP_US: i64 = 5_000_000;

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
}

impl VideoView {
    /// Textures die with their device: so does the player's decoder (its surfaces are on it), reopened where it was.
    pub(super) fn device_lost(&mut self) {
        self.conv = None;
        self.bitmap = None;
        if let Some(p) = self.player.take() {
            self.resume = Some((p.position(), p.playing()));
        }
    }
}

impl App {
    /// After every navigation: a video on screen gets a player; anything else drops the last one.
    pub(super) fn sync_video(&mut self) {
        let want = self.current_path().filter(|p| format::from_extension(p.extension().and_then(|e| e.to_str())) == format::Format::Video).map(Path::to_path_buf);
        let Some(path) = want else {
            self.video = None;
            return;
        };
        if self.video.as_ref().is_some_and(|v| v.path == path) {
            return;
        }
        self.video = Some(VideoView { path, player: None, conv: None, bitmap: None, failed: false, resume: None });
    }

    /// A video is the current file (its keys and wheel apply, even before its first picture).
    pub(super) fn video_shown(&self) -> bool {
        self.placeholder.is_none() && self.video.as_ref().is_some_and(|v| !v.failed)
    }

    /// Frames are moving, or the player is still coming up: draw again next vsync.
    pub(super) fn video_animating(&self) -> bool {
        self.video.as_ref().is_some_and(|v| !v.failed && v.player.as_ref().is_none_or(|p| p.animating()))
    }

    /// Before the frame begins (Direct2D isn't mid-draw): opens the player once the device is right, and turns
    /// the picture due now into the bitmap drawn this frame.
    pub(super) fn prepare_video(&mut self, g: &Gfx) {
        let (volume, muted) = (self.settings.video_volume, self.settings.video_muted);
        let Some(v) = self.video.as_mut().filter(|v| !v.failed) else { return };
        if v.player.is_none() {
            // The first ~200 ms after launch draw on WARP: wait for the hardware device to decode on its GPU.
            if g.is_warp() && !HW_FAILED.load(Ordering::Relaxed) {
                return;
            }
            let dev = if g.is_warp() { std::ptr::null_mut() } else { g.d3d().as_raw() };
            let (at, play) = v.resume.take().unwrap_or((0, true));
            let mut p = Player::open(&v.path, gfx::Sendable(dev), play, volume, muted);
            if at > 0 {
                p.seek(at, true);
            }
            v.player = Some(p);
        }
        let p = v.player.as_mut().unwrap();
        match p.poll_open() {
            None => return,
            Some(Err(_)) => {
                v.failed = true; // the still stays
                return;
            }
            Some(Ok(())) => {}
        }
        let Some(due) = p.due(Instant::now()) else { return };
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
        let white = g.space().is_scrgb().then(|| g.space().white_scale());
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
                "video: first picture {}x{} layout {} planes {} {}-bit (shift {}) chroma {}/{} matrix {} full {}",
                f.width, f.height, f.layout, f.nplanes, f.bit_depth, f.shift, f.chroma_w_shift, f.chroma_h_shift, f.matrix, f.full_range
            ));
        }
        let conv = v.conv.as_mut().unwrap();
        match conv.convert(&due.picture, info.rotation, w, h, white) {
            Ok(remade) => {
                if remade || v.bitmap.is_none() {
                    let format = if white.is_some() { DXGI_FORMAT_R16G16B16A16_FLOAT } else { DXGI_FORMAT_B8G8R8A8_UNORM };
                    v.bitmap = conv.out.as_ref().and_then(|o| g.texture_bitmap(&o.texture, format).ok());
                }
            }
            Err(e) => crate::trace::mark(format!("video: picture: {e}")),
        }
    }

    /// The current picture, to draw instead of the still.
    pub(super) fn video_bitmap(&self) -> Option<ID2D1Bitmap1> {
        self.video.as_ref().filter(|_| self.placeholder.is_none()).and_then(|v| v.bitmap.clone())
    }

    fn player(&mut self) -> Option<&mut Player> {
        self.video.as_mut().filter(|v| !v.failed).and_then(|v| v.player.as_mut()).filter(|p| p.info.is_some())
    }

    pub(super) fn video_volume_by(&mut self, delta: f32) {
        let v = (self.settings.video_volume + delta).clamp(0.0, 1.0);
        self.settings.video_volume = (v * 100.0).round() / 100.0;
        self.settings.video_muted = false;
        self.apply_volume();
        self.show_toast(format!("Volume {:.0}%", self.settings.video_volume * 100.0), true);
    }

    fn apply_volume(&mut self) {
        let (v, m) = (self.settings.video_volume, self.settings.video_muted);
        if let Some(p) = self.player() {
            p.set_volume(v, m);
        }
        settings::save(&self.settings);
    }

    /// A key for the video; false when it isn't one (or no video is up).
    pub(super) fn video_key(&mut self, vk: VIRTUAL_KEY, shift: bool, ctrl: bool) -> bool {
        if ctrl || !self.video_shown() {
            return false;
        }
        let explorer = self.explorer_shown();
        match vk {
            VK_M => {
                self.settings.video_muted = !self.settings.video_muted;
                self.apply_volume();
                self.show_toast(if self.settings.video_muted { "Muted" } else { "Sound on" }, true);
            }
            VK_UP if !explorer => self.video_volume_by(0.05),
            VK_DOWN if !explorer => self.video_volume_by(-0.05),
            _ => {
                let Some(p) = self.player() else {
                    // Not open yet: the keys that would act on it are still taken, not passed to the photo keys.
                    return matches!(vk, VK_SPACE | VK_K | VK_L | VK_J | VK_OEM_COMMA | VK_OEM_PERIOD) || (shift && matches!(vk, VK_LEFT | VK_RIGHT));
                };
                let mut toast = None;
                match vk {
                    VK_SPACE => {
                        let play = !p.playing();
                        p.set_speed(1.0);
                        p.set_playing(play);
                    }
                    VK_K => {
                        p.set_playing(false);
                        p.set_speed(1.0);
                    }
                    VK_L => {
                        if !p.playing() {
                            p.set_speed(1.0);
                            p.set_playing(true);
                        } else {
                            let s = (p.speed() * 2.0).min(4.0);
                            p.set_speed(s);
                            toast = Some(format!("{s}×"));
                        }
                    }
                    VK_J => {
                        if p.playing() && p.speed() > 1.0 {
                            let s = p.speed() / 2.0;
                            p.set_speed(s);
                            toast = Some(format!("{s}×"));
                        } else {
                            let t = p.position() - JUMP_US;
                            p.seek(t, true);
                        }
                    }
                    VK_OEM_PERIOD => p.step_forward(),
                    VK_OEM_COMMA => p.step_back(),
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
}
