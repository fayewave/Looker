//! Clean view: a single click on a photo fades the UI out (title bar, toolbar, status row, the gradients behind
//! them, the page and Live Photo pills) and slides the cards and the strip away, leaving the photo alone; it still
//! zooms, pans and steps. Another single click brings it all back (Faye, 2026-11-13). The click waits out the
//! double-click time, so a double-click still zooms without the UI blinking.

use std::time::Instant;

use super::*;

pub(super) const TIMER_CLEAN: usize = 16;

impl App {
    /// What clean view applies to: a still photo (or a PDF) on screen, no Settings over it. A video's click plays
    /// and pauses, a model's drag orbits, and the text, archive and audio views are UI themselves.
    fn clean_possible(&self) -> bool {
        self.photo_up()
            && !self.video_shown()
            && !self.model_active()
            && !self.text_shown()
            && !self.archive_shown()
            && !self.audio_shown()
            && !self.page_open()
    }

    /// The UI is (going) away.
    pub(super) fn clean(&self) -> bool {
        self.clean_view && self.clean_possible()
    }

    /// The UI's opacity this frame (1 shown, 0 gone), easing towards where clean view says. Leaving what it
    /// applies to (a video, Settings) brings the UI back for good.
    pub(super) fn step_clean(&mut self) -> f32 {
        if self.clean_view && !self.clean_possible() {
            self.clean_view = false;
        }
        let now = Instant::now();
        self.clean_fade.set([if self.clean() { 0.0 } else { 1.0 }], now);
        let moving = self.clean_fade.moving(now);
        let landed = self.clean_fade.settle(now);
        self.clean_k = self.clean_fade.value(now)[0];
        // The photo's room follows the bars every frame of the fade, and once more where it lands.
        if moving || landed {
            self.layout_changed();
        }
        self.clean_k
    }

    pub(super) fn clean_moving(&self) -> bool {
        self.clean_fade.moving(Instant::now())
    }

    /// A single click on the photo (not a pan): toggles once the double-click time has passed without a second.
    pub(super) fn clean_click(&mut self) {
        if !self.clean_possible() {
            return;
        }
        unsafe {
            SetTimer(Some(self.hwnd), TIMER_CLEAN, GetDoubleClickTime(), None);
        }
    }

    /// A double-click: the first click's toggle is called off.
    pub(super) fn clean_cancel(&mut self) {
        unsafe {
            let _ = KillTimer(Some(self.hwnd), TIMER_CLEAN);
        }
    }

    pub(super) fn on_clean_timer(&mut self) {
        self.clean_cancel();
        if !self.clean_possible() {
            return;
        }
        self.set_clean(!self.clean_view);
    }

    /// Into or out of clean view: the UI fades, the panels slide as their toggles would.
    pub(super) fn set_clean(&mut self, on: bool) {
        if self.clean_view == on {
            return;
        }
        self.clean_view = on;
        if on {
            self.hide_tooltip();
        }
        self.animate_panels();
    }
}
