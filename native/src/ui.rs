//! The custom-drawn widget toolkit, immediate mode: each frame draws widgets straight from app state and
//! records where each one is ([`Hits`]); input messages hit-test against the last frame's list, so a fast
//! press-and-release between two frames is never lost. A little state is retained per widget id: hover fades
//! ([`Fades`]) and the tooltip timer.
//!
//! Visuals follow WinUI's dark theme as the C# app shows it (Looker's brighter button fills, white on accent).

use std::collections::HashMap;
use std::hash::Hash;
use std::time::Instant;

use windows::Win32::Graphics::Direct2D::Common::{D2D_RECT_F, D2D1_COLOR_F};
use windows::Win32::Graphics::DirectWrite::IDWriteTextFormat;

use crate::gfx::{Align, Gfx, rect, rgba, white};

pub const ACCENT: u32 = 0xF52524;
/// TextFillColorSecondary / Tertiary / Disabled on the dark theme.
pub const TEXT_SECONDARY: u8 = 0xC5;
pub const TEXT_TERTIARY: u8 = 0x8B;
pub const TEXT_DISABLED: u8 = 0x5D;
/// WinUI's brush transition for pointer-over states.
const FADE_MS: f32 = 83.0;

pub fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().collect()
}

fn lerp(a: D2D1_COLOR_F, b: D2D1_COLOR_F, t: f32) -> D2D1_COLOR_F {
    D2D1_COLOR_F { r: a.r + (b.r - a.r) * t, g: a.g + (b.g - a.g) * t, b: a.b + (b.b - a.b) * t, a: a.a + (b.a - a.a) * t }
}

pub fn contains(r: &D2D_RECT_F, x: f32, y: f32) -> bool {
    x >= r.left && x < r.right && y >= r.top && y < r.bottom
}

// --- Hit regions -------------------------------------------------------------------------------------

/// Where each interactive thing was drawn last frame, in draw order (later = on top).
pub struct Hits<T> {
    items: Vec<(T, D2D_RECT_F)>,
}

impl<T: Copy + PartialEq> Hits<T> {
    pub fn new() -> Self {
        Hits { items: Vec::new() }
    }
    pub fn clear(&mut self) {
        self.items.clear();
    }
    pub fn add(&mut self, id: T, r: D2D_RECT_F) {
        self.items.push((id, r));
    }
    pub fn at(&self, x: f32, y: f32) -> Option<T> {
        self.items.iter().rev().find(|(_, r)| contains(r, x, y)).map(|(id, _)| *id)
    }
    pub fn rect_of(&self, id: T) -> Option<D2D_RECT_F> {
        self.items.iter().rev().find(|(i, _)| *i == id).map(|(_, r)| *r)
    }
}

// --- Fades -------------------------------------------------------------------------------------------

/// Per-id 0..1 values that ease toward a target (hover in/out), and whether any is still moving.
pub struct Fades<T> {
    map: HashMap<T, (f32, Instant)>,
    pub moving: bool,
}

impl<T: Copy + Eq + Hash> Fades<T> {
    pub fn new() -> Self {
        Fades { map: HashMap::new(), moving: false }
    }

    /// Call once per frame before drawing.
    pub fn begin(&mut self) {
        self.moving = false;
    }

    pub fn get(&mut self, id: T, on: bool) -> f32 {
        let target = if on { 1.0 } else { 0.0 };
        let now = Instant::now();
        let e = self.map.entry(id).or_insert((target, now));
        let dt = now.duration_since(e.1).as_secs_f32() * 1000.0;
        e.1 = now;
        let step = (dt / FADE_MS).min(1.0);
        if e.0 < target {
            e.0 = (e.0 + step).min(target);
        } else if e.0 > target {
            e.0 = (e.0 - step).max(target);
        }
        if e.0 != target {
            self.moving = true;
        }
        e.0
    }
}

// --- Buttons -----------------------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq)]
pub enum Kind {
    /// Default button: Looker's brighter resting fill on black.
    Standard,
    /// AccentButtonStyle: brand red, white content.
    Accent,
    /// A ToggleButton: standard until checked, then accent.
    Toggle(bool),
    /// No resting fill (caption buttons, menu items, links).
    Subtle,
}

#[derive(Clone, Copy)]
pub struct State {
    pub hover: f32,
    pub pressed: bool,
    pub enabled: bool,
}

/// Fill, border and content colour for a button in a state (WinUI dark-theme resources).
pub fn colors(kind: Kind, s: &State) -> (D2D1_COLOR_F, Option<D2D1_COLOR_F>, D2D1_COLOR_F) {
    let accent = |a: f32| rgba(ACCENT, a);
    match kind {
        Kind::Accent | Kind::Toggle(true) => {
            if !s.enabled {
                (white(0x28), None, white(0x87))
            } else if s.pressed {
                (accent(0.8), None, white(0xCC))
            } else {
                (lerp(accent(1.0), accent(0.9), s.hover), None, white(0xFF))
            }
        }
        Kind::Standard | Kind::Toggle(false) => {
            if !s.enabled {
                (white(0x0B), Some(white(0x12)), white(TEXT_DISABLED))
            } else if s.pressed {
                (white(0x1C), Some(white(0x12)), white(0xCC))
            } else {
                (lerp(white(0x24), white(0x33), s.hover), Some(white(0x12)), white(0xFF))
            }
        }
        Kind::Subtle => {
            if !s.enabled {
                (white(0), None, white(TEXT_DISABLED))
            } else if s.pressed {
                (white(0x0A), None, white(0xCC))
            } else {
                (lerp(white(0), white(0x0F), s.hover), None, white(0xFF))
            }
        }
    }
}

pub fn button_frame(g: &Gfx, r: D2D_RECT_F, kind: Kind, s: &State) -> D2D1_COLOR_F {
    let (fill, stroke, fg) = colors(kind, s);
    if fill.a > 0.0 {
        g.fill_round(r, 4.0, fill);
    }
    if let Some(c) = stroke {
        g.outline_round(r, 4.0, c, g.px());
    }
    fg
}

pub fn icon(g: &Gfx, glyph: u16, r: D2D_RECT_F, fg: D2D1_COLOR_F) {
    g.text(&[glyph], &g.fonts.icons, r, fg, Align::Center);
}

pub fn label(g: &Gfx, text: &str, fmt: &IDWriteTextFormat, r: D2D_RECT_F, fg: D2D1_COLOR_F, align: Align) {
    g.text(&wide(text), fmt, r, fg, align);
}

/// A soft drop shadow under a floating surface (flyouts, cards): a few widening, fading rounded rects, which
/// reads like WinUI's elevation shadow without an effect graph.
pub fn shadow(g: &Gfx, r: D2D_RECT_F, radius: f32) {
    shadow_faded(g, r, radius, 1.0);
}

/// [`shadow`] at an opacity, for surfaces that fade in and out.
pub fn shadow_faded(g: &Gfx, r: D2D_RECT_F, radius: f32, opacity: f32) {
    for i in 1..=6 {
        let d = i as f32 * 2.0;
        let rr = D2D_RECT_F { left: r.left - d * 0.5, top: r.top - d * 0.2, right: r.right + d * 0.5, bottom: r.bottom + d };
        g.fill_round(rr, radius + d, rgba(0x000000, 0.07 * opacity));
    }
}

/// WinUI's keyboard focus visual: a 2 px white ring outside the control with a 1 px dark ring inside it.
pub fn focus_ring(g: &Gfx, r: D2D_RECT_F, radius: f32) {
    let out = D2D_RECT_F { left: r.left - 3.0, top: r.top - 3.0, right: r.right + 3.0, bottom: r.bottom + 3.0 };
    g.outline_round(out, radius + 3.0, white(0xFF), 2.0);
    let inner = D2D_RECT_F { left: r.left - 1.0, top: r.top - 1.0, right: r.right + 1.0, bottom: r.bottom + 1.0 };
    g.outline_round(inner, radius + 1.0, rgba(0x000000, 0.7), 1.0);
}

/// SurfaceStrokeColorDefault on the dark theme: the hairline round flyouts, dialogs and toasts.
fn surface_stroke(opacity: f32) -> D2D1_COLOR_F {
    rgba(0x757575, 0.4 * opacity)
}

// --- Dialogs -----------------------------------------------------------------------------------------

/// The black theme's two dialog layers (`ThemeColors.DialogOverlay` over `DialogStrip` in the C# app).
const DIALOG_BODY: u32 = 0x141414;
const DIALOG_STRIP: u32 = 0x0C0C0C;
const DIALOG_PAD: f32 = 24.0;
const DIALOG_MIN_W: f32 = 320.0;
const DIALOG_MAX_W: f32 = 548.0;
const DIALOG_TITLE_H: f32 = 28.0;
const DIALOG_BUTTON_GAP: f32 = 8.0;

pub struct DialogButton<A> {
    pub label: String,
    pub action: A,
}

/// A modal ContentDialog: smoke over the window, a centred card with a title and text, and a strip of
/// equal-width buttons. The default button is the accent one and starts with keyboard focus.
pub struct Dialog<A> {
    pub title: String,
    pub body: String,
    pub buttons: Vec<DialogButton<A>>,
    pub default: usize,
    pub focus: usize,
    /// Show the focus ring (the dialog was opened from the keyboard, or Tab was pressed).
    pub focus_visible: bool,
}

pub struct DialogLayout {
    pub card: D2D_RECT_F,
    pub buttons: Vec<D2D_RECT_F>,
    title: D2D_RECT_F,
    body: D2D_RECT_F,
    strip_top: f32,
}

impl<A: Copy> Dialog<A> {
    pub fn new(title: String, body: String, buttons: Vec<DialogButton<A>>, default: usize, from_keyboard: bool) -> Dialog<A> {
        Dialog { title, body, buttons, default, focus: default, focus_visible: from_keyboard }
    }

    pub fn layout(&self, g: &Gfx, bounds: D2D_RECT_F) -> DialogLayout {
        let natural = g.measure(&wide(&self.title), &g.fonts.title).max(g.measure(&wide(&self.body), &g.fonts.body_wrap));
        let w = (natural.ceil() + DIALOG_PAD * 2.0).clamp(DIALOG_MIN_W, DIALOG_MAX_W).min(bounds.right - bounds.left - 32.0);
        let inner = w - DIALOG_PAD * 2.0;
        let body_h = g.measure_height(&wide(&self.body), &g.fonts.body_wrap, inner).ceil();
        let content_h = DIALOG_PAD + DIALOG_TITLE_H + 12.0 + body_h + DIALOG_PAD;
        let strip_h = DIALOG_PAD * 2.0 + 32.0;
        let h = content_h + strip_h;
        let x = g.snap((bounds.left + bounds.right - w) / 2.0);
        let y = g.snap((bounds.top + bounds.bottom - h) / 2.0);
        let card = rect(x, y, w, h);
        let n = self.buttons.len().max(1) as f32;
        let bw = (inner - DIALOG_BUTTON_GAP * (n - 1.0)) / n;
        let by = y + content_h + DIALOG_PAD;
        let buttons = (0..self.buttons.len()).map(|i| rect(x + DIALOG_PAD + i as f32 * (bw + DIALOG_BUTTON_GAP), by, bw, 32.0)).collect();
        DialogLayout {
            card,
            buttons,
            title: rect(x + DIALOG_PAD, y + DIALOG_PAD, inner, DIALOG_TITLE_H),
            body: rect(x + DIALOG_PAD, y + DIALOG_PAD + DIALOG_TITLE_H + 12.0, inner, body_h),
            strip_top: y + content_h,
        }
    }

    /// Tab / arrow keys: the next or previous button, wrapping.
    pub fn move_focus(&mut self, forward: bool) {
        let n = self.buttons.len();
        if n > 0 {
            self.focus = if forward { (self.focus + 1) % n } else { (self.focus + n - 1) % n };
        }
        self.focus_visible = true;
    }

    pub fn draw(&self, g: &Gfx, l: &DialogLayout, window: D2D_RECT_F, state: impl Fn(usize) -> State) {
        g.fill(window, rgba(0x000000, 0x4D as f32 / 255.0)); // SmokeFillColorDefault
        let c = l.card;
        shadow(g, c, 8.0);
        g.fill_round(c, 8.0, rgba(DIALOG_STRIP, 1.0));
        // The content layer: rounded at the top, square where it meets the button strip.
        g.fill_round(D2D_RECT_F { bottom: l.strip_top, ..c }, 8.0, rgba(DIALOG_BODY, 1.0));
        g.fill(D2D_RECT_F { top: l.strip_top - 8.0, bottom: l.strip_top, ..c }, rgba(DIALOG_BODY, 1.0));
        g.outline_round(c, 8.0, surface_stroke(1.0), g.px());
        label(g, &self.title, &g.fonts.title, l.title, white(0xFF), Align::Left);
        label(g, &self.body, &g.fonts.body_wrap, l.body, white(0xFF), Align::Left);
        for (i, (b, r)) in self.buttons.iter().zip(&l.buttons).enumerate() {
            let kind = if i == self.default { Kind::Accent } else { Kind::Standard };
            let fg = button_frame(g, *r, kind, &state(i));
            label(g, &b.label, &g.fonts.body, *r, fg, Align::Center);
            if self.focus_visible && i == self.focus {
                focus_ring(g, *r, 4.0);
            }
        }
    }
}

// --- Toasts ------------------------------------------------------------------------------------------

/// The OSD toast (OsdOverlay): fades in, holds, fades out. Showing again while one is up replaces the text
/// and restarts the hold from the current opacity, so a burst (wheel zoom) reads as one steady pill.
pub struct Toast {
    pub text: String,
    start: Instant,
    from: f32,
    hold_ms: f32,
    fade_out_ms: f32,
}

const TOAST_FADE_IN_MS: f32 = 120.0;

pub enum ToastPhase {
    /// Moving: draw at this opacity and keep redrawing.
    Fading(f32),
    /// Fully shown; nothing changes for this many ms.
    Holding(u32),
    Done,
}

impl Toast {
    /// `quick`: a continuous readout (zoom %) that is only interesting while it changes.
    pub fn new(text: String, quick: bool, previous: Option<&Toast>) -> Toast {
        let from = previous.map_or(0.0, |p| p.opacity());
        let (hold_ms, fade_out_ms) = if quick { (350.0, 180.0) } else { (1200.0, 400.0) };
        Toast { text, start: Instant::now(), from, hold_ms, fade_out_ms }
    }

    pub fn phase(&self) -> ToastPhase {
        let t = self.start.elapsed().as_secs_f32() * 1000.0;
        if t < TOAST_FADE_IN_MS {
            ToastPhase::Fading(self.from + (1.0 - self.from) * t / TOAST_FADE_IN_MS)
        } else if t < self.hold_ms {
            ToastPhase::Holding((self.hold_ms - t).ceil().max(1.0) as u32)
        } else if t < self.hold_ms + self.fade_out_ms {
            ToastPhase::Fading(1.0 - (t - self.hold_ms) / self.fade_out_ms)
        } else {
            ToastPhase::Done
        }
    }

    fn opacity(&self) -> f32 {
        match self.phase() {
            ToastPhase::Fading(a) => a,
            ToastPhase::Holding(_) => 1.0,
            ToastPhase::Done => 0.0,
        }
    }

    /// Bottom-right of `area`, 16 DIPs in.
    pub fn draw(&self, g: &Gfx, area: D2D_RECT_F, opacity: f32) {
        let w = (g.measure(&wide(&self.text), &g.fonts.body_strong) + 28.0).ceil();
        let h = 36.0;
        let r = rect(g.snap(area.right - 16.0 - w), g.snap(area.bottom - 16.0 - h), w, h);
        shadow_faded(g, r, 8.0, opacity);
        g.fill_round(r, 8.0, rgba(0x202020, opacity)); // SolidBackgroundFillColorBase
        g.outline_round(r, 8.0, surface_stroke(opacity), g.px());
        label(g, &self.text, &g.fonts.body_strong, r, white((255.0 * opacity) as u8), Align::Center);
    }
}

// --- Menus -------------------------------------------------------------------------------------------

pub const MENU_ITEM_H: f32 = 32.0;
const MENU_PAD: f32 = 4.0;
const MENU_SEP_H: f32 = 9.0;
pub const MENU_BG: u32 = 0x2C2C2C;

pub struct MenuItem<A> {
    pub action: A,
    pub glyph: Option<u16>,
    pub label: String,
    pub accel: Option<&'static str>,
    /// A radio item's dot (sort menu).
    pub checked: bool,
    pub enabled: bool,
}

pub enum Entry<A> {
    Item(MenuItem<A>),
    Separator,
}

pub struct Menu<A> {
    pub entries: Vec<Entry<A>>,
    /// Top-left in DIPs, already clamped into the window.
    pub x: f32,
    pub y: f32,
    pub width: f32,
    /// Keyboard cursor (index into entries).
    pub cursor: Option<usize>,
}

impl<A: Copy> Menu<A> {
    /// Sizes the menu for its content and places it at (x, y), flipped/clamped to stay inside `bounds`.
    pub fn open(g: &Gfx, entries: Vec<Entry<A>>, x: f32, y: f32, bounds: D2D_RECT_F) -> Menu<A> {
        let mut text_w: f32 = 0.0;
        let mut accel_w: f32 = 0.0;
        let mut any_icon = false;
        let mut any_check = false;
        for e in &entries {
            if let Entry::Item(i) = e {
                text_w = text_w.max(g.measure(&wide(&i.label), &g.fonts.body));
                if let Some(a) = i.accel {
                    accel_w = accel_w.max(g.measure(&wide(a), &g.fonts.caption));
                }
                any_icon |= i.glyph.is_some();
                any_check |= i.checked;
            }
        }
        let lead = if any_icon || any_check { 12.0 + 16.0 + 12.0 } else { 12.0 };
        let width = (lead + text_w + if accel_w > 0.0 { 24.0 + accel_w } else { 0.0 } + 12.0 + MENU_PAD * 2.0).max(120.0);
        let mut m = Menu { entries, x, y, width, cursor: None };
        let h = m.height();
        if m.x + width > bounds.right - 4.0 {
            m.x = (x - width).max(bounds.left + 4.0);
        }
        if m.y + h > bounds.bottom - 4.0 {
            m.y = (y - h).max(bounds.top + 4.0);
        }
        m
    }

    pub fn height(&self) -> f32 {
        MENU_PAD * 2.0 + self.entries.iter().map(|e| if matches!(e, Entry::Item(_)) { MENU_ITEM_H } else { MENU_SEP_H }).sum::<f32>()
    }

    pub fn bounds(&self) -> D2D_RECT_F {
        rect(self.x, self.y, self.width, self.height())
    }

    /// Item rects, by entry index.
    pub fn item_rects(&self) -> Vec<(usize, D2D_RECT_F)> {
        let mut y = self.y + MENU_PAD;
        let mut out = Vec::new();
        for (i, e) in self.entries.iter().enumerate() {
            match e {
                Entry::Item(_) => {
                    out.push((i, rect(self.x + MENU_PAD, y, self.width - MENU_PAD * 2.0, MENU_ITEM_H)));
                    y += MENU_ITEM_H;
                }
                Entry::Separator => y += MENU_SEP_H,
            }
        }
        out
    }

    pub fn action_at(&self, index: usize) -> Option<A> {
        match self.entries.get(index) {
            Some(Entry::Item(i)) if i.enabled => Some(i.action),
            _ => None,
        }
    }

    /// Moves the keyboard cursor to the next enabled item in a direction.
    pub fn move_cursor(&mut self, down: bool) {
        let items: Vec<usize> = self
            .entries
            .iter()
            .enumerate()
            .filter_map(|(i, e)| matches!(e, Entry::Item(it) if it.enabled).then_some(i))
            .collect();
        if items.is_empty() {
            return;
        }
        let pos = self.cursor.and_then(|c| items.iter().position(|&i| i == c));
        let next = match (pos, down) {
            (None, true) => 0,
            (None, false) => items.len() - 1,
            (Some(p), true) => (p + 1) % items.len(),
            (Some(p), false) => (p + items.len() - 1) % items.len(),
        };
        self.cursor = Some(items[next]);
    }

    /// Draws the flyout. `hover` is the entry under the pointer, `fade` its hover amount.
    pub fn draw(&self, g: &Gfx, hover: Option<usize>, pressed: Option<usize>) {
        let b = self.bounds();
        shadow(g, b, 8.0);
        g.fill_round(b, 8.0, rgba(MENU_BG, 1.0));
        g.outline_round(b, 8.0, white(0x14), g.px());
        let any_lead = self.entries.iter().any(|e| matches!(e, Entry::Item(i) if i.glyph.is_some() || i.checked));
        let mut y = self.y + MENU_PAD;
        for (i, e) in self.entries.iter().enumerate() {
            match e {
                Entry::Separator => {
                    g.hline(self.x, g.snap(y + MENU_SEP_H / 2.0), self.width, white(0x15));
                    y += MENU_SEP_H;
                }
                Entry::Item(it) => {
                    let r = rect(self.x + MENU_PAD, y, self.width - MENU_PAD * 2.0, MENU_ITEM_H);
                    let on = hover == Some(i) || self.cursor == Some(i);
                    let st = State { hover: if on { 1.0 } else { 0.0 }, pressed: pressed == Some(i), enabled: it.enabled };
                    let (fill, _, fg) = colors(Kind::Subtle, &st);
                    if fill.a > 0.0 {
                        g.fill_round(r, 4.0, fill);
                    }
                    let mut x = r.left + 12.0;
                    if any_lead {
                        if it.checked {
                            // RadioMenuFlyoutItem's bullet.
                            let d = rect(x + 5.0, r.top + MENU_ITEM_H / 2.0 - 3.0, 6.0, 6.0);
                            g.fill_round(d, 3.0, fg);
                        } else if let Some(gl) = it.glyph {
                            icon(g, gl, rect(x, r.top, 16.0, MENU_ITEM_H), fg);
                        }
                        x += 16.0 + 12.0;
                    }
                    label(g, &it.label, &g.fonts.body, rect(x, r.top, r.right - x - 12.0, MENU_ITEM_H), fg, Align::Left);
                    if let Some(a) = it.accel {
                        let c = if it.enabled { white(TEXT_SECONDARY) } else { white(TEXT_DISABLED) };
                        label(g, a, &g.fonts.caption, rect(r.left, r.top, r.right - r.left - 12.0, MENU_ITEM_H), c, Align::Right);
                    }
                    y += MENU_ITEM_H;
                }
            }
        }
    }
}

// --- Tooltips ----------------------------------------------------------------------------------------

pub const TOOLTIP_DELAY_MS: u32 = 800;

/// A tooltip under (or above, near the bottom edge) an anchor rect, clamped into `bounds`.
pub fn tooltip(g: &Gfx, text: &str, anchor: D2D_RECT_F, bounds: D2D_RECT_F) {
    let w = g.measure(&wide(text), &g.fonts.caption) + 18.0;
    let h = 30.0;
    let mut x = (anchor.left + anchor.right) / 2.0 - w / 2.0;
    let mut y = anchor.bottom + 8.0;
    if y + h > bounds.bottom - 4.0 {
        y = anchor.top - 8.0 - h;
    }
    x = x.clamp(bounds.left + 4.0, (bounds.right - w - 4.0).max(bounds.left + 4.0));
    let r = rect(g.snap(x), g.snap(y), w, h);
    shadow(g, r, 4.0);
    g.fill_round(r, 4.0, rgba(MENU_BG, 1.0));
    g.outline_round(r, 4.0, white(0x14), g.px());
    label(g, text, &g.fonts.caption, r, white(0xFF), Align::Center);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hits_prefer_the_topmost() {
        let mut h: Hits<u8> = Hits::new();
        h.add(1, rect(0.0, 0.0, 100.0, 100.0));
        h.add(2, rect(10.0, 10.0, 10.0, 10.0));
        assert_eq!(h.at(15.0, 15.0), Some(2));
        assert_eq!(h.at(50.0, 50.0), Some(1));
        assert_eq!(h.at(150.0, 50.0), None);
    }

    #[test]
    fn fades_reach_their_target() {
        let mut f: Fades<u8> = Fades::new();
        assert_eq!(f.get(1, true), 1.0); // a new id starts settled
        std::thread::sleep(std::time::Duration::from_millis(100));
        assert_eq!(f.get(1, false), 0.0);
    }
}
