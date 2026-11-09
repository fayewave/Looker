//! The custom-drawn widget toolkit, immediate mode: each frame draws widgets straight from app state and
//! records where each one is ([`Hits`]); input messages hit-test against the last frame's list, so a fast
//! press-and-release between two frames is never lost. A little state is retained per widget id: hover fades
//! ([`Fades`]) and the tooltip timer.
//!
//! Looks and moves like Snapper (and VEIL Transient before it), in Looker's red (theme.rs):
//! - Buttons are boxes: a neutral grey fill with a divider hairline at rest; on hover the hover fill and the
//!   outline warming to the accent; solid accent while pressed; a toggle that's on is an accent-tinted fill with an accent
//!   outline. Radii: 2.5 rows, 4 menu highlights, 5 buttons, fields and tooltips, 8 menus, 10 cards and dialogs.
//! - One motion rule (motion.rs): hover washes come on at once and fade out; switches, panels, menus (fade and
//!   grow from 94 % about the corner they hang from), dialogs and cards (from 96 %) ease over 190 ms.

use std::collections::HashMap;
use std::hash::Hash;
use std::time::Instant;

use windows::Win32::Graphics::Direct2D::Common::{D2D_RECT_F, D2D1_COLOR_F};
use windows::Win32::Graphics::DirectWrite::IDWriteTextFormat;

use crate::gfx::{Align, Gfx, rect, rgb, rgba};
use crate::motion::{self, EASE, Ease};
use crate::textedit::TextEdit;
use crate::theme::{self, Theme, mix};

pub fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().collect()
}

pub fn contains(r: &D2D_RECT_F, x: f32, y: f32) -> bool {
    x >= r.left && x < r.right && y >= r.top && y < r.bottom
}

/// `c` at `a` times its own opacity.
fn faded(c: D2D1_COLOR_F, a: f32) -> D2D1_COLOR_F {
    D2D1_COLOR_F { a: c.a * a, ..c }
}

/// A rect grown by `d` on every side.
pub fn grow(r: D2D_RECT_F, d: f32) -> D2D_RECT_F {
    D2D_RECT_F { left: r.left - d, top: r.top - d, right: r.right + d, bottom: r.bottom + d }
}

// --- Panels ------------------------------------------------------------------------------------------

/// A panel sliding in or out (the cards, the strip): 0 = hidden, 1 = shown, on the one curve.
pub struct Slide {
    e: Ease<1>,
}

impl Slide {
    pub fn new(shown: bool) -> Slide {
        Slide { e: Ease::at([shown as u8 as f32]) }
    }

    /// How far in the panel is now.
    pub fn value(&self) -> f32 {
        self.e.value(Instant::now())[0]
    }

    /// Where it is going.
    pub fn shown(&self) -> bool {
        self.e.target()[0] > 0.5
    }

    pub fn moving(&self) -> bool {
        self.e.moving(Instant::now())
    }

    /// Slides from wherever it is now (a toggle mid-slide turns it round).
    pub fn animate(&mut self, shown: bool) {
        self.e.set([shown as u8 as f32], Instant::now());
    }

    /// Jumps there (the launch, the landing page).
    pub fn snap(&mut self, shown: bool) {
        self.e = Ease::at([shown as u8 as f32]);
    }

    /// True once, on the first call after the slide has run its course.
    pub fn settle(&mut self) -> bool {
        self.e.settle(Instant::now())
    }
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

/// Per-id 0..1 values eased toward a target, and whether any is still moving.
pub struct Fades<T> {
    map: HashMap<T, Ease<1>>,
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

    /// Eased both ways (a switch's knob, an edge fade). A new id starts at its target.
    pub fn get(&mut self, id: T, on: bool) -> f32 {
        let target = on as u8 as f32;
        let now = Instant::now();
        let e = self.map.entry(id).or_insert_with(|| Ease::at([target]));
        e.set([target], now);
        if e.moving(now) {
            self.moving = true;
        }
        e.value(now)[0]
    }

    /// A hover highlight: on at once, fading back out once it isn't.
    pub fn hover(&mut self, id: T, on: bool) -> f32 {
        if on {
            self.map.insert(id, Ease::at([1.0]));
            return 1.0;
        }
        self.get(id, false)
    }
}

// --- Buttons -----------------------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq)]
pub enum Kind {
    /// The boxed button.
    Standard,
    /// Solid accent, white content: the one thing to press (Save, a dialog's action).
    Accent,
    /// A boxed toggle: the selection fill and an accent outline while on.
    Toggle(bool),
    /// No box: an ink wash on hover (icon buttons, a card's close button).
    Subtle,
}

#[derive(Clone, Copy)]
pub struct State {
    pub hover: f32,
    pub pressed: bool,
    pub enabled: bool,
}

impl State {
    pub const IDLE: State = State { hover: 0.0, pressed: false, enabled: true };
}

/// Draws a button's frame in `r` and returns its content colour.
pub fn button_frame(g: &Gfx, r: D2D_RECT_F, kind: Kind, s: &State) -> D2D1_COLOR_F {
    frame(g, r, 5.0, kind, s)
}

/// `button_frame` with a radius of its own (the theme chips).
pub fn frame(g: &Gfx, r: D2D_RECT_F, radius: f32, kind: Kind, s: &State) -> D2D1_COLOR_F {
    let t = theme::t();
    // Disabled: drawn at 45 %, and nothing reacts.
    let k = if s.enabled { 1.0 } else { 0.45 };
    let h = if s.enabled { s.hover } else { 0.0 };
    let pressed = s.pressed && s.enabled;
    match kind {
        Kind::Accent => {
            g.fill_round(r, radius, faded(rgb(t.accent), k));
            faded(rgb(t.on_accent), k)
        }
        Kind::Subtle => {
            let a = if pressed { 0.04 } else { 0.06 * h };
            if a > 0.0 {
                g.fill_round(r, radius, rgba(t.ink, a));
            }
            faded(rgb(t.text), k)
        }
        Kind::Standard | Kind::Toggle(_) => {
            if pressed {
                g.fill_round(r, radius, rgb(t.accent));
                return rgb(t.on_accent);
            }
            if kind == Kind::Toggle(true) {
                g.fill_round(r, radius, faded(rgb(t.selected), k));
                stroke(g, r, radius, faded(rgb(t.accent), k));
                return faded(rgb(t.text), k);
            }
            g.fill_round(r, radius, faded(mix(t.button, t.hover_fill, h), k));
            stroke(g, r, radius, faded(mix(t.divider, t.accent, h), k));
            faded(rgb(t.text), k)
        }
    }
}

/// A hairline just inside `r`.
pub fn stroke(g: &Gfx, r: D2D_RECT_F, radius: f32, c: D2D1_COLOR_F) {
    g.outline_round(r, radius, c, g.px());
}

/// The wash behind a text link (a breadcrumb, a path): fades in with the hover.
pub fn link_fill(g: &Gfx, r: D2D_RECT_F, hover: f32, pressed: bool) {
    let a = if pressed { 0.04 } else { 0.06 * hover };
    if a > 0.0 {
        g.fill_round(r, 4.0, rgba(theme::t().ink, a));
    }
}

/// A link's text colour: the label colour, warming to the text colour with the hover.
pub fn link_color(hover: f32) -> D2D1_COLOR_F {
    let t = theme::t();
    mix(t.label, t.text, hover)
}

pub fn label(g: &Gfx, text: &str, fmt: &IDWriteTextFormat, r: D2D_RECT_F, fg: D2D1_COLOR_F, align: Align) {
    g.text(&wide(text), fmt, r, fg, align);
}

/// An UPPERCASE section heading, in the label colour.
pub fn section(g: &Gfx, text: &str, r: D2D_RECT_F) {
    g.text(&wide(&text.to_uppercase()), &g.fonts.overline, r, rgb(theme::t().label), Align::Left);
}

/// The soft three-step shadow under menus, dialogs and cards.
pub fn shadow(g: &Gfx, r: D2D_RECT_F, radius: f32) {
    shadow_faded(g, r, radius, 1.0);
}

/// [`shadow`] at an opacity, for surfaces that fade in and out.
pub fn shadow_faded(g: &Gfx, r: D2D_RECT_F, radius: f32, opacity: f32) {
    for (d, a) in [(3.0, 0.05), (2.0, 0.07), (1.0, 0.10)] {
        let s = D2D_RECT_F { left: r.left - d, top: r.top - d + 2.0, right: r.right + d, bottom: r.bottom + d + 2.0 };
        g.fill_round(s, radius + d, rgba(0x000000, a * opacity));
    }
}

/// The keyboard focus: a 2 px ring in the signal red just outside the control.
pub fn focus_ring(g: &Gfx, r: D2D_RECT_F, radius: f32) {
    g.outline_round(grow(r, 3.0), radius + 3.0, rgb(theme::t().signal), 2.0);
}

/// A scroll region's thumb (`r` is its full size): the buttons' fill, their hover fill while it's held.
pub fn scroll_thumb(g: &Gfx, r: D2D_RECT_F, held: bool) {
    let t = theme::t();
    let c = if held { rgb(t.hover_fill) } else { rgb(t.track) };
    g.fill_round(r, (r.right - r.left) / 2.0, c);
}

/// A card's resize edge: an ink line on hover, the accent while it's dragged.
pub fn grip_line(g: &Gfx, r: D2D_RECT_F, hover: f32, dragging: bool) {
    let t = theme::t();
    let c = if dragging { rgb(t.accent) } else { rgba(t.ink, 0.45 * hover) };
    if c.a > 0.0 {
        g.fill_round(r, 1.0, c);
    }
}

/// A card floating over the photo: the lighter fill, a divider hairline, radius 10.
pub fn card(g: &Gfx, r: D2D_RECT_F) {
    let t = theme::t();
    g.fill_round(r, 10.0, rgb(t.sidebar));
    stroke(g, r, 10.0, rgb(t.divider));
}

// --- Dialogs -----------------------------------------------------------------------------------------

const DIALOG_PAD: f32 = 20.0;
const DIALOG_MIN_W: f32 = 400.0;
const DIALOG_MAX_W: f32 = 520.0;
const DIALOG_BUTTON_H: f32 = 30.0;
const DIALOG_BUTTON_GAP: f32 = 8.0;

pub struct DialogButton<A> {
    pub label: String,
    pub action: A,
    /// A disabled button can't be clicked or focused (Rename with an empty name).
    pub enabled: bool,
}

impl<A> DialogButton<A> {
    pub fn new(label: &str, action: A) -> DialogButton<A> {
        DialogButton { label: label.into(), action, enabled: true }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Focus {
    Field,
    Button(usize),
}

/// A modal card, as Snapper asks "Delete this capture?": the window dimmed, a card with a title, text and/or a
/// text box, and its buttons along the bottom: the first (the action) solid accent at the right, the others
/// boxed from the left. Enter presses the default button, which has the keyboard focus to start with unless
/// there is a text box (Enter in it presses the default button too). Opens fading in and growing from 96 %.
pub struct Dialog<A> {
    pub title: String,
    pub body: String,
    pub field: Option<TextField>,
    pub buttons: Vec<DialogButton<A>>,
    pub default: usize,
    pub focus: Focus,
    /// Show the focus ring (the dialog was opened from the keyboard, or Tab was pressed).
    pub focus_visible: bool,
    opened: Instant,
}

pub struct DialogLayout {
    pub card: D2D_RECT_F,
    pub buttons: Vec<D2D_RECT_F>,
    pub field: Option<D2D_RECT_F>,
    /// The text box's clear button, while it shows.
    pub field_clear: Option<D2D_RECT_F>,
    title: D2D_RECT_F,
    body: D2D_RECT_F,
}

impl<A: Copy> Dialog<A> {
    pub fn new(title: String, body: String, buttons: Vec<DialogButton<A>>, default: usize, from_keyboard: bool) -> Dialog<A> {
        Dialog { title, body, field: None, buttons, default, focus: Focus::Button(default), focus_visible: from_keyboard, opened: Instant::now() }
    }

    /// Adds a text box, which takes the keyboard focus.
    pub fn with_field(mut self, field: TextField) -> Dialog<A> {
        self.field = Some(field);
        self.focus = Focus::Field;
        self
    }

    pub fn field_focused(&self) -> bool {
        self.focus == Focus::Field && self.field.is_some()
    }

    /// Still opening: draw again next frame.
    pub fn moving(&self) -> bool {
        motion::progress(self.opened) < 1.0
    }

    fn button_w(g: &Gfx, label: &str) -> f32 {
        (g.measure(&wide(label), &g.fonts.body) + 32.0).ceil().max(80.0)
    }

    pub fn layout(&self, g: &Gfx, bounds: D2D_RECT_F) -> DialogLayout {
        let mut natural = g.measure(&wide(&self.title), &g.fonts.title).max(g.measure(&wide(&self.body), &g.fonts.body_wrap));
        if self.field.is_some() {
            natural = natural.max(FIELD_MIN_W);
        }
        let w = (natural.ceil() + DIALOG_PAD * 2.0).clamp(DIALOG_MIN_W, DIALOG_MAX_W).min(bounds.right - bounds.left - 32.0);
        let inner = w - DIALOG_PAD * 2.0;
        let body_h = if self.body.is_empty() { 0.0 } else { g.measure_height(&wide(&self.body), &g.fonts.body_wrap, inner).ceil() };
        let field_h = if self.field.is_some() { FIELD_H + if body_h > 0.0 { 12.0 } else { 0.0 } } else { 0.0 };
        let content_h = body_h + field_h;
        let h = 48.0 + content_h + 24.0 + DIALOG_BUTTON_H + 18.0;
        let x = g.snap((bounds.left + bounds.right - w) / 2.0);
        let y = g.snap((bounds.top + bounds.bottom - h) / 2.0);
        let card = rect(x, y, w, h);
        let by = y + h - 18.0 - DIALOG_BUTTON_H;
        // The action at the right; the others (Cancel) from the left.
        let mut left = x + DIALOG_PAD;
        let buttons = self
            .buttons
            .iter()
            .enumerate()
            .map(|(i, b)| {
                let bw = Self::button_w(g, &b.label);
                if i == 0 {
                    rect(x + w - DIALOG_PAD - bw, by, bw, DIALOG_BUTTON_H)
                } else {
                    let r = rect(left, by, bw, DIALOG_BUTTON_H);
                    left += bw + DIALOG_BUTTON_GAP;
                    r
                }
            })
            .collect();
        let body_top = y + 48.0;
        let field = self.field.as_ref().map(|_| rect(x + DIALOG_PAD, body_top + content_h - FIELD_H, inner, FIELD_H));
        let field_clear = match (&self.field, field) {
            (Some(f), Some(r)) if self.focus == Focus::Field && !f.edit.units().is_empty() => Some(TextField::clear_rect(r)),
            _ => None,
        };
        DialogLayout {
            card,
            buttons,
            field,
            field_clear,
            title: rect(x + DIALOG_PAD, y + 18.0, inner, 22.0),
            body: rect(x + DIALOG_PAD, body_top, inner, body_h),
        }
    }

    /// Tab / arrow keys: the next or previous focusable thing (the text box, then enabled buttons), wrapping.
    pub fn move_focus(&mut self, forward: bool) {
        let mut order: Vec<Focus> = Vec::new();
        if self.field.is_some() {
            order.push(Focus::Field);
        }
        order.extend(self.buttons.iter().enumerate().filter(|(_, b)| b.enabled).map(|(i, _)| Focus::Button(i)));
        if order.is_empty() {
            return;
        }
        let n = order.len();
        let at = order.iter().position(|f| *f == self.focus).unwrap_or(0);
        self.focus = order[if forward { (at + 1) % n } else { (at + n - 1) % n }];
        self.focus_visible = true;
    }

    /// What Enter does: the focused button, or the default one from the text box.
    pub fn enter_action(&self) -> Option<A> {
        let i = match self.focus {
            Focus::Field => self.default,
            Focus::Button(i) => i,
        };
        self.buttons.get(i).filter(|b| b.enabled).map(|b| b.action)
    }

    /// Draws the dialog. Returns the caret rect while the text box has focus (for placing the IME window).
    pub fn draw(&mut self, g: &Gfx, l: &DialogLayout, window: D2D_RECT_F, buttons: &[State], field_hover: f32, clear: State) -> Option<D2D_RECT_F> {
        let e = motion::progress(self.opened);
        let raised = theme::raised();
        g.fill(window, rgba(0x000000, 0.5 * e));
        let c = l.card;
        g.push_fade(e, 0.96 + 0.04 * e, (c.left + c.right) / 2.0, (c.top + c.bottom) / 2.0);
        shadow(g, c, 10.0);
        g.fill_round(c, 10.0, rgb(raised.popup));
        stroke(g, c, 10.0, rgb(raised.divider));
        label(g, &self.title, &g.fonts.title, l.title, rgb(raised.text), Align::Left);
        if !self.body.is_empty() {
            label(g, &self.body, &g.fonts.body_wrap, l.body, rgb(raised.label), Align::Left);
        }
        let focused = self.focus == Focus::Field;
        let caret = match (&mut self.field, l.field) {
            (Some(f), Some(r)) => f.draw(g, r, raised, focused, field_hover, l.field_clear.map(|cr| (cr, clear))),
            _ => None,
        };
        for (i, (b, r)) in self.buttons.iter().zip(&l.buttons).enumerate() {
            let kind = if i == 0 { Kind::Accent } else { Kind::Standard };
            let st = State { enabled: b.enabled, ..buttons.get(i).copied().unwrap_or(State::IDLE) };
            let fg = button_frame(g, *r, kind, &st);
            label(g, &b.label, &g.fonts.body, *r, fg, Align::Center);
            if self.focus_visible && self.focus == Focus::Button(i) {
                focus_ring(g, *r, 5.0);
            }
        }
        g.pop_fade();
        caret
    }
}

// --- Text box ----------------------------------------------------------------------------------------

pub const FIELD_H: f32 = 30.0;
/// A dialog's text box is at least this wide (the rename box).
const FIELD_MIN_W: f32 = 320.0;
const FIELD_PAD: f32 = 9.0;
const FIELD_CLEAR_W: f32 = 26.0;

/// A single-line text box: the [`TextEdit`] model plus its horizontal scroll and caret blink.
pub struct TextField {
    pub edit: TextEdit,
    scroll: f32,
    /// The caret's blink phase (reset to visible on every edit or move).
    pub caret_on: bool,
}

impl TextField {
    pub fn new(edit: TextEdit) -> TextField {
        TextField { edit, scroll: 0.0, caret_on: true }
    }

    pub fn clear_rect(r: D2D_RECT_F) -> D2D_RECT_F {
        rect(r.right - FIELD_CLEAR_W - 2.0, r.top + 3.0, FIELD_CLEAR_W, r.bottom - r.top - 6.0)
    }

    fn text_rect(r: D2D_RECT_F, with_clear: bool) -> D2D_RECT_F {
        let right = if with_clear { r.right - FIELD_CLEAR_W - 2.0 } else { r.right - FIELD_PAD };
        D2D_RECT_F { left: r.left + FIELD_PAD, right, ..r }
    }

    /// The caret position under DIP `x` (mouse press or drag), for a box drawn at `r`.
    pub fn position_at(&self, g: &Gfx, r: D2D_RECT_F, with_clear: bool, x: f32) -> usize {
        let tr = Self::text_rect(r, with_clear);
        match g.layout(self.edit.units(), &g.fonts.body, r.bottom - r.top) {
            Some(l) => g.position_at(&l, x - tr.left + self.scroll),
            None => 0,
        }
    }

    /// Draws the box in theme `t`'s colours; `clear` is the clear button's rect and state while it shows.
    /// Returns the caret rect when focused.
    pub fn draw(&mut self, g: &Gfx, r: D2D_RECT_F, t: &Theme, focused: bool, hover: f32, clear: Option<(D2D_RECT_F, State)>) -> Option<D2D_RECT_F> {
        g.fill_round(r, 5.0, rgb(t.field));
        let edge = if focused { 1.0 } else { hover * 0.5 };
        stroke(g, r, 5.0, mix(t.divider, t.accent, edge));
        self.draw_text(g, r, t, focused, clear)
    }

    /// The text, selection, caret and clear button without the box (for a box drawn by the caller, with an
    /// icon in it, say). Returns the caret rect when focused.
    pub fn draw_text(&mut self, g: &Gfx, r: D2D_RECT_F, t: &Theme, focused: bool, clear: Option<(D2D_RECT_F, State)>) -> Option<D2D_RECT_F> {
        let tr = Self::text_rect(r, clear.is_some());
        let Some(layout) = g.layout(self.edit.units(), &g.fonts.body, r.bottom - r.top) else { return None };
        let width = tr.right - tr.left;
        let (cx, _, _) = g.caret_at(&layout, self.edit.caret());
        // Keep the caret in view, and never scroll further than the text needs.
        if cx - self.scroll > width - 2.0 {
            self.scroll = cx - width + 2.0;
        }
        if cx - self.scroll < 0.0 {
            self.scroll = cx;
        }
        self.scroll = self.scroll.min((g.layout_width(&layout) - width + 2.0).max(0.0)).max(0.0);

        g.push_clip(tr);
        let x0 = tr.left - self.scroll;
        let (a, b) = self.edit.selection();
        if focused && a != b {
            let (xa, _, _) = g.caret_at(&layout, a);
            let (xb, _, _) = g.caret_at(&layout, b);
            g.fill(rect(x0 + xa, r.top + 5.0, xb - xa, r.bottom - r.top - 10.0), rgb(t.accent_fill));
        }
        g.draw_layout(&layout, x0, r.top, rgb(t.text));
        let caret = rect(g.snap(x0 + cx), r.top + 6.0, g.px().max(1.0), r.bottom - r.top - 12.0);
        if focused && self.caret_on {
            g.fill(caret, rgb(t.text));
        }
        g.pop_clip();

        if let Some((cr, st)) = clear {
            let fg = button_frame(g, cr, Kind::Subtle, &st);
            g.text(&[0xE894], &g.fonts.small_icons, cr, if st.hover > 0.0 { fg } else { rgb(t.label) }, Align::Center);
        }
        focused.then_some(caret)
    }
}

// --- Toasts ------------------------------------------------------------------------------------------

/// The OSD toast: a pill that fades in rising 16 px and growing from 97 %, holds, and fades out. Showing again
/// while one is up replaces the text and restarts the hold from the current opacity, so a burst (wheel zoom)
/// reads as one steady pill.
pub struct Toast {
    pub text: String,
    start: Instant,
    from: f32,
    hold_ms: f32,
    fade_out_ms: f32,
}

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
        let (hold_ms, fade_out_ms) = if quick { (350.0, 190.0) } else { (1200.0, 380.0) };
        Toast { text, start: Instant::now(), from, hold_ms, fade_out_ms }
    }

    pub fn phase(&self) -> ToastPhase {
        let t = self.start.elapsed().as_secs_f32() * 1000.0;
        let fade_in = EASE * 1000.0;
        let animate = motion::enabled();
        if t < fade_in && animate && self.from < 1.0 {
            ToastPhase::Fading(self.from + (1.0 - self.from) * motion::ease_out(t / fade_in))
        } else if t < self.hold_ms {
            ToastPhase::Holding((self.hold_ms - t).ceil().max(1.0) as u32)
        } else if t < self.hold_ms + self.fade_out_ms && animate {
            ToastPhase::Fading(1.0 - motion::ease_out((t - self.hold_ms) / self.fade_out_ms))
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
        let raised = theme::raised();
        let w = (g.measure(&wide(&self.text), &g.fonts.body) + 28.0).ceil();
        let h = 32.0;
        // Rising into place as it comes in, not as it goes.
        let coming = self.start.elapsed().as_secs_f32() < EASE;
        let rise = if coming { 16.0 * (1.0 - opacity) } else { 0.0 };
        let r = rect(g.snap(area.right - 16.0 - w), g.snap(area.bottom - 16.0 - h + rise), w, h);
        let scale = if coming { 0.97 + 0.03 * opacity } else { 1.0 };
        g.push_fade(opacity, scale, r.right, r.bottom);
        shadow(g, r, 8.0);
        g.fill_round(r, 8.0, rgb(raised.popup));
        stroke(g, r, 8.0, rgb(raised.divider));
        label(g, &self.text, &g.fonts.body, r, rgb(raised.text), Align::Center);
        g.pop_fade();
    }
}

// --- Menus -------------------------------------------------------------------------------------------

const MENU_ROW: f32 = 25.0;
const MENU_SEP: f32 = 11.0;
const MENU_PAD_TOP: f32 = 4.0;
const MENU_PAD_BOTTOM: f32 = 6.0;
const MENU_INSET: f32 = 6.0;
const MENU_MIN_W: f32 = 200.0;
/// Where labels start, without and with an icon column; and where the icons start.
const MENU_LABEL_X: f32 = 14.0;
const MENU_LABEL_X_ICONS: f32 = 35.0;
const MENU_ICON_X: f32 = 12.0;

pub struct MenuItem<A> {
    pub action: A,
    /// A Segoe Fluent glyph in the icon column.
    pub glyph: Option<u16>,
    pub label: String,
    /// A shortcut, set small at the right.
    pub accel: Option<&'static str>,
    /// A tickbox at the right: ticked, or empty.
    pub check: Option<bool>,
    pub enabled: bool,
}

pub enum Entry<A> {
    Item(MenuItem<A>),
    Separator,
    /// A row of colour swatches (Transient's colour tags): each an action, its colour, and whether it's on.
    Swatches(Vec<(A, u32, bool)>),
}

const MENU_SWATCHES: f32 = 30.0;
const SWATCH_CELL: f32 = 24.0;

fn entry_h<A>(e: &Entry<A>) -> f32 {
    match e {
        Entry::Item(_) => MENU_ROW,
        Entry::Separator => MENU_SEP,
        Entry::Swatches(_) => MENU_SWATCHES,
    }
}

/// A menu, as Snapper draws its own: rows 25 tall, the hovered row a solid accent bar, a hairline border, radius
/// 8, a soft three-step shadow. If any item has an icon, icons sit in a column 12 px in and labels move 35 px
/// in; ticks are boxes at the right. It opens with a fade, growing from 94 % about the corner it hangs from, and
/// closes instantly.
pub struct Menu<A> {
    pub entries: Vec<Entry<A>>,
    /// Top-left in DIPs, already placed inside the window.
    pub x: f32,
    pub y: f32,
    pub width: f32,
    /// Keyboard cursor (index into entries).
    pub cursor: Option<usize>,
    /// The corner it grows from.
    origin: (f32, f32),
    opened: Instant,
}

impl<A: Copy> Menu<A> {
    fn natural_width(g: &Gfx, entries: &[Entry<A>]) -> f32 {
        let icons = entries.iter().any(|e| matches!(e, Entry::Item(i) if i.glyph.is_some()));
        let checks = entries.iter().any(|e| matches!(e, Entry::Item(i) if i.check.is_some()));
        let label_x = if icons { MENU_LABEL_X_ICONS } else { MENU_LABEL_X };
        let mut w = MENU_MIN_W;
        for e in entries {
            if let Entry::Item(i) = e {
                let hint = i.accel.map_or(0.0, |a| g.measure(&wide(a), &g.fonts.caption) + 24.0);
                let check = if checks { 32.0 } else { 0.0 };
                w = w.max(label_x + g.measure(&wide(&i.label), &g.fonts.body) + hint + check + 18.0);
            }
        }
        w.ceil()
    }

    fn make(entries: Vec<Entry<A>>, x: f32, y: f32, width: f32, origin: (f32, f32)) -> Menu<A> {
        Menu { entries, x, y, width, cursor: None, origin, opened: Instant::now() }
    }

    /// A menu with its top-left at (x, y) (the pointer), flipped to stay inside `bounds`.
    pub fn open(g: &Gfx, entries: Vec<Entry<A>>, x: f32, y: f32, bounds: D2D_RECT_F) -> Menu<A> {
        let width = Self::natural_width(g, &entries);
        let h = Self::height_of(&entries);
        let (mut mx, mut my, mut ox, mut oy) = (x, y, x, y);
        if mx + width > bounds.right - 4.0 {
            mx = (x - width).max(bounds.left + 4.0);
            ox = mx + width;
        }
        if my + h > bounds.bottom - 4.0 {
            my = (y - h).max(bounds.top + 4.0);
            oy = my + h;
        }
        Self::make(entries, mx, my, width, (ox, oy))
    }

    /// A menu hanging 4 DIPs below `anchor` (a drop-down, the sort button), at least as wide as it; above it
    /// when there's no room below.
    pub fn below(g: &Gfx, entries: Vec<Entry<A>>, anchor: D2D_RECT_F, bounds: D2D_RECT_F) -> Menu<A> {
        let width = Self::natural_width(g, &entries).max((anchor.right - anchor.left).ceil());
        let h = Self::height_of(&entries);
        let (mut x, mut ox) = (anchor.left, anchor.left);
        if x + width > bounds.right - 4.0 {
            x = (anchor.right - width).max(bounds.left + 4.0);
            ox = x + width;
        }
        let (y, oy) = if anchor.bottom + 4.0 + h > bounds.bottom - 4.0 && anchor.top - 4.0 - h >= bounds.top {
            (anchor.top - 4.0 - h, anchor.top - 4.0)
        } else {
            (anchor.bottom + 4.0, anchor.bottom + 4.0)
        };
        Self::make(entries, x, y, width, (ox, oy))
    }

    fn height_of(entries: &[Entry<A>]) -> f32 {
        MENU_PAD_TOP + MENU_PAD_BOTTOM + entries.iter().map(entry_h).sum::<f32>()
    }

    pub fn height(&self) -> f32 {
        Self::height_of(&self.entries)
    }

    pub fn bounds(&self) -> D2D_RECT_F {
        rect(self.x, self.y, self.width, self.height())
    }

    /// Still opening: draw again next frame.
    pub fn moving(&self) -> bool {
        motion::progress(self.opened) < 1.0
    }

    /// Item rects, by entry index.
    pub fn item_rects(&self) -> Vec<(usize, D2D_RECT_F)> {
        let mut y = self.y + MENU_PAD_TOP;
        let mut out = Vec::new();
        for (i, e) in self.entries.iter().enumerate() {
            match e {
                Entry::Item(_) => {
                    out.push((i, rect(self.x + MENU_INSET, y, self.width - MENU_INSET * 2.0, MENU_ROW)));
                    y += MENU_ROW;
                }
                e => y += entry_h(e),
            }
        }
        out
    }

    /// Each swatch's rect: (entry, swatch, rect).
    pub fn swatch_rects(&self) -> Vec<(usize, usize, D2D_RECT_F)> {
        let mut y = self.y + MENU_PAD_TOP;
        let mut out = Vec::new();
        for (i, e) in self.entries.iter().enumerate() {
            if let Entry::Swatches(s) = e {
                for k in 0..s.len() {
                    out.push((i, k, rect(self.x + MENU_INSET + 4.0 + k as f32 * SWATCH_CELL, y + (MENU_SWATCHES - SWATCH_CELL) / 2.0, SWATCH_CELL, SWATCH_CELL)));
                }
            }
            y += entry_h(e);
        }
        out
    }

    pub fn swatch_action(&self, i: usize, k: usize) -> Option<A> {
        match self.entries.get(i) {
            Some(Entry::Swatches(s)) => s.get(k).map(|(a, _, _)| *a),
            _ => None,
        }
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

    /// Draws the menu. `hover` is the entry under the pointer, `swatch` the swatch.
    pub fn draw(&self, g: &Gfx, hover: Option<usize>, swatch: Option<(usize, usize)>) {
        let raised = theme::raised();
        let e = motion::progress(self.opened);
        g.push_fade(e, 0.94 + 0.06 * e, self.origin.0, self.origin.1);
        let panel = self.bounds();
        shadow(g, panel, 8.0);
        g.fill_round(panel, 8.0, rgb(raised.popup));
        stroke(g, panel, 8.0, rgb(raised.divider));
        let icons = self.entries.iter().any(|e| matches!(e, Entry::Item(i) if i.glyph.is_some()));
        let checks = self.entries.iter().any(|e| matches!(e, Entry::Item(i) if i.check.is_some()));
        let label_x = if icons { MENU_LABEL_X_ICONS } else { MENU_LABEL_X };
        let mut y = panel.top + MENU_PAD_TOP;
        for (i, en) in self.entries.iter().enumerate() {
            let it = match en {
                Entry::Separator => {
                    g.hline(panel.left + 1.0, g.snap((y + MENU_SEP / 2.0).floor()), self.width - 2.0, rgb(raised.divider));
                    y += MENU_SEP;
                    continue;
                }
                Entry::Swatches(s) => {
                    for (k, &(_, colour, on)) in s.iter().enumerate() {
                        let cell = rect(panel.left + MENU_INSET + 4.0 + k as f32 * SWATCH_CELL, y + (MENU_SWATCHES - SWATCH_CELL) / 2.0, SWATCH_CELL, SWATCH_CELL);
                        let (cx, cy) = ((cell.left + cell.right) / 2.0, (cell.top + cell.bottom) / 2.0);
                        let hot = swatch == Some((i, k));
                        let d = if hot { 16.0 } else { 14.0 };
                        g.fill_round(rect(cx - d / 2.0, cy - d / 2.0, d, d), d / 2.0, rgb(colour));
                        if on {
                            g.outline_round(rect(cx - 10.0, cy - 10.0, 20.0, 20.0), 10.0, rgb(raised.text), 1.5);
                        }
                    }
                    y += MENU_SWATCHES;
                    continue;
                }
                Entry::Item(it) => it,
            };
            let row = rect(panel.left + MENU_INSET, y, self.width - MENU_INSET * 2.0, MENU_ROW);
            let hot = it.enabled && (hover == Some(i) || (hover.is_none() && self.cursor == Some(i)));
            if hot {
                g.fill_round(row, 4.0, rgb(raised.accent));
            }
            let fg = if hot {
                rgb(raised.on_accent)
            } else if it.enabled {
                rgb(raised.text)
            } else {
                rgb(raised.placeholder)
            };
            if let Some(glyph) = it.glyph {
                let ic = if hot || !it.enabled { fg } else { rgb(raised.icon) };
                g.text(&[glyph], &g.fonts.body_icons, rect(panel.left + MENU_ICON_X, y, 16.0, MENU_ROW), ic, Align::Center);
            }
            label(g, &it.label, &g.fonts.body, rect(panel.left + label_x, y, self.width - label_x - 14.0, MENU_ROW), fg, Align::Left);
            let mut right = panel.right - 13.0;
            if checks {
                let b = rect(panel.right - 11.0 - 16.0, y + (MENU_ROW - 16.0) / 2.0, 16.0, 16.0);
                match it.check {
                    Some(true) => {
                        let on = if hot { rgb(raised.on_accent) } else { faded(rgb(raised.accent), if it.enabled { 1.0 } else { 0.45 }) };
                        g.fill_round(b, 4.0, on);
                        let tick = if hot { rgb(raised.accent) } else { rgb(raised.on_accent) };
                        g.text(&[0xE73E], &g.fonts.caption_icons, b, tick, Align::Center);
                    }
                    Some(false) => stroke(g, b, 4.0, if hot { rgb(raised.on_accent) } else { rgb(raised.divider) }),
                    None => {}
                }
                right = b.left - 8.0;
            }
            if let Some(a) = it.accel {
                let hc = if hot { faded(rgb(raised.on_accent), 0.8) } else { rgb(raised.label) };
                label(g, a, &g.fonts.caption, rect(right - 200.0, y, 200.0, MENU_ROW), hc, Align::Right);
            }
            y += MENU_ROW;
        }
        g.pop_fade();
    }
}

// --- Drop-down, switch, theme chip -------------------------------------------------------------------

/// A closed drop-down: the value and a chevron in a field-styled box, its outline warming to the accent on
/// hover or while its menu is open.
pub fn dropdown(g: &Gfx, r: D2D_RECT_F, value: &str, s: &State, open: bool) {
    let t = theme::t();
    g.fill_round(r, 5.0, rgb(t.field));
    stroke(g, r, 5.0, mix(t.divider, t.accent, if open { 1.0 } else { s.hover }));
    label(g, value, &g.fonts.body, rect(r.left + 10.0, r.top, (r.right - r.left - 34.0).max(0.0), r.bottom - r.top), rgb(t.text), Align::Left);
    g.text(&[0xE70D], &g.fonts.caption_icons, rect(r.right - 28.0, r.top, 20.0, r.bottom - r.top), rgb(t.text), Align::Center);
}

pub const SWITCH_W: f32 = 36.0;
pub const SWITCH_H: f32 = 20.0;

/// A switch's track and knob in `r` (36 x 20), `on` eased 0..1 so the knob slides and the track fills: off,
/// the track colour with a divider outline (hover tints both); on, solid accent.
pub fn toggle_switch(g: &Gfx, r: D2D_RECT_F, on: f32, s: &State) {
    let t = theme::t();
    let off = mix(t.track, t.hover_fill, s.hover);
    let acc = rgb(t.accent);
    let fill = D2D1_COLOR_F { r: off.r + (acc.r - off.r) * on, g: off.g + (acc.g - off.g) * on, b: off.b + (acc.b - off.b) * on, a: 1.0 };
    g.fill_round(r, 10.0, fill);
    if on < 1.0 {
        stroke(g, r, 10.0, faded(mix(t.divider, t.accent, s.hover), 1.0 - on));
    }
    let knob = rect(r.left + 4.0 + 16.0 * on, r.top + 4.0, 12.0, 12.0);
    g.fill_round(knob, 6.0, mix(t.label, t.on_accent, on));
}

/// A theme to pick: a square of its page colour with its accent as a dot in the middle, then its name. Picked
/// like a toggle when `on`.
pub fn theme_chip(g: &Gfx, r: D2D_RECT_F, name: &str, on: bool, s: &State, of: &Theme) {
    let t = theme::t();
    let fg = frame(g, r, 6.0, Kind::Toggle(on), s);
    let sw = rect(r.left + 9.0, g.snap(r.top + (r.bottom - r.top - 22.0) / 2.0), 22.0, 22.0);
    g.fill_round(sw, 5.0, rgb(of.content));
    stroke(g, sw, 5.0, rgba(t.ink, 0.22));
    g.fill_round(rect(sw.left + 6.0, sw.top + 6.0, 10.0, 10.0), 5.0, rgb(of.accent));
    label(g, name, &g.fonts.body, rect(sw.right + 10.0, r.top, (r.right - sw.right - 14.0).max(0.0), r.bottom - r.top), fg, Align::Left);
}

/// A boxed button with a glyph before its label, both centred (an empty label: just the glyph).
pub fn icon_label_button(g: &Gfx, r: D2D_RECT_F, glyph: u16, text: &str, kind: Kind, s: &State) {
    let fg = button_frame(g, r, kind, s);
    let tw = if text.is_empty() { 0.0 } else { 8.0 + g.measure(&wide(text), &g.fonts.body).ceil() };
    let x = g.snap(r.left + (r.right - r.left - 16.0 - tw) / 2.0);
    g.text(&[glyph], &g.fonts.icons, rect(x, r.top, 16.0, r.bottom - r.top), fg, Align::Center);
    if !text.is_empty() {
        // Centred in the box as it is: Inter's capitals then centre within half a pixel of the icon. (Snapper
        // nudges its labels a pixel up; here that sat the text visibly high beside the icon.)
        label(g, text, &g.fonts.body, rect(x + 24.0, r.top, tw, r.bottom - r.top), fg, Align::Left);
    }
}

/// How wide `icon_label_button` needs to be for `text`.
pub fn icon_label_width(g: &Gfx, text: &str) -> f32 {
    let tw = if text.is_empty() { 0.0 } else { 8.0 + g.measure(&wide(text), &g.fonts.body).ceil() };
    (16.0 + tw + 28.0).max(32.0)
}

// --- Tooltips ----------------------------------------------------------------------------------------

pub const TOOLTIP_DELAY_MS: u32 = 550;

/// A tooltip under (or above, near the bottom edge) an anchor rect, clamped into `bounds`.
pub fn tooltip(g: &Gfx, text: &str, anchor: D2D_RECT_F, bounds: D2D_RECT_F) {
    let raised = theme::raised();
    let w = (g.measure(&wide(text), &g.fonts.caption) + 16.0).ceil();
    let h = 26.0;
    let mut x = (anchor.left + anchor.right) / 2.0 - w / 2.0;
    let mut y = anchor.bottom + 6.0;
    if y + h > bounds.bottom - 4.0 {
        y = anchor.top - 6.0 - h;
    }
    x = x.clamp(bounds.left + 4.0, (bounds.right - w - 4.0).max(bounds.left + 4.0));
    let r = rect(g.snap(x), g.snap(y), w, h);
    g.fill_round(r, 5.0, rgb(raised.popup));
    stroke(g, r, 5.0, rgb(raised.divider));
    label(g, text, &g.fonts.caption, r, rgb(raised.text), Align::Center);
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
        std::thread::sleep(std::time::Duration::from_millis(250));
        f.get(1, false);
        std::thread::sleep(std::time::Duration::from_millis(250));
        assert_eq!(f.get(1, false), 0.0);
        assert_eq!(f.hover(2, true), 1.0);
    }
}
