//! The Settings page (MainWindow.ShowSettingsPage + Controls/AppPage): a full-window page over the viewport,
//! strip and status row, with a Back button. Every control applies the moment it changes; there is no Save.
//! The page is responsive at one breakpoint, like the C# one: below 1008 DIPs one 560-wide column with About
//! under the preferences; at or above it the two side by side, each 480 wide with a 48 gap.
//!
//! While it is open the image toolbar and every shortcut but Escape are off, as in the C# app.

use super::*;
use crate::settings::CACHE_CHOICES;
use crate::store::UpdateStatus;

const PAD_X: f32 = 24.0;
const PAD_TOP: f32 = 16.0;
const PAD_BOTTOM: f32 = 20.0;
const ONE_COLUMN: f32 = 560.0;
const COLUMN: f32 = 480.0;
const GAP: f32 = 48.0;
const TWO_COLUMNS: f32 = COLUMN * 2.0 + GAP;
const SPACING: f32 = 20.0;
const SCROLL_STEP: f32 = 48.0;

const GITHUB: &str = "https://github.com/fayewave/Looker";
const STORE: &str = "ms-windows-store://pdp/?productid=9NV130N4C2GZ";

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub(super) enum Setting {
    Wheel,
    Zoom,
    Theme,
    Cache,
    Recents,
    Window,
    Checkerboard,
}

pub(super) struct Page {
    scroll: f32,
    max_scroll: f32,
}

impl Page {
    pub fn new() -> Page {
        Page { scroll: 0.0, max_scroll: 0.0 }
    }
}

/// The choices of a drop-down setting, and which is selected.
fn choices(s: &Settings, which: Setting) -> (Vec<&'static str>, usize) {
    match which {
        Setting::Wheel => (vec!["Zoom in and out", "Next and previous image"], s.wheel_navigates as usize),
        Setting::Zoom => (vec!["The mouse pointer", "The middle of the view"], s.zoom_center as usize),
        Setting::Theme => (vec!["Black", "Dark grey"], s.dark_grey as usize),
        Setting::Cache => (
            vec!["128 MB", "256 MB", "512 MB", "1 GB", "2 GB"],
            CACHE_CHOICES.iter().position(|&m| m >= s.cache_mb).unwrap_or(2),
        ),
        Setting::Recents | Setting::Window | Setting::Checkerboard => (Vec::new(), 0),
    }
}

impl App {
    pub(super) fn theme(&self) -> ui::Theme {
        ui::Theme::new(self.settings.dark_grey)
    }

    pub(super) fn open_page(&mut self) {
        self.stop_slideshow();
        // The Updates block asks again unless an answer that needs action is in.
        if matches!(self.update, UpdateStatus::Unknown | UpdateStatus::UpToDate) {
            self.check_updates();
        }
        self.close_menu();
        self.hide_tooltip();
        self.page = Some(Page::new());
        self.layout_changed();
    }

    pub(super) fn close_page(&mut self) {
        if self.page.take().is_some() {
            self.layout_changed();
        }
    }

    pub(super) fn toggle_page(&mut self) {
        if self.page.is_some() { self.close_page() } else { self.open_page() }
    }

    pub(super) fn scroll_page(&mut self, delta: f32) {
        if let Some(p) = &mut self.page {
            p.scroll = (p.scroll - delta / 120.0 * SCROLL_STEP).clamp(0.0, p.max_scroll);
            self.invalidate();
        }
    }

    /// A drop-down was clicked: its list, over it.
    pub(super) fn open_combo(&mut self, which: Setting) {
        let Some(r) = self.hits.rect_of(Hit::Combo(which)) else { return };
        let (items, sel) = choices(&self.settings, which);
        let entries = items
            .iter()
            .enumerate()
            .map(|(i, l)| ui::Entry::Item(ui::MenuItem { action: menus::Action::Choose(i), glyph: None, label: (*l).into(), accel: None, checked: i == sel, enabled: true }))
            .collect();
        let (w, h) = self.size_dip();
        self.menu = Some((menus::MenuKind::Combo(which), ui::Menu::combo(entries, r, rect(0.0, 0.0, w, h))));
        self.hide_tooltip();
        self.invalidate();
    }

    /// A drop-down choice was picked: applied at once and saved.
    pub(super) fn choose(&mut self, which: Setting, i: usize) {
        let s = &mut self.settings;
        match which {
            Setting::Wheel => s.wheel_navigates = i == 1,
            Setting::Zoom => s.zoom_center = i == 1,
            Setting::Theme => s.dark_grey = i == 1,
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

    pub(super) fn page_link(&mut self, i: u8) {
        match i {
            0 => {
                shell_open(GITHUB);
            }
            1 => {
                if !shell_open(STORE) {
                    shell_open("https://apps.microsoft.com/detail/9NV130N4C2GZ");
                }
            }
            3 => {
                if crate::store::appinstaller_uri().is_some() {
                    self.update = UpdateStatus::Installing;
                    crate::store::install_update(self.hwnd, self.current_path());
                    self.invalidate();
                } else {
                    shell_open(crate::store::STORE_UPDATES);
                }
            }
            4 => self.check_updates(),
            5 => {
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
        self.explorer.resort(self.settings.sort);
        self.landing.stale();
        self.layout_changed();
        self.show_toast("Looker reset to defaults", false);
    }

    // --- Drawing ------------------------------------------------------------------------------------

    pub(super) fn draw_page(&mut self, g: &Gfx) {
        let Some(p) = &self.page else { return };
        let (scroll_prev, _) = (p.scroll, ());
        let (w, h) = self.size_dip();
        let area = rect(0.0, TITLE_H + TOOLBAR_H, w, (h - TITLE_H - TOOLBAR_H).max(0.0));
        g.fill(area, rgb(self.theme().window));
        self.hits.add(Hit::PageSurface, area);

        let inner = D2D_RECT_F { left: area.left + PAD_X, top: area.top + PAD_TOP, right: area.right - PAD_X, bottom: area.bottom - PAD_BOTTOM };
        let avail = (inner.right - inner.left).max(0.0);
        let wide = avail >= TWO_COLUMNS;
        let block = if wide { TWO_COLUMNS } else { avail.min(ONE_COLUMN) };
        let x0 = g.snap(inner.left + (avail - block) / 2.0);

        // Header: Back, and the page title unless the two columns carry their own headings.
        let back = rect(x0, inner.top, 34.0, 32.0);
        self.hits.add(Hit::PageBack, back);
        let st = self.state(Hit::PageBack, true);
        let fg = ui::button_frame(g, back, ui::Kind::Standard, &st);
        g.text(&[0xE72B], &g.fonts.body_icons, back, fg, Align::Center);
        if !wide {
            g.text(&wide_str("Settings"), &g.fonts.page_title, rect(back.right + 12.0, inner.top - 2.0, 400.0, 36.0), white(0xFF), Align::Left);
        }
        let top = inner.top + 36.0 + 16.0;
        let region = D2D_RECT_F { top, ..inner };
        g.push_clip(D2D_RECT_F { left: area.left, right: area.right, ..region });

        let col_w = if wide { COLUMN } else { block };
        let mut y = top - scroll_prev;
        if wide {
            g.text(&wide_str("Settings"), &g.fonts.title, rect(x0, y, col_w, 28.0), white(0xFF), Align::Left);
            y += 28.0 + SPACING;
        }
        let y_end_prefs = self.draw_preferences(g, x0, y, col_w);
        let (ax, mut ay) = if wide { (x0 + COLUMN + GAP, top - scroll_prev) } else { (x0, y_end_prefs) };
        if !wide {
            g.hline(ax, g.snap(ay + 8.0), col_w, white(0x15));
            ay += 9.0 + SPACING;
        }
        g.text(&wide_str("About"), &g.fonts.title, rect(ax, ay, col_w, 28.0), white(0xFF), Align::Left);
        ay += 28.0 + SPACING;
        let y_end_about = self.draw_about(g, ax, ay, col_w);
        g.pop_clip();

        let content_bottom = y_end_prefs.max(y_end_about) + scroll_prev + 24.0;
        if let Some(p) = &mut self.page {
            p.max_scroll = (content_bottom - region.bottom).max(0.0);
            p.scroll = p.scroll.min(p.max_scroll);
        }
    }

    /// The preferences column; returns where it ends.
    fn draw_preferences(&mut self, g: &Gfx, x: f32, mut y: f32, w: f32) -> f32 {
        y = self.draw_updates(g, x, y, w);
        y = self.combo_row(g, x, y, w, "Mouse wheel over the image", Setting::Wheel);
        y = self.combo_row(g, x, y, w, "Zoom towards", Setting::Zoom);
        y = hint(g, x, y, w, "Where the wheel and a double-click zoom into. Ctrl + and Ctrl - always zoom into the middle.");
        y = self.combo_row(g, x, y, w, "Theme", Setting::Theme);
        y = hint(g, x, y, w, "The checkerboard behind a photo stays the same in both themes.");
        y = self.toggle_row(g, x, y, "Checkerboard background", Setting::Checkerboard, self.settings.checkerboard);
        y = hint(g, x, y, w, "The squares behind the photo, which show where it is transparent. Off: plain dark, the colour of its darker squares.");
        y = self.toggle_row(g, x, y, "Recent photos", Setting::Recents, self.settings.recents_enabled);
        y = hint(g, x, y, w, "Shows the photos you looked at last on the landing page. Turning it off also forgets the current list.");
        y = self.toggle_row(g, x, y, "Remember window size and position", Setting::Window, self.settings.remember_window);
        y = hint(g, x, y, w, "Looker opens where you left it. Off: every launch opens a default-sized window in the middle of the screen.");
        y = self.combo_row(g, x, y, w, "Decode cache budget", Setting::Cache);
        y = hint(
            g,
            x,
            y,
            w,
            "How much memory Looker keeps decoded photos in, so going back to one is instant. Raise it for large RAW or HEIC files; lower it on a machine short on memory.",
        );
        y - SPACING
    }

    /// First on the page: the one thing that may need action.
    fn draw_updates(&mut self, g: &Gfx, x: f32, mut y: f32, w: f32) -> f32 {
        g.text(&wide_str("Updates"), &g.fonts.body_strong, rect(x, y, w, 20.0), white(0xFF), Align::Left);
        y += 20.0 + 2.0;
        let github = crate::store::appinstaller_uri().is_some();
        let status = match (self.update, github) {
            (UpdateStatus::Checking, _) => "Checking for updates\u{2026}",
            (UpdateStatus::UpToDate, _) => "Looker is up to date.",
            (UpdateStatus::Available, false) => "A newer version of Looker is available. Windows installs Store updates automatically; open the Store to get it now.",
            (UpdateStatus::Available, true) => "A newer version of Looker is available. It installs the next time Looker starts, or now: Looker closes and opens again.",
            (UpdateStatus::Installing, _) => "Installing the update\u{2026} Looker closes and opens again when it's in.",
            (UpdateStatus::Failed, _) => "Couldn't install the update. Windows tries again the next time Looker starts.",
            (UpdateStatus::Unknown, false) => "Couldn't check for updates. Updates are delivered automatically through the Microsoft Store.",
            (UpdateStatus::Unknown, true) => "Couldn't check for updates. Looker updates itself from GitHub when it starts.",
        };
        let sh = g.measure_height(&wide_str(status), &g.fonts.body_wrap, w).ceil();
        g.text(&wide_str(status), &g.fonts.body_wrap, rect(x, y, w, sh), white(TEXT_SECONDARY), Align::Left);
        y += sh + 2.0 + 6.0;
        let mut bx = x;
        if matches!(self.update, UpdateStatus::Available | UpdateStatus::Failed) {
            let label = if github { "Update now" } else { "Update now in Microsoft Store" };
            bx += self.icon_button(g, bx, y, 0xE896, label, Hit::PageLink(3), true) + 8.0;
        }
        let idle = !matches!(self.update, UpdateStatus::Checking | UpdateStatus::Installing);
        self.icon_button(g, bx, y, 0xE72C, "Check for updates", Hit::PageLink(4), idle);
        y + 32.0 + SPACING
    }

    fn draw_about(&mut self, g: &Gfx, x: f32, mut y: f32, w: f32) -> f32 {
        const LOGO_H: f32 = 44.0;
        if let Some(bmp) = self.landing_wordmark(g) {
            let size = unsafe { bmp.GetPixelSize() };
            let lw = LOGO_H * size.width as f32 / size.height.max(1) as f32;
            g.draw_bitmap(&bmp, rect(x, g.snap(y), lw, LOGO_H), 1.0);
        }
        y += LOGO_H + 16.0;
        let tagline = "A fast, native photo viewer for Windows 11.";
        let th = g.measure_height(&wide_str(tagline), &g.fonts.body_wrap, w).ceil();
        g.text(&wide_str(tagline), &g.fonts.body_wrap, rect(x, y, w, th), white(TEXT_SECONDARY), Align::Left);
        y += th + 16.0;
        let version = format!("Version {} \u{00B7} {}", env!("CARGO_PKG_VERSION"), std::env::consts::ARCH);
        g.text(&wide_str(&version), &g.fonts.caption, rect(x, y, w, 16.0), white(TEXT_SECONDARY), Align::Left);
        y += 16.0 + 16.0;
        let gw = self.icon_button(g, x, y, 0xE943, "GitHub", Hit::PageLink(0), true);
        self.icon_button(g, x + gw + 8.0, y, 0xE719, "Microsoft Store", Hit::PageLink(1), true);
        y += 32.0 + 16.0;
        self.icon_button(g, x, y, 0xE71D, "Set as default photo viewer", Hit::PageLink(5), true);
        y += 32.0 + 16.0;
        self.icon_button(g, x, y, 0xE7A7, "Reset Looker", Hit::PageLink(2), true);
        y += 32.0 + 16.0 - 8.0;
        let reset = "Puts every setting back to its default, clears the recent list and the decode cache, and forgets the saved window size. Your photos are not touched.";
        let rh = g.measure_height(&wide_str(reset), &g.fonts.caption_wrap, w).ceil();
        g.text(&wide_str(reset), &g.fonts.caption_wrap, rect(x, y, w, rh), white(TEXT_SECONDARY), Align::Left);
        y += rh + 16.0;
        let credits = "Uses the Inter typeface (SIL Open Font License), and, where Windows can't decode HEIC or AVIF itself, libheif and libde265 (LGPL-3.0) and dav1d (BSD-2-Clause), for animated AVIF libavif (BSD-2-Clause), and, where Windows lacks the Raw Image Extension, LibRaw (LGPL-2.1 or CDDL-1.0) with Little CMS (MIT), JasPer and zlib. The licences are in the codecs\\licenses folder beside Looker.exe.";
        let ch = g.measure_height(&wide_str(credits), &g.fonts.caption_wrap, w).ceil();
        g.text(&wide_str(credits), &g.fonts.caption_wrap, rect(x, y, w, ch), white(TEXT_SECONDARY), Align::Left);
        y += ch + 16.0;
        let logs = format!("Logs: {}looker-native*.log", std::env::temp_dir().display().to_string().trim_end_matches('\\').to_string() + "\\");
        let lh = g.measure_height(&wide_str(&logs), &g.fonts.caption_wrap, w).ceil();
        g.text(&wide_str(&logs), &g.fonts.caption_wrap, rect(x, y, w, lh), gfx::rgba(0xFFFFFF, TEXT_SECONDARY as f32 / 255.0 * 0.6), Align::Left);
        y + lh
    }

    /// A header over a ComboBox; returns the y after it and the spacing.
    fn combo_row(&mut self, g: &Gfx, x: f32, y: f32, w: f32, header: &str, which: Setting) -> f32 {
        g.text(&wide_str(header), &g.fonts.body, rect(x, y, w, 20.0), white(0xFF), Align::Left);
        let r = rect(x, y + 28.0, w, 32.0);
        let id = Hit::Combo(which);
        self.hits.add(id, r);
        let st = self.state(id, true);
        let open = matches!(&self.menu, Some((menus::MenuKind::Combo(c), _)) if *c == which);
        let (items, sel) = choices(&self.settings, which);
        ui::combo_box(g, r, items.get(sel).copied().unwrap_or(""), &st, open);
        y + 60.0 + SPACING
    }

    /// A header over a ToggleSwitch with its On/Off label; the whole row toggles.
    fn toggle_row(&mut self, g: &Gfx, x: f32, y: f32, header: &str, which: Setting, on: bool) -> f32 {
        g.text(&wide_str(header), &g.fonts.body, rect(x, y, 480.0, 20.0), white(0xFF), Align::Left);
        let track = rect(x, y + 28.0 + 6.0, ui::SWITCH_W, ui::SWITCH_H);
        let id = Hit::Toggle(which);
        self.hits.add(id, rect(x, y + 28.0, ui::SWITCH_W + 12.0 + 40.0, 32.0));
        let st = self.state(id, true);
        let t = self.fades.get(Hit::ToggleAnim(which), on);
        ui::toggle_switch(g, track, t, &st);
        g.text(&wide_str(if on { "On" } else { "Off" }), &g.fonts.body, rect(track.right + 12.0, y + 28.0, 40.0, 32.0), white(0xFF), Align::Left);
        y + 60.0 + SPACING
    }

    /// A standard button with a glyph before its label; returns its width.
    fn icon_button(&mut self, g: &Gfx, x: f32, y: f32, glyph: u16, label: &str, id: Hit, enabled: bool) -> f32 {
        let w = (14.0 + 8.0 + g.measure(&wide_str(label), &g.fonts.body) + 22.0).ceil();
        let r = rect(g.snap(x), g.snap(y), w, 32.0);
        if enabled {
            self.hits.add(id, r);
        }
        let st = self.state(id, enabled);
        let fg = ui::button_frame(g, r, ui::Kind::Standard, &st);
        g.text(&[glyph], &g.fonts.body_icons, rect(r.left + 11.0, r.top, 14.0, 32.0), fg, Align::Center);
        g.text(&wide_str(label), &g.fonts.body, rect(r.left + 11.0 + 14.0 + 8.0, r.top, w, 32.0), fg, Align::Left);
        w
    }
}

fn wide_str(s: &str) -> Vec<u16> {
    s.encode_utf16().collect()
}

/// An explanation tucked 10 DIPs under the control above it; returns the y after it and the spacing.
fn hint(g: &Gfx, x: f32, y: f32, w: f32, text: &str) -> f32 {
    let y = y - 10.0;
    let h = g.measure_height(&wide_str(text), &g.fonts.caption_wrap, w).ceil();
    g.text(&wide_str(text), &g.fonts.caption_wrap, rect(x, y, w, h), white(TEXT_SECONDARY), Align::Left);
    y + h + SPACING
}

/// Opens a URL with the shell (the browser, the Store). False when nothing handled it.
pub(super) fn shell_open(url: &str) -> bool {
    unsafe { ShellExecuteW(None, w!("open"), &HSTRING::from(url), PCWSTR::null(), PCWSTR::null(), SW_SHOWNORMAL).0 as isize > 32 }
}
