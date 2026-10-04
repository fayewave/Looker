//! Fit / zoom / pan state for one image, ported from `Rendering/ZoomPanController.cs`, plus a short eased
//! animation between the settled ("target") view and what is drawn.
//!
//! Units: viewport coordinates are DIPs; content is the oriented native image in pixels; `scale` is DIPs per
//! image pixel, so 100% (one image pixel per display pixel) is `1 / rasterization`.

use std::time::Instant;

const MIN_SCALE_FACTOR: f64 = 0.5;
const MAX_SCALE_FACTOR: f64 = 8.0;
const MIN_VISIBLE_FRACTION: f64 = 0.2;
const ANIM_MS: f64 = 160.0;

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Mode {
    Fit,
    Actual,
    Free,
}

#[derive(Clone, Copy, PartialEq, Debug, Default)]
struct Pose {
    scale: f64,
    x: f64,
    y: f64,
}

pub struct View {
    vw: f64,
    vh: f64,
    cw: f64,
    ch: f64,
    raster: f64,
    pub mode: Mode,
    target: Pose,
    from: Pose,
    anim_start: Option<Instant>,
    /// The part of the content that fit and centre refer to instead of all of it: a PDF is one wide strip of
    /// pages, and fitting means fitting the current page. Pan and clamp still use the whole content, so the
    /// neighbouring pages stay reachable. `None` = the whole content (every image).
    focus: Option<Rect>,
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

fn clamp(v: f64, lo: f64, hi: f64) -> f64 {
    if v < lo { lo } else if v > hi { hi } else { v }
}

fn nearly(a: f64, b: f64) -> bool {
    (a - b).abs() <= 1e-6 * 1f64.max(a.abs().max(b.abs()))
}

fn ease_out_cubic(t: f64) -> f64 {
    1.0 - (1.0 - t).powi(3)
}

impl View {
    pub fn new() -> View {
        View {
            vw: 0.0,
            vh: 0.0,
            cw: 0.0,
            ch: 0.0,
            raster: 1.0,
            mode: Mode::Fit,
            target: Pose { scale: 1.0, x: 0.0, y: 0.0 },
            from: Pose::default(),
            anim_start: None,
            focus: None,
        }
    }

    pub fn has_content(&self) -> bool {
        self.cw > 0.0 && self.ch > 0.0 && self.vw > 0.0 && self.vh > 0.0
    }

    fn focus_rect(&self) -> Rect {
        match self.focus {
            Some(f) if f.w > 0.0 && f.h > 0.0 => f,
            _ => Rect { x: 0.0, y: 0.0, w: self.cw, h: self.ch },
        }
    }

    /// Fits the focus (one page), else the whole image.
    pub fn fit_scale(&self) -> f64 {
        let f = self.focus_rect();
        if self.has_content() { (self.vw / f.w).min(self.vh / f.h) } else { 1.0 }
    }
    pub fn actual_scale(&self) -> f64 {
        1.0 / self.raster
    }
    /// Shrink to fit, but never blow a small image up past 100%.
    pub fn default_scale(&self) -> f64 {
        self.fit_scale().min(self.actual_scale())
    }
    fn min_scale(&self) -> f64 {
        self.fit_scale().min(self.actual_scale()) * MIN_SCALE_FACTOR
    }
    fn max_scale(&self) -> f64 {
        self.fit_scale().max(self.actual_scale()) * MAX_SCALE_FACTOR
    }

    pub fn zoom_percent(&self) -> f64 {
        self.target.scale / self.actual_scale() * 100.0
    }

    /// New image: fit, centred on `focus` (`None` = all of it), no animation.
    pub fn reset(&mut self, vw: f64, vh: f64, cw: f64, ch: f64, raster: f64, focus: Option<Rect>) {
        self.vw = vw;
        self.vh = vh;
        self.cw = cw;
        self.ch = ch;
        self.raster = if raster > 0.0 { raster } else { 1.0 };
        self.focus = focus;
        self.apply_fit();
        self.anim_start = None;
    }

    /// Moves the focus (another PDF page). With `recenter`, Fit mode fits the new focus and any other zoom keeps
    /// its scale and centres on it, so a reader zoomed into the text stays zoomed while turning pages; it
    /// glides there. Without, only the fit and clamp reference moves (the user panned there themselves).
    pub fn set_focus(&mut self, focus: Option<Rect>, recenter: bool) {
        self.focus = focus;
        if !self.has_content() || !recenter {
            return;
        }
        self.begin_anim();
        if self.mode == Mode::Fit {
            self.apply_fit();
        } else {
            let (x, y) = self.centered(self.target.scale);
            self.target.x = x;
            self.target.y = y;
            self.clamp_offset();
        }
    }

    /// The content x under the middle of the view, once settled (which page the view is on).
    pub fn center_content_x(&self) -> f64 {
        (self.vw / 2.0 - self.target.x) / self.target.scale
    }

    pub fn clear(&mut self) {
        self.cw = 0.0;
        self.ch = 0.0;
        self.anim_start = None;
    }

    /// Viewport resized or DPI changed: refit in Fit mode, else keep the zoom and re-clamp. No animation.
    pub fn set_viewport(&mut self, vw: f64, vh: f64, raster: f64) {
        self.vw = vw;
        self.vh = vh;
        self.raster = if raster > 0.0 { raster } else { 1.0 };
        if self.mode == Mode::Fit {
            self.apply_fit();
        } else {
            self.clamp_offset();
        }
        self.anim_start = None;
    }

    fn begin_anim(&mut self) {
        self.from = self.current_pose();
        self.anim_start = Some(Instant::now());
    }

    pub fn fit(&mut self) {
        if !self.has_content() {
            return;
        }
        self.begin_anim();
        self.apply_fit();
    }

    pub fn actual_size_at(&mut self, ax: f64, ay: f64) {
        if !self.has_content() {
            return;
        }
        self.begin_anim();
        self.set_scale_at(self.actual_scale(), ax, ay);
        self.mode = Mode::Actual;
    }

    pub fn zoom_at(&mut self, factor: f64, ax: f64, ay: f64) {
        if !self.has_content() {
            return;
        }
        self.begin_anim();
        self.set_scale_at(self.target.scale * factor, ax, ay);
        let s = self.target.scale;
        self.mode = if nearly(s, self.default_scale()) {
            Mode::Fit
        } else if nearly(s, self.actual_scale()) {
            Mode::Actual
        } else {
            Mode::Free
        };
    }

    pub fn zoom_center(&mut self, factor: f64) {
        self.zoom_at(factor, self.vw / 2.0, self.vh / 2.0);
    }

    pub fn toggle_fit_actual(&mut self, ax: f64, ay: f64) {
        if self.at_fit_position() { self.actual_size_at(ax, ay) } else { self.fit() }
    }

    /// Drag: moves immediately (an animation would make the image lag the pointer).
    pub fn pan(&mut self, dx: f64, dy: f64) {
        if !self.has_content() {
            return;
        }
        self.target = self.current_pose();
        self.anim_start = None;
        self.target.x += dx;
        self.target.y += dy;
        self.clamp_offset();
    }

    pub fn at_fit_position(&self) -> bool {
        if !self.has_content() || !nearly(self.target.scale, self.default_scale()) {
            return false;
        }
        let (x, y) = self.centered(self.target.scale);
        (self.target.x - x).abs() < 0.5 && (self.target.y - y).abs() < 0.5
    }

    pub fn animating(&self) -> bool {
        self.anim_start.is_some()
    }

    /// What to draw this frame. Ends the animation once it has run its course.
    pub fn frame(&mut self) -> Rect {
        let p = self.current_pose();
        if let Some(t0) = self.anim_start {
            if t0.elapsed().as_secs_f64() * 1000.0 >= ANIM_MS {
                self.anim_start = None;
            }
        }
        Rect { x: p.x, y: p.y, w: self.cw * p.scale, h: self.ch * p.scale }
    }

    /// The settled rect (for deciding which resolution to decode).
    pub fn target_rect(&self) -> Rect {
        let p = self.target;
        Rect { x: p.x, y: p.y, w: self.cw * p.scale, h: self.ch * p.scale }
    }

    fn current_pose(&self) -> Pose {
        let Some(t0) = self.anim_start else { return self.target };
        let t = (t0.elapsed().as_secs_f64() * 1000.0 / ANIM_MS).min(1.0);
        let k = ease_out_cubic(t);
        // Interpolate the scale geometrically and the offsets so the anchored point stays put mid-flight.
        let (a, b) = (self.from, self.target);
        let scale = a.scale * (b.scale / a.scale).powf(k);
        let u = if (b.scale - a.scale).abs() > 1e-9 { (scale - a.scale) / (b.scale - a.scale) } else { k };
        Pose { scale, x: a.x + (b.x - a.x) * u, y: a.y + (b.y - a.y) * u }
    }

    fn apply_fit(&mut self) {
        self.mode = Mode::Fit;
        let s = if self.has_content() { self.default_scale() } else { 1.0 };
        let (x, y) = self.centered(s);
        self.target = Pose { scale: s, x, y };
    }

    fn centered(&self, s: f64) -> (f64, f64) {
        let f = self.focus_rect();
        ((self.vw - f.w * s) / 2.0 - f.x * s, (self.vh - f.h * s) / 2.0 - f.y * s)
    }

    fn set_scale_at(&mut self, new_scale: f64, ax: f64, ay: f64) {
        let new_scale = clamp(new_scale, self.min_scale(), self.max_scale());
        let t = self.target;
        let ix = (ax - t.x) / t.scale;
        let iy = (ay - t.y) / t.scale;
        self.target = Pose { scale: new_scale, x: ax - ix * new_scale, y: ay - iy * new_scale };
        self.clamp_offset();
    }

    fn clamp_offset(&mut self) {
        let s = self.target.scale;
        // "Keep 20% on screen" is measured against the focus (one page): against a 100-page strip it would be
        // 20 pages, which forbids the very fit-to-page positions a document opens at.
        let f = self.focus_rect();
        self.target.x = clamp_axis(self.target.x, self.cw * s, self.vw, self.cw.min(f.w) * s);
        self.target.y = clamp_axis(self.target.y, self.ch * s, self.vh, self.ch.min(f.h) * s);
    }
}

fn clamp_axis(offset: f64, content: f64, viewport: f64, keep_extent: f64) -> f64 {
    if content <= viewport {
        return (viewport - content) / 2.0;
    }
    let keep = keep_extent * MIN_VISIBLE_FRACTION;
    clamp(offset, keep - content, viewport - keep)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn small_image_opens_at_actual_size() {
        let mut v = View::new();
        v.reset(1000.0, 800.0, 200.0, 100.0, 1.5, None);
        assert!((v.target_rect().w - 200.0 / 1.5).abs() < 1e-9);
        assert!(v.at_fit_position());
    }

    #[test]
    fn large_image_fits_and_centres() {
        let mut v = View::new();
        v.reset(1000.0, 800.0, 4000.0, 2000.0, 1.0, None);
        let r = v.target_rect();
        assert!((r.w - 1000.0).abs() < 1e-9 && (r.h - 500.0).abs() < 1e-9);
        assert!((r.y - 150.0).abs() < 1e-9);
    }

    #[test]
    fn zoom_keeps_anchor_fixed() {
        let mut v = View::new();
        v.reset(1000.0, 800.0, 4000.0, 2000.0, 1.0, None);
        let before = v.target_rect();
        let (ax, ay) = (600.0, 400.0);
        let ix = (ax - before.x) / before.w;
        v.zoom_at(2.0, ax, ay);
        let after = v.target_rect();
        assert!(((ax - after.x) / after.w - ix).abs() < 1e-9);
        assert_eq!(v.mode, Mode::Free);
    }

    #[test]
    fn a_page_strip_fits_and_turns_by_its_focus() {
        // Three Letter pages side by side; the view fits page 1 (at 100%: the viewport is big enough).
        let l = crate::pages::Layout::compute(&[(816.0, 1056.0); 3]);
        let mut v = View::new();
        v.reset(1600.0, 1200.0, l.width, l.height, 1.0, Some(l.pages[0]));
        let r = v.target_rect();
        assert!((r.x - (1600.0 - 816.0) / 2.0).abs() < 1e-9, "page 1 centred, not the strip");
        assert!(v.at_fit_position());
        assert_eq!(l.page_at(v.center_content_x()), 0);
        v.set_focus(Some(l.pages[2]), true);
        assert_eq!(l.page_at(v.center_content_x()), 2);
        assert!(v.at_fit_position());
        // Panned onto page 2 by hand: the focus follows without moving the view, and the next double-click
        // fits that page instead of jumping to 100%. (The page turn glides; a pan starts from where the
        // glide is, so let it land first.)
        v.anim_start = None;
        v.pan(848.0, 0.0);
        let x = v.target_rect().x;
        assert_eq!(l.page_at(v.center_content_x()), 1);
        v.set_focus(Some(l.pages[1]), false);
        assert_eq!(v.target_rect().x, x);
        assert!(v.at_fit_position());
    }

    #[test]
    fn double_click_toggles() {
        let mut v = View::new();
        v.reset(1000.0, 800.0, 4000.0, 2000.0, 1.0, None);
        v.toggle_fit_actual(500.0, 400.0);
        assert_eq!(v.mode, Mode::Actual);
        v.toggle_fit_actual(500.0, 400.0);
        assert!(v.at_fit_position());
    }
}
