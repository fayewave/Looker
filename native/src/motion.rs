//! The one motion rule, as Snapper has it: ease-out quart over 190 ms, for everything (panels sliding, hover
//! washes fading out, switches, menus and cards opening). Retargeting mid-flight starts a new curve from
//! wherever the value is, and a curve begins one frame in, so the first frame drawn already moves. With
//! Windows' "Animation effects" off, everything jumps to its end.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use windows::Win32::UI::WindowsAndMessaging::{SPI_GETCLIENTAREAANIMATION, SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS, SystemParametersInfoW};
use windows::core::BOOL;

/// Seconds.
pub const EASE: f32 = 0.19;
const FRAME: f32 = 1.0 / 60.0;

static ENABLED: AtomicBool = AtomicBool::new(true);

pub fn ease_out(t: f32) -> f32 {
    1.0 - (1.0 - t.clamp(0.0, 1.0)).powi(4)
}

/// Reads Windows' "Animation effects" (once per frame is plenty: the window calls this before drawing).
pub fn refresh() {
    let mut on = BOOL(1);
    unsafe {
        let _ = SystemParametersInfoW(SPI_GETCLIENTAREAANIMATION, 0, Some(&mut on as *mut _ as *mut _), SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0));
    }
    ENABLED.store(on.as_bool(), Ordering::Relaxed);
}

/// False when Windows' "Animation effects" is off.
pub fn enabled() -> bool {
    ENABLED.load(Ordering::Relaxed)
}

/// How far (0..1, eased) something that started at `since` has come.
pub fn progress(since: Instant) -> f32 {
    if !enabled() {
        return 1.0;
    }
    ease_out((since.elapsed().as_secs_f32() + FRAME) / EASE)
}

/// An eased group of N values (a rect is 4).
#[derive(Clone, Copy, Debug)]
pub struct Ease<const N: usize> {
    from: [f32; N],
    to: [f32; N],
    start: Option<Instant>,
}

impl<const N: usize> Ease<N> {
    pub fn at(v: [f32; N]) -> Self {
        Ease { from: v, to: v, start: None }
    }

    pub fn value(&self, now: Instant) -> [f32; N] {
        let Some(start) = self.start else { return self.to };
        let k = ease_out(now.saturating_duration_since(start).as_secs_f32() / EASE);
        std::array::from_fn(|i| self.from[i] + (self.to[i] - self.from[i]) * k)
    }

    pub fn target(&self) -> [f32; N] {
        self.to
    }

    /// Moves towards `v` from the current value (jumps when animation effects are off).
    pub fn set(&mut self, v: [f32; N], now: Instant) {
        if v == self.to {
            return;
        }
        if !enabled() {
            *self = Ease::at(v);
            return;
        }
        self.from = self.value(now);
        self.to = v;
        self.start = Some(now - Duration::from_secs_f32(FRAME));
    }

    pub fn moving(&self, now: Instant) -> bool {
        self.start.is_some_and(|s| now.saturating_duration_since(s).as_secs_f32() < EASE)
    }

    /// True once, on the first call after the curve has run its course.
    pub fn settle(&mut self, now: Instant) -> bool {
        if self.start.is_some() && !self.moving(now) {
            self.start = None;
            return true;
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn eases_to_its_target() {
        let t0 = Instant::now();
        let mut e = Ease::at([0.0]);
        e.set([1.0], t0);
        let mid = e.value(t0 + Duration::from_millis(60))[0];
        assert!(mid > 0.3 && mid < 1.0, "{mid}");
        assert_eq!(e.value(t0 + Duration::from_millis(400))[0], 1.0);
        assert!(!e.moving(t0 + Duration::from_millis(400)));
    }
}
