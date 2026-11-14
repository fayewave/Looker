//! The accent colour, picked in Settings > Appearance as Snapper's is (Faye, 2026-11-10): a chip per accent, and
//! Custom's colour picker over the Settings card (Snapper's `ui/picker.rs` and its accent card): a saturation and
//! brightness square, a hue strip, the colour with its hex code in a box, a row of ready-made colours, Cancel and
//! Done. Every change shows at once across the window; Done (Enter, or a press outside) keeps it, Cancel (Esc) puts
//! back the accent from before. The Subtitle styles card's colour buttons open it too, for a colour of the style
//! in use (`target`), with subtitle colours as its ready-made ones.

use std::time::Instant;

use windows::Win32::Graphics::Direct2D::Common::D2D1_COLOR_F;

use super::*;
use crate::motion;
use crate::theme::Accent;
use crate::ui::TextField;

/// The picker's controls with their margins (Snapper's `picker::W`, `picker::H`).
const PICKER_W: f32 = 236.0;
const PICKER_H: f32 = 268.0;
/// A row of ready-made colours (Snapper's).
const PRESETS: [u32; 8] = [0xFF2D55, 0xFF9500, 0xAF52DE, 0x5856D6, 0x00C7BE, 0x8E8E93, 0xA2845E, 0x1C1C1E];
/// A subtitle colour's: white, yellow, black and greys, and a few clear colours.
const SUB_PRESETS: [u32; 8] = [0xFFFFFF, 0xFFE14D, 0x000000, 0x404040, 0xA0A0A0, 0x7FD8FF, 0x8EE07A, 0xFF7A7A];

/// The Custom accent's colour picker: the colour as hue (0-360), saturation and value (0-1), its hex box, when it
/// opened (and began closing), and the accent and colour to go back to if it's cancelled.
pub(super) struct AccentPicker {
    h: f32,
    s: f32,
    v: f32,
    pub(super) hex: TextField,
    pub(super) hex_focused: bool,
    opened: Instant,
    closing: Option<Instant>,
    before: (Accent, u32),
    /// A subtitle style's colour (the style in use's), and what it was; None: the accent.
    target: Option<(crate::substyle::Paint, u32)>,
}

impl AccentPicker {
    fn colour(&self) -> u32 {
        hsv_to_rgb(self.h, self.s, self.v)
    }

    fn set(&mut self, c: u32) {
        (self.h, self.s, self.v) = rgb_to_hsv(c);
    }

    fn live(&self) -> bool {
        self.closing.is_none()
    }

    fn presets(&self) -> [u32; 8] {
        if self.target.is_some() { SUB_PRESETS } else { PRESETS }
    }
}

impl App {
    /// The settings' accent and Custom colour, put in use for every theme.
    pub(super) fn set_accent_globals(&self) {
        theme::set_accent(self.settings.accent);
        theme::set_custom(self.settings.accent_custom);
    }

    /// An accent chip was clicked: Custom opens its picker (picked already or not), the rest are put in use.
    pub(super) fn pick_accent(&mut self, i: usize) {
        let Some(a) = Accent::PICKER.get(i).copied() else { return };
        if a == Accent::Custom {
            self.open_accent_picker();
            return;
        }
        if self.settings.accent != a {
            self.settings.accent = a;
            settings::save(&self.settings);
            self.apply_theme();
        }
    }

    /// Opens the colour picker for Custom, which it switches to straight away.
    fn open_accent_picker(&mut self) {
        let before = (self.settings.accent, self.settings.accent_custom);
        self.settings.accent = Accent::Custom;
        self.apply_theme();
        let c = self.settings.accent_custom;
        let (h, s, v) = rgb_to_hsv(c);
        let mut hex = TextField::new(crate::textedit::TextEdit::new(&format!("#{c:06X}")));
        hex.edit.select_all();
        self.accent_picker = Some(AccentPicker { h, s, v, hex, hex_focused: false, opened: Instant::now(), closing: None, before, target: None });
        self.invalidate();
    }

    /// The styles card's colour button `i` (`Paint::ALL`): the picker for that colour of the style in use.
    pub(super) fn open_style_picker(&mut self, i: usize) {
        let Some(paint) = crate::substyle::Paint::ALL.get(i).copied() else { return };
        let c = self.shown_sub_style().paint(paint);
        let (h, s, v) = rgb_to_hsv(c);
        let mut hex = TextField::new(crate::textedit::TextEdit::new(&format!("#{c:06X}")));
        hex.edit.select_all();
        let before = (self.settings.accent, self.settings.accent_custom);
        self.accent_picker = Some(AccentPicker { h, s, v, hex, hex_focused: false, opened: Instant::now(), closing: None, before, target: Some((paint, c)) });
        self.invalidate();
    }

    /// The picker is open and taking input.
    pub(super) fn picker_live(&self) -> bool {
        self.accent_picker.as_ref().is_some_and(|p| p.live())
    }

    /// The picker's hex box has the keys.
    pub(super) fn picker_field(&mut self) -> Option<&mut TextField> {
        self.accent_picker.as_mut().filter(|p| p.live() && p.hex_focused).map(|p| &mut p.hex)
    }

    /// The colour changed in the picker: it's the accent at once (saved when Done).
    fn picker_changed(&mut self, from_box: bool) {
        let Some(p) = &mut self.accent_picker else { return };
        let c = p.colour();
        if !from_box {
            p.hex.edit = crate::textedit::TextEdit::new(&format!("#{c:06X}"));
        }
        if let Some((paint, _)) = p.target {
            self.style_set_paint(paint, c);
            return;
        }
        self.settings.accent = Accent::Custom;
        self.settings.accent_custom = c;
        self.apply_theme();
    }

    /// Puts the picker away: keeping the colour (saved), or back to the accent from before.
    fn close_accent_picker(&mut self, keep: bool) {
        let Some(p) = &mut self.accent_picker else { return };
        if !p.live() {
            return;
        }
        p.closing = Some(Instant::now());
        p.hex_focused = false;
        if let Some((paint, was)) = p.target {
            if !keep {
                self.style_set_paint(paint, was);
            }
            self.invalidate();
            return;
        }
        if !keep {
            (self.settings.accent, self.settings.accent_custom) = p.before;
        }
        settings::save(&self.settings);
        self.apply_theme();
    }

    /// A press while the picker is open: it has every press (the card under it takes none).
    pub(super) fn picker_press(&mut self, h: Option<Hit>, x: f32, y: f32) {
        unsafe {
            SetCapture(self.hwnd);
        }
        self.pressed = h;
        let field_was = self.accent_picker.as_ref().is_some_and(|p| p.hex_focused);
        if let Some(p) = &mut self.accent_picker {
            p.hex_focused = h == Some(Hit::PickerHex);
        }
        match h {
            Some(Hit::PickerSv | Hit::PickerHue) => self.picker_drag(x, y),
            Some(Hit::PickerHex) => {
                if !field_was {
                    self.restart_caret();
                }
                self.field_press(x, false);
            }
            Some(Hit::PickerCard | Hit::PickerPreset(_) | Hit::PickerDone | Hit::PickerCancel) => {}
            // Outside the picker: Done.
            _ => self.close_accent_picker(true),
        }
        self.invalidate();
    }

    /// The square or the hue strip held and dragged.
    pub(super) fn picker_drag(&mut self, x: f32, y: f32) {
        let held = self.pressed;
        let (Some(sv), Some(hue)) = (self.hits.rect_of(Hit::PickerSv), self.hits.rect_of(Hit::PickerHue)) else { return };
        let Some(p) = &mut self.accent_picker else { return };
        match held {
            Some(Hit::PickerSv) => {
                p.s = ((x - sv.left) / (sv.right - sv.left)).clamp(0.0, 1.0);
                p.v = 1.0 - ((y - sv.top) / (sv.bottom - sv.top)).clamp(0.0, 1.0);
            }
            Some(Hit::PickerHue) => p.h = ((x - hue.left) / (hue.right - hue.left)).clamp(0.0, 1.0) * 360.0,
            _ => return,
        }
        self.picker_changed(false);
    }

    /// A click (press and release on it) in the picker.
    pub(super) fn picker_click(&mut self, h: Hit) {
        match h {
            Hit::PickerPreset(i) => {
                if let Some(p) = &mut self.accent_picker {
                    let presets = p.presets();
                    p.set(presets[(i as usize).min(presets.len() - 1)]);
                }
                self.picker_changed(false);
            }
            Hit::PickerDone => self.close_accent_picker(true),
            Hit::PickerCancel => self.close_accent_picker(false),
            _ => {}
        }
    }

    /// Keys while the picker is open: Esc cancels, Enter keeps; the rest go to the hex box when it has them.
    pub(super) fn picker_key(&mut self, vk: VIRTUAL_KEY, shift: bool, ctrl: bool) {
        match vk {
            VK_ESCAPE => self.close_accent_picker(false),
            VK_RETURN => self.close_accent_picker(true),
            _ if self.picker_field().is_some() => {
                let before = self.picker_field().map(|f| f.edit.text());
                self.field_key(vk, shift, ctrl);
                if self.picker_field().map(|f| f.edit.text()) != before {
                    self.picker_typed();
                }
            }
            _ => {}
        }
        self.invalidate();
    }

    /// A character for the hex box: the digits and #.
    pub(super) fn picker_char(&mut self, unit: u16) {
        let ok = char::from_u32(unit as u32).is_some_and(|c| c.is_ascii_hexdigit() || c == '#');
        let Some(f) = self.picker_field() else { return };
        if ok {
            f.edit.type_unit(unit);
            self.picker_typed();
        }
        self.restart_caret();
    }

    /// The hex box's text changed: a whole colour is used at once.
    fn picker_typed(&mut self) {
        let Some(p) = &mut self.accent_picker else { return };
        if let Some(c) = parse_hex(&p.hex.edit.text()) {
            p.set(c);
            self.picker_changed(true);
        }
    }

    /// The picker card, in the middle of the Settings card `over`: it fades in growing from 94 %, over the card
    /// dimmed, and goes the same way (Snapper's). True while it moves.
    pub(super) fn draw_accent_picker(&mut self, g: &Gfx, over: D2D_RECT_F) -> bool {
        let Some(p) = &self.accent_picker else { return false };
        let ease = |at: Instant| if motion::enabled() { motion::ease_out(at.elapsed().as_secs_f32() / motion::EASE) } else { 1.0 };
        let e = match p.closing {
            Some(at) => 1.0 - ease(at),
            None => ease(p.opened),
        };
        if p.closing.is_some() && e <= 0.0 {
            self.accent_picker = None;
            return false;
        }
        let moving = e > 0.0 && e < 1.0;
        let live = p.live();
        let t = theme::raised();
        g.fill_round(over, 10.0, gfx::rgba(0x000000, 0.35 * e));
        let (w, h) = (PICKER_W, 44.0 + PICKER_H + 46.0);
        let card = rect(g.snap(over.left + (over.right - over.left - w) / 2.0), g.snap(over.top + (over.bottom - over.top - h) / 2.0), w, h);
        if live {
            // Everything under it is the picker's: a press there is Done.
            let (ww, wh) = self.size_dip();
            self.hits.add(Hit::PickerOutside, rect(0.0, 0.0, ww, wh));
            self.hits.add(Hit::PickerCard, card);
        }
        g.push_fade(e, 0.94 + 0.06 * e, (card.left + card.right) / 2.0, (card.top + card.bottom) / 2.0);
        for (grow, a) in [(6.0, 0.05), (4.0, 0.07), (2.0, 0.09)] {
            g.fill_round(D2D_RECT_F { left: card.left - grow, top: card.top - grow + 3.0, right: card.right + grow, bottom: card.bottom + grow + 3.0 }, 10.0 + grow, gfx::rgba(0x000000, a));
        }
        g.fill_round(card, 10.0, rgb(t.popup));
        ui::stroke(g, card, 10.0, rgb(t.divider));
        let title = p.target.map_or("Custom accent", |(paint, _)| paint.title());
        let presets = p.presets();
        g.text(&wide(title), &g.fonts.title, rect(card.left + 16.0, card.top + 14.0, w - 32.0, 22.0), rgb(t.text), Align::Left);

        // The controls (Snapper's picker): saturation across and brightness down over the pure hue, then the hues.
        let (top, left) = (card.top + 34.0, card.left);
        let sv = rect(left + 14.0, top + 14.0, PICKER_W - 28.0, 140.0);
        let hue = rect(sv.left, sv.bottom + 12.0, sv.right - sv.left, 12.0);
        if live {
            self.hits.add(Hit::PickerSv, sv);
            self.hits.add(Hit::PickerHue, D2D_RECT_F { top: hue.top - 4.0, bottom: hue.bottom + 4.0, ..hue });
        }
        let Some(p) = &mut self.accent_picker else { return moving };
        let (ph, ps, pv, current) = (p.h, p.s, p.v, p.colour());
        g.fill_round(sv, 6.0, rgb(hsv_to_rgb(ph, 1.0, 1.0)));
        g.fill_gradient_round(sv, 6.0, true, &[(0.0, gfx::rgba(0xFFFFFF, 1.0)), (1.0, gfx::rgba(0xFFFFFF, 0.0))]);
        g.fill_gradient_round(sv, 6.0, false, &[(0.0, gfx::rgba(0x000000, 0.0)), (1.0, gfx::rgba(0x000000, 1.0))]);
        let (kx, ky) = (sv.left + ps * (sv.right - sv.left), sv.top + (1.0 - pv) * (sv.bottom - sv.top));
        g.fill_round(rect(kx - 8.0, ky - 8.0, 16.0, 16.0), 8.0, rgb(0xFFFFFF));
        g.fill_round(rect(kx - 6.0, ky - 6.0, 12.0, 12.0), 6.0, rgb(current));
        let spectrum: Vec<(f32, D2D1_COLOR_F)> =
            [0xFF0000, 0xFFFF00, 0x00FF00, 0x00FFFF, 0x0000FF, 0xFF00FF, 0xFF0000].iter().enumerate().map(|(i, &c)| (i as f32 / 6.0, rgb(c))).collect();
        g.fill_gradient_round(hue, 6.0, true, &spectrum);
        let hx = hue.left + ph / 360.0 * (hue.right - hue.left);
        let knob = rect(hx - 4.0, hue.top - 3.0, 8.0, hue.bottom - hue.top + 6.0);
        g.fill_round(knob, 4.0, rgb(0xFFFFFF));
        ui::stroke(g, knob, 4.0, gfx::rgba(0x000000, 0.3));

        // The colour, its hex code, and ready-made colours.
        let row = hue.bottom + 14.0;
        let dot = rect(sv.left, row, 30.0, 30.0);
        g.fill_round(dot, 15.0, rgb(current));
        ui::stroke(g, dot, 15.0, gfx::rgba(t.ink, 0.25));
        let field = rect(dot.right + 10.0, row, sv.right - dot.right - 10.0, 30.0);
        let focused = p.hex_focused && live;
        p.hex.draw(g, field, t, focused, 0.0, None);
        if live {
            self.hits.add(Hit::PickerHex, field);
        }
        let py = row + 42.0;
        let step = (sv.right - sv.left - 20.0) / (presets.len() - 1) as f32;
        for (i, c) in presets.into_iter().enumerate() {
            let r = rect(sv.left + i as f32 * step, py, 20.0, 20.0);
            if live {
                self.hits.add(Hit::PickerPreset(i as u8), r);
            }
            g.fill_round(r, 10.0, rgb(c));
            ui::stroke(g, r, 10.0, gfx::rgba(t.ink, 0.25));
        }

        let y = card.bottom - 46.0;
        let cancel = rect(card.left + 14.0, y, 80.0, 30.0);
        let done = rect(card.right - 14.0 - 80.0, y, 80.0, 30.0);
        if live {
            self.hits.add(Hit::PickerCancel, cancel);
            self.hits.add(Hit::PickerDone, done);
        }
        let st = self.state(Hit::PickerCancel, live);
        let fg = ui::button_frame(g, cancel, ui::Kind::Standard, &st);
        g.text(&wide("Cancel"), &g.fonts.body, cancel, fg, Align::Center);
        let st = self.state(Hit::PickerDone, live);
        let fg = ui::button_frame(g, done, ui::Kind::Accent, &st);
        g.text(&wide("Done"), &g.fonts.body, done, fg, Align::Center);
        g.pop_fade();
        moving
    }
}

pub(super) fn hsv_to_rgb(h: f32, s: f32, v: f32) -> u32 {
    let c = v * s;
    let hp = h.rem_euclid(360.0) / 60.0;
    let x = c * (1.0 - (hp % 2.0 - 1.0).abs());
    let (r, g, b) = match hp as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let m = v - c;
    let ch = |f: f32| ((f + m) * 255.0).round().clamp(0.0, 255.0) as u32;
    (ch(r) << 16) | (ch(g) << 8) | ch(b)
}

pub(super) fn rgb_to_hsv(c: u32) -> (f32, f32, f32) {
    let (r, g, b) = (((c >> 16) & 0xFF) as f32 / 255.0, ((c >> 8) & 0xFF) as f32 / 255.0, (c & 0xFF) as f32 / 255.0);
    let max = r.max(g).max(b);
    let d = max - r.min(g).min(b);
    let h = if d == 0.0 {
        0.0
    } else if max == r {
        60.0 * ((g - b) / d).rem_euclid(6.0)
    } else if max == g {
        60.0 * ((b - r) / d + 2.0)
    } else {
        60.0 * ((r - g) / d + 4.0)
    };
    (h, if max == 0.0 { 0.0 } else { d / max }, max)
}

/// "#RRGGBB" (the # optional) as 0xRRGGBB.
pub(super) fn parse_hex(s: &str) -> Option<u32> {
    let t = s.trim().trim_start_matches('#');
    (t.len() == 6).then(|| u32::from_str_radix(t, 16).ok()).flatten()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hsv_round_trips_and_hex_parses() {
        for c in [0xF52524u32, 0x2EC4B6, 0x000000, 0xFFFFFF, 0x808080, 0x5856D6] {
            let (h, s, v) = rgb_to_hsv(c);
            assert_eq!(hsv_to_rgb(h, s, v), c, "{c:06X}");
        }
        assert_eq!(parse_hex("#2ec4b6"), Some(0x2EC4B6));
        assert_eq!(parse_hex("2EC4B6"), Some(0x2EC4B6));
        assert_eq!(parse_hex("#2EC4B"), None);
    }
}
