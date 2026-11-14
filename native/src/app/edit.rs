//! Basic editing (Faye, 2026-11-13): crop, straighten and exposure. The Edit button (or E) turns the viewer into
//! edit mode: the photo alone, fitted with room for its crop frame, and a panel at the right with the crop's
//! aspect, the Straighten and Exposure sliders, and Reset, Cancel, Save and Save a copy. Nothing touches the file
//! until a save: Save a copy writes "name (edited).ext" beside it (the default: Enter), Save writes over it with
//! the original moved to the Recycle Bin first. The pixels are photoedit.rs's, at full size on a thread; the
//! preview is Direct2D's (`Gfx::draw_photo_edit`), the same sums.
//!
//! The crop is kept upright on screen, in pixels from the image's middle after its quarter turns and the
//! straightening (`photoedit::Params::crop`), and always on the image.
//!
//! Round 3 (Faye 2026-11-14): Vibrance, a tone curve per channel (the panel's Curve: click the line to add a
//! point, drag points, double-click one to take it out; Tab to it, then Left/Right pick a point, Up/Down move it,
//! Delete takes it out), and Copy / Paste edits (the head's buttons, Ctrl+Shift+C / V): the tone and colour, not
//! the crop or turns. Saving also copies, so a photo just edited can be pasted onto others from the explorer's row
//! menu (`App::paste_edits`, app/batchedit.rs) or the viewer's. As Lightroom's (Faye, 2026-11-14): the
//! frame holds still in the middle of the area, as big as fits, and the photo moves under it: a drag inside it
//! pans the photo with the pointer, a drag outside it turns the photo about the frame's middle (the straightening;
//! the frame shrinks to stay on the photo, the photo zooming to keep it the same size), and its edges and corners
//! resize it with the photo still, the frame gliding back to the middle when let go.

use std::path::PathBuf;
use std::time::Instant;

use super::*;
use crate::photoedit::{self, Params};

/// The panel's width, and the gap around it and the photo.
const PANEL_W: f32 = 300.0;
const GAP: f32 = 12.0;
const PAD: f32 = 20.0;
const SLIDER_H: f32 = 28.0;
/// The crop's corner handles' reach, its edges' band, and its smallest side in image pixels.
const CORNER: f32 = 26.0;
const EDGE: f32 = 14.0;
const MIN_SIDE: f32 = 16.0;
/// The room around the frame in the area, for its handles and to turn the photo in (Faye, 2026-11-14: was 24).
const FRAME_MARGIN: f32 = 56.0;
pub(super) const STRAIGHTEN_MAX: f32 = 45.0;
pub(super) const EXPOSURE_MAX: f32 = 3.0;

/// The aspect choices, by place: Free, Original, Square, 4:3, 3:2, 16:9.
const ASPECTS: [&str; 6] = ["Free", "Original", "Square", "4:3", "3:2", "16:9"];
const ORIGINAL: u8 = 1;
/// The buttons under the aspects, by place (`Hit::EditTool`).
const TOOLS: [&str; 4] = ["Rotate left", "Rotate right", "Flip horizontal", "Flip vertical"];

/// The sliders, by place (round 2, Faye 2026-11-14): name, reach either side of 0, the arrow keys' step and
/// Shift's. 0 is `Hit::EditStraighten`, 1 `Hit::EditExposure`, the rest `Hit::EditAdjust(place - 2)`.
const SLIDERS: [(&str, f32, f32, f32); 9] = [
    ("Straighten", STRAIGHTEN_MAX, 0.1, 1.0),
    ("Exposure", EXPOSURE_MAX, 0.1, 0.5),
    ("Contrast", 100.0, 1.0, 10.0),
    ("Highlights", 100.0, 1.0, 10.0),
    ("Shadows", 100.0, 1.0, 10.0),
    ("Temperature", 100.0, 1.0, 10.0),
    ("Tint", 100.0, 1.0, 10.0),
    ("Vibrance", 100.0, 1.0, 10.0),
    ("Saturation", 100.0, 1.0, 10.0),
];

/// The tone curve's channels, by place (`Hit::EditCurveChannel`; `Params::curves` in the same order), and their
/// lines' colours.
const CURVE_CHANNELS: [&str; 4] = ["RGB", "Red", "Green", "Blue"];
const CURVE_COLOURS: [u32; 3] = [0xE5484D, 0x46A758, 0x3E8EED];

/// Undo groups: changes of one kind in a row (the wheel's notches, a slider's key presses) make one step.
const GROUP_WHEEL: u8 = 1;
const GROUP_NUDGE: u8 = 2;
const GROUP_ASPECT: u8 = 3;
const GROUP_BUTTON: u8 = 4;
const GROUP_CURVE: u8 = 5;
const GROUP_SLIDER: u8 = 16;

fn slider_hit(s: usize) -> Hit {
    match s {
        0 => Hit::EditStraighten,
        1 => Hit::EditExposure,
        _ => Hit::EditAdjust(s as u8 - 2),
    }
}

pub(super) fn slider_index(h: Hit) -> Option<usize> {
    match h {
        Hit::EditStraighten => Some(0),
        Hit::EditExposure => Some(1),
        Hit::EditAdjust(i) if (i as usize) < SLIDERS.len() - 2 => Some(i as usize + 2),
        _ => None,
    }
}

fn adjust_mut(p: &mut Params, i: usize) -> &mut f32 {
    match i {
        0 => &mut p.contrast,
        1 => &mut p.highlights,
        2 => &mut p.shadows,
        3 => &mut p.temp,
        4 => &mut p.tint,
        5 => &mut p.vibrance,
        _ => &mut p.saturation,
    }
}

pub(super) fn slider_get(p: &Params, s: usize) -> f32 {
    match s {
        0 => p.angle,
        1 => p.ev,
        _ => *adjust_mut(&mut p.clone(), s - 2),
    }
}

/// A slider's name and value, for the panel and readers.
pub(super) fn slider_name(s: usize) -> &'static str {
    SLIDERS.get(s).map_or("", |s| s.0)
}

pub(super) fn slider_text(s: usize, v: f32) -> String {
    match s {
        0 => degrees_text(v),
        1 => ev_text(v),
        _ if v.abs() < 0.5 => "0".into(),
        _ => format!("{v:+.0}"),
    }
}

/// What undo puts back: the edit and the crop's settings (and the image's size, which a quarter turn swaps).
#[derive(Clone, Copy, PartialEq)]
struct Snap {
    p: Params,
    aspect: u8,
    portrait: bool,
    auto: bool,
    w: f32,
    h: f32,
}

pub(super) struct Edit {
    pub path: PathBuf,
    /// The image's size after its quarter turns, in pixels.
    w: f32,
    h: f32,
    pub p: Params,
    pub aspect: u8,
    /// The fixed aspects stand taller than wide.
    pub portrait: bool,
    drag: Option<Drag>,
    /// The crop hasn't been moved by hand: straightening keeps it the largest that fits.
    auto: bool,
    /// The keyboard's place in the panel (Tab), and whether to ring it.
    pub focus: Option<Hit>,
    pub focus_visible: bool,
    pub saving: bool,
    /// Where the last frame put the image: its middle on screen and DIPs per image pixel.
    centre: (f32, f32),
    k: f32,
    /// The view: DIPs per image pixel and the image's middle on screen, eased only when the frame glides back to
    /// the middle (after a resize, a new aspect); a pan or a turn follows the pointer at once.
    view: crate::motion::Ease<3>,
    placed: bool,
    glide: bool,
    /// The wheel's zoom eases the crop (`zooming` while it does), the view following it frame by frame so the
    /// frame holds still and the photo zooms smoothly under it.
    zoom: crate::motion::Ease<4>,
    zooming: bool,
    /// Undo and redo (`record` makes the steps), where the edit began (Reset), the state when the press now held
    /// began, and the last step's group and time (`GROUP_*`).
    undo: Vec<Snap>,
    redo: Vec<Snap>,
    start: Snap,
    press: Option<Snap>,
    group: Option<(u8, Instant)>,
    /// The photo as it was (tone and colour) while the Before button or \ is held.
    pub before: bool,
    before_key: bool,
    /// The panel's middle scrolled down by `scroll` DIPs (at most `scroll_max`, last frame's); `reveal` brings the
    /// keyboard's control into view on the next frame.
    scroll: f32,
    scroll_max: f32,
    reveal: bool,
    /// The curve shown (0 RGB, 1-3 red, green, blue), its point picked (the keys move it), the point being dragged,
    /// and where last frame drew the graph (whole, even when scrolled partly out of view).
    pub curve_ch: u8,
    pub curve_sel: Option<usize>,
    curve_drag: Option<usize>,
    curve_rect: D2D_RECT_F,
}

#[derive(Clone, Copy)]
struct Drag {
    hit: Hit,
    from: (f32, f32),
    crop: [f32; 4],
    angle: f32,
}

impl Edit {
    /// The fixed aspects' width over height: landscape, or turned for portrait (Square is square either way).
    fn ratio(&self) -> Option<f32> {
        let r = match self.aspect {
            1 => (self.w / self.h).max(self.h / self.w),
            2 => 1.0,
            3 => 4.0 / 3.0,
            4 => 1.5,
            5 => 16.0 / 9.0,
            _ => return None,
        };
        Some(if self.portrait { 1.0 / r } else { r })
    }

    /// Turns the photo to `angle` about the middle of the crop (`from_crop` at `from_angle`): the crop keeps its
    /// size and place on screen, shrinking about its own middle only as far as it must to stay on the photo.
    fn turn(&mut self, angle: f32, from_crop: [f32; 4], from_angle: f32) {
        self.p.angle = angle;
        let (s, c) = (angle - from_angle).to_radians().sin_cos();
        let (mx, my) = ((from_crop[0] + from_crop[2]) / 2.0, (from_crop[1] + from_crop[3]) / 2.0);
        let (hw, hh) = ((from_crop[2] - from_crop[0]) / 2.0, (from_crop[3] - from_crop[1]) / 2.0);
        // The image turns about the crop's middle: seen from the image's middle, the crop's middle turns with it.
        let (nx, ny) = (c * mx - s * my, s * mx + c * my);
        let at = |m: f32| [nx - hw * m, ny - hh * m, nx + hw * m, ny + hh * m];
        if photoedit::crop_fits(self.w, self.h, angle, at(1.0)) {
            self.p.crop = at(1.0);
            return;
        }
        if photoedit::on_image(self.w, self.h, angle, nx, ny) {
            let (mut lo, mut hi) = (0.0f32, 1.0f32);
            for _ in 0..30 {
                let m = (lo + hi) / 2.0;
                if photoedit::crop_fits(self.w, self.h, angle, at(m)) {
                    lo = m;
                } else {
                    hi = m;
                }
            }
            if lo > 0.0 {
                self.p.crop = at(lo);
                return;
            }
        }
        self.p.crop = at(1.0);
        self.refit();
    }

    /// Where the view should be: the crop as big as fits in `area`, in its middle.
    fn target(&self, area: D2D_RECT_F) -> [f32; 3] {
        let [l, t, r, b] = self.p.crop;
        let k = ((area.right - area.left) / (r - l)).min((area.bottom - area.top) / (b - t)).max(0.0001);
        let (ax, ay) = ((area.left + area.right) / 2.0, (area.top + area.bottom) / 2.0);
        [k, ax - k * (l + r) / 2.0, ay - k * (t + b) / 2.0]
    }

    /// A resize handle is being dragged: the view holds still meanwhile.
    fn resizing(&self) -> bool {
        self.drag.is_some_and(|d| matches!(d.hit, Hit::EditCrop(1..=8)))
    }

    fn full(&self) -> [f32; 4] {
        [-self.w / 2.0, -self.h / 2.0, self.w / 2.0, self.h / 2.0]
    }

    fn fits(&self, c: [f32; 4]) -> bool {
        c[2] - c[0] >= MIN_SIDE.min(self.w) - 0.01 && c[3] - c[1] >= MIN_SIDE.min(self.h) - 0.01 && photoedit::crop_fits(self.w, self.h, self.p.angle, c)
    }

    /// The crop after straightening: kept if it still fits, else drawn in towards the image's middle until it does.
    fn refit(&mut self) {
        let c = self.p.crop;
        if photoedit::crop_fits(self.w, self.h, self.p.angle, c) {
            return;
        }
        let (mut lo, mut hi) = (0.0f32, 1.0f32);
        for _ in 0..30 {
            let m = (lo + hi) / 2.0;
            if photoedit::crop_fits(self.w, self.h, self.p.angle, c.map(|v| v * m)) {
                lo = m;
            } else {
                hi = m;
            }
        }
        self.p.crop = c.map(|v| v * lo);
    }

    /// Sets the crop to the biggest of the aspect chosen (Free: the whole image as straightened).
    fn reset_crop(&mut self) {
        let a = self.ratio().unwrap_or(self.w / self.h);
        self.p.crop = photoedit::largest(self.w, self.h, self.p.angle, a);
    }

    /// Screen point of image point `(x, y)`.
    fn screen(&self, x: f32, y: f32) -> (f32, f32) {
        (self.centre.0 + x * self.k, self.centre.1 + y * self.k)
    }

    fn crop_rect(&self) -> D2D_RECT_F {
        let (l, t) = self.screen(self.p.crop[0], self.p.crop[1]);
        let (r, b) = self.screen(self.p.crop[2], self.p.crop[3]);
        D2D_RECT_F { left: l, top: t, right: r, bottom: b }
    }

    pub fn changed(&self) -> bool {
        !photoedit::is_identity(&self.p, self.w, self.h)
    }

    /// From crop `from` (on the image) as far towards `to` as stays on it.
    fn toward(&self, from: [f32; 4], to: [f32; 4]) -> [f32; 4] {
        if self.fits(to) {
            return to;
        }
        let at = |m: f32| [0, 1, 2, 3].map(|i| from[i] + (to[i] - from[i]) * m);
        let (mut lo, mut hi) = (0.0f32, 1.0f32);
        for _ in 0..24 {
            let m = (lo + hi) / 2.0;
            if self.fits(at(m)) {
                lo = m;
            } else {
                hi = m;
            }
        }
        at(lo)
    }

    /// The wheel (Faye, 2026-11-14): crop `from` zoomed by `f` (> 1 in: the crop shrinks in image pixels, its aspect
    /// kept) about image point `at` (under the pointer) or its middle, as far as the photo allows. Out against the
    /// photo's edge it slides back onto it rather than stopping there: a crop of a given size fits wherever its
    /// middle lies in a rectangle of the photo's own axes (the photo's half-size less the crop's reach along each),
    /// so the middle is clamped into that.
    fn zoomed(&self, from: [f32; 4], f: f32, at: Option<(f32, f32)>) -> [f32; 4] {
        let (hw1, hh1) = ((from[2] - from[0]) / 2.0, (from[3] - from[1]) / 2.0);
        let mid = ((from[0] + from[2]) / 2.0, (from[1] + from[3]) / 2.0);
        let (sn, cs) = self.p.angle.to_radians().sin_cos();
        let (sa, ca) = (sn.abs(), cs.abs());
        // The crop's reach along the photo's x and y at this size.
        let (ex1, ey1) = (ca * hw1 + sa * hh1, sa * hw1 + ca * hh1);
        let most = (self.w / 2.0 / ex1).min(self.h / 2.0 / ey1) * 0.9999;
        let least = (MIN_SIDE.min(self.w) / (2.0 * hw1)).max(MIN_SIDE.min(self.h) / (2.0 * hh1));
        let m = (1.0 / f).min(most).max(least);
        let p = at.unwrap_or(mid);
        let (cx, cy) = (p.0 + (mid.0 - p.0) * m, p.1 + (mid.1 - p.1) * m);
        let (hw, hh) = (hw1 * m, hh1 * m);
        // Into the photo's axes, clamped, and back.
        let (qx, qy) = (cs * cx + sn * cy, -sn * cx + cs * cy);
        let bx = (self.w / 2.0 - (ca * hw + sa * hh)).max(0.0);
        let by = (self.h / 2.0 - (sa * hw + ca * hh)).max(0.0);
        let (qx, qy) = (qx.clamp(-bx, bx), qy.clamp(-by, by));
        let (cx, cy) = (cs * qx - sn * qy, sn * qx + cs * qy);
        [cx - hw, cy - hh, cx + hw, cy + hh]
    }

    /// The edit as it stands (a zoom under way as where it's going).
    fn snap_now(&self) -> Snap {
        let mut p = self.p;
        if self.zooming {
            p.crop = self.zoom.target();
        }
        Snap { p, aspect: self.aspect, portrait: self.portrait, auto: self.auto, w: self.w, h: self.h }
    }

    fn restore(&mut self, s: Snap) {
        self.zooming = false;
        self.drag = None;
        self.p = s.p;
        self.aspect = s.aspect;
        self.portrait = s.portrait;
        self.auto = s.auto;
        self.w = s.w;
        self.h = s.h;
        self.glide = true;
    }

    /// Makes `before` an undo step if the edit has changed since; a change in `group` close after the last one in
    /// it joins that step instead.
    fn record(&mut self, before: Snap, group: Option<u8>) {
        if self.snap_now() == before {
            return;
        }
        let now = Instant::now();
        if let (Some(g), Some((last, at))) = (group, self.group) {
            if g == last && now.duration_since(at).as_millis() < 800 {
                self.group = Some((g, now));
                return;
            }
        }
        self.undo.push(before);
        if self.undo.len() > 200 {
            self.undo.remove(0);
        }
        self.redo.clear();
        self.group = group.map(|g| (g, now));
    }

    /// A quarter turn of the whole photo (`cw` clockwise), the crop turned with it.
    fn quarter(&mut self, cw: bool) {
        let [l, t, r, b] = self.p.crop;
        self.p.crop = if cw { [-b, l, -t, r] } else { [t, -r, b, -l] };
        self.p.turns = (self.p.turns + if cw { 1 } else { 3 }) % 4;
        std::mem::swap(&mut self.w, &mut self.h);
        self.portrait = !self.portrait;
        self.glide = true;
    }

    /// Mirrors the photo as seen, left to right (or top to bottom: `vertical`), the crop and the straightening
    /// with it. As `Params` holds it (a mirror first, then the quarter turns): the turns run the other way after.
    fn mirror(&mut self, vertical: bool) {
        let [l, t, r, b] = self.p.crop;
        self.p.crop = if vertical { [l, -b, r, -t] } else { [-r, t, -l, b] };
        self.p.angle = -self.p.angle;
        self.p.turns = if vertical { (6 - self.p.turns) % 4 } else { (4 - self.p.turns) % 4 };
        self.p.flip = !self.p.flip;
        self.glide = true;
    }

    /// A wheel zoom still easing ends where it was going (before anything else changes the crop).
    fn settle_zoom(&mut self) {
        if self.zooming {
            self.p.crop = self.zoom.target();
            self.zooming = false;
        }
    }

    /// A handle's drag to pointer `(x, y)`: the crop from where the drag began, moved or resized, kept to the
    /// aspect and on the image (as far as it can go towards where the pointer asks).
    fn drag_to(&mut self, x: f32, y: f32) {
        let Some(d) = self.drag else { return };
        let Hit::EditCrop(part) = d.hit else { return };
        let (dx, dy) = ((x - d.from.0) / self.k, (y - d.from.1) / self.k);
        let [l, t, r, b] = d.crop;
        if part == 0 {
            // The photo follows the pointer: the crop moves the other way over it, each way as far as the photo
            // goes (so it slides along an edge instead of sticking to it).
            let along = |me: &Edit, c: [f32; 4], mx: f32, my: f32| -> [f32; 4] {
                let to = [c[0] + mx, c[1] + my, c[2] + mx, c[3] + my];
                if me.fits(to) {
                    return to;
                }
                let (mut lo, mut hi) = (0.0f32, 1.0f32);
                for _ in 0..24 {
                    let m = (lo + hi) / 2.0;
                    if me.fits([c[0] + mx * m, c[1] + my * m, c[2] + mx * m, c[3] + my * m]) {
                        lo = m;
                    } else {
                        hi = m;
                    }
                }
                [c[0] + mx * lo, c[1] + my * lo, c[2] + mx * lo, c[3] + my * lo]
            };
            let c = along(self, d.crop, -dx, 0.0);
            let c = along(self, c, 0.0, -dy);
            if self.fits(c) {
                self.p.crop = c;
                self.auto = false;
            }
            return;
        }
        let mut c = {
            {
                let (mut l, mut t, mut r, mut b) = (l, t, r, b);
                // 1 top left, 2 top, 3 top right, 4 right, 5 bottom right, 6 bottom, 7 bottom left, 8 left.
                if matches!(part, 1 | 7 | 8) {
                    l = (l + dx).min(r - MIN_SIDE);
                }
                if matches!(part, 3 | 4 | 5) {
                    r = (r + dx).max(l + MIN_SIDE);
                }
                if matches!(part, 1 | 2 | 3) {
                    t = (t + dy).min(b - MIN_SIDE);
                }
                if matches!(part, 5 | 6 | 7) {
                    b = (b + dy).max(t + MIN_SIDE);
                }
                if let Some(a) = self.ratio() {
                    let (w, h) = (r - l, b - t);
                    match part {
                        // An edge: the other side follows, about the middle.
                        2 | 6 => {
                            let nw = h * a;
                            let mx = (d.crop[0] + d.crop[2]) / 2.0;
                            l = mx - nw / 2.0;
                            r = mx + nw / 2.0;
                        }
                        4 | 8 => {
                            let nh = w / a;
                            let my = (d.crop[1] + d.crop[3]) / 2.0;
                            t = my - nh / 2.0;
                            b = my + nh / 2.0;
                        }
                        // A corner: whichever way moved further leads; the opposite corner stays.
                        _ => {
                            let (nw, nh) = if w / h > a { (w, w / a) } else { (h * a, h) };
                            if matches!(part, 1 | 7) {
                                l = r - nw;
                            } else {
                                r = l + nw;
                            }
                            if matches!(part, 1 | 3) {
                                t = b - nh;
                            } else {
                                b = t + nh;
                            }
                        }
                    }
                }
                [l, t, r, b]
            }
        };
        if !self.fits(c) {
            // As far from where it was as stays on the image. Free goes each way on its own (Faye, 2026-11-14): a
            // corner pulled past the photo's edge one way still follows the pointer the other way, instead of the
            // whole drag shrinking back towards where it began.
            let from = d.crop;
            c = if self.ratio().is_none() {
                let across = self.toward(from, [c[0], from[1], c[2], from[3]]);
                self.toward(across, [across[0], c[1], across[2], c[3]])
            } else {
                self.toward(from, c)
            };
            if !self.fits(c) {
                return;
            }
        }
        self.p.crop = c;
        self.auto = false;
    }
}

/// Where a slider's value sits on its track.
fn slider_track(r: D2D_RECT_F) -> D2D_RECT_F {
    rect(r.left + 8.0, (r.top + r.bottom) / 2.0 - 2.0, r.right - r.left - 16.0, 4.0)
}

fn slider_value(r: D2D_RECT_F, x: f32, max: f32) -> f32 {
    let t = slider_track(r);
    let f = ((x - t.left) / (t.right - t.left)).clamp(0.0, 1.0);
    (f * 2.0 - 1.0) * max
}

pub(super) fn degrees_text(a: f32) -> String {
    format!("{:.1}\u{00B0}", if a.abs() < 0.05 { 0.0 } else { a })
}

pub(super) fn ev_text(ev: f32) -> String {
    if ev.abs() < 0.05 { "0.0".into() } else { format!("{ev:+.1}") }
}

impl App {
    /// Whether the photo showing can be edited: a still picture Looker decoded (not a page, video, model, text,
    /// archive or animation), not covered, not being saved.
    pub(super) fn can_edit(&self) -> bool {
        self.edit.is_none()
            && self.can_rotate()
            && !self.text_shown()
            && !self.archive_shown()
            && self.viewer.current_entry().is_some_and(|e| e.frames.borrow().len() == 1 && !matches!(e.format, format::Format::Text | format::Format::Archive | format::Format::Pdf))
    }

    /// Edit mode: the panel and the crop frame, the cards and bars put aside.
    pub(super) fn begin_edit(&mut self) {
        if !self.can_edit() {
            return;
        }
        let Some(path) = self.current_path().map(Path::to_path_buf) else { return };
        let Some((w, h)) = self.viewer.current_entry().map(|e| (e.native_w as f32, e.native_h as f32)) else { return };
        let turns = self.turns & 3;
        let (w, h) = if turns & 1 == 1 { (h, w) } else { (w, h) };
        self.close_menu();
        self.hide_tooltip();
        self.stop_slideshow();
        let mut e = Edit {
            path,
            w,
            h,
            p: Params { turns, ..Params::default() },
            // Original's aspect to begin with (Faye, 2026-11-14).
            aspect: ORIGINAL,
            portrait: h > w,
            drag: None,
            auto: true,
            focus: None,
            focus_visible: false,
            saving: false,
            centre: (0.0, 0.0),
            k: 1.0,
            view: crate::motion::Ease::at([1.0, 0.0, 0.0]),
            placed: false,
            glide: false,
            zoom: crate::motion::Ease::at([0.0; 4]),
            zooming: false,
            undo: Vec::new(),
            redo: Vec::new(),
            start: Snap { p: Params::default(), aspect: ORIGINAL, portrait: false, auto: true, w, h },
            press: None,
            group: None,
            before: false,
            before_key: false,
            scroll: 0.0,
            scroll_max: 0.0,
            reveal: false,
            curve_ch: 0,
            curve_sel: None,
            curve_drag: None,
            curve_rect: D2D_RECT_F::default(),
        };
        e.p.crop = e.full();
        e.start = e.snap_now();
        self.edit = Some(e);
        self.a11y_say("Edit mode. Tab moves through the panel; Ctrl+Z undoes, hold backslash to see the photo before; Enter saves a copy, Esc cancels.");
        self.invalidate();
    }

    /// The explorer's Edit: the file opens in the viewer and edit mode follows once it's decoded (`edit_when_up`).
    pub(super) fn edit_file(&mut self, path: PathBuf) {
        if self.current_path().is_some_and(|p| crate::explorer::same_path(p, &path)) && self.can_edit() {
            self.leave_browse();
            self.begin_edit();
            return;
        }
        self.edit_pending = Some((path.clone(), Instant::now()));
        self.open_file(path);
        self.leave_browse();
        self.invalidate();
    }

    /// Each frame: an Edit asked for before its photo was up starts once it is (or is dropped, when another
    /// file is showing or this one can't be edited).
    pub(super) fn edit_when_up(&mut self) {
        let Some((want, asked)) = &self.edit_pending else { return };
        if !self.current_path().is_some_and(|cur| crate::explorer::same_path(cur, want)) {
            // Not there yet (a folder still being listed), or left for another file: given a few seconds.
            if asked.elapsed().as_secs() >= 5 {
                self.edit_pending = None;
            }
            return;
        }
        if self.placeholder.is_some() {
            self.edit_pending = None;
            return;
        }
        if self.viewer.current_entry().is_none() {
            return; // still decoding
        }
        self.edit_pending = None;
        if self.can_edit() {
            self.begin_edit();
        }
    }

    pub(super) fn cancel_edit(&mut self) {
        if self.edit.as_ref().is_some_and(|e| e.saving) {
            return;
        }
        self.edit = None;
        self.pressed = None;
        self.invalidate();
    }

    /// Back to where the edit began (a step undo can take back).
    fn edit_reset(&mut self) {
        if let Some(e) = &mut self.edit {
            e.settle_zoom();
            let start = e.start;
            e.restore(start);
        }
        self.invalidate();
    }

    fn edit_aspect(&mut self, i: u8) {
        if let Some(e) = &mut self.edit {
            e.settle_zoom();
            e.aspect = i;
            if i != 0 || e.auto {
                e.auto = true;
                e.reset_crop();
            }
            e.glide = true;
        }
        self.invalidate();
    }

    /// The fixed aspect's frame wider than tall (`portrait` false) or taller than wide.
    fn edit_orient(&mut self, portrait: bool) {
        if let Some(e) = &mut self.edit {
            e.settle_zoom();
            if e.aspect == 0 || e.aspect == 2 || e.portrait == portrait {
                return;
            }
            e.portrait = portrait;
            e.auto = true;
            e.reset_crop();
            e.glide = true;
        }
        self.invalidate();
    }

    /// Straightens to `a` degrees: the photo turns about the frame's middle (the slider, the keys and a drag
    /// outside the frame alike).
    fn edit_set_angle(&mut self, a: f32) {
        if let Some(e) = &mut self.edit {
            e.settle_zoom();
            let a = ((a * 10.0).round() / 10.0).clamp(-STRAIGHTEN_MAX, STRAIGHTEN_MAX);
            // Untouched, the frame stays the largest the straightened image holds (growing back towards 0); moved by
            // hand, it's kept, drawn in only as far as it must be.
            if e.auto {
                e.p.angle = a;
                e.reset_crop();
            } else {
                // From where a turn began, so shrinking on the way doesn't stick when turning back.
                let (crop, from) = match e.drag {
                    Some(d) if matches!(d.hit, Hit::EditRotate | Hit::EditStraighten) => (d.crop, d.angle),
                    _ => (e.p.crop, e.p.angle),
                };
                e.turn(a, crop, from);
            }
        }
        self.invalidate();
    }

    fn edit_set_ev(&mut self, ev: f32) {
        if let Some(e) = &mut self.edit {
            e.p.ev = ((ev * 10.0).round() / 10.0).clamp(-EXPOSURE_MAX, EXPOSURE_MAX);
        }
        self.invalidate();
    }

    /// Saves on a thread: a copy beside the photo, or over it (`copy` false).
    pub(super) fn edit_save(&mut self, copy: bool) {
        let Some(e) = &mut self.edit else { return };
        e.settle_zoom();
        if e.saving || !e.changed() || (!copy && !photoedit::writable(&e.path)) {
            return;
        }
        e.saving = true;
        let (path, p) = (e.path.clone(), e.p);
        // What's saved can be pasted onto other photos (round 3).
        if !p.neutral() {
            self.edit_clipboard = Some(p.tone_only());
        }
        let owner = self.hwnd.0 as isize;
        self.show_toast(if copy { "Saving a copy\u{2026}" } else { "Saving\u{2026}" }, false);
        crate::fileops::spawn(self.hwnd, false, move || {
            let out = photoedit::save(&path, &p, copy, owner);
            if let Err(e) = &out {
                crate::trace::mark(format!("edit save failed: {e}"));
            }
            crate::fileops::Done::Edited { path, out, copy }
        });
        self.invalidate();
    }

    pub(super) fn on_edited(&mut self, path: PathBuf, out: Result<PathBuf, String>, copy: bool) {
        if let Some(e) = &mut self.edit {
            e.saving = false;
        }
        match out {
            Err(e) => self.show_toast(format!("Couldn't save: {e}"), false),
            Ok(out) => {
                self.edit = None;
                self.pressed = None;
                if !copy {
                    // The new pixels are upright: the rotation preview is in them.
                    self.turns = 0;
                }
                // The folder learns of the new file (or the rewritten one) from its watcher; the viewer shows it.
                let stamp = self.viewer.listing.as_mut().and_then(|l| l.refresh(&out)).unwrap_or_else(|| crate::engine::stamp(&out));
                self.viewer.forget(&path);
                let shown = self.viewer.show(out.clone(), stamp, self.gfx.as_ref());
                self.apply(shown);
                self.current_changed();
                self.show_toast(if copy { format!("Saved {}", file_name(&out)) } else { "Saved".to_string() }, false);
            }
        }
        self.invalidate();
    }

    /// The area the photo is fitted into: below the title bar, left of the panel.
    fn edit_area(&self) -> D2D_RECT_F {
        let (w, h) = self.size_dip();
        let top = if self.chrome() { TITLE_H } else { 0.0 } + GAP;
        rect(GAP * 2.0, top + GAP, (w - PANEL_W - GAP * 5.0).max(40.0), (h - top - GAP * 3.0).max(40.0))
    }

    fn edit_panel_rect(&self) -> D2D_RECT_F {
        let (w, h) = self.size_dip();
        let top = if self.chrome() { TITLE_H } else { 0.0 } + GAP;
        rect(w - PANEL_W - GAP, top, PANEL_W, (h - top - GAP).max(120.0))
    }

    /// Draws the photo being edited with its crop frame (in place of the viewer's photo); `frame` is its bitmap
    /// as decoded (before the quarter turns).
    pub(super) fn draw_edit_photo(&mut self, g: &Gfx, frame: &ID2D1Bitmap1) {
        let area = self.edit_area();
        let (w, h) = self.size_dip();
        let Some(e) = &mut self.edit else { return };
        // The frame as big as fits in the middle (with room around it for its handles), the photo placed under it.
        let now = Instant::now();
        let zooming = e.zooming;
        if zooming {
            e.p.crop = e.zoom.value(now);
            if !e.zoom.moving(now) {
                e.p.crop = e.zoom.target();
                e.zooming = false;
            }
        }
        let target = e.target(ui::grow(area, -FRAME_MARGIN));
        if !e.placed {
            e.view.jump(target);
            e.placed = true;
        } else if e.resizing() {
            // Resizing: the photo holds still while the frame's edge follows the pointer.
        } else if e.glide {
            e.view.set(target, now);
            e.glide = false;
        } else if e.view.target() != target {
            e.view.jump(target);
        }
        let [k, ox, oy] = e.view.value(now);
        let moving = e.view.moving(now) || zooming;
        e.k = k;
        e.centre = (ox, oy);
        // The bitmap as decoded: unturned, so its box swaps sides for odd turns, and turns by the quarter turns too.
        let (uw, uh) = if e.p.turns & 1 == 1 { (e.h, e.w) } else { (e.w, e.h) };
        let dest = rect(e.centre.0 - uw * k / 2.0, e.centre.1 - uh * k / 2.0, uw * k, uh * k);
        let angle = e.p.turns as f32 * 90.0 + e.p.angle;
        // The room the frame's hits take: off the title bar and the panel. The photo and the dim go under both
        // (Faye, 2026-11-14: the page showed undimmed round the panel and under the title bar).
        let room = rect(0.0, area.top - GAP, w - PANEL_W - GAP * 2.0, h - area.top + GAP);
        let look = if e.before { e.p.geometry_only() } else { e.p };
        if let Err(err) = g.draw_photo_edit(frame, dest, angle, e.p.flip, &look) {
            crate::trace::mark(format!("edit preview failed: {err}"));
            crate::trace::flush("edit preview failed");
            g.draw_photo(frame, dest, 1.0, true);
        }
        let cr = e.crop_rect();
        let dragging = e.drag.is_some();
        // Outside the crop dimmed, over the whole window.
        let dim = gfx::rgba(0x000000, 0.6);
        let (ct, cb) = (cr.top.clamp(0.0, h), cr.bottom.clamp(0.0, h));
        g.fill(rect(0.0, 0.0, w, ct), dim);
        g.fill(rect(0.0, cb, w, (h - cb).max(0.0)), dim);
        g.fill(rect(0.0, ct, cr.left.clamp(0.0, w), (cb - ct).max(0.0)), dim);
        g.fill(rect(cr.right.clamp(0.0, w), ct, (w - cr.right).max(0.0), (cb - ct).max(0.0)), dim);
        let white = gfx::rgba(0xFFFFFF, 0.95);
        let px = g.px();
        // Thirds while it's handled (finer while straightening, to line things up against).
        let straightening = matches!(self.pressed, Some(Hit::EditStraighten | Hit::EditRotate));
        let lines = if straightening { 6 } else if dragging { 3 } else { 0 };
        for i in 1..lines {
            let f = i as f32 / lines as f32;
            let x = g.snap(cr.left + (cr.right - cr.left) * f);
            let y = g.snap(cr.top + (cr.bottom - cr.top) * f);
            g.fill(rect(x, cr.top, px, cr.bottom - cr.top), gfx::rgba(0xFFFFFF, 0.45));
            g.fill(rect(cr.left, y, cr.right - cr.left, px), gfx::rgba(0xFFFFFF, 0.45));
        }
        // The frame, and L-shaped corners and short bars at the edges' middles.
        let (l, t, r, b) = (g.snap(cr.left), g.snap(cr.top), g.snap(cr.right), g.snap(cr.bottom));
        g.fill(rect(l, t, r - l, px), white);
        g.fill(rect(l, b - px, r - l, px), white);
        g.fill(rect(l, t, px, b - t), white);
        g.fill(rect(r - px, t, px, b - t), white);
        // L-shaped corners and bars at the edges' middles, inside the frame.
        let th = 4.0;
        let arm = 24.0f32.min((r - l) / 3.0).min((b - t) / 3.0);
        g.fill(rect(l, t, arm, th), white);
        g.fill(rect(l, t, th, arm), white);
        g.fill(rect(r - arm, t, arm, th), white);
        g.fill(rect(r - th, t, th, arm), white);
        g.fill(rect(r - arm, b - th, arm, th), white);
        g.fill(rect(r - th, b - arm, th, arm), white);
        g.fill(rect(l, b - th, arm, th), white);
        g.fill(rect(l, b - arm, th, arm), white);
        let (mx, my) = ((l + r) / 2.0, (t + b) / 2.0);
        let bar = 28.0f32.min((r - l) / 4.0).min((b - t) / 4.0);
        g.fill(rect(mx - bar / 2.0, t, bar, th), white);
        g.fill(rect(mx - bar / 2.0, b - th, bar, th), white);
        g.fill(rect(l, my - bar / 2.0, th, bar), white);
        g.fill(rect(r - th, my - bar / 2.0, th, bar), white);
        // "Before" over the frame's corner while the photo shows as it was.
        if e.before {
            let label = ui::wide("Before");
            let tw = g.measure(&label, &g.fonts.body).ceil();
            let r = rect(l + 12.0, t + 12.0, tw + 20.0, 26.0);
            g.fill_round(r, 6.0, gfx::rgba(0x000000, 0.65));
            g.text(&label, &g.fonts.body, r, gfx::rgba(0xFFFFFF, 0.95), Align::Center);
        }
        // Hits: outside the frame turns the photo, inside moves it, edges and corners (on top) resize the frame.
        let saving = e.saving;
        if moving {
            self.invalidate();
        }
        if !saving {
            self.hits.add(Hit::EditRotate, room);
            self.hits.add(Hit::EditCrop(0), cr);
            let band = |x0: f32, y0: f32, x1: f32, y1: f32| D2D_RECT_F { left: x0, top: y0, right: x1, bottom: y1 };
            self.hits.add(Hit::EditCrop(2), band(cr.left, cr.top - EDGE / 2.0, cr.right, cr.top + EDGE / 2.0));
            self.hits.add(Hit::EditCrop(6), band(cr.left, cr.bottom - EDGE / 2.0, cr.right, cr.bottom + EDGE / 2.0));
            self.hits.add(Hit::EditCrop(8), band(cr.left - EDGE / 2.0, cr.top, cr.left + EDGE / 2.0, cr.bottom));
            self.hits.add(Hit::EditCrop(4), band(cr.right - EDGE / 2.0, cr.top, cr.right + EDGE / 2.0, cr.bottom));
            let h2 = CORNER / 2.0;
            for (part, x, y) in [(1u8, cr.left, cr.top), (3, cr.right, cr.top), (5, cr.right, cr.bottom), (7, cr.left, cr.bottom)] {
                self.hits.add(Hit::EditCrop(part), band(x - h2, y - h2, x + h2, y + h2));
            }
        }
    }

    /// The panel: undo, redo and before in its head; the crop's aspect, rotate and flip, the sliders (scrolling
    /// when they don't fit); the buttons at its foot.
    pub(super) fn draw_edit_panel(&mut self, g: &Gfx) {
        let Some(e) = &self.edit else { return };
        let (aspect, portrait, p, saving, changed, writable, focus, ring, before) = (e.aspect, e.portrait, e.p, e.saving, e.changed(), photoedit::writable(&e.path), e.focus, e.focus_visible, e.before);
        let (can_undo, can_redo, scroll, reveal) = (!e.undo.is_empty(), !e.redo.is_empty(), e.scroll, e.reveal);
        let panel = self.edit_panel_rect();
        ui::card(g, panel);
        self.hits.add(Hit::EditPanel, panel);
        let t = theme::raised();
        let (x, w) = (panel.left + PAD, panel.right - panel.left - PAD * 2.0);
        let ring_on = |h: Hit, r: D2D_RECT_F| {
            if ring && focus == Some(h) {
                ui::focus_ring(g, ui::grow(r, 2.0), 6.0);
            }
        };

        // The head: the title, and Before, Redo, Undo from the right.
        let top = panel.top + PAD;
        g.text(&ui::wide("Edit"), &g.fonts.title, rect(x, top, w, 26.0), rgb(t.text), Align::Left);
        let ib = 30.0;
        let can_paste = self.edit_clipboard.is_some_and(|c| p.with_tone(&c) != p);
        let heads = [
            (Hit::EditBefore, 0xE7B3u16, !saving, before),
            (Hit::EditRedo, 0xE7A6, !saving && can_redo, false),
            (Hit::EditUndo, 0xE7A7, !saving && can_undo, false),
            (Hit::EditPaste, 0xE77F, !saving && can_paste, false),
            (Hit::EditCopy, 0xE8C8, !saving && !p.neutral(), false),
        ];
        for (i, (hit, glyph, enabled, on)) in heads.into_iter().enumerate() {
            // Undo, redo and before at the right; paste and copy a little apart from them.
            let r = rect(x + w - ib - i as f32 * (ib + 6.0) - if i >= 3 { 10.0 } else { 0.0 }, top - 2.0, ib, ib);
            if enabled {
                self.hits.add(hit, r);
            }
            let st = self.state(hit, enabled);
            let fg = ui::button_frame(g, r, ui::Kind::Toggle(on), &st);
            g.text(&[glyph], &g.fonts.icons, r, fg, Align::Center);
            ring_on(hit, r);
        }

        // The foot, from the bottom: Save a copy (the one to press), then Cancel | Save, then Reset.
        let bh = 32.0;
        let mut by = panel.bottom - PAD - bh;
        let buttons: [(Hit, &str, ui::Kind, bool, D2D_RECT_F); 4] = {
            let copy = rect(x, by, w, bh);
            by -= bh + 8.0;
            let half = (w - 8.0) / 2.0;
            let cancel = rect(x, by, half, bh);
            let save = rect(x + half + 8.0, by, half, bh);
            by -= bh + 8.0;
            let reset = rect(x, by, w, bh);
            [
                (Hit::EditReset, "Reset", ui::Kind::Standard, !saving && changed, reset),
                (Hit::EditCancel, "Cancel", ui::Kind::Standard, !saving, cancel),
                (Hit::EditSave, "Save", ui::Kind::Standard, !saving && changed && writable, save),
                (Hit::EditSaveCopy, "Save a copy", ui::Kind::Accent, !saving && changed, copy),
            ]
        };
        for (hit, label, kind, enabled, r) in buttons {
            if enabled {
                self.hits.add(hit, r);
            }
            let st = self.state(hit, enabled);
            let fg = ui::button_frame(g, r, kind, &st);
            g.text(&ui::wide(label), &g.fonts.body, r, fg, Align::Center);
            ring_on(hit, r);
        }
        let mut foot = by;
        if !writable {
            let note = "This format can't be written back: Save a copy makes a JPEG or PNG.";
            let nh = g.measure_height(&ui::wide(note), &g.fonts.caption_wrap, w).ceil();
            foot = by - nh - 4.0;
            g.text(&ui::wide(note), &g.fonts.caption_wrap, rect(x, foot, w, nh), rgb(t.label), Align::Left);
        }

        // The middle scrolls (the wheel over the panel) when it's taller than its room.
        let view = D2D_RECT_F { left: panel.left + 1.0, top: top + 40.0, right: panel.right - 1.0, bottom: (foot - 12.0).max(top + 40.0) };
        let shown = |r: D2D_RECT_F| -> Option<D2D_RECT_F> {
            let c = D2D_RECT_F { top: r.top.max(view.top), bottom: r.bottom.min(view.bottom), ..r };
            (c.bottom - c.top >= 4.0).then_some(c)
        };
        let mut placed: Vec<(Hit, D2D_RECT_F)> = Vec::new();
        g.push_clip(view);
        let y0 = view.top - scroll;
        let mut y = y0;

        ui::section(g, "Crop", rect(x, y, w, 20.0));
        y += 26.0;
        let (cw, ch, cg) = ((w - 12.0) / 3.0, 32.0, 6.0);
        for (i, name) in ASPECTS.iter().enumerate() {
            let r = rect(x + (i % 3) as f32 * (cw + cg), y + (i / 3) as f32 * (ch + cg), cw, ch);
            let hit = Hit::EditAspect(i as u8);
            if let Some(v) = shown(r).filter(|_| !saving) {
                self.hits.add(hit, v);
            }
            placed.push((hit, r));
            self.a11y_mark(hit, super::a11y::Mark::Selected(aspect == i as u8));
            let st = self.state(hit, !saving);
            let fg = ui::button_frame(g, r, ui::Kind::Toggle(aspect == i as u8), &st);
            g.text(&ui::wide(name), &g.fonts.body, r, fg, Align::Center);
            ring_on(hit, r);
        }
        y += 2.0 * (ch + cg);
        // Landscape | Portrait, for the aspects that have both (not Free, not Square).
        let orient_ok = !saving && aspect != 0 && aspect != 2;
        let half = (w - cg) / 2.0;
        for (i, name) in ["Landscape", "Portrait"].into_iter().enumerate() {
            let r = rect(x + i as f32 * (half + cg), y, half, ch);
            let hit = Hit::EditOrient(i as u8);
            if let Some(v) = shown(r).filter(|_| orient_ok) {
                self.hits.add(hit, v);
            }
            placed.push((hit, r));
            let on = orient_ok && portrait == (i == 1);
            self.a11y_mark(hit, super::a11y::Mark::Selected(on));
            let st = self.state(hit, orient_ok);
            let fg = ui::button_frame(g, r, ui::Kind::Toggle(on), &st);
            g.text(&ui::wide(name), &g.fonts.body, r, fg, Align::Center);
            ring_on(hit, r);
        }
        y += ch + cg;
        // Rotate left | Rotate right, Flip horizontal | Flip vertical.
        for (i, name) in TOOLS.iter().enumerate() {
            let r = rect(x + (i % 2) as f32 * (half + cg), y + (i / 2) as f32 * (ch + cg), half, ch);
            let hit = Hit::EditTool(i as u8);
            if let Some(v) = shown(r).filter(|_| !saving) {
                self.hits.add(hit, v);
            }
            placed.push((hit, r));
            let st = self.state(hit, !saving);
            let fg = ui::button_frame(g, r, ui::Kind::Standard, &st);
            g.text(&ui::wide(name), &g.fonts.body, r, fg, Align::Center);
            ring_on(hit, r);
        }
        y += 2.0 * (ch + cg) + 14.0;

        for s in 0..SLIDERS.len() {
            match s {
                1 => {
                    ui::section(g, "Light", rect(x, y, w, 20.0));
                    y += 26.0;
                }
                5 => {
                    ui::section(g, "Colour", rect(x, y, w, 20.0));
                    y += 26.0;
                }
                _ => {}
            }
            let (name, max, ..) = SLIDERS[s];
            let hit = slider_hit(s);
            let v = slider_get(&p, s);
            g.text(&ui::wide(name), &g.fonts.body, rect(x, y, w, 20.0), rgb(t.text), Align::Left);
            g.text_tabular(&ui::wide(&slider_text(s, v)), &g.fonts.body, rect(x, y, w, 20.0), rgb(t.label), Align::Right);
            y += 20.0;
            let r = rect(x, y, w, SLIDER_H);
            if let Some(vr) = shown(r).filter(|_| !saving) {
                self.hits.add(hit, vr);
            }
            placed.push((hit, r));
            let held = self.pressed == Some(hit);
            let k = self.fades.get(hit, self.hover == Some(hit) || held);
            let track = slider_track(r);
            let mid = (track.left + track.right) / 2.0;
            // Temperature and tint show which way is which: blue | amber, green | magenta.
            match s {
                5 | 6 => {
                    let (a, b) = if s == 5 { (0x4A8FE0, 0xE0A24A) } else { (0x4CB86A, 0xC85AC4) };
                    g.fill_round(rect(track.left, track.top, mid - track.left, track.bottom - track.top), 2.0, gfx::rgba(a, 0.75));
                    g.fill_round(rect(mid, track.top, track.right - mid, track.bottom - track.top), 2.0, gfx::rgba(b, 0.75));
                }
                _ => g.fill_round(track, 2.0, rgb(t.track)),
            }
            let at = mid + (track.right - track.left) / 2.0 * (v / max).clamp(-1.0, 1.0);
            if s < 5 {
                let (a, b) = if at < mid { (at, mid) } else { (mid, at) };
                g.fill(rect(a, track.top, b - a, track.bottom - track.top), rgb(t.text));
            }
            g.fill(rect(g.snap(mid), track.top - 4.0, g.px(), track.bottom - track.top + 8.0), rgb(t.label));
            let knob = 7.0 + 2.0 * k;
            g.fill_round(rect(at - knob, (track.top + track.bottom) / 2.0 - knob, knob * 2.0, knob * 2.0), knob, rgb(t.text));
            ring_on(hit, r);
            y += SLIDER_H + 14.0;
        }

        // The tone curve: its channel, then the graph (the photo's histogram behind, the diagonal, the other
        // channels' curves faintly under RGB's), the points on the line.
        ui::section(g, "Curve", rect(x, y, w, 20.0));
        y += 26.0;
        let (ch_now, sel) = self.edit.as_ref().map_or((0, None), |e| (e.curve_ch as usize, e.curve_sel));
        let qw = (w - cg * 3.0) / 4.0;
        for (i, name) in CURVE_CHANNELS.iter().enumerate() {
            let r = rect(x + i as f32 * (qw + cg), y, qw, 28.0);
            let hit = Hit::EditCurveChannel(i as u8);
            if let Some(v) = shown(r).filter(|_| !saving) {
                self.hits.add(hit, v);
            }
            placed.push((hit, r));
            self.a11y_mark(hit, super::a11y::Mark::Selected(ch_now == i));
            let st = self.state(hit, !saving);
            let fg = ui::button_frame(g, r, ui::Kind::Toggle(ch_now == i), &st);
            g.text(&ui::wide(name), &g.fonts.body, r, fg, Align::Center);
            ring_on(hit, r);
        }
        y += 28.0 + 10.0;
        let gr = rect(x, y, w, w);
        if let Some(v) = shown(gr).filter(|_| !saving) {
            self.hits.add(Hit::EditCurve, v);
        }
        placed.push((Hit::EditCurve, gr));
        g.fill_round(gr, 4.0, gfx::rgba(t.track, 0.6));
        let at = |px: f32, py: f32| (gr.left + px * (gr.right - gr.left), gr.bottom - py * (gr.bottom - gr.top));
        if let Some(hist) = self.viewer.current_entry().and_then(|e| e.histogram.clone()) {
            let bins = match ch_now {
                1 => &hist.red,
                2 => &hist.green,
                3 => &hist.blue,
                _ => &hist.luma,
            };
            let peak = bins.iter().copied().max().unwrap_or(1).max(1) as f32;
            let mut pts = vec![at(0.0, 0.0)];
            pts.extend(bins.iter().enumerate().map(|(i, &v)| at(i as f32 / 255.0, (v as f32 / peak).sqrt() * 0.9)));
            pts.push(at(1.0, 0.0));
            let tint = if ch_now == 0 { t.label } else { CURVE_COLOURS[ch_now - 1] };
            g.fill_polygon(&pts, gfx::rgba(tint, 0.18));
        }
        for i in 1..4 {
            let f = i as f32 / 4.0;
            let (vx, _) = at(f, 0.0);
            let (_, hy) = at(0.0, f);
            g.fill(rect(g.snap(vx), gr.top, g.px(), gr.bottom - gr.top), gfx::rgba(t.label, 0.22));
            g.fill(rect(gr.left, g.snap(hy), gr.right - gr.left, g.px()), gfx::rgba(t.label, 0.22));
        }
        g.polyline(&[at(0.0, 0.0), at(1.0, 1.0)], gfx::rgba(t.label, 0.35), 1.0);
        let line = |c: &photoedit::Curve| -> Vec<(f32, f32)> { (0..=96).map(|i| i as f32 / 96.0).map(|v| at(v, c.eval(v))).collect() };
        if ch_now == 0 {
            for k in 1..4 {
                if !p.curves[k].identity() {
                    g.polyline(&line(&p.curves[k]), gfx::rgba(CURVE_COLOURS[k - 1], 0.45), 1.5);
                }
            }
        }
        let colour = if ch_now == 0 { rgb(t.text) } else { gfx::rgba(CURVE_COLOURS[ch_now - 1], 1.0) };
        let cur = p.curves[ch_now];
        g.polyline(&line(&cur), colour, 2.0);
        for (i, q) in cur.points().iter().enumerate() {
            let (px, py) = at(q[0], q[1]);
            let big = sel == Some(i);
            let rad = if big { 6.0 } else { 4.5 };
            g.fill_round(rect(px - rad - 1.5, py - rad - 1.5, (rad + 1.5) * 2.0, (rad + 1.5) * 2.0), rad + 1.5, gfx::rgba(0x000000, 0.45));
            g.fill_round(rect(px - rad, py - rad, rad * 2.0, rad * 2.0), rad, if big { colour } else { rgb(t.text) });
        }
        ring_on(Hit::EditCurve, gr);
        y += w + 14.0;
        let content = y - y0;
        g.pop_clip();

        // A thin bar at the right while it scrolls; the scroll kept in range, and the keyboard's control shown.
        let room = view.bottom - view.top;
        let most = (content - room).max(0.0);
        let mut next = scroll.clamp(0.0, most);
        if reveal {
            if let Some(&(_, r)) = placed.iter().find(|(h, _)| Some(*h) == focus) {
                if r.top < view.top {
                    next -= view.top - r.top + 8.0;
                } else if r.bottom > view.bottom {
                    next += r.bottom - view.bottom + 8.0;
                }
                next = next.clamp(0.0, most);
            }
        }
        if most > 0.0 {
            let bar_h = (room * room / content).max(24.0);
            let bar_y = view.top + (room - bar_h) * (next / most);
            g.fill_round(rect(panel.right - 7.0, bar_y, 3.0, bar_h), 1.5, gfx::rgba(t.label, 0.5));
        }
        if let Some(e) = &mut self.edit {
            e.reveal = false;
            e.scroll_max = most;
            e.curve_rect = gr;
            if (next - e.scroll).abs() > 0.01 {
                e.scroll = next;
                self.invalidate();
            }
        }
    }

    /// The panel's controls in Tab order.
    fn edit_tab_order(&self) -> Vec<Hit> {
        let mut v: Vec<Hit> = (0..6).map(Hit::EditAspect).collect();
        v.extend([Hit::EditOrient(0), Hit::EditOrient(1)]);
        v.extend((0..TOOLS.len() as u8).map(Hit::EditTool));
        v.extend((0..SLIDERS.len()).map(slider_hit));
        v.extend((0..4).map(Hit::EditCurveChannel));
        v.push(Hit::EditCurve);
        v.extend([Hit::EditReset, Hit::EditCancel, Hit::EditSave, Hit::EditSaveCopy]);
        v
    }

    fn edit_click(&mut self, h: Hit) {
        match h {
            Hit::EditAspect(i) => self.edit_aspect(i),
            Hit::EditOrient(i) => self.edit_orient(i == 1),
            Hit::EditTool(i) => {
                if let Some(e) = &mut self.edit {
                    e.settle_zoom();
                    match i {
                        0 => e.quarter(false),
                        1 => e.quarter(true),
                        2 => e.mirror(false),
                        _ => e.mirror(true),
                    }
                }
                self.invalidate();
            }
            Hit::EditUndo => self.edit_undo(false),
            Hit::EditRedo => self.edit_undo(true),
            Hit::EditCopy => self.edit_copy(),
            Hit::EditPaste => self.edit_paste(),
            Hit::EditCurveChannel(i) => {
                if let Some(e) = &mut self.edit {
                    e.curve_ch = i.min(3);
                    e.curve_sel = None;
                }
                self.invalidate();
            }
            Hit::EditReset => self.edit_reset(),
            Hit::EditCancel => self.cancel_edit(),
            Hit::EditSave => self.edit_save(false),
            Hit::EditSaveCopy => self.edit_save(true),
            _ => {}
        }
    }

    /// Undo (`redo` false) or redo one step.
    fn edit_undo(&mut self, redo: bool) {
        if let Some(e) = &mut self.edit {
            if e.saving {
                return;
            }
            e.settle_zoom();
            let now = e.snap_now();
            let (from, to) = if redo { (&mut e.redo, &mut e.undo) } else { (&mut e.undo, &mut e.redo) };
            if let Some(s) = from.pop() {
                to.push(now);
                e.restore(s);
            }
            e.group = None;
        }
        self.a11y_say(if redo { "Redone" } else { "Undone" });
        self.invalidate();
    }

    /// Copy edits: the tone and colour, for pasting onto this or other photos.
    fn edit_copy(&mut self) {
        let Some(e) = &self.edit else { return };
        if e.p.neutral() {
            return;
        }
        self.edit_clipboard = Some(e.p.tone_only());
        self.show_toast("Edits copied", false);
        self.invalidate();
    }

    /// Paste edits: the tone and colour copied, over this photo's (its crop and turns kept). Undo takes it back.
    pub(super) fn edit_paste(&mut self) {
        let Some(clip) = self.edit_clipboard else { return };
        if let Some(e) = &mut self.edit {
            if e.saving {
                return;
            }
            e.settle_zoom();
            let before = e.snap_now();
            e.p = e.p.with_tone(&clip);
            e.record(before, None);
            e.group = None;
        }
        self.a11y_say("Edits pasted");
        self.invalidate();
    }

    /// The point of the curve showing under `(x, y)` (within reach), else where a click there adds one.
    fn curve_press(&mut self, x: f32, y: f32) {
        let Some(e) = &mut self.edit else { return };
        let gr = e.curve_rect;
        let (gw, gh) = (gr.right - gr.left, gr.bottom - gr.top);
        if gw <= 0.0 || gh <= 0.0 {
            return;
        }
        let ch = e.curve_ch as usize;
        let c = &mut e.p.curves[ch];
        let near = c
            .points()
            .iter()
            .enumerate()
            .map(|(i, q)| (i, (gr.left + q[0] * gw - x).hypot(gr.bottom - q[1] * gh - y)))
            .filter(|&(_, d)| d <= 10.0)
            .min_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(i, _)| i);
        let i = near.or_else(|| c.insert(((x - gr.left) / gw).clamp(0.0, 1.0)));
        e.curve_sel = i;
        e.curve_drag = i;
    }

    /// The point being dragged follows the pointer.
    fn curve_drag_to(&mut self, x: f32, y: f32) {
        let Some(e) = &mut self.edit else { return };
        let Some(i) = e.curve_drag else { return };
        let gr = e.curve_rect;
        let (gw, gh) = (gr.right - gr.left, gr.bottom - gr.top);
        if gw <= 0.0 || gh <= 0.0 {
            return;
        }
        let ch = e.curve_ch as usize;
        e.p.curves[ch].set(i, (x - gr.left) / gw, (gr.bottom - y) / gh);
        self.invalidate();
    }

    /// A slider's value (`s`: its place in `SLIDERS`) set from the slider, the keys or a double-click.
    fn edit_set_slider(&mut self, s: usize, v: f32) {
        match s {
            0 => self.edit_set_angle(v),
            1 => self.edit_set_ev(v),
            _ => {
                if let Some(e) = &mut self.edit {
                    *adjust_mut(&mut e.p, s - 2) = v.round().clamp(-100.0, 100.0);
                }
                self.invalidate();
            }
        }
    }

    /// Edit mode's input: the crop's handles, the sliders, the panel's buttons and keys. None lets the window's
    /// own handling run (painting, hover, the title bar).
    pub(super) fn edit_message(&mut self, msg: u32, wp: WPARAM, lp: LPARAM) -> Option<LRESULT> {
        self.edit.as_ref()?;
        if self.dialog.is_some() || self.menu.is_some() {
            return None;
        }
        if matches!(msg, WM_LBUTTONDOWN | WM_LBUTTONDBLCLK | WM_KEYDOWN) {
            if let Some(e) = &mut self.edit {
                e.settle_zoom();
            }
        }
        match msg {
            WM_LBUTTONDOWN | WM_LBUTTONDBLCLK => {
                let (x, y) = self.dip_from_lparam(lp);
                self.hide_tooltip();
                let h = self.hit(x, y);
                let Some(h) = h.filter(|h| is_edit_hit(*h)) else { return Some(LRESULT(0)) };
                let before = self.edit.as_ref().map(Edit::snap_now)?;
                if let Some(e) = &mut self.edit {
                    e.focus_visible = false;
                }
                // A double-click on a curve's point takes it out (not an end).
                if msg == WM_LBUTTONDBLCLK && h == Hit::EditCurve {
                    self.curve_press(x, y);
                    if let Some(e) = &mut self.edit {
                        let ch = e.curve_ch as usize;
                        if let Some(i) = e.curve_sel {
                            e.p.curves[ch].remove(i);
                        }
                        e.curve_sel = None;
                        e.curve_drag = None;
                        e.focus = Some(Hit::EditCurve);
                        e.record(before, None);
                    }
                    self.invalidate();
                    return Some(LRESULT(0));
                }
                // A double-click on a slider puts it back to 0.
                if msg == WM_LBUTTONDBLCLK {
                    if let Some(s) = slider_index(h) {
                        self.edit_set_slider(s, 0.0);
                        if let Some(e) = &mut self.edit {
                            e.record(before, None);
                        }
                        return Some(LRESULT(0));
                    }
                }
                unsafe {
                    SetCapture(self.hwnd);
                }
                self.pressed = Some(h);
                if let Some(e) = &mut self.edit {
                    // The panel's controls take the keys; the frame and the photo leave them where they were.
                    if !matches!(h, Hit::EditCrop(_) | Hit::EditRotate | Hit::EditPanel | Hit::EditBefore | Hit::EditUndo | Hit::EditRedo | Hit::EditCopy | Hit::EditPaste) {
                        e.focus = Some(h);
                    }
                    e.drag = Some(Drag { hit: h, from: (x, y), crop: e.p.crop, angle: e.p.angle });
                    e.press = Some(before);
                    if h == Hit::EditBefore {
                        e.before = true;
                    }
                }
                if let Some(s) = slider_index(h) {
                    let r = self.hits.rect_of(h).unwrap_or_default();
                    self.edit_set_slider(s, slider_value(r, x, SLIDERS[s].1));
                }
                if h == Hit::EditCurve {
                    self.curve_press(x, y);
                }
                self.invalidate();
                Some(LRESULT(0))
            }
            WM_MOUSEMOVE => {
                let (x, y) = self.dip_from_lparam(lp);
                match self.pressed {
                    Some(h @ Hit::EditCrop(_)) => {
                        if let Some(e) = &mut self.edit {
                            if e.drag.is_some_and(|d| d.hit == h) {
                                e.drag_to(x, y);
                            }
                        }
                        self.invalidate();
                    }
                    // Outside the frame: the photo turns about the frame's middle as the pointer goes round it.
                    Some(Hit::EditRotate) => {
                        let turn = self.edit.as_ref().and_then(|e| {
                            let d = e.drag?;
                            let cr = e.crop_rect();
                            let (cx, cy) = ((cr.left + cr.right) / 2.0, (cr.top + cr.bottom) / 2.0);
                            let a0 = (d.from.1 - cy).atan2(d.from.0 - cx);
                            let a1 = (y - cy).atan2(x - cx);
                            let mut da = (a1 - a0).to_degrees();
                            if da > 180.0 {
                                da -= 360.0;
                            } else if da < -180.0 {
                                da += 360.0;
                            }
                            Some(d.angle + da)
                        });
                        if let Some(a) = turn {
                            self.edit_set_angle(a);
                        }
                    }
                    Some(h) if slider_index(h).is_some() => {
                        let s = slider_index(h).unwrap_or(0);
                        let r = self.hits.rect_of(h).unwrap_or_default();
                        self.edit_set_slider(s, slider_value(r, x, SLIDERS[s].1));
                    }
                    Some(Hit::EditCurve) => self.curve_drag_to(x, y),
                    _ => {}
                }
                None // hover and the cursor as usual
            }
            WM_LBUTTONUP => {
                let pressed = self.pressed.take()?;
                if !is_edit_hit(pressed) {
                    self.pressed = Some(pressed);
                    return None;
                }
                unsafe {
                    let _ = ReleaseCapture();
                }
                let (x, y) = self.dip_from_lparam(lp);
                if let Some(e) = &mut self.edit {
                    // A resized frame glides back to the middle, the photo with it.
                    if e.resizing() {
                        e.glide = true;
                    }
                    e.drag = None;
                    e.curve_drag = None;
                    e.before = e.before_key;
                }
                if self.hit(x, y) == Some(pressed) {
                    self.edit_click(pressed);
                }
                // One step for the whole press (a drag, a slider's run, a button), unless it was undo or redo.
                if let Some(e) = &mut self.edit {
                    if let Some(b) = e.press.take().filter(|_| !matches!(pressed, Hit::EditUndo | Hit::EditRedo | Hit::EditPaste)) {
                        e.record(b, None);
                    }
                }
                self.invalidate();
                Some(LRESULT(0))
            }
            WM_MOUSEWHEEL => {
                let delta = ((wp.0 >> 16) & 0xFFFF) as i16 as f32;
                let (x, y) = self.screen_to_dip(lp);
                let panel = self.edit_panel_rect();
                let over_panel = x >= panel.left && x <= panel.right && y >= panel.top && y <= panel.bottom;
                if over_panel {
                    // The panel scrolls (kept in range as it's drawn).
                    if let Some(e) = &mut self.edit {
                        e.scroll = (e.scroll - delta / 120.0 * 48.0).clamp(0.0, e.scroll_max);
                    }
                    self.invalidate();
                } else if self.pressed.is_none() {
                    if let Some(e) = self.edit.as_mut().filter(|e| !e.saving) {
                        let before = e.snap_now();
                        let cr = e.crop_rect();
                        let inside = x >= cr.left && x <= cr.right && y >= cr.top && y <= cr.bottom;
                        let at = inside.then(|| ((x - e.centre.0) / e.k, (y - e.centre.1) / e.k));
                        // From where a zoom under way is going, so quick notches add up.
                        let from = if e.zooming { e.zoom.target() } else { e.p.crop };
                        let to = e.zoomed(from, 1.15f32.powf(delta / 120.0), at);
                        if to != from && e.fits(to) {
                            if !e.zooming {
                                e.zoom.jump(e.p.crop);
                            }
                            e.zoom.set(to, Instant::now());
                            e.zooming = true;
                            e.auto = false;
                        }
                        e.record(before, Some(GROUP_WHEEL));
                    }
                    self.invalidate();
                }
                Some(LRESULT(0))
            }
            WM_RBUTTONDOWN | WM_RBUTTONUP | WM_MOUSEHWHEEL | WM_XBUTTONDOWN | WM_XBUTTONUP => Some(LRESULT(0)),
            WM_KEYUP => {
                if VIRTUAL_KEY(wp.0 as u16) == VK_OEM_5 {
                    let held = self.pressed == Some(Hit::EditBefore);
                    if let Some(e) = &mut self.edit {
                        e.before_key = false;
                        e.before = held;
                    }
                    self.invalidate();
                    return Some(LRESULT(0));
                }
                None
            }
            WM_KEYDOWN => {
                let vk = VIRTUAL_KEY(wp.0 as u16);
                let shift = unsafe { GetKeyState(VK_SHIFT.0 as i32) } < 0;
                let ctrl = unsafe { GetKeyState(VK_CONTROL.0 as i32) } < 0;
                let focus = self.edit.as_ref().and_then(|e| e.focus);
                let before = self.edit.as_ref().map(Edit::snap_now)?;
                // What the key changed is one undo step with the same key's changes just before it.
                let mut group = None;
                match vk {
                    VK_ESCAPE => self.cancel_edit(),
                    // \ held shows the photo as it was (its tone and colour; the crop stays).
                    VK_OEM_5 => {
                        if let Some(e) = &mut self.edit {
                            e.before_key = true;
                            e.before = true;
                        }
                        self.invalidate();
                    }
                    VK_Z if ctrl => self.edit_undo(shift),
                    VK_Y if ctrl => self.edit_undo(true),
                    VK_C if ctrl && shift => self.edit_copy(),
                    VK_V if ctrl && shift => self.edit_paste(),
                    VK_DELETE | VK_BACK if focus == Some(Hit::EditCurve) => {
                        if let Some(e) = &mut self.edit {
                            let ch = e.curve_ch as usize;
                            if let Some(i) = e.curve_sel {
                                let n = e.p.curves[ch].n as usize;
                                if i > 0 && i + 1 < n {
                                    e.p.curves[ch].remove(i);
                                    e.curve_sel = Some(i - 1);
                                }
                            }
                        }
                        group = Some(GROUP_BUTTON);
                        self.invalidate();
                    }
                    VK_TAB => {
                        let order = self.edit_tab_order();
                        let i = focus.and_then(|f| order.iter().position(|&o| o == f));
                        let n = order.len();
                        let next = match (i, shift) {
                            (None, false) => 0,
                            (None, true) => n - 1,
                            (Some(i), false) => (i + 1) % n,
                            (Some(i), true) => (i + n - 1) % n,
                        };
                        if let Some(e) = &mut self.edit {
                            e.focus = Some(order[next]);
                            e.focus_visible = true;
                            e.reveal = true;
                        }
                        self.invalidate();
                    }
                    VK_LEFT | VK_RIGHT | VK_UP | VK_DOWN => {
                        let dir = if matches!(vk, VK_RIGHT | VK_UP) { 1.0 } else { -1.0 };
                        match focus {
                            // The curve: Left/Right pick a point, Up/Down move it (Shift: further).
                            Some(Hit::EditCurve) => {
                                if let Some(e) = &mut self.edit {
                                    let ch = e.curve_ch as usize;
                                    let n = e.p.curves[ch].n as usize;
                                    if matches!(vk, VK_LEFT | VK_RIGHT) {
                                        let i = e.curve_sel.map_or(if dir > 0.0 { 0 } else { n - 1 } as i32, |i| i as i32 + dir as i32);
                                        e.curve_sel = Some(i.clamp(0, n as i32 - 1) as usize);
                                    } else {
                                        let i = *e.curve_sel.get_or_insert(0);
                                        let q = e.p.curves[ch].pts[i];
                                        e.p.curves[ch].set(i, q[0], q[1] + dir * if shift { 0.05 } else { 0.01 });
                                        group = Some(GROUP_CURVE);
                                    }
                                }
                                self.invalidate();
                            }
                            Some(h) if slider_index(h).is_some() => {
                                let s = slider_index(h).unwrap_or(0);
                                let (_, _, step, big) = SLIDERS[s];
                                let v = self.edit.as_ref().map_or(0.0, |e| slider_get(&e.p, s));
                                self.edit_set_slider(s, v + dir * if shift { big } else { step });
                                group = Some(GROUP_SLIDER + s as u8);
                            }
                            Some(Hit::EditOrient(_)) => {
                                let portrait = dir > 0.0;
                                if let Some(e) = &mut self.edit {
                                    e.focus = Some(Hit::EditOrient(portrait as u8));
                                }
                                self.edit_orient(portrait);
                                group = Some(GROUP_ASPECT);
                            }
                            Some(Hit::EditAspect(i)) => {
                                let j = (i as i32 + dir as i32).rem_euclid(6) as u8;
                                if let Some(e) = &mut self.edit {
                                    e.focus = Some(Hit::EditAspect(j));
                                }
                                self.edit_aspect(j);
                                group = Some(GROUP_ASPECT);
                            }
                            _ => {
                                // Nothing in the panel has the keys: the arrows nudge the crop (Shift: further).
                                let step = if shift { 10.0 } else { 1.0 };
                                if let Some(e) = &mut self.edit {
                                    let (dx, dy) = match vk {
                                        VK_LEFT => (-step, 0.0),
                                        VK_RIGHT => (step, 0.0),
                                        VK_UP => (0.0, -step),
                                        _ => (0.0, step),
                                    };
                                    let c = e.p.crop;
                                    let moved = [c[0] + dx, c[1] + dy, c[2] + dx, c[3] + dy];
                                    if e.fits(moved) {
                                        e.p.crop = moved;
                                        e.auto = false;
                                    }
                                }
                                group = Some(GROUP_NUDGE);
                                self.invalidate();
                            }
                        }
                    }
                    VK_RETURN | VK_SPACE => match focus {
                        Some(h @ (Hit::EditAspect(_) | Hit::EditOrient(_) | Hit::EditTool(_) | Hit::EditCurveChannel(_) | Hit::EditReset)) => {
                            self.edit_click(h);
                            group = Some(GROUP_BUTTON);
                        }
                        Some(h @ (Hit::EditCancel | Hit::EditSave | Hit::EditSaveCopy)) => self.edit_click(h),
                        _ if vk == VK_RETURN => self.edit_save(true),
                        _ => {}
                    },
                    // Ctrl+S saves over the photo, as elsewhere.
                    VK_S if ctrl => self.edit_save(false),
                    VK_F11 => return None,
                    _ => {}
                }
                if let (Some(g), Some(e)) = (group, &mut self.edit) {
                    e.record(before, Some(g));
                    if g == GROUP_BUTTON {
                        e.group = None;
                    }
                }
                Some(LRESULT(0))
            }
            WM_CHAR => Some(LRESULT(0)),
            _ => None,
        }
    }
}

/// Whether a file is the kind edit mode can take (a still picture; an animation turns out not to be once open).
pub(super) fn editable_format(path: &std::path::Path) -> bool {
    editable_kind(format::of_path(path))
}

/// `editable_format` by the format itself: a still picture (the toolbar's Rotate and Edit are for these only).
pub(super) fn editable_kind(f: format::Format) -> bool {
    use format::Format as F;
    !matches!(f, F::Unknown | F::Pdf | F::Mtl | F::Text | F::Model | F::Video | F::Audio | F::Archive)
}

pub(super) fn is_edit_hit(h: Hit) -> bool {
    matches!(
        h,
        Hit::EditCrop(_)
            | Hit::EditRotate
            | Hit::EditAspect(_)
            | Hit::EditOrient(_)
            | Hit::EditTool(_)
            | Hit::EditStraighten
            | Hit::EditExposure
            | Hit::EditAdjust(_)
            | Hit::EditCurve
            | Hit::EditCurveChannel(_)
            | Hit::EditCopy
            | Hit::EditPaste
            | Hit::EditUndo
            | Hit::EditRedo
            | Hit::EditBefore
            | Hit::EditReset
            | Hit::EditCancel
            | Hit::EditSave
            | Hit::EditSaveCopy
            | Hit::EditPanel
    )
}

/// A rotate or flip button's name, for readers.
pub(super) fn tool_name(i: u8) -> &'static str {
    TOOLS.get(i as usize).copied().unwrap_or("")
}

/// A curve channel's name, for readers.
pub(super) fn curve_channel_name(i: u8) -> &'static str {
    CURVE_CHANNELS.get(i as usize).copied().unwrap_or("")
}

/// The aspect choice's name, for readers.
pub(super) fn aspect_name(i: u8) -> &'static str {
    ASPECTS.get(i as usize).copied().unwrap_or("")
}

thread_local! {
    static ROTATE_CURSOR: std::cell::Cell<isize> = const { std::cell::Cell::new(0) };
}

/// The pointer outside the crop frame: a curved double arrow (Windows has no rotate cursor), made once at the
/// system's cursor size, white with a dark edge as Windows' own.
pub(super) fn rotate_cursor() -> Option<HCURSOR> {
    let have = ROTATE_CURSOR.with(|c| c.get());
    if have != 0 {
        return Some(HCURSOR(have as _));
    }
    let made = unsafe { make_rotate_cursor() }?;
    ROTATE_CURSOR.with(|c| c.set(made.0 as isize));
    Some(made)
}

unsafe fn make_rotate_cursor() -> Option<HCURSOR> {
    unsafe {
        let n = GetSystemMetrics(SM_CXCURSOR).clamp(32, 128) as usize;
        let s = n as f32;
        let (cx, cy, r, th) = (s * 0.5, s * 0.6, s * 0.3, s * 0.075);
        let (a0, a1) = (205f32.to_radians(), 335f32.to_radians());
        // The arrowheads: at each end of the arc, pointing on along it.
        let head = |a: f32, out: f32| -> [(f32, f32); 3] {
            let (sn, cs) = a.sin_cos();
            let (ex, ey) = (cx + r * cs, cy + r * sn);
            let (dx, dy) = (-sn * out, cs * out);
            let (len, half) = (s * 0.17, s * 0.13);
            [(ex + dx * len, ey + dy * len), (ex - dy * half, ey + dx * half), (ex + dy * half, ey - dx * half)]
        };
        let heads = [head(a0, -1.0), head(a1, 1.0)];
        let in_tri = |p: (f32, f32), t: &[(f32, f32); 3]| {
            let sign = |a: (f32, f32), b: (f32, f32), c: (f32, f32)| (a.0 - c.0) * (b.1 - c.1) - (b.0 - c.0) * (a.1 - c.1);
            let (d1, d2, d3) = (sign(p, t[0], t[1]), sign(p, t[1], t[2]), sign(p, t[2], t[0]));
            !((d1 < 0.0 || d2 < 0.0 || d3 < 0.0) && (d1 > 0.0 || d2 > 0.0 || d3 > 0.0))
        };
        let shape = |x: f32, y: f32| {
            let (dx, dy) = (x - cx, y - cy);
            let mut a = dy.atan2(dx);
            if a < 0.0 {
                a += std::f32::consts::TAU;
            }
            ((dx * dx + dy * dy).sqrt() - r).abs() <= th / 2.0 && a >= a0 && a <= a1 || heads.iter().any(|t| in_tri((x, y), t))
        };
        // Coverage, 4 x 4 samples a pixel.
        let mut white = vec![0f32; n * n];
        for y in 0..n {
            for x in 0..n {
                let mut hit = 0;
                for sy in 0..4 {
                    for sx in 0..4 {
                        if shape(x as f32 + (sx as f32 + 0.5) / 4.0, y as f32 + (sy as f32 + 0.5) / 4.0) {
                            hit += 1;
                        }
                    }
                }
                white[y * n + x] = hit as f32 / 16.0;
            }
        }
        // The dark edge: the shape grown by about a pixel and a half.
        let grow = (s / 32.0 * 1.5).ceil() as i32;
        let mut px = vec![0u8; n * n * 4];
        for y in 0..n as i32 {
            for x in 0..n as i32 {
                let mut dark = 0f32;
                for oy in -grow..=grow {
                    for ox in -grow..=grow {
                        let (xx, yy) = (x + ox, y + oy);
                        if xx >= 0 && yy >= 0 && xx < n as i32 && yy < n as i32 && ((ox * ox + oy * oy) as f32) <= (grow * grow) as f32 + 0.5 {
                            dark = dark.max(white[yy as usize * n + xx as usize]);
                        }
                    }
                }
                let w = white[y as usize * n + x as usize];
                let a = dark.max(w);
                let v = if a > 0.0 { (w / a * 255.0) as u8 } else { 0 };
                let i = (y as usize * n + x as usize) * 4;
                px[i..i + 4].copy_from_slice(&[v, v, v, (a * 255.0) as u8]);
            }
        }
        let bmi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: n as i32,
                biHeight: -(n as i32),
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut bits: *mut std::ffi::c_void = std::ptr::null_mut();
        let color = CreateDIBSection(None, &bmi, DIB_RGB_COLORS, &mut bits, None, 0).ok()?;
        std::ptr::copy_nonoverlapping(px.as_ptr(), bits as *mut u8, px.len());
        let zeros = vec![0u8; n * n / 8];
        let mask = CreateBitmap(n as i32, n as i32, 1, 1, Some(zeros.as_ptr() as *const _));
        let info = ICONINFO { fIcon: false.into(), xHotspot: (cx) as u32, yHotspot: (cy - r) as u32, hbmMask: mask, hbmColor: color };
        let icon = CreateIconIndirect(&info).ok();
        let _ = DeleteObject(color.into());
        let _ = DeleteObject(mask.into());
        icon.map(|i| HCURSOR(i.0))
    }
}
