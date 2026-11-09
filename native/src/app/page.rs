//! The Settings card, as Snapper has it: a card in the middle of the window over the viewer, which blurs and
//! dims behind it as the card fades in and grows from 96 %. A sidebar of sections at the left (Viewing,
//! Appearance, General, Updates, About); the picked section's rows at the right, each a label with a hint under
//! it and its control at the right, divided by hairlines. A new section fades in with its rows rising 10 px;
//! the rows scroll on a spring when they don't fit. Every control applies the moment it changes; there is no
//! Save. Escape, the close button, Ctrl+, or a press outside the card puts it away.
//!
//! While it is open every shortcut but Escape and Ctrl+, is off, and the viewer behind takes no input.

use std::time::Instant;

use super::*;
use crate::motion::{self, Ease};
use crate::settings::CACHE_CHOICES;
use crate::store::UpdateStatus;
use crate::theme::{self, ThemeId};

const SIDEBAR_W: f32 = 210.0;
const ITEM_H: f32 = 34.0;
const ROW_H: f32 = 44.0;
const CARD_W: f32 = 800.0;
const CARD_H: f32 = 600.0;
const CARD_MAX_W: f32 = 960.0;
const CARD_MAX_H: f32 = 900.0;
const CARD_SHARE_W: f32 = 0.72;
const CARD_SHARE_H: f32 = 0.85;
const PANE_LEFT: f32 = 28.0;
const PANE_RIGHT: f32 = 24.0;
const PANE_HEAD: f32 = 64.0;
/// The backdrop's blur, fully in (DIPs).
const BACKDROP_BLUR: f32 = 6.0;
const WHEEL_STEP: f32 = 110.0;

const GITHUB: &str = "https://github.com/fayewave/Looker";
const STORE: &str = "ms-windows-store://pdp/?productid=9NV130N4C2GZ";

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub(super) enum Setting {
    Wheel,
    Zoom,
    Cache,
    Recents,
    Window,
    Checkerboard,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub(super) enum Section {
    Viewing,
    Appearance,
    General,
    Updates,
    About,
}

impl Section {
    pub const ALL: [Section; 5] = [Section::Viewing, Section::Appearance, Section::General, Section::Updates, Section::About];

    fn label(self) -> &'static str {
        match self {
            Section::Viewing => "Viewing",
            Section::Appearance => "Appearance",
            Section::General => "General",
            Section::Updates => "Updates",
            Section::About => "About",
        }
    }

    fn icon(self) -> u16 {
        match self {
            Section::Viewing => 0xE91B,
            Section::Appearance => 0xE790,
            Section::General => 0xE713,
            Section::Updates => 0xE895,
            Section::About => 0xE946,
        }
    }
}

/// Link buttons, by what they do.
pub(super) mod link {
    pub const GITHUB: u8 = 0;
    pub const STORE: u8 = 1;
    pub const RESET: u8 = 2;
    pub const UPDATE_NOW: u8 = 3;
    pub const CHECK: u8 = 4;
    pub const DEFAULT_APP: u8 = 5;
}

pub(super) struct Page {
    /// False while it fades away (it takes no input then).
    pub open: bool,
    shown: Ease<1>,
    section: Section,
    switched: Instant,
    /// The rows' scroll: where it's going, and where it is, how fast, and when that was (a spring).
    target: f32,
    pos: f32,
    vel: f32,
    then: Instant,
    content_h: f32,
    view_h: f32,
    /// Dragging the scrollbar: the pointer's offset from where the thumb's travel starts.
    thumb_drag: Option<f32>,
}

impl Page {
    pub fn new(section: Section) -> Page {
        let mut shown = Ease::at([0.0]);
        let now = Instant::now();
        shown.set([1.0], now);
        Page { open: true, shown, section, switched: now - std::time::Duration::from_secs(1), target: 0.0, pos: 0.0, vel: 0.0, then: now, content_h: 0.0, view_h: 0.0, thumb_drag: None }
    }

    fn max_scroll(&self) -> f32 {
        (self.content_h - self.view_h).max(0.0)
    }

    /// Scrolling follows a critically damped spring rather than the ease: each wheel notch moves the target and
    /// the speed carries on smoothly instead of restarting the curve. True while it moves.
    fn step_scroll(&mut self) -> bool {
        const OMEGA: f32 = 38.0;
        let now = Instant::now();
        self.target = self.target.clamp(0.0, self.max_scroll());
        if !motion::enabled() {
            self.pos = self.target;
            self.vel = 0.0;
            self.then = now;
            return false;
        }
        let dt = now.saturating_duration_since(self.then).as_secs_f32().min(0.05);
        self.then = now;
        let x = self.pos - self.target;
        let k = (-OMEGA * dt).exp();
        let b = self.vel + OMEGA * x;
        self.pos = self.target + (x + b * dt) * k;
        self.vel = (self.vel - OMEGA * b * dt) * k;
        if (self.pos - self.target).abs() < 0.25 && self.vel.abs() < 4.0 {
            self.pos = self.target;
            self.vel = 0.0;
            return false;
        }
        true
    }
}

/// The choices of a drop-down setting, and which is picked.
fn choices(s: &Settings, which: Setting) -> (Vec<&'static str>, usize) {
    match which {
        Setting::Wheel => (vec!["Zoom in and out", "Next and previous image"], s.wheel_navigates as usize),
        Setting::Zoom => (vec!["The mouse pointer", "The middle of the view"], s.zoom_center as usize),
        Setting::Cache => (
            vec!["128 MB", "256 MB", "512 MB", "1 GB", "2 GB"],
            CACHE_CHOICES.iter().position(|&m| m >= s.cache_mb).unwrap_or(2),
        ),
        Setting::Recents | Setting::Window | Setting::Checkerboard => (Vec::new(), 0),
    }
}

/// Lays rows out down the pane.
struct Rows {
    x: f32,
    w: f32,
    y: f32,
}

impl Rows {
    /// A row with `label` (and a hint under it, if any), its divider below; returns the row rect.
    fn row(&mut self, g: &Gfx, label: &str, hint: &str) -> D2D_RECT_F {
        self.row_with(g, label, hint, 0.0)
    }

    /// A row with `extra` room under its label for more controls (the divider goes below that). Returns the
    /// label part, as `row` does.
    fn row_with(&mut self, g: &Gfx, label: &str, hint: &str, extra: f32) -> D2D_RECT_F {
        let t = theme::raised();
        let h = if hint.is_empty() { ROW_H } else { ROW_H + 10.0 };
        let r = rect(self.x, self.y, self.w, h);
        if hint.is_empty() {
            g.text(&wide(label), &g.fonts.body, rect(r.left, r.top, self.w * 0.5, h), rgb(t.text), Align::Left);
        } else {
            g.text(&wide(label), &g.fonts.body, rect(r.left, r.top + 6.0, self.w * 0.5, 20.0), rgb(t.text), Align::Left);
            g.text(&wide(hint), &g.fonts.caption, rect(r.left, r.top + 25.0, self.w * 0.55, 16.0), rgb(t.label), Align::Left);
        }
        g.hline(r.left, g.snap((r.bottom + extra - g.px()).floor()), self.w, rgb(t.divider));
        self.y += h + extra;
        r
    }

    fn heading(&mut self, g: &Gfx, text: &str) {
        self.y += 18.0;
        ui::section(g, text, rect(self.x, self.y, self.w, 20.0));
        self.y += 24.0;
    }

    /// Wrapped small text in the label colour, under the rows.
    fn note(&mut self, g: &Gfx, text: &str) {
        let h = g.measure_height(&wide(text), &g.fonts.caption_wrap, self.w).ceil();
        g.text(&wide(text), &g.fonts.caption_wrap, rect(self.x, self.y + 10.0, self.w, h), rgb(theme::raised().label), Align::Left);
        self.y += h + 14.0;
    }
}

/// A `w`-wide slice from `r`'s right edge, `h` tall, centred vertically.
fn right_part(r: D2D_RECT_F, w: f32, h: f32) -> D2D_RECT_F {
    rect(r.right - w, r.top + (r.bottom - r.top - h) / 2.0, w, h)
}

impl App {
    pub(super) fn theme(&self) -> &'static theme::Theme {
        theme::t()
    }

    /// The Settings card is up and taking input (not on its way out).
    pub(super) fn page_open(&self) -> bool {
        self.page.as_ref().is_some_and(|p| p.open)
    }

    pub(super) fn open_page(&mut self) {
        // An update waiting is what the toolbar's dot points at: open on it.
        let section = if matches!(self.update, UpdateStatus::Available) { Section::Updates } else { Section::Viewing };
        self.open_page_at(section);
    }

    fn open_page_at(&mut self, section: Section) {
        self.stop_slideshow();
        // The Updates section asks again unless an answer that needs action is in.
        if matches!(self.update, UpdateStatus::Unknown | UpdateStatus::UpToDate) {
            self.check_updates();
        }
        self.close_menu();
        self.hide_tooltip();
        self.pressed = None;
        match &mut self.page {
            // Reopened on its way out: it comes back from where it is.
            Some(p) => {
                p.open = true;
                p.shown.set([1.0], Instant::now());
            }
            None => self.page = Some(Page::new(section)),
        }
        self.invalidate();
    }

    pub(super) fn close_page(&mut self) {
        if let Some(p) = &mut self.page {
            if p.open {
                p.open = false;
                p.thumb_drag = None;
                p.shown.set([0.0], Instant::now());
                self.close_menu();
                self.hide_tooltip();
                self.invalidate();
            }
        }
    }

    pub(super) fn toggle_page(&mut self) {
        if self.page_open() { self.close_page() } else { self.open_page() }
    }

    pub(super) fn page_section(&mut self, i: usize) {
        let Some(sec) = Section::ALL.get(i).copied() else { return };
        if let Some(p) = &mut self.page {
            if p.section != sec {
                p.section = sec;
                p.switched = Instant::now();
                p.target = 0.0;
                p.pos = 0.0;
                p.vel = 0.0;
                self.close_menu();
                self.invalidate();
            }
        }
    }

    pub(super) fn scroll_page(&mut self, delta: f32) {
        if let Some(p) = &mut self.page {
            p.target = (p.target - delta / 120.0 * WHEEL_STEP).clamp(0.0, p.max_scroll());
            self.invalidate();
        }
    }

    /// A press on the scrollbar: the thumb follows the pointer from here.
    pub(super) fn page_thumb_press(&mut self, y: f32) {
        let Some(view) = self.hits.rect_of(Hit::PageThumb) else { return };
        if let Some(p) = &mut self.page {
            let max = p.max_scroll();
            let thumb_h = (p.view_h * p.view_h / p.content_h.max(1.0)).max(28.0);
            let travel = (view.bottom - view.top - thumb_h).max(1.0);
            p.thumb_drag = Some(y - p.target / max.max(1.0) * travel);
        }
    }

    pub(super) fn page_thumb_drag(&mut self, y: f32) {
        let Some(view) = self.hits.rect_of(Hit::PageThumb) else { return };
        if let Some(p) = &mut self.page {
            let Some(from) = p.thumb_drag else { return };
            let thumb_h = (p.view_h * p.view_h / p.content_h.max(1.0)).max(28.0);
            let travel = view.bottom - view.top - thumb_h;
            if travel > 0.0 {
                p.target = ((y - from) / travel).clamp(0.0, 1.0) * p.max_scroll();
                p.pos = p.target;
                p.vel = 0.0;
            }
            self.invalidate();
        }
    }

    pub(super) fn page_thumb_release(&mut self) {
        if let Some(p) = &mut self.page {
            p.thumb_drag = None;
        }
    }

    /// A drop-down was clicked: its choices, hanging below it.
    pub(super) fn open_combo(&mut self, which: Setting) {
        let Some(r) = self.hits.rect_of(Hit::Combo(which)) else { return };
        let Some(g) = &self.gfx else { return };
        let (items, sel) = choices(&self.settings, which);
        let entries = items.iter().enumerate().map(|(i, l)| menus::check(menus::Action::Choose(i), l, i == sel)).collect();
        let (w, h) = self.size_dip();
        self.menu = Some((menus::MenuKind::Combo(which), ui::Menu::below(g, entries, r, rect(0.0, 0.0, w, h))));
        self.hide_tooltip();
        self.invalidate();
    }

    /// A drop-down choice was picked: applied at once and saved.
    pub(super) fn choose(&mut self, which: Setting, i: usize) {
        let s = &mut self.settings;
        match which {
            Setting::Wheel => s.wheel_navigates = i == 1,
            Setting::Zoom => s.zoom_center = i == 1,
            Setting::Cache => {
                s.cache_mb = CACHE_CHOICES[i.min(CACHE_CHOICES.len() - 1)];
                self.viewer.set_budget((s.cache_mb as usize) << 20);
            }
            Setting::Recents | Setting::Window | Setting::Checkerboard => {}
        }
        settings::save(&self.settings);
        self.invalidate();
    }

    pub(super) fn toggle_setting(&mut self, which: Setting) {
        let s = &mut self.settings;
        match which {
            Setting::Recents => {
                s.recents_enabled = !s.recents_enabled;
                if !s.recents_enabled {
                    s.recents.clear(); // off means off: nothing lingers in the file either
                    self.landing.stale();
                }
            }
            Setting::Window => {
                s.remember_window = !s.remember_window;
                if !s.remember_window {
                    s.window = None; // the next launch opens the default window
                }
            }
            Setting::Checkerboard => s.checkerboard = !s.checkerboard,
            _ => {}
        }
        settings::save(&self.settings);
        self.invalidate();
    }

    /// A theme chip was clicked.
    pub(super) fn pick_theme(&mut self, i: usize) {
        let Some(id) = ThemeId::ALL.get(i).copied() else { return };
        if self.settings.theme == id {
            return;
        }
        self.settings.theme = id;
        settings::save(&self.settings);
        self.apply_theme();
    }

    /// Puts the settings' theme in use, the window frame included.
    pub(super) fn apply_theme(&mut self) {
        theme::set(self.settings.theme);
        apply_frame_colours(self.hwnd);
        self.landing.stale();
        self.invalidate();
    }

    pub(super) fn page_link(&mut self, i: u8) {
        match i {
            link::GITHUB => {
                shell_open(GITHUB);
            }
            link::STORE => {
                if !shell_open(STORE) {
                    shell_open("https://apps.microsoft.com/detail/9NV130N4C2GZ");
                }
            }
            link::UPDATE_NOW => {
                if crate::store::appinstaller_uri().is_some() {
                    self.update = UpdateStatus::Installing;
                    crate::store::install_update(self.hwnd, self.current_path());
                    self.invalidate();
                } else {
                    shell_open(crate::store::STORE_UPDATES);
                }
            }
            link::CHECK => self.check_updates(),
            link::DEFAULT_APP => {
                let uri = crate::store::default_apps_uri();
                if !shell_open(&uri) {
                    shell_open("ms-settings:defaultapps");
                }
            }
            _ => self.confirm_reset(),
        }
    }

    pub(super) fn check_updates(&mut self) {
        if matches!(self.update, UpdateStatus::Checking | UpdateStatus::Installing) {
            return;
        }
        self.update = UpdateStatus::Checking;
        crate::store::check_updates(self.hwnd);
        self.invalidate();
    }

    pub(super) fn dismiss_default_hint(&mut self) {
        self.settings.default_hint_dismissed = true;
        settings::save(&self.settings);
        self.invalidate();
    }

    /// Reset Looker: every setting back to its default, the caches emptied, the saved window forgotten.
    fn confirm_reset(&mut self) {
        let body = "Every setting goes back to its default, the recent list and the decode cache are cleared, and the saved window size and position are forgotten. Your photos are not touched.";
        let buttons = vec![ui::DialogButton::new("Reset", actions::Choice::Confirm), ui::DialogButton::new("Cancel", actions::Choice::Cancel)];
        self.open_dialog_ui(actions::DialogKind::Reset, ui::Dialog::new("Reset Looker?".into(), body.into(), buttons, 1, false));
    }

    pub(super) fn reset_app(&mut self) {
        self.settings = Settings::default();
        settings::save(&self.settings);
        self.skip_placement_save = true;
        if let Some(dir) = settings::data_dir() {
            let _ = std::fs::remove_file(dir.join("wallpaper.png"));
        }
        self.viewer.set_budget((self.settings.cache_mb as usize) << 20);
        self.viewer.clear_cache();
        self.viewer.set_sort(self.settings.sort);
        self.explorer.resort(self.settings.sort, self.explorer.look.group);
        self.apply_theme();
        self.layout_changed();
        self.show_toast("Looker reset to defaults", false);
    }

    // --- Drawing ------------------------------------------------------------------------------------

    /// Draws the card (and the blur and dim behind it) over everything drawn so far. True while something on
    /// it moves.
    pub(super) fn draw_page(&mut self, g: &mut Gfx) -> bool {
        let Some(p) = &mut self.page else { return false };
        let now = Instant::now();
        let shown = p.shown.value(now)[0];
        let mut moving = p.shown.moving(now);
        if !p.open && !moving {
            self.page = None;
            return false;
        }
        moving |= p.step_scroll();
        let open = p.open;
        let (w, h) = self.size_dip();
        let window = rect(0.0, 0.0, w, h);
        // Under the card the viewer blurs as it comes in (dimmed only, should the blur fail).
        g.blur_backdrop(BACKDROP_BLUR * shown);
        g.fill(window, gfx::rgba(0x000000, 0.5 * shown));
        let body = rect(0.0, TITLE_H, w, h - TITLE_H);
        let cw = ((body.right - body.left) * CARD_SHARE_W).clamp(CARD_W, CARD_MAX_W).min(body.right - body.left - 32.0).max(480.0);
        let ch = ((body.bottom - body.top) * CARD_SHARE_H).clamp(CARD_H, CARD_MAX_H).min(body.bottom - body.top - 64.0).max(240.0);
        let card = rect(g.snap((w - cw) / 2.0), g.snap(body.top + ((body.bottom - body.top - ch) / 2.0).max(8.0)), cw, ch);
        if open {
            // Modal: the whole window is the card's, and a press outside it puts it away.
            self.hits.add(Hit::PageSurface, window);
            self.hits.add(Hit::PageCard, card);
        }
        let t = theme::raised();
        g.push_fade(shown, 0.96 + 0.04 * shown, (card.left + card.right) / 2.0, (card.top + card.bottom) / 2.0);
        ui::shadow(g, card, 10.0);
        // A step off the page's own colour, not the brighter menu grey.
        g.fill_round(card, 10.0, rgb(t.sidebar));
        ui::stroke(g, card, 10.0, rgb(t.divider));
        let side = rect(card.left, card.top, SIDEBAR_W, ch);
        self.draw_page_sidebar(g, side, open);
        let pane = D2D_RECT_F { left: side.right + 1.0, ..card };
        moving |= self.draw_page_content(g, pane, open);
        g.pop_fade();
        moving
    }

    fn draw_page_sidebar(&mut self, g: &Gfx, side: D2D_RECT_F, open: bool) {
        let t = theme::raised();
        g.fill(rect(side.right, side.top + 1.0, g.px(), side.bottom - side.top - 2.0), rgb(t.divider));
        g.text(&wide("Settings"), &g.fonts.page_title, rect(side.left + 22.0, side.top + 18.0, SIDEBAR_W - 34.0, 28.0), rgb(t.text), Align::Left);
        let current = self.page.as_ref().map(|p| p.section);
        let mut y = side.top + PANE_HEAD;
        for (i, sec) in Section::ALL.into_iter().enumerate() {
            let r = rect(side.left + 12.0, y, SIDEBAR_W - 24.0, ITEM_H);
            let id = Hit::PageNav(i as u8);
            if open {
                self.hits.add(id, r);
            }
            let on = self.fades.get(Hit::PageNavOn(i as u8), current == Some(sec));
            let hov = self.state(id, open).hover;
            // The picked section's tile is accent-tinted, as a button that's on; hover a neutral grey.
            if hov > 0.0 && on < 1.0 {
                g.fill_round(r, 5.0, gfx::rgba(t.button, hov * (1.0 - on)));
            }
            if on > 0.0 {
                g.fill_round(r, 5.0, gfx::rgba(t.selected, on));
            }
            let fg = theme::mix(t.label, t.text, on.max(hov));
            g.text(&[sec.icon()], &g.fonts.small_icons, rect(r.left + 6.0, r.top, 24.0, ITEM_H), fg, Align::Center);
            g.text(&wide(sec.label()), &g.fonts.body, rect(r.left + 38.0, r.top, r.right - r.left - 42.0, ITEM_H), fg, Align::Left);
            // The update waiting, marked where it is.
            if sec == Section::Updates && self.update == UpdateStatus::Available {
                g.fill_round(rect(r.right - 16.0, r.top + (ITEM_H - 7.0) / 2.0, 7.0, 7.0), 3.5, rgb(t.accent));
            }
            y += ITEM_H;
        }
    }

    /// The picked section's rows. True while they move.
    fn draw_page_content(&mut self, g: &Gfx, pane: D2D_RECT_F, open: bool) -> bool {
        let Some(p) = &self.page else { return false };
        let t = theme::raised();
        let section = p.section;
        let e = motion::progress(p.switched);
        let off = (p.pos / g.px()).round() * g.px();
        let x = pane.left + PANE_LEFT;
        let w = pane.right - pane.left - PANE_LEFT - PANE_RIGHT;
        let head = rect(x, pane.top + 18.0, w, 28.0);
        g.text(&wide(section.label()), &g.fonts.page_title, head, rgb(t.text), Align::Left);
        let close = rect(head.right - 30.0, head.top, 30.0, 28.0);
        if open {
            self.hits.add(Hit::PageClose, close);
        }
        let st = self.state(Hit::PageClose, open);
        let fg = ui::button_frame(g, close, ui::Kind::Subtle, &st);
        g.text(&[0xE8BB], &g.fonts.small_icons, close, fg, Align::Center);

        let view = D2D_RECT_F { left: pane.left, top: pane.top + PANE_HEAD, right: pane.right, bottom: pane.bottom - 12.0 };
        g.push_clip(view);
        g.push_fade(e, 1.0, 0.0, 0.0);
        let mut rows = Rows { x, w, y: view.top - off + (1.0 - e) * 10.0 };
        match section {
            Section::Viewing => self.page_viewing(g, &mut rows, open),
            Section::Appearance => self.page_appearance(g, &mut rows, open),
            Section::General => self.page_general(g, &mut rows, open),
            Section::Updates => self.page_updates(g, &mut rows, open),
            Section::About => self.page_about(g, &mut rows, open),
        }
        g.pop_fade();
        g.pop_clip();
        let used = rows.y + off - view.top + 16.0 - (1.0 - e) * 10.0;
        let Some(p) = &mut self.page else { return false };
        p.content_h = used;
        p.view_h = view.bottom - view.top;
        // The scrollbar, when the rows don't fit.
        if used > p.view_h + 0.5 {
            let max = used - p.view_h;
            let strip = rect(view.right - 10.0, view.top, 10.0, p.view_h);
            let thumb_h = (p.view_h * p.view_h / used).max(28.0);
            let ty = view.top + p.pos.clamp(0.0, max) / max * (p.view_h - thumb_h);
            let held = p.thumb_drag.is_some();
            if open {
                self.hits.add(Hit::PageThumb, strip);
            }
            ui::scroll_thumb(g, rect(view.right - 9.0, ty, 7.0, thumb_h), held);
        }
        e < 1.0
    }

    fn page_viewing(&mut self, g: &Gfx, rows: &mut Rows, open: bool) {
        let r = rows.row(g, "Mouse wheel over the image", "Ctrl + wheel always zooms");
        self.combo(g, right_part(r, 200.0, 30.0), Setting::Wheel, open);
        let r = rows.row(g, "Zoom towards", "Where the wheel and a double-click zoom into");
        self.combo(g, right_part(r, 200.0, 30.0), Setting::Zoom, open);
        rows.note(g, "Ctrl + and Ctrl - always zoom into the middle of the view.");
    }

    fn page_appearance(&mut self, g: &Gfx, rows: &mut Rows, open: bool) {
        const CHIP_W: f32 = 116.0;
        const CHIP_H: f32 = 34.0;
        let r = rows.row_with(g, "Theme", "", CHIP_H + 14.0);
        let mut cx = r.left;
        for (i, id) in ThemeId::ALL.into_iter().enumerate() {
            let c = rect(cx, r.bottom, CHIP_W, CHIP_H);
            let hit = Hit::ThemeChip(i as u8);
            if open {
                self.hits.add(hit, c);
            }
            let st = self.state(hit, open);
            ui::theme_chip(g, c, id.name(), self.settings.theme == id, &st, id.theme());
            cx += CHIP_W + 6.0;
        }
        let r = rows.row(g, "Checkerboard background", "Shows where a photo is transparent. Off: plain");
        self.switch(g, r, Setting::Checkerboard, self.settings.checkerboard, open);
    }

    fn page_general(&mut self, g: &Gfx, rows: &mut Rows, open: bool) {
        let r = rows.row(g, "Recent photos", "On the landing page. Off also forgets the list");
        self.switch(g, r, Setting::Recents, self.settings.recents_enabled, open);
        let r = rows.row(g, "Remember window size and position", "Off: a default window in the middle each launch");
        self.switch(g, r, Setting::Window, self.settings.remember_window, open);
        let r = rows.row(g, "Decode cache", "Memory for decoded photos, so going back is instant");
        self.combo(g, right_part(r, 200.0, 30.0), Setting::Cache, open);
        let r = rows.row(g, "Default photo viewer", "Open photos in Looker from File Explorer");
        self.text_button(g, r, "Set as default", Hit::PageLink(link::DEFAULT_APP), ui::Kind::Standard, open);
        rows.note(g, "Raise the decode cache for large RAW or HEIC files; lower it on a machine short on memory.");
    }

    fn page_updates(&mut self, g: &Gfx, rows: &mut Rows, open: bool) {
        let github = crate::store::appinstaller_uri().is_some();
        let status = match self.update {
            UpdateStatus::Checking => "Checking for updates\u{2026}",
            UpdateStatus::UpToDate => "Looker is up to date",
            UpdateStatus::Available => "A newer version is available",
            UpdateStatus::Installing => "Installing the update\u{2026}",
            UpdateStatus::Failed => "Couldn't install the update",
            UpdateStatus::Unknown => "Couldn't check for updates",
        };
        let r = rows.row(g, &format!("Looker {}", env!("CARGO_PKG_VERSION")), status);
        let idle = !matches!(self.update, UpdateStatus::Checking | UpdateStatus::Installing);
        self.text_button(g, r, "Check now", Hit::PageLink(link::CHECK), ui::Kind::Standard, open && idle);
        if matches!(self.update, UpdateStatus::Available | UpdateStatus::Failed) {
            let (label, hint) = if github { ("Update now", "Looker closes and opens again") } else { ("Open the Store", "Get it from the Microsoft Store now") };
            let r = rows.row(g, "Install the update", hint);
            self.text_button(g, r, label, Hit::PageLink(link::UPDATE_NOW), ui::Kind::Accent, open);
        }
        let note = match (self.update, github) {
            (UpdateStatus::Available, false) => "Windows installs Store updates automatically; open the Store to get it now.",
            (UpdateStatus::Available, true) => "It installs the next time Looker starts, or now: Looker closes and opens again.",
            (UpdateStatus::Installing, _) => "Looker closes and opens again when it's in.",
            (UpdateStatus::Failed, _) => "Windows tries again the next time Looker starts.",
            (_, false) => "Updates are delivered automatically through the Microsoft Store.",
            (_, true) => "Looker updates itself from GitHub when it starts.",
        };
        rows.note(g, note);
    }

    fn page_about(&mut self, g: &Gfx, rows: &mut Rows, open: bool) {
        const LOGO_H: f32 = 36.0;
        if let Some(bmp) = self.landing_wordmark(g) {
            let size = unsafe { bmp.GetPixelSize() };
            let lw = LOGO_H * size.width as f32 / size.height.max(1) as f32;
            g.draw_bitmap(&bmp, rect(rows.x, g.snap(rows.y + 6.0), lw, LOGO_H), 1.0);
        }
        rows.y += LOGO_H + 18.0;
        rows.row(g, &format!("Version {} \u{00B7} {}", env!("CARGO_PKG_VERSION"), std::env::consts::ARCH), "A fast, native photo viewer for Windows 11");
        rows.heading(g, "More");
        let r = rows.row(g, "Source code", "github.com/fayewave/Looker");
        self.text_button(g, r, "Open GitHub", Hit::PageLink(link::GITHUB), ui::Kind::Standard, open);
        let r = rows.row(g, "Microsoft Store", "Rate Looker, or get it on another PC");
        self.text_button(g, r, "Open Store", Hit::PageLink(link::STORE), ui::Kind::Standard, open);
        let r = rows.row(g, "Reset Looker", "Every setting to its default; photos aren't touched");
        self.text_button(g, r, "Reset\u{2026}", Hit::PageLink(link::RESET), ui::Kind::Standard, open);
        rows.note(g, "Uses the Inter typeface (SIL Open Font License), and, where Windows can't decode HEIC or AVIF itself, libheif and libde265 (LGPL-3.0) and dav1d (BSD-2-Clause), for animated AVIF libavif (BSD-2-Clause), and, where Windows lacks the Raw Image Extension, LibRaw (LGPL-2.1 or CDDL-1.0) with Little CMS (MIT), JasPer and zlib, and for video FFmpeg (LGPL-2.1 or later). The licences are in the codecs\\licenses folder beside Looker.exe.");
        let logs = format!("Logs: {}\\looker-native*.log", std::env::temp_dir().display().to_string().trim_end_matches('\\'));
        rows.note(g, &logs);
    }

    /// A drop-down at `r` for a setting.
    fn combo(&mut self, g: &Gfx, r: D2D_RECT_F, which: Setting, open: bool) {
        let id = Hit::Combo(which);
        if open {
            self.hits.add(id, r);
        }
        let st = self.state(id, open);
        let menu_open = matches!(&self.menu, Some((menus::MenuKind::Combo(c), _)) if *c == which);
        let (items, sel) = choices(&self.settings, which);
        ui::dropdown(g, r, items.get(sel).copied().unwrap_or(""), &st, menu_open);
    }

    /// A switch at the right of row `r`; the whole row toggles it.
    fn switch(&mut self, g: &Gfx, r: D2D_RECT_F, which: Setting, on: bool, open: bool) {
        let id = Hit::Toggle(which);
        if open {
            self.hits.add(id, r);
        }
        let st = self.state(id, open);
        let k = self.fades.get(Hit::ToggleAnim(which), on);
        ui::toggle_switch(g, right_part(r, ui::SWITCH_W, ui::SWITCH_H), k, &st);
    }

    /// A text button at the right of row `r`.
    fn text_button(&mut self, g: &Gfx, r: D2D_RECT_F, label: &str, id: Hit, kind: ui::Kind, enabled: bool) {
        let bw = (g.measure(&wide(label), &g.fonts.body) + 32.0).ceil().max(120.0);
        let b = right_part(r, bw, 30.0);
        let b = rect(g.snap(b.left), g.snap(b.top), bw, 30.0);
        if enabled {
            self.hits.add(id, b);
        }
        let st = self.state(id, enabled);
        let fg = ui::button_frame(g, b, kind, &st);
        g.text(&wide(label), &g.fonts.body, b, fg, Align::Center);
    }
}

/// The window frame in the theme's colours: dark or light caption handling, and the border in its divider.
pub(super) fn apply_frame_colours(hwnd: HWND) {
    use windows::Win32::Graphics::Dwm::DWMWA_BORDER_COLOR;
    let t = theme::t();
    unsafe {
        let dark = windows::core::BOOL(t.dark as i32);
        let _ = DwmSetWindowAttribute(hwnd, DWMWA_USE_IMMERSIVE_DARK_MODE, &dark as *const _ as _, size_of::<windows::core::BOOL>() as u32);
        let d = t.divider;
        let border = COLORREF(((d & 0xFF) << 16) | (d & 0xFF00) | ((d >> 16) & 0xFF));
        let _ = DwmSetWindowAttribute(hwnd, DWMWA_BORDER_COLOR, &border as *const _ as _, size_of::<COLORREF>() as u32);
    }
}

/// Opens a URL with the shell (the browser, the Store). False when nothing handled it.
pub(super) fn shell_open(url: &str) -> bool {
    unsafe { ShellExecuteW(None, w!("open"), &HSTRING::from(url), PCWSTR::null(), PCWSTR::null(), SW_SHOWNORMAL).0 as isize > 32 }
}

