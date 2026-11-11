//! Motion photos (`crate::motionphoto`): a photo with a short video. In photo mode a Motion pill sits where a video's
//! bar would, at the bottom in the middle, in the bar's own look; pressing it grows it smoothly into the full video
//! bar (video mode: the clip plays with every control a video has, its Motion toggle at the bar's left end), and
//! that toggle shrinks it back into the pill and the still. Holding the left button still on the photo in photo
//! mode plays the clip while it's held, as a phone does. The clip plays in the video view (`VideoView` with
//! `motion` set: no resume point; while held, no bar or keys either), drawn in the photo's own box.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use super::*;

/// Held this long without moving, a press on a motion photo plays it (shorter, and every click would flicker).
const HOLD: Duration = Duration::from_millis(280);

/// The current photo's motion part: found (and copied out) on a thread of its own.
pub(super) struct MotionPhoto {
    pub photo: PathBuf,
    /// The thread's answer: Some(the video to play) or Some(None) when it has none; None while it looks.
    found: Arc<Mutex<Option<Option<PathBuf>>>>,
    pub video: Option<PathBuf>,
    pub looked: bool,
}

/// The Motion mark: a ring around a dot, centred on (cx, cy).
pub(super) fn draw_mark(g: &Gfx, cx: f32, cy: f32, c: windows::Win32::Graphics::Direct2D::Common::D2D1_COLOR_F) {
    ui::stroke(g, rect(cx - 6.5, cy - 6.5, 13.0, 13.0), 6.5, c);
    g.fill_round(rect(cx - 2.5, cy - 2.5, 5.0, 5.0), 2.5, c);
}

fn lerp(a: f32, b: f32, k: f32) -> f32 {
    a + (b - a) * k
}

impl App {
    /// Each frame, before the video's: a new photo is looked at for a motion part (and starts in photo mode); a
    /// motion video playing for a photo no longer shown stops; a hold long enough starts one; a held one played to
    /// its end stops; video mode, once shrunk back into the pill, lets its video go.
    pub(super) fn sync_motion(&mut self) {
        let current = self.current_path().filter(|_| self.photo_up()).map(Path::to_path_buf);
        if self.motion.as_ref().map(|m| &m.photo) != current.as_ref() {
            self.stop_motion();
            self.motion_hold = None;
            self.motion_open = false;
            self.fades.set_now(Hit::MotionMorph, false);
            self.motion = current.map(|photo| {
                let found = Arc::new(Mutex::new(None));
                let (f, p) = (found.clone(), photo.clone());
                std::thread::Builder::new()
                    .name("motion-photo".into())
                    .spawn(move || {
                        let v = crate::motionphoto::find(&p).and_then(|s| crate::motionphoto::playable(&p, &s));
                        if let Ok(mut f) = f.lock() {
                            *f = Some(v);
                        }
                    })
                    .ok();
                MotionPhoto { photo, found, video: None, looked: false }
            });
        }
        if let Some(m) = self.motion.as_mut().filter(|m| !m.looked) {
            if let Some(v) = m.found.lock().ok().and_then(|mut f| f.take()) {
                m.video = v;
                m.looked = true;
                self.invalidate(); // the pill
            }
        }
        // Held still long enough on the photo: play.
        if let Some(at) = self.motion_hold {
            if self.pressed != Some(Hit::Viewport) || self.motion_open {
                self.motion_hold = None;
            } else if at.elapsed() >= HOLD && self.motion_ready() && !self.motion_playing() {
                self.motion_hold = None;
                self.start_motion(true);
            }
        }
        // A held clip played through: the still again.
        let done = self.motion_held && self.video.as_ref().filter(|v| v.motion).and_then(|v| v.player.as_ref()).is_some_and(|p| p.info().is_some() && p.at_end());
        if done {
            self.stop_motion();
        }
        // Back in photo mode and the bar all the way back into the pill: the video goes.
        if !self.motion_open && !self.motion_held && self.motion_playing() && self.fades.get(Hit::MotionMorph, false) <= 0.0 {
            self.stop_motion();
        }
    }

    /// The current photo has a motion part ready to play.
    pub(super) fn motion_ready(&self) -> bool {
        self.motion.as_ref().is_some_and(|m| m.video.is_some()) && self.photo_up() && self.placeholder.is_none()
    }

    pub(super) fn motion_playing(&self) -> bool {
        self.video.as_ref().is_some_and(|v| v.motion)
    }

    /// Still looking for a motion part, waiting out a hold, or a motion video up (a held one's end, which the
    /// player itself no longer animates, brings the still back): draw again next vsync.
    pub(super) fn motion_animating(&self) -> bool {
        self.motion.as_ref().is_some_and(|m| !m.looked) || self.motion_hold.is_some() || (self.motion_playing() && self.motion_held)
    }

    /// Plays the motion part from its start. `held`: while the button is down (it stops when it comes up); else
    /// video mode.
    fn start_motion(&mut self, held: bool) {
        let Some(video) = self.motion.as_ref().and_then(|m| m.video.clone()) else { return };
        self.stop_motion();
        self.motion_held = held;
        self.video = Some(video::VideoView::for_motion(video, !held));
        self.invalidate();
    }

    /// Back to the still.
    pub(super) fn stop_motion(&mut self) {
        if self.motion_playing() {
            self.video = None;
            self.invalidate();
        }
        self.motion_held = false;
    }

    /// The Motion button: photo mode to video mode (the pill grows into the bar, the clip plays), or back (the
    /// still at once, the bar shrinking back into the pill, then the video goes).
    pub(super) fn toggle_motion_open(&mut self) {
        if !self.motion_open {
            if !self.motion_ready() {
                return;
            }
            self.motion_hold = None;
            self.start_motion(false);
            self.motion_open = true;
            self.video_moved = Instant::now();
        } else {
            self.motion_open = false;
            if let Some(p) = self.player() {
                p.set_playing(false);
            }
        }
        self.hide_tooltip();
        self.invalidate();
    }

    /// The left button down on the photo: a hold may follow (photo mode only; in video mode a click plays and
    /// pauses, as on any video).
    pub(super) fn motion_press(&mut self) {
        if self.motion_ready() && !self.motion_playing() && !self.motion_open {
            self.motion_hold = Some(Instant::now());
        }
    }

    /// The pointer moved while pressed: a drag (a pan) isn't a hold.
    pub(super) fn motion_moved(&mut self, x: f32, y: f32) {
        if self.motion_hold.is_some() && (x - self.press_at.0).abs() + (y - self.press_at.1).abs() > self.click_slop() {
            self.motion_hold = None;
        }
    }

    /// The left button up: a hold's video stops.
    pub(super) fn motion_release(&mut self) {
        self.motion_hold = None;
        if self.motion_held {
            self.stop_motion();
        }
    }

    /// The pill, in the bar's place and look, and its growing into the bar (and back): the box eases from the
    /// pill's width to the bar's (the motion curve), the pill's mark and word fading out over the first half and the
    /// bar's controls fading in over the second, inside the box. All the way open, the video bar draws itself.
    pub(super) fn draw_motion_bar(&mut self, g: &Gfx) {
        if !self.motion_ready() && !(self.motion_playing() && !self.motion_held) {
            return;
        }
        let k = self.fades.get(Hit::MotionMorph, self.motion_open);
        if k >= 1.0 && self.motion_open && self.video_shown() && self.player_ref().is_some() {
            return;
        }
        let Some((full, radius)) = self.video_bar_rect(g) else { return };
        let label = wide("Motion");
        let tw = g.measure(&label, &g.fonts.caption).ceil();
        let pill_w = (12.0 + 13.0 + 8.0 + tw + 16.0).min(full.right - full.left);
        let cx = (full.left + full.right) / 2.0;
        let c = rect(
            g.snap(lerp(cx - pill_w / 2.0, full.left, k)),
            full.top,
            g.snap(lerp(pill_w, full.right - full.left, k)),
            full.bottom - full.top,
        );
        let t = theme::t();
        ui::shadow(g, c, radius);
        g.fill_round(c, radius, rgb(t.sidebar));
        let over = self.hover == Some(Hit::MotionBadge) && k < 0.5;
        if over {
            g.fill_round(c, radius, gfx::rgba(t.text, 0.06));
        }
        ui::stroke(g, c, radius, rgb(t.divider));
        // The pill's face: the mark and the word, centred.
        let face = (1.0 - k * 2.0).clamp(0.0, 1.0);
        if face > 0.0 {
            g.push_fade(face, 1.0, 0.0, 0.0);
            let w = 13.0 + 8.0 + tw;
            let left = (c.left + c.right) / 2.0 - w / 2.0;
            let cy = (c.top + c.bottom) / 2.0;
            draw_mark(g, left + 6.5, cy, rgb(t.text));
            g.text(&label, &g.fonts.caption, rect(left + 13.0 + 8.0, c.top, tw + 2.0, c.bottom - c.top), rgb(t.text), Align::Left);
            g.pop_fade();
        }
        if k < 0.5 && !self.motion_open {
            self.hits.add(Hit::MotionBadge, c);
        }
        // The bar's controls, coming in (or going) inside the box.
        let inside = ((k - 0.5) * 2.0).clamp(0.0, 1.0);
        if inside > 0.0 && self.video.as_ref().is_some_and(|v| v.motion) && self.player_ref().is_some() {
            self.draw_video_bar_with(g, Some((c, inside)));
        }
    }
}
