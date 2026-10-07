//! The slideshow (F5): fullscreen (when not already), a step every few seconds with a 300 ms dissolve from
//! the outgoing image, Space to pause, Escape or F5 to end. A manual step restarts the dwell so it stays
//! fair, and is an ordinary hard cut.

use std::time::Instant;

use super::*;

pub(super) const TIMER_SLIDESHOW: usize = 11;
const FADE_MS: f32 = 300.0;

#[derive(Default)]
pub(super) struct Slideshow {
    pub running: bool,
    pub paused: bool,
    /// It went fullscreen itself, so it leaves fullscreen when it ends.
    entered_fullscreen: bool,
    /// The step in progress is the slideshow's own (it dissolves, and doesn't restart the dwell).
    pub(super) stepping: bool,
    /// The next image swap dissolves (set by the slideshow's step, consumed by the swap).
    fade_armed: bool,
}

/// The outgoing image, drawn over the incoming one at a falling opacity.
pub(super) struct Fade {
    bitmap: ID2D1Bitmap1,
    rect: D2D_RECT_F,
    start: Instant,
}

impl App {
    pub(super) fn toggle_slideshow(&mut self) {
        if self.slideshow.running { self.stop_slideshow() } else { self.start_slideshow() }
    }

    fn start_slideshow(&mut self) {
        if self.viewer.current.is_none() || self.page.is_some() {
            return;
        }
        if self.fullscreen.is_none() {
            self.toggle_fullscreen();
            self.slideshow.entered_fullscreen = true;
        }
        self.slideshow.running = true;
        self.slideshow.paused = false;
        self.slide_panels(); // the cards and the strip step aside for the photos
        self.arm_slideshow();
        self.show_toast("Slideshow", false);
    }

    pub(super) fn stop_slideshow(&mut self) {
        if !self.slideshow.running {
            return;
        }
        unsafe {
            let _ = KillTimer(Some(self.hwnd), TIMER_SLIDESHOW);
        }
        let leave = self.slideshow.entered_fullscreen && self.fullscreen.is_some();
        self.slideshow = Slideshow::default();
        if leave {
            self.toggle_fullscreen();
        }
        self.slide_panels();
        self.show_toast("Slideshow ended", false);
    }

    /// The window left fullscreen by other means (F11): ending the slideshow later mustn't toggle it back.
    pub(super) fn fullscreen_left(&mut self) {
        self.slideshow.entered_fullscreen = false;
    }

    /// (Re)starts the dwell from now.
    fn arm_slideshow(&self) {
        let ms = self.settings.slideshow_seconds.clamp(1, 120) * 1000;
        unsafe {
            SetTimer(Some(self.hwnd), TIMER_SLIDESHOW, ms, None);
        }
    }

    pub(super) fn on_slideshow_timer(&mut self) {
        if !self.slideshow.running || self.slideshow.paused {
            return;
        }
        self.slideshow.stepping = true;
        self.step(1);
        self.slideshow.stepping = false;
    }

    /// Space: pause or resume a running slideshow. False when none is running.
    pub(super) fn slideshow_play_pause(&mut self) -> bool {
        if !self.slideshow.running {
            return false;
        }
        self.slideshow.paused = !self.slideshow.paused;
        if self.slideshow.paused {
            unsafe {
                let _ = KillTimer(Some(self.hwnd), TIMER_SLIDESHOW);
            }
            self.show_toast("Paused", false);
        } else {
            self.arm_slideshow();
            self.show_toast("Playing", false);
        }
        true
    }

    /// Every navigation: the slideshow's own step arms the dissolve; a manual one restarts the dwell and
    /// cuts.
    pub(super) fn slideshow_navigating(&mut self) {
        if self.slideshow.stepping {
            self.slideshow.fade_armed = true;
            return;
        }
        self.slideshow.fade_armed = false;
        if self.slideshow.running && !self.slideshow.paused {
            self.arm_slideshow();
        }
    }

    /// A new image is on screen: dissolve from what was drawn last if the swap was the slideshow's, else drop
    /// any dissolve still running (a ghost must never linger over a hard cut).
    pub(super) fn image_swapped(&mut self) {
        self.fade = if std::mem::take(&mut self.slideshow.fade_armed) {
            self.last_drawn.take().map(|(bitmap, rect)| Fade { bitmap, rect, start: Instant::now() })
        } else {
            None
        };
    }

    /// Draws the outgoing image over the incoming one. True while the dissolve is running.
    pub(super) fn draw_fade(&mut self, g: &Gfx, clip: D2D_RECT_F) -> bool {
        let Some(f) = &self.fade else { return false };
        let t = f.start.elapsed().as_secs_f32() * 1000.0 / FADE_MS;
        if t >= 1.0 {
            self.fade = None;
            return false;
        }
        g.push_clip(clip);
        g.draw_bitmap(&f.bitmap, f.rect, 1.0 - t);
        g.pop_clip();
        true
    }
}
