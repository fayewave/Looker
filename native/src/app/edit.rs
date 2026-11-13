//! Basic editing (Faye, 2026-11-13): crop, straighten and exposure. The Edit button (or E) turns the viewer into
//! edit mode: the photo alone, fitted with room for its crop frame, and a panel at the right with the crop's
//! aspect, the Straighten and Exposure sliders, and Reset, Cancel, Save and Save a copy. Nothing touches the file
//! until a save: Save a copy writes "name (edited).ext" beside it (the default: Enter), Save writes over it with
//! the original moved to the Recycle Bin first. The pixels are photoedit.rs's, at full size on a thread; the
//! preview is Direct2D's (`Gfx::draw_photo_edit`), the same sums.
//!
//! The crop is kept upright on screen, in pixels from the image's middle after its quarter turns and the
//! straightening (`photoedit::Params::crop`), and always on the image: a drag that would take it off stops at
//! the edge, and straightening shrinks it towards the middle when it no longer fits.

use std::path::PathBuf;

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
pub(super) const STRAIGHTEN_MAX: f32 = 45.0;
pub(super) const EXPOSURE_MAX: f32 = 3.0;

/// The aspect choices, by place: Free, Original, Square, 4:3, 3:2, 16:9.
const ASPECTS: [&str; 6] = ["Free", "Original", "Square", "4:3", "3:2", "16:9"];

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
}

#[derive(Clone, Copy)]
struct Drag {
    hit: Hit,
    from: (f32, f32),
    crop: [f32; 4],
}

impl Edit {
    fn ratio(&self) -> Option<f32> {
        let r = match self.aspect {
            1 => self.w / self.h,
            2 => 1.0,
            3 => 4.0 / 3.0,
            4 => 1.5,
            5 => 16.0 / 9.0,
            _ => return None,
        };
        // Original is the image's own; the others stand as the frame is turned.
        Some(if self.aspect == 1 { if self.portrait != (self.h > self.w) { 1.0 / r } else { r } } else if self.portrait { 1.0 / r } else { r })
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

    /// A handle's drag to pointer `(x, y)`: the crop from where the drag began, moved or resized, kept to the
    /// aspect and on the image (as far as it can go towards where the pointer asks).
    fn drag_to(&mut self, x: f32, y: f32) {
        let Some(d) = self.drag else { return };
        let Hit::EditCrop(part) = d.hit else { return };
        let (dx, dy) = ((x - d.from.0) / self.k, (y - d.from.1) / self.k);
        let [l, t, r, b] = d.crop;
        let mut c = match part {
            0 => [l + dx, t + dy, r + dx, b + dy],
            _ => {
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
            // As far from where it was as stays on the image.
            let (mut lo, mut hi) = (0.0f32, 1.0f32);
            let from = d.crop;
            let at = |m: f32| [0, 1, 2, 3].map(|i| from[i] + (c[i] - from[i]) * m);
            for _ in 0..24 {
                let m = (lo + hi) / 2.0;
                if self.fits(at(m)) {
                    lo = m;
                } else {
                    hi = m;
                }
            }
            c = at(lo);
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
            p: Params { turns, angle: 0.0, crop: [0.0; 4], ev: 0.0 },
            aspect: 0,
            portrait: h > w,
            drag: None,
            auto: true,
            focus: None,
            focus_visible: false,
            saving: false,
            centre: (0.0, 0.0),
            k: 1.0,
        };
        e.p.crop = e.full();
        self.edit = Some(e);
        self.a11y_say("Edit mode. Tab moves through the panel; Enter saves a copy, Esc cancels.");
        self.invalidate();
    }

    pub(super) fn cancel_edit(&mut self) {
        if self.edit.as_ref().is_some_and(|e| e.saving) {
            return;
        }
        self.edit = None;
        self.pressed = None;
        self.invalidate();
    }

    fn edit_reset(&mut self) {
        if let Some(e) = &mut self.edit {
            e.p.angle = 0.0;
            e.p.ev = 0.0;
            e.aspect = 0;
            e.portrait = e.h > e.w;
            e.auto = true;
            e.p.crop = e.full();
        }
        self.invalidate();
    }

    fn edit_aspect(&mut self, i: u8) {
        if let Some(e) = &mut self.edit {
            e.aspect = i;
            if i != 0 || e.auto {
                e.auto = true;
                e.reset_crop();
            }
        }
        self.invalidate();
    }

    fn edit_flip_frame(&mut self) {
        if let Some(e) = &mut self.edit {
            if e.aspect == 0 || e.aspect == 2 {
                return;
            }
            e.portrait = !e.portrait;
            e.auto = true;
            e.reset_crop();
        }
        self.invalidate();
    }

    fn edit_set_angle(&mut self, a: f32) {
        if let Some(e) = &mut self.edit {
            let a = (a * 10.0).round() / 10.0;
            e.p.angle = a.clamp(-STRAIGHTEN_MAX, STRAIGHTEN_MAX);
            // Untouched, the frame stays the largest the straightened image holds (growing back towards 0); moved by
            // hand, it's kept, drawn in only as far as it must be.
            if e.auto {
                e.reset_crop();
            } else {
                e.refit();
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
        if e.saving || !e.changed() || (!copy && !photoedit::writable(&e.path)) {
            return;
        }
        e.saving = true;
        let (path, p) = (e.path.clone(), e.p);
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
        let (s, c) = e.p.angle.to_radians().sin_cos();
        let (bw, bh) = (e.w * c.abs() + e.h * s.abs(), e.w * s.abs() + e.h * c.abs());
        let k = ((area.right - area.left) / bw).min((area.bottom - area.top) / bh).max(0.001);
        e.k = k;
        e.centre = ((area.left + area.right) / 2.0, (area.top + area.bottom) / 2.0);
        // The bitmap as decoded: unturned, so its box swaps sides for odd turns, and turns by the quarter turns too.
        let (uw, uh) = if e.p.turns & 1 == 1 { (e.h, e.w) } else { (e.w, e.h) };
        let dest = rect(e.centre.0 - uw * k / 2.0, e.centre.1 - uh * k / 2.0, uw * k, uh * k);
        let angle = e.p.turns as f32 * 90.0 + e.p.angle;
        if g.draw_photo_edit(frame, dest, angle, e.p.ev).is_err() {
            g.draw_photo(frame, dest, 1.0, true);
        }
        let cr = e.crop_rect();
        let dragging = e.drag.is_some();
        // Outside the crop dimmed (over the whole area the image could cover).
        let dim = gfx::rgba(0x000000, 0.6);
        let top = area.top - GAP;
        g.fill(rect(0.0, top, w - PANEL_W - GAP * 2.0, (cr.top - top).max(0.0)), dim);
        g.fill(rect(0.0, cr.bottom, w - PANEL_W - GAP * 2.0, (h - cr.bottom).max(0.0)), dim);
        g.fill(rect(0.0, cr.top, cr.left.max(0.0), cr.bottom - cr.top), dim);
        g.fill(rect(cr.right, cr.top, (w - PANEL_W - GAP * 2.0 - cr.right).max(0.0), cr.bottom - cr.top), dim);
        let white = gfx::rgba(0xFFFFFF, 0.95);
        let px = g.px();
        // Thirds while it's handled (finer while straightening, to line things up against).
        let straightening = matches!(self.pressed, Some(Hit::EditStraighten));
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
        // Hits: inside moves, edges and corners (on top) resize.
        let saving = e.saving;
        if !saving {
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

    /// The panel: the crop's aspect, Straighten, Exposure, and the buttons.
    pub(super) fn draw_edit_panel(&mut self, g: &Gfx) {
        let Some(e) = &self.edit else { return };
        let (aspect, portrait, angle, ev, saving, changed, writable, focus, ring) = (e.aspect, e.portrait, e.p.angle, e.p.ev, e.saving, e.changed(), photoedit::writable(&e.path), e.focus, e.focus_visible);
        let panel = self.edit_panel_rect();
        ui::card(g, panel);
        self.hits.add(Hit::EditPanel, panel);
        let t = theme::raised();
        let (x, w) = (panel.left + PAD, panel.right - panel.left - PAD * 2.0);
        let mut y = panel.top + PAD;
        g.text(&ui::wide("Edit"), &g.fonts.title, rect(x, y, w, 26.0), rgb(t.text), Align::Left);
        y += 40.0;
        let ring_on = |me: &App, h: Hit, r: D2D_RECT_F| {
            if ring && focus == Some(h) {
                ui::focus_ring(g, ui::grow(r, 2.0), 6.0);
            }
            let _ = me;
        };

        ui::section(g, "Crop", rect(x, y, w, 20.0));
        y += 26.0;
        let (cw, ch, cg) = ((w - 12.0) / 3.0, 32.0, 6.0);
        for (i, name) in ASPECTS.iter().enumerate() {
            let r = rect(x + (i % 3) as f32 * (cw + cg), y + (i / 3) as f32 * (ch + cg), cw, ch);
            let hit = Hit::EditAspect(i as u8);
            if !saving {
                self.hits.add(hit, r);
            }
            self.a11y_mark(hit, super::a11y::Mark::Selected(aspect == i as u8));
            let st = self.state(hit, !saving);
            let fg = ui::button_frame(g, r, ui::Kind::Toggle(aspect == i as u8), &st);
            g.text(&ui::wide(name), &g.fonts.body, r, fg, Align::Center);
            ring_on(self, hit, r);
        }
        y += 2.0 * (ch + cg) + 4.0;
        let flip_ok = !saving && aspect != 0 && aspect != 2;
        let r = rect(x, y, w, 32.0);
        if flip_ok {
            self.hits.add(Hit::EditFlip, r);
        }
        let st = self.state(Hit::EditFlip, flip_ok);
        let label = if portrait { "Make the frame wide" } else { "Make the frame tall" };
        ui::icon_label_button(g, r, 0xE7AD, label, ui::Kind::Standard, &st);
        ring_on(self, Hit::EditFlip, r);
        y += 32.0 + 22.0;

        for (hit, name, value, v, max) in [
            (Hit::EditStraighten, "Straighten", degrees_text(angle), angle, STRAIGHTEN_MAX),
            (Hit::EditExposure, "Exposure", ev_text(ev), ev, EXPOSURE_MAX),
        ] {
            ui::section(g, name, rect(x, y, w, 20.0));
            g.text_tabular(&ui::wide(&value), &g.fonts.body, rect(x, y - 2.0, w, 22.0), rgb(t.label), Align::Right);
            y += 24.0;
            let r = rect(x, y, w, SLIDER_H);
            if !saving {
                self.hits.add(hit, r);
            }
            let held = self.pressed == Some(hit);
            let k = self.fades.get(hit, self.hover == Some(hit) || held);
            let track = slider_track(r);
            g.fill_round(track, 2.0, rgb(t.track));
            let mid = (track.left + track.right) / 2.0;
            let at = mid + (track.right - track.left) / 2.0 * (v / max);
            let (a, b) = if at < mid { (at, mid) } else { (mid, at) };
            g.fill(rect(a, track.top, b - a, track.bottom - track.top), rgb(t.text));
            g.fill(rect(g.snap(mid), track.top - 4.0, g.px(), track.bottom - track.top + 8.0), rgb(t.label));
            let knob = 7.0 + 2.0 * k;
            g.fill_round(rect(at - knob, (track.top + track.bottom) / 2.0 - knob, knob * 2.0, knob * 2.0), knob, rgb(t.text));
            ring_on(self, hit, r);
            y += SLIDER_H + 22.0;
        }

        // The buttons, from the bottom: Save a copy (the one to press), then Cancel | Save, then Reset.
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
            ring_on(self, hit, r);
        }
        if !writable {
            let note = "This format can't be written back: Save a copy makes a JPEG or PNG.";
            let nh = g.measure_height(&ui::wide(note), &g.fonts.caption_wrap, w).ceil();
            g.text(&ui::wide(note), &g.fonts.caption_wrap, rect(x, by - nh - 4.0, w, nh), rgb(t.label), Align::Left);
        }
    }

    /// The panel's controls in Tab order.
    fn edit_tab_order(&self) -> Vec<Hit> {
        let mut v: Vec<Hit> = (0..6).map(Hit::EditAspect).collect();
        v.extend([Hit::EditFlip, Hit::EditStraighten, Hit::EditExposure, Hit::EditReset, Hit::EditCancel, Hit::EditSave, Hit::EditSaveCopy]);
        v
    }

    fn edit_click(&mut self, h: Hit) {
        match h {
            Hit::EditAspect(i) => self.edit_aspect(i),
            Hit::EditFlip => self.edit_flip_frame(),
            Hit::EditReset => self.edit_reset(),
            Hit::EditCancel => self.cancel_edit(),
            Hit::EditSave => self.edit_save(false),
            Hit::EditSaveCopy => self.edit_save(true),
            _ => {}
        }
    }

    /// Edit mode's input: the crop's handles, the sliders, the panel's buttons and keys. None lets the window's
    /// own handling run (painting, hover, the title bar).
    pub(super) fn edit_message(&mut self, msg: u32, wp: WPARAM, lp: LPARAM) -> Option<LRESULT> {
        self.edit.as_ref()?;
        if self.dialog.is_some() || self.menu.is_some() {
            return None;
        }
        match msg {
            WM_LBUTTONDOWN | WM_LBUTTONDBLCLK => {
                let (x, y) = self.dip_from_lparam(lp);
                self.hide_tooltip();
                let h = self.hit(x, y);
                let Some(h) = h.filter(|h| is_edit_hit(*h)) else { return Some(LRESULT(0)) };
                if let Some(e) = &mut self.edit {
                    e.focus_visible = false;
                }
                // A double-click on a slider puts it back to 0.
                if msg == WM_LBUTTONDBLCLK && matches!(h, Hit::EditStraighten | Hit::EditExposure) {
                    if h == Hit::EditStraighten {
                        self.edit_set_angle(0.0);
                    } else {
                        self.edit_set_ev(0.0);
                    }
                    return Some(LRESULT(0));
                }
                unsafe {
                    SetCapture(self.hwnd);
                }
                self.pressed = Some(h);
                if let Some(e) = &mut self.edit {
                    e.focus = matches!(h, Hit::EditCrop(_)).then_some(e.focus).flatten().or(Some(h)).filter(|f| !matches!(f, Hit::EditCrop(_)));
                    e.drag = Some(Drag { hit: h, from: (x, y), crop: e.p.crop });
                }
                match h {
                    Hit::EditStraighten => {
                        let r = self.hits.rect_of(h).unwrap_or_default();
                        self.edit_set_angle(slider_value(r, x, STRAIGHTEN_MAX));
                    }
                    Hit::EditExposure => {
                        let r = self.hits.rect_of(h).unwrap_or_default();
                        self.edit_set_ev(slider_value(r, x, EXPOSURE_MAX));
                    }
                    _ => {}
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
                    Some(h @ (Hit::EditStraighten | Hit::EditExposure)) => {
                        let r = self.hits.rect_of(h).unwrap_or_default();
                        if h == Hit::EditStraighten {
                            self.edit_set_angle(slider_value(r, x, STRAIGHTEN_MAX));
                        } else {
                            self.edit_set_ev(slider_value(r, x, EXPOSURE_MAX));
                        }
                    }
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
                    e.drag = None;
                }
                if self.hit(x, y) == Some(pressed) {
                    self.edit_click(pressed);
                }
                self.invalidate();
                Some(LRESULT(0))
            }
            WM_RBUTTONDOWN | WM_RBUTTONUP | WM_MOUSEWHEEL | WM_MOUSEHWHEEL | WM_XBUTTONDOWN | WM_XBUTTONUP => Some(LRESULT(0)),
            WM_KEYDOWN => {
                let vk = VIRTUAL_KEY(wp.0 as u16);
                let shift = unsafe { GetKeyState(VK_SHIFT.0 as i32) } < 0;
                let focus = self.edit.as_ref().and_then(|e| e.focus);
                match vk {
                    VK_ESCAPE => self.cancel_edit(),
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
                        }
                        self.invalidate();
                    }
                    VK_LEFT | VK_RIGHT | VK_UP | VK_DOWN => {
                        let dir = if matches!(vk, VK_RIGHT | VK_UP) { 1.0 } else { -1.0 };
                        match focus {
                            Some(Hit::EditStraighten) => {
                                let a = self.edit.as_ref().map_or(0.0, |e| e.p.angle);
                                self.edit_set_angle(a + dir * if shift { 1.0 } else { 0.1 });
                            }
                            Some(Hit::EditExposure) => {
                                let v = self.edit.as_ref().map_or(0.0, |e| e.p.ev);
                                self.edit_set_ev(v + dir * if shift { 0.5 } else { 0.1 });
                            }
                            Some(Hit::EditAspect(i)) => {
                                let j = (i as i32 + dir as i32).rem_euclid(6) as u8;
                                if let Some(e) = &mut self.edit {
                                    e.focus = Some(Hit::EditAspect(j));
                                }
                                self.edit_aspect(j);
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
                                self.invalidate();
                            }
                        }
                    }
                    VK_RETURN | VK_SPACE => match focus {
                        Some(h @ (Hit::EditAspect(_) | Hit::EditFlip | Hit::EditReset | Hit::EditCancel | Hit::EditSave | Hit::EditSaveCopy)) => self.edit_click(h),
                        _ if vk == VK_RETURN => self.edit_save(true),
                        _ => {}
                    },
                    // Ctrl+S saves over the photo, as elsewhere.
                    VK_S if unsafe { GetKeyState(VK_CONTROL.0 as i32) } < 0 => self.edit_save(false),
                    VK_F11 => return None,
                    _ => {}
                }
                Some(LRESULT(0))
            }
            WM_CHAR => Some(LRESULT(0)),
            _ => None,
        }
    }
}

pub(super) fn is_edit_hit(h: Hit) -> bool {
    matches!(
        h,
        Hit::EditCrop(_) | Hit::EditAspect(_) | Hit::EditFlip | Hit::EditStraighten | Hit::EditExposure | Hit::EditReset | Hit::EditCancel | Hit::EditSave | Hit::EditSaveCopy | Hit::EditPanel
    )
}

/// The aspect choice's name, for readers.
pub(super) fn aspect_name(i: u8) -> &'static str {
    ASPECTS.get(i as usize).copied().unwrap_or("")
}
