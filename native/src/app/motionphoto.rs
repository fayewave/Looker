//! Motion photos (`crate::motionphoto`): a photo with a short video plays it while the left button is held on the
//! photo (as a phone plays one while a finger rests on it), or once from the Motion badge in its corner. The video
//! plays in the video view (`VideoView` with `motion` set: no bar, no keys, no resume point), drawn in the photo's
//! own box; at its end, or when the button comes up, the still is back.

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

impl App {
    /// Each frame, before the video's: a new photo is looked at for a motion part; a motion video playing for a
    /// photo no longer shown stops; a hold long enough starts one; one played to its end stops.
    pub(super) fn sync_motion(&mut self) {
        let current = self.current_path().filter(|_| self.photo_up()).map(Path::to_path_buf);
        if self.motion.as_ref().map(|m| &m.photo) != current.as_ref() {
            self.stop_motion();
            self.motion_hold = None;
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
                self.invalidate(); // the badge
            }
        }
        // Held still long enough on the photo: play.
        if let Some(at) = self.motion_hold {
            if self.pressed != Some(Hit::Viewport) {
                self.motion_hold = None;
            } else if at.elapsed() >= HOLD && self.motion_ready() && !self.motion_playing() {
                self.motion_hold = None;
                self.start_motion(true);
            }
        }
        // Played through: the still again.
        let done = self.video.as_ref().filter(|v| v.motion).and_then(|v| v.player.as_ref()).is_some_and(|p| p.info().is_some() && p.at_end());
        if done {
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

    /// Still looking for a motion part, waiting out a hold, or a motion video up (its end, which the player itself
    /// no longer animates, brings the still back): draw again next vsync.
    pub(super) fn motion_animating(&self) -> bool {
        self.motion.as_ref().is_some_and(|m| !m.looked) || self.motion_hold.is_some() || self.motion_playing()
    }

    /// Plays the motion part from its start. `held`: while the button is down (it stops when it comes up).
    pub(super) fn start_motion(&mut self, held: bool) {
        let Some(video) = self.motion.as_ref().and_then(|m| m.video.clone()) else { return };
        self.stop_motion();
        self.motion_held = held;
        self.video = Some(video::VideoView::for_motion(video));
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

    /// The left button down on the photo: a hold may follow.
    pub(super) fn motion_press(&mut self) {
        if self.motion_ready() && !self.motion_playing() {
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

    /// The Motion badge in the photo's top-left corner (inside the photo, below the toolbar): a ring and a dot, and
    /// the word; a click plays the video once (again: stops it).
    pub(super) fn draw_motion_badge(&mut self, g: &Gfx) {
        if !self.motion_ready() {
            return;
        }
        let r = self.view.frame();
        let a = self.image_area();
        let top_min = if self.chrome() { Self::toolbar_y() + BUTTON_H + 8.0 } else { a.top + 12.0 };
        let left = (a.left + r.x as f32 + 12.0).max(a.left + 12.0);
        let top = (a.top + r.y as f32 + 12.0).max(top_min);
        let label = wide("Motion");
        let tw = g.measure(&label, &g.fonts.caption).ceil();
        let b = rect(g.snap(left), g.snap(top), 12.0 + 14.0 + 6.0 + tw + 12.0, 28.0);
        if b.right > a.right - 12.0 || b.bottom > a.bottom - 12.0 {
            return; // a photo too small for it
        }
        self.hits.add(Hit::MotionBadge, b);
        let playing = self.motion_playing();
        let raised = theme::raised();
        let over = self.hover == Some(Hit::MotionBadge);
        g.fill_round(b, 14.0, gfx::rgba(raised.popup, if over { 0.95 } else { 0.8 }));
        let fg = if playing { rgb(theme::t().accent) } else { rgb(raised.text) };
        // The mark: a ring around a dot.
        let (cx, cy) = (b.left + 12.0 + 7.0, (b.top + b.bottom) / 2.0);
        ui::stroke(g, rect(cx - 6.5, cy - 6.5, 13.0, 13.0), 6.5, fg);
        g.fill_round(rect(cx - 2.5, cy - 2.5, 5.0, 5.0), 2.5, fg);
        g.text(&label, &g.fonts.caption, rect(cx + 7.0 + 6.0, b.top, tw + 2.0, b.bottom - b.top), fg, Align::Left);
    }

    /// A click on the badge: plays the video through, or stops it.
    pub(super) fn motion_badge_click(&mut self) {
        if self.motion_playing() {
            self.stop_motion();
        } else {
            self.start_motion(false);
        }
    }
}
