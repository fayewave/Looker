//! The Settings card, as Snapper has it: a card in the middle of the window over the viewer, which blurs and
//! dims behind it as the card fades in and grows from 96 %. A sidebar of sections at the left (General, Photos,
//! Video and audio, Appearance, Advanced, About; Faye, 2026-11-13); the picked section's rows at the right, each a
//! label with a hint under it (wrapped, never cut short) and its control at the right, divided by hairlines. A new section fades in with its rows rising 10 px;
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
fn item_h() -> f32 { crate::gfx::tsr(34.0) }
const ROW_H: f32 = 44.0;
const CARD_W: f32 = 800.0;
const CARD_H: f32 = 600.0;
const CARD_MAX_W: f32 = 960.0;
const CARD_MAX_H: f32 = 900.0;
const CARD_SHARE_W: f32 = 0.72;
const CARD_SHARE_H: f32 = 0.85;
/// Which theme a chip picks (its `Hit::ThemeChip` code's high bits): the one in use, or with Match Windows on, the
/// one for when Windows is dark or light.
const CHIPS_ANY: u8 = 0x00;
const CHIPS_DARK: u8 = 0x10;
const CHIPS_LIGHT: u8 = 0x20;
const PANE_LEFT: f32 = 28.0;
const PANE_RIGHT: f32 = 24.0;
const PANE_HEAD: f32 = 64.0;
/// A row's label and hint stop this far short of the right edge: the room for its control (a drop-down is 200).
const CONTROL_ROOM: f32 = 224.0;
/// The slideshow's choices, in seconds (a value set otherwise shows among them too).
const SLIDESHOW_CHOICES: [u32; 7] = [2, 3, 5, 10, 15, 30, 60];
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
    CheckerboardVideo,
    ThemeFollow,
    BarsOpaque,
    VideoAutoplay,
    VideoLoop,
    VideoMuted,
    VideoResume,
    Slideshow,
    StripOther,
    CleanClick,
    SubtitleSize,
    Passthrough,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub(super) enum Section {
    General,
    Photos,
    Video,
    Appearance,
    Advanced,
    /// The version, updates, links and licences.
    About,
}

impl Section {
    pub const ALL: [Section; 6] = [Section::General, Section::Photos, Section::Video, Section::Appearance, Section::Advanced, Section::About];

    fn label(self) -> &'static str {
        match self {
            Section::General => "General",
            Section::Photos => "Photos",
            Section::Video => "Video and audio",
            Section::Appearance => "Appearance",
            Section::Advanced => "Advanced",
            Section::About => "About",
        }
    }

    fn icon(self) -> u16 {
        match self {
            Section::General => 0xE713,
            Section::Photos => 0xE91B,
            Section::Video => 0xE714,
            Section::Appearance => 0xE790,
            Section::Advanced => 0xE9E9,
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
    pub const LOGS: u8 = 6;
    pub const LICENCES: u8 = 7;
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

/// The slideshow's choices: `SLIDESHOW_CHOICES`, and the value in use if it's another.
fn slideshow_choices(s: &Settings) -> Vec<u32> {
    let mut v = SLIDESHOW_CHOICES.to_vec();
    let now = s.slideshow_seconds.clamp(1, 120);
    if !v.contains(&now) {
        v.push(now);
        v.sort_unstable();
    }
    v
}

/// The choices of a drop-down setting, and which is picked.
fn choices(s: &Settings, which: Setting) -> (Vec<String>, usize) {
    let owned = |v: &[&str]| v.iter().map(|l| l.to_string()).collect::<Vec<_>>();
    match which {
        Setting::Wheel => (owned(&["Zoom in and out", "Next and previous file"]), s.wheel_navigates as usize),
        Setting::Zoom => (owned(&["The mouse pointer", "The middle of the view"]), s.zoom_center as usize),
        Setting::Cache => (
            owned(&["128 MB", "256 MB", "512 MB", "1 GB", "2 GB"]),
            CACHE_CHOICES.iter().position(|&m| m >= s.cache_mb).unwrap_or(2),
        ),
        Setting::VideoLoop => (
            owned(&["Never", "5 seconds", "10 seconds", "30 seconds", "1 minute", "Always"]),
            settings::LOOP_CHOICES.iter().position(|&c| c == s.video_loop_secs).unwrap_or(2),
        ),
        Setting::Slideshow => {
            let v = slideshow_choices(s);
            let now = s.slideshow_seconds.clamp(1, 120);
            (v.iter().map(|&n| if n == 1 { "1 second".into() } else { format!("{n} seconds") }).collect(), v.iter().position(|&n| n == now).unwrap_or(0))
        }
        Setting::SubtitleSize => (
            settings::SUBTITLE_SIZES.iter().map(|&p| if p == 100 { "Normal".into() } else { format!("{p}%") }).collect(),
            settings::SUBTITLE_SIZES.iter().position(|&p| p == s.subtitle_size).unwrap_or(1),
        ),
        _ => (Vec::new(), 0),
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
        self.row_room(g, label, hint, CONTROL_ROOM, 0.0)
    }

    /// A row whose control is a switch: its text runs nearly to the edge.
    fn switch_row(&mut self, g: &Gfx, label: &str, hint: &str) -> D2D_RECT_F {
        self.row_room(g, label, hint, ui::SWITCH_W + 32.0, 0.0)
    }

    /// A row whose control is a text button.
    fn button_row(&mut self, g: &Gfx, label: &str, hint: &str) -> D2D_RECT_F {
        self.row_room(g, label, hint, 168.0, 0.0)
    }

    /// A row with `extra` room under its label for more controls (the divider goes below that). Returns the
    /// label part, as `row` does.
    fn row_with(&mut self, g: &Gfx, label: &str, hint: &str, extra: f32) -> D2D_RECT_F {
        self.row_room(g, label, hint, CONTROL_ROOM, extra)
    }

    fn row_room(&mut self, g: &Gfx, label: &str, hint: &str, room: f32, extra: f32) -> D2D_RECT_F {
        let t = theme::raised();
        // The label and hint run up to the control's room; a long hint wraps and the row grows (they were cut
        // short with an ellipsis at half the width: Faye, 2026-11-13).
        let tw = (self.w - room).max(160.0);
        let hint_h = if hint.is_empty() { 0.0 } else { g.measure_height(&wide(hint), &g.fonts.caption_wrap, tw).ceil() };
        // The label's line, and the row, grow with the text size.
        let (row_h, line) = (crate::gfx::tsr(ROW_H), crate::gfx::tsr(19.0));
        let block = line + hint_h;
        let h = if hint.is_empty() { row_h } else { (row_h + 10.0).max(block + 19.0) };
        let r = rect(self.x, self.y, self.w, h);
        if hint.is_empty() {
            g.text(&wide(label), &g.fonts.body, rect(r.left, r.top, tw, h), rgb(t.text), Align::Left);
        } else {
            // The label and hint centred in the row, a DIP high: the room above the label's capitals then matches
            // the room under the hint's baseline (they sat 6 from the top and 13 from the bottom: Faye, 2026-11-13).
            let top = r.top + ((h - block) / 2.0 - 1.0).round();
            g.text(&wide(label), &g.fonts.body, rect(r.left, top, tw, line + 1.0), rgb(t.text), Align::Left);
            g.text(&wide(hint), &g.fonts.caption_wrap, rect(r.left, top + line, tw, hint_h), rgb(t.label), Align::Left);
        }
        g.hline(r.left, g.snap((r.bottom + extra - g.px()).floor()), self.w, rgb(t.divider));
        self.y += h + extra;
        r
    }

    fn heading(&mut self, g: &Gfx, text: &str) {
        self.y += 18.0;
        ui::section(g, text, rect(self.x, self.y, self.w, crate::gfx::tsr(20.0)));
        self.y += crate::gfx::tsr(24.0);
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
        let section = if matches!(self.update, UpdateStatus::Available) { Section::About } else { Section::General };
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
        let entries = items.iter().enumerate().map(|(i, l)| menus::check(menus::Action::Choose(i), l.as_str(), i == sel)).collect();
        let (w, h) = self.size_dip();
        self.menu = Some((menus::MenuKind::Combo(which), ui::Menu::below(g, entries, r, rect(0.0, 0.0, w, h))));
        self.hide_tooltip();
        self.invalidate();
    }

    /// A drop-down choice was picked: applied at once and saved.
    pub(super) fn choose(&mut self, which: Setting, i: usize) {
        if which == Setting::SubtitleSize {
            self.subtitle_size(settings::SUBTITLE_SIZES[i.min(settings::SUBTITLE_SIZES.len() - 1)]);
            return;
        }
        let slideshow = slideshow_choices(&self.settings);
        let s = &mut self.settings;
        match which {
            Setting::Wheel => s.wheel_navigates = i == 1,
            Setting::Zoom => s.zoom_center = i == 1,
            Setting::Cache => {
                s.cache_mb = CACHE_CHOICES[i.min(CACHE_CHOICES.len() - 1)];
                self.viewer.set_budget((s.cache_mb as usize) << 20);
            }
            Setting::VideoLoop => s.video_loop_secs = settings::LOOP_CHOICES[i.min(settings::LOOP_CHOICES.len() - 1)],
            Setting::Slideshow => s.slideshow_seconds = slideshow[i.min(slideshow.len() - 1)],
            _ => {}
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
            Setting::CheckerboardVideo => s.checkerboard_video = !s.checkerboard_video,
            Setting::BarsOpaque => s.bars_opaque = !s.bars_opaque,
            Setting::ThemeFollow => {
                s.theme_follow = !s.theme_follow;
                // The theme in use becomes the pick for its side, so nothing changes until Windows does (Snapper's).
                if s.theme_follow {
                    if s.theme.theme().dark {
                        s.theme_dark = s.theme;
                    } else {
                        s.theme_light = s.theme;
                    }
                    s.follow_windows();
                }
                settings::save(&self.settings);
                self.apply_theme();
                return;
            }
            Setting::StripOther => s.strip_other_files = !s.strip_other_files,
            Setting::CleanClick => {
                s.click_hides_ui = !s.click_hides_ui;
                // Off brings the UI back if it's away.
                if !s.click_hides_ui {
                    self.clean_view = false;
                }
            }
            Setting::Passthrough => {
                self.toggle_passthrough();
                return;
            }
            Setting::VideoAutoplay => s.video_autoplay = !s.video_autoplay,
            Setting::VideoMuted => s.video_start_muted = !s.video_start_muted,
            Setting::VideoResume => s.video_resume = !s.video_resume,
            _ => {}
        }
        settings::save(&self.settings);
        self.invalidate();
    }

    /// A theme chip was clicked: `code` is its side (`CHIPS_*`) and its place in `ThemeId::ALL`.
    pub(super) fn pick_theme(&mut self, code: usize) {
        let (side, i) = (code as u8 & 0xF0, code & 0x0F);
        let Some(id) = ThemeId::ALL.get(i).copied() else { return };
        let s = &mut self.settings;
        match side {
            CHIPS_DARK => s.theme_dark = id,
            CHIPS_LIGHT => s.theme_light = id,
            _ => s.theme = id,
        }
        s.follow_windows();
        settings::save(&self.settings);
        self.apply_theme();
    }

    /// Puts the settings' theme in use, the window frame included.
    pub(super) fn apply_theme(&mut self) {
        self.set_accent_globals();
        let changed = theme::id() != self.settings.theme;
        theme::set(self.settings.theme);
        if changed {
            self.viewer.theme_changed();
        }
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
            link::LOGS => {
                shell_open(&std::env::temp_dir().to_string_lossy());
            }
            link::LICENCES => {
                // Beside Looker.exe; the exe's own folder if a dev build has no codecs.
                if let Some(dir) = std::env::current_exe().ok().and_then(|e| e.parent().map(|p| p.to_path_buf())) {
                    let lic = dir.join("codecs").join("licenses");
                    shell_open(&if lic.is_dir() { lic } else { dir }.to_string_lossy());
                }
            }
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
        let v = self.settings.default_view();
        self.adopt_view(v);
        self.explorer.resort(self.explorer.look.sort, self.explorer.look.group);
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
        moving |= self.draw_accent_picker(g, card);
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
            let r = rect(side.left + 12.0, y, SIDEBAR_W - 24.0, item_h());
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
            g.text(&[sec.icon()], &g.fonts.small_icons, rect(r.left + 6.0, r.top, 24.0, item_h()), fg, Align::Center);
            g.text(&wide(sec.label()), &g.fonts.body, rect(r.left + 38.0, r.top, r.right - r.left - 42.0, item_h()), fg, Align::Left);
            // The update waiting, marked where it is.
            if sec == Section::About && self.update == UpdateStatus::Available {
                g.fill_round(rect(r.right - 16.0, r.top + (item_h() - 7.0) / 2.0, 7.0, 7.0), 3.5, rgb(t.accent));
            }
            y += item_h();
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
            Section::General => self.page_general(g, &mut rows, open),
            Section::Photos => self.page_photos(g, &mut rows, open),
            Section::Video => self.page_video(g, &mut rows, open),
            Section::Appearance => self.page_appearance(g, &mut rows, open),
            Section::Advanced => self.page_advanced(g, &mut rows, open),
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
            ui::scroll_thumb(g, rect(view.right - 2.0 - ui::SCROLL_W, ty, ui::SCROLL_W, thumb_h), held);
        }
        e < 1.0
    }

    fn page_general(&mut self, g: &Gfx, rows: &mut Rows, open: bool) {
        let r = rows.button_row(g, "Default photo viewer", "Open photos in Looker when you double-click them in File Explorer");
        self.text_button(g, r, "Set as default", Hit::PageLink(link::DEFAULT_APP), ui::Kind::Standard, open);
        let r = rows.switch_row(g, "Remember window size and position", "Off: Looker opens a standard-size window in the middle of the screen");
        self.switch(g, r, Setting::Window, self.settings.remember_window, open);
        let r = rows.switch_row(g, "Show recent files on Home", "Turning this off also clears the list");
        self.switch(g, r, Setting::Recents, self.settings.recents_enabled, open);
    }

    fn page_photos(&mut self, g: &Gfx, rows: &mut Rows, open: bool) {
        let r = rows.row(g, "Mouse wheel", "Hold Ctrl and the wheel always zooms");
        self.combo(g, right_part(r, 200.0, 30.0), Setting::Wheel, open);
        let r = rows.row(g, "Zoom towards", "Where the wheel and a double-click zoom in. Ctrl + and Ctrl − always zoom into the middle");
        self.combo(g, right_part(r, 200.0, 30.0), Setting::Zoom, open);
        let r = rows.switch_row(g, "Click a photo to hide everything else", "The toolbar, panels and status bar fade away; click again to bring them back. A double-click still zooms");
        self.switch(g, r, Setting::CleanClick, self.settings.click_hides_ui, open);
        let r = rows.row(g, "Slideshow speed", "How long each photo stays on screen");
        self.combo(g, right_part(r, 200.0, 30.0), Setting::Slideshow, open);
        let r = rows.switch_row(g, "Show other files in the thumbnail strip", "Files Looker can't open appear greyed out, and the arrow keys stop on them too");
        self.switch(g, r, Setting::StripOther, self.settings.strip_other_files, open);
    }

    fn page_video(&mut self, g: &Gfx, rows: &mut Rows, open: bool) {
        let r = rows.switch_row(g, "Play videos when opened", "Off: a video opens paused on its first frame");
        self.switch(g, r, Setting::VideoAutoplay, self.settings.video_autoplay, open);
        let r = rows.row(g, "Loop videos shorter than", "Short clips repeat like an animated GIF. The loop button changes it for one video");
        self.combo(g, right_part(r, 200.0, 30.0), Setting::VideoLoop, open);
        let r = rows.switch_row(g, "Start videos muted", "Press M or the speaker button to turn the sound on");
        self.switch(g, r, Setting::VideoMuted, self.settings.video_start_muted, open);
        let r = rows.switch_row(g, "Resume long videos", "Videos over 5 minutes open where you stopped watching");
        self.switch(g, r, Setting::VideoResume, self.settings.video_resume, open);
        let r = rows.row(g, "Subtitle size", "");
        self.combo(g, right_part(r, 200.0, 30.0), Setting::SubtitleSize, open);
        let r = rows.switch_row(g, "Send surround sound to a receiver", "Dolby and DTS sound goes untouched to an AV receiver, when the sound output accepts it");
        self.switch(g, r, Setting::Passthrough, self.settings.video_passthrough, open);
        rows.note(g, "Over a video the mouse wheel changes the volume; hold Ctrl to zoom.");
    }

    fn page_appearance(&mut self, g: &Gfx, rows: &mut Rows, open: bool) {
        // Snapper's: one theme, or (matching Windows) one for when Windows is dark and one for when it's light.
        let r = rows.switch_row(g, "Match Windows", "Switch between a dark and a light theme when Windows does");
        self.switch(g, r, Setting::ThemeFollow, self.settings.theme_follow, open);
        if self.settings.theme_follow {
            self.theme_chips(g, rows, "When Windows is dark", CHIPS_DARK, open);
            self.theme_chips(g, rows, "When Windows is light", CHIPS_LIGHT, open);
        } else {
            self.theme_chips(g, rows, "Theme", CHIPS_ANY, open);
        }
        self.accent_chips(g, rows, open);
        let r = rows.row_room(g, "Text size", "Bigger or smaller text everywhere in Looker; icons stay as they are", 144.0, 0.0);
        self.text_size_stepper(g, right_part(r, 120.0, 30.0), open);
        let r = rows.switch_row(g, "Solid status bar", "A solid bar behind the file details at the bottom. Off: they sit on the photo");
        self.switch(g, r, Setting::BarsOpaque, self.settings.bars_opaque, open);
        // Both checkerboards here, with the rest of how things look (Faye, 2026-11-13).
        let r = rows.switch_row(g, "Checkerboard behind transparency", "Shows which parts of a picture are see-through. Off: a plain background");
        self.switch(g, r, Setting::Checkerboard, self.settings.checkerboard, open);
        let r = rows.switch_row(g, "Checkerboard behind videos", "Off: a plain background");
        self.switch(g, r, Setting::CheckerboardVideo, self.settings.checkerboard_video, open);
    }

    fn page_advanced(&mut self, g: &Gfx, rows: &mut Rows, open: bool) {
        let r = rows.row(g, "Memory for photos", "Photos you've opened stay in memory so going back is instant. Raise it for big RAW or HEIC files, lower it if the PC is short on memory");
        self.combo(g, right_part(r, 200.0, 30.0), Setting::Cache, open);
        let r = rows.button_row(g, "Logs", "Helpful when reporting a problem");
        self.text_button(g, r, "Open folder", Hit::PageLink(link::LOGS), ui::Kind::Standard, open);
        let r = rows.button_row(g, "Reset Looker", "Every setting back to its default. Your files aren't touched");
        self.text_button(g, r, "Reset\u{2026}", Hit::PageLink(link::RESET), ui::Kind::Standard, open);
    }

    /// A row of theme chips under `label`: the dark themes (`CHIPS_DARK`), the light ones, or all (`CHIPS_ANY`),
    /// each ticked as the theme for that side.
    fn theme_chips(&mut self, g: &Gfx, rows: &mut Rows, label: &str, side: u8, open: bool) {
        const CHIP_W: f32 = 116.0;
        const CHIP_H: f32 = 34.0;
        let r = rows.row_with(g, label, "", CHIP_H + 14.0);
        let chosen = match side {
            CHIPS_DARK => self.settings.theme_dark,
            CHIPS_LIGHT => self.settings.theme_light,
            _ => self.settings.theme,
        };
        let mut cx = r.left;
        for (i, id) in ThemeId::ALL.into_iter().enumerate() {
            if (side == CHIPS_DARK && !id.theme().dark) || (side == CHIPS_LIGHT && id.theme().dark) {
                continue;
            }
            let c = rect(cx, r.bottom, CHIP_W, CHIP_H);
            let hit = Hit::ThemeChip(side | i as u8);
            if open {
                self.hits.add(hit, c);
            }
            let st = self.state(hit, open);
            ui::theme_chip(g, c, id.name(), chosen == id, &st, Some(id.theme().content));
            cx += CHIP_W + 6.0;
        }
    }

    /// The accent chips, each a square of its colour as it is in the theme in use, on a line (or more, if
    /// narrow): Snapper's (Faye, 2026-11-10). Custom opens its colour picker.
    fn accent_chips(&mut self, g: &Gfx, rows: &mut Rows, open: bool) {
        const CHIP_W: f32 = 116.0;
        const CHIP_H: f32 = 34.0;
        const GAP: f32 = 6.0;
        let n = theme::Accent::PICKER.len();
        let per_line = (((rows.w + GAP) / (CHIP_W + GAP)).floor() as usize).max(1);
        let lines = n.div_ceil(per_line);
        let r = rows.row_with(g, "Accent", "", lines as f32 * (CHIP_H + GAP) + 8.0);
        let id = self.settings.theme;
        for (i, a) in theme::Accent::PICKER.into_iter().enumerate() {
            let (col, line) = ((i % per_line) as f32, (i / per_line) as f32);
            let c = rect(r.left + col * (CHIP_W + GAP), r.bottom + line * (CHIP_H + GAP), CHIP_W, CHIP_H);
            let hit = Hit::AccentChip(i as u8);
            if open {
                self.hits.add(hit, c);
            }
            let st = self.state(hit, open);
            // Custom shows as an empty outline (Faye, 2026-11-10): its colour is what you pick.
            let swatch = (a != theme::Accent::Custom).then(|| a.colour(id));
            ui::theme_chip(g, c, a.name(), self.settings.accent == a, &st, swatch);
        }
    }

    /// The version and its update, under the logo.
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
        let r = rows.row(g, &format!("Version {} \u{00B7} {}", env!("LOOKER_VERSION"), std::env::consts::ARCH), status);
        let idle = !matches!(self.update, UpdateStatus::Checking | UpdateStatus::Installing);
        self.text_button(g, r, "Check now", Hit::PageLink(link::CHECK), ui::Kind::Standard, open && idle);
        if matches!(self.update, UpdateStatus::Available | UpdateStatus::Failed) {
            let (label, hint) = if github { ("Update now", "Looker closes and opens again") } else { ("Open the Store", "Get it from the Microsoft Store now") };
            let r = rows.button_row(g, "Install the update", hint);
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
        rows.y += LOGO_H + 12.0;
        self.page_updates(g, rows, open);
        rows.heading(g, "More");
        let r = rows.button_row(g, "Source code", "github.com/fayewave/Looker");
        self.text_button(g, r, "Open GitHub", Hit::PageLink(link::GITHUB), ui::Kind::Standard, open);
        let r = rows.button_row(g, "Microsoft Store", "Rate Looker, or install it on another PC");
        self.text_button(g, r, "Open Store", Hit::PageLink(link::STORE), ui::Kind::Standard, open);
        let r = rows.button_row(g, "Open-source licences", "The components Looker is built with, and their licences");
        self.text_button(g, r, "View licences", Hit::PageLink(link::LICENCES), ui::Kind::Standard, open);
        // Every component Build-Codecs ships (its codecs\licenses folder), with the font (2026-11-13).
        rows.note(g, "Inter (SIL Open Font License); libheif, libde265, dav1d and libavif for HEIC and AVIF; LibRaw, Little CMS and JasPer for camera RAW; FFmpeg, mpg123, libogg, libvorbis and liblc3 for video and sound; libass, FreeType, HarfBuzz, FriBidi, libaribcaption and libzvbi for subtitles; libopenmpt, FluidSynth and the GeneralUser GS sound bank for music files; libarchive, zlib, bzip2, liblzma, LZ4 and Zstandard for archives. Portions of this software are copyright \u{00A9} The FreeType Project (www.freetype.org).");
    }

    /// The text size: Transient's stepper, − 100% +, in 5 % steps (the menus' Height stepper, at a row's size).
    fn text_size_stepper(&mut self, g: &Gfx, b: D2D_RECT_F, open: bool) {
        let (lo, hi, _) = settings::TEXT_SCALES;
        let v = self.settings.text_scale;
        let can = [v > lo + 1e-3, v < hi - 1e-3];
        let (minus, plus) = ui::stepper_buttons(b);
        // The number between the buttons drags left and right, a step every 6 DIPs (Transient's).
        let mid = D2D_RECT_F { left: minus.right, right: plus.left, ..b };
        if open {
            self.hits.add(Hit::TextSizeStep(2), mid);
        }
        let mut hot = [0.0; 2];
        for (k, r) in [(0usize, minus), (1, plus)] {
            let id = Hit::TextSizeStep(k as u8);
            if open && can[k] {
                self.hits.add(id, r);
            }
            hot[k] = self.state(id, open && can[k]).hover;
        }
        ui::stepper(g, b, &format!("{:.0}%", v * 100.0), can, hot);
    }

    /// The stepper's number dragged to `x`: a step for every 6 DIPs from where the press began.
    pub(super) fn text_size_drag_to(&mut self, x: f32) {
        let Some((x0, v0)) = self.text_size_drag else { return };
        let (lo, hi, step) = settings::TEXT_SCALES;
        let v = ((v0 + ((x - x0) / 6.0).round() * step).clamp(lo, hi) * 100.0).round() / 100.0;
        if v != self.settings.text_scale {
            self.settings.text_scale = v;
            self.invalidate();
        }
    }

    /// The stepper's − (0) or +: a step smaller or bigger. The fonts ease to it.
    pub(super) fn text_size_step(&mut self, k: u8) {
        let (lo, hi, step) = settings::TEXT_SCALES;
        let v = self.settings.text_scale + if k == 0 { -step } else { step };
        self.settings.text_scale = ((v.clamp(lo, hi)) * 100.0).round() / 100.0;
        settings::save(&self.settings);
        self.invalidate();
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
        ui::dropdown(g, r, items.get(sel).map_or("", String::as_str), &st, menu_open);
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

