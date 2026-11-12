//! Drawing the explorer (explorer.rs has what it does): the sidebar, the breadcrumb row, the column headers,
//! the rows, the filter box and count, and the label that follows a drag. Laid out from the card's rect each
//! frame, recording hits as it goes.
//!
//! Looks after Transient's file lists: tight 26 px rows with faint stripes, metadata much dimmer than names,
//! folders in yellow, the picked rows in the accent tint with the cursor's outlined, stars and colour dots
//! at the end of the name.

use std::time::Instant;

use super::explorer::{EField, FOLDER_TINT, Meta, SideTarget};
use super::*;
use crate::explorer::{self, ColKind, Kind, Place};
use crate::shell_icons;

pub(super) const ROW: f32 = 26.0;
const NAV: f32 = 28.0;
/// The tab strip's height (Transient's).
const STRIP_H: f32 = 32.0;
/// The sidebar column's width range (its width is the user's, `Settings::sidebar_width`).
pub(super) const SIDEBAR_MIN: f32 = 150.0;
pub(super) const SIDEBAR_MAX: f32 = 420.0;
/// From this inner width the sidebar is a column of its own; narrower, it's behind a toggle.
pub(super) const SIDEBAR_AT: f32 = 600.0;
const HEADER: f32 = 26.0;
pub(super) const FILTER_H: f32 = 28.0;
pub(super) const ICON_W: f32 = 24.0;
/// The crumb popup's rows, and how many show before it scrolls.
pub(super) const POPUP_ROW: f32 = 25.0;
pub(super) const POPUP_ROWS: usize = 12;
pub(super) const ICON_H: f32 = 18.0;
/// Windows' icons are drawn this many DIPs across (the shell's small icon, as Transient's Browser draws them).
pub(super) const SHELL_ICON: f32 = 16.0;

/// Windows' own icon for `kind`, centred in `r` (as Transient's Browser tab draws folders, drives and places):
/// faded by `opacity`, recoloured in `tint`. False when Windows has none (the caller draws its glyph); nothing
/// is drawn while it loads.
pub(super) fn shell_icon(g: &Gfx, kind: &shell_icons::Kind, r: D2D_RECT_F, opacity: f32, tint: Option<u32>) -> bool {
    shell_icon_at(g, kind, r, SHELL_ICON, opacity, tint)
}

/// The same, `dip` across (the system image lists go to 48 at 100 %).
pub(super) fn shell_icon_at(g: &Gfx, kind: &shell_icons::Kind, r: D2D_RECT_F, dip: f32, opacity: f32, tint: Option<u32>) -> bool {
    let px = (dip * g.dpi / 96.0).round() as u32;
    // A text type, PDF or audio file Looker opens: Looker's own red icon for it, not Notepad's, a PDF reader's or
    // a player's (Faye, 2026-11-11).
    if let shell_icons::Kind::Extension(e) = kind {
        if let Some(icon) = crate::file_icons::explorer_icon(e, px) {
            g.draw_icon(&icon, r, dip, opacity, tint);
            return true;
        }
    }
    match shell_icons::get(kind, px) {
        shell_icons::Lookup::Ready(icon) => {
            g.draw_icon(&icon, r, dip, opacity, tint);
            true
        }
        shell_icons::Lookup::Pending => true,
        shell_icons::Lookup::Missing => false,
    }
}

/// The icon a listed folder or drive gets: drives and customised folders (Desktop, Downloads, …) their own, by
/// path; any other folder the one folder icon, never read from disk.
fn row_icon(r: &explorer::Row) -> shell_icons::Kind {
    if r.kind == Kind::Drive || r.custom_icon { shell_icons::Kind::Path(r.path.clone()) } else { shell_icons::Kind::Folder }
}

/// A place's icon (a tab's, a sidebar row's): a folder by path, This PC.
pub(super) fn place_icon(p: &Place) -> Option<shell_icons::Kind> {
    match p {
        Place::Folder(f) => Some(shell_icons::Kind::Path(f.clone())),
        Place::Computer => Some(shell_icons::Kind::Computer),
        Place::Collection(_) => None,
    }
}

/// A cell's text (not Name's, which draws its own way).
pub(super) fn cell_text(r: &explorer::Row, c: ColKind, m: Option<&Meta>) -> String {
    let ft = |t: u64| format_column_time(FILETIME { dwLowDateTime: t as u32, dwHighDateTime: (t >> 32) as u32 });
    match c {
        ColKind::Name => r.name.clone(),
        ColKind::Extension if !r.is_folder() => r.path.extension().map(|e| e.to_string_lossy().to_uppercase()).unwrap_or_default(),
        ColKind::Type => r.type_name(),
        ColKind::Size if !r.is_folder() => format_bytes(r.size),
        ColKind::Parent => r.path.parent().map(explorer::folder_name).unwrap_or_default(),
        ColKind::Path => r.path.parent().map(|p| p.to_string_lossy().into_owned()).unwrap_or_default(),
        ColKind::Modified if r.kind != Kind::Drive => ft(r.stamp),
        ColKind::Created if r.kind != Kind::Drive => ft(r.created),
        ColKind::Taken => m.and_then(|m| m.taken).map_or(String::new(), |t| {
            let st = SYSTEMTIME { wYear: t.year, wMonth: t.month, wDay: t.day, wHour: t.hour, wMinute: t.minute, wSecond: t.second, ..Default::default() };
            format_column(&st)
        }),
        ColKind::Dimensions => m.and_then(|m| m.dims).map_or(String::new(), |(w, h)| format!("{w} \u{00D7} {h}")),
        ColKind::Megapixels => m.and_then(|m| m.dims).map_or(String::new(), |(w, h)| format!("{:.1} MP", w as f64 * h as f64 / 1_000_000.0)),
        ColKind::Camera => m.and_then(|m| m.camera.clone()).unwrap_or_default(),
        ColKind::Lens => m.and_then(|m| m.lens.clone()).unwrap_or_default(),
        ColKind::Width => m.and_then(|m| m.dims).map_or(String::new(), |(w, _)| w.to_string()),
        ColKind::Height => m.and_then(|m| m.dims).map_or(String::new(), |(_, h)| h.to_string()),
        ColKind::AspectRatio => m.and_then(|m| m.dims).map_or(String::new(), |(w, h)| aspect_text(w, h)),
        ColKind::Duration => m.and_then(|m| m.duration_us).map_or(String::new(), crate::imaging::video::duration_text),
        c if c.lazy() => m.and_then(|m| m.field(c)).unwrap_or_default().to_string(),
        _ => String::new(),
    }
}

/// A column as drawn this frame (eased, or following the pointer while it's dragged).
#[derive(Clone, Copy)]
struct ColDraw {
    idx: usize,
    kind: ColKind,
    x: f32,
    w: f32,
    dragged: bool,
}

/// Name's narrowest: the layout drops columns that would leave it less, and an edge drag stops there too (it let Name
/// go to 120, so a drag past 160 made the last column vanish: Faye, 2026-11-10).
pub(super) const NAME_MIN: f32 = 160.0;

impl App {
    /// The columns that fit `avail`, in the user's order, Name taking what the others leave (those that would
    /// leave it under 160 DIPs drop off the end). While a header is dragged it follows the pointer and the
    /// others close round the slot it would land in; columns ease to their places while they're moving.
    /// Returns them (the dragged one last), its slot, and whether any is still moving.
    fn col_layout(&mut self, left: f32, avail: f32) -> (Vec<ColDraw>, Option<(f32, f32)>, bool) {
        let cols = self.explorer.look.columns.clone();
        let mut shown: Vec<usize> = (0..cols.len()).collect();
        let others = |shown: &[usize]| shown.iter().filter(|&&i| cols[i].0 != ColKind::Name).map(|&i| cols[i].1).sum::<f32>();
        while others(&shown) > avail - NAME_MIN {
            let Some(pos) = shown.iter().rposition(|&i| cols[i].0 != ColKind::Name) else { break };
            shown.remove(pos);
        }
        let name_w = (avail - others(&shown)).max(NAME_MIN.min(avail));
        let width = |i: usize| if cols[i].0 == ColKind::Name { name_w } else { cols[i].1 };
        self.explorer.col_shown = shown.iter().map(|&i| (i, cols[i].0, width(i))).collect();

        // A dragged header: the others in order, and where its middle falls among them.
        let drag = self.explorer.header_drag.as_ref().filter(|d| d.active && shown.contains(&d.idx)).map(|d| (d.idx, d.x - d.grab));
        let mut order = shown.clone();
        if let Some((di, fx)) = drag {
            let rest: Vec<usize> = shown.iter().copied().filter(|&i| i != di).collect();
            let centre = fx + width(di) / 2.0;
            let mut x = left;
            let mut insert = 0;
            for &i in &rest {
                if x + width(i) / 2.0 < centre {
                    insert += 1;
                }
                x += width(i);
            }
            if let Some(d) = &mut self.explorer.header_drag {
                d.insert = insert;
            }
            order = rest;
            order.insert(insert.min(order.len()), di);
        }

        let now = Instant::now();
        let animate = crate::motion::enabled() && now < self.explorer.col_anim_until;
        let mut out = Vec::new();
        let mut dragged = None;
        let mut slot = None;
        let mut moving = false;
        let mut x = left;
        for i in order {
            let (k, w) = (cols[i].0, width(i));
            if let Some((di, fx)) = drag.filter(|&(di, _)| di == i) {
                slot = Some((x, w));
                // From here it eases into its slot when it's let go.
                self.explorer.col_pos.insert(k, crate::motion::Ease::at([fx, w]));
                dragged = Some(ColDraw { idx: di, kind: k, x: fx, w, dragged: true });
            } else {
                let e = self.explorer.col_pos.entry(k).or_insert_with(|| crate::motion::Ease::at([x, w]));
                if animate {
                    e.set([x, w], now);
                } else {
                    *e = crate::motion::Ease::at([x, w]);
                }
                moving |= e.moving(now);
                let v = e.value(now);
                out.push(ColDraw { idx: i, kind: k, x: v[0], w: v[1], dragged: false });
            }
            x += w;
        }
        out.extend(dragged);
        (out, slot, moving || drag.is_some())
    }

    pub(super) fn draw_explorer(&mut self, g: &Gfx) -> bool {
        if self.slides.explorer.value() <= 0.0 {
            return false;
        }
        let card = self.explorer_rect();
        if card.bottom - card.top < 120.0 || card.right - card.left < 120.0 {
            return false;
        }
        self.hits.add(Hit::ExplorerCard, card);
        ui::card(g, card);
        let inner = D2D_RECT_F { left: card.left + 1.0, top: card.top + 1.0, right: card.right - 1.0, bottom: card.bottom - 1.0 };
        let w = inner.right - inner.left;
        // The sidebar comes with browse mode (Faye, 2026-11-10: never the card alone), sliding in from the left and
        // back out on the app's one curve and length (it popped in), given room for a list beside it.
        let side_room = self.settings.sidebar_width.clamp(SIDEBAR_MIN, SIDEBAR_MAX) + 240.0;
        let slide = self.fades.get_over(Hit::SidebarColumn, self.browsing() && w >= side_room.min(SIDEBAR_AT), super::explorer::EDGE_SECS);
        let column_sidebar = slide > 0.0;
        g.push_clip(inner);
        let mut moving = slide > 0.0 && slide < 1.0;
        let main = if column_sidebar {
            let side_w = self.sidebar_w().clamp(SIDEBAR_MIN, SIDEBAR_MAX.min(w - 360.0).max(SIDEBAR_MIN));
            moving |= self.explorer.side_settle.is_some_and(|s| s.moving());
            let shown = g.snap(side_w * slide);
            let side = D2D_RECT_F { left: inner.left + shown - side_w, right: inner.left + shown, ..inner };
            // Its own fill, a step further from the page than the list's (theme `pane`), inside the card's corners.
            g.push_clip(D2D_RECT_F { right: side.right, ..inner });
            g.fill_round(inner, 9.0, rgb(theme::t().pane));
            g.pop_clip();
            moving |= self.draw_sidebar(g, side);
            g.fill(rect(side.right, inner.top, g.px(), inner.bottom - inner.top), rgb(theme::t().divider));
            // Its edge resizes it: a line in the accent while held, the ink on hover.
            let grip = rect(side.right - 4.0, inner.top, 8.0, inner.bottom - inner.top);
            self.hits.add(Hit::SidebarGrip, grip);
            let dragging = self.explorer.side_drag.is_some();
            let k = self.fades.hover(Hit::SidebarGrip, self.hover == Some(Hit::SidebarGrip) || dragging);
            ui::grip_line(g, rect(side.right - 1.0, inner.top + 10.0, 2.0, inner.bottom - inner.top - 20.0), k, dragging);
            D2D_RECT_F { left: side.right + 1.0, ..inner }
        } else {
            inner
        };
        let (strip_bottom, strip_moving) = self.draw_tabs(g, main, inner);
        moving |= strip_moving;
        let main = D2D_RECT_F { top: strip_bottom, ..main };
        let top = self.draw_nav(g, main, slide);
        let body = D2D_RECT_F { top, ..main };
        if !column_sidebar && self.explorer.sidebar_open {
            moving |= self.draw_sidebar(g, body);
        } else if self.columns_on() {
            moving |= self.draw_columns(g, body);
        } else {
            moving |= self.draw_list(g, body);
        }
        g.pop_clip();

        // The grip straddles the right border, in browse mode too.
        let grip = rect(card.right - 4.0, card.top, 8.0, card.bottom - card.top);
        self.hits.add(Hit::ExplorerGrip, grip);
        let dragging = self.explorer.drag.is_some();
        let t = self.fades.hover(Hit::ExplorerGrip, self.hover == Some(Hit::ExplorerGrip) || dragging);
        ui::grip_line(g, rect(card.right - 1.0, card.top + 10.0, 2.0, card.bottom - card.top - 20.0), t, dragging);
        moving
    }

    /// A small borderless icon button of the breadcrumb row.
    pub(super) fn nav_button(&mut self, g: &Gfx, r: D2D_RECT_F, id: Hit, glyph: u16, enabled: bool, on: bool) {
        self.hits.add(id, r);
        let st = self.state(id, enabled);
        let fg = ui::button_frame(g, r, if on { ui::Kind::Toggle(true) } else { ui::Kind::Subtle }, &st);
        g.text(&[glyph], &g.fonts.body_icons, r, fg, Align::Center);
    }

    /// The Folders and files button: a folder with a gear at its bottom right (Faye, 2026-11-10), the gear in a
    /// notch cut out of the folder.
    fn folder_settings_button(&mut self, g: &Gfx, r: D2D_RECT_F, on: bool) {
        let id = Hit::ExplorerGroup;
        self.hits.add(id, r);
        let st = self.state(id, true);
        let fg = ui::button_frame(g, r, if on { ui::Kind::Toggle(true) } else { ui::Kind::Subtle }, &st);
        let (cx, cy) = ((r.left + r.right) / 2.0, (r.top + r.bottom) / 2.0);
        // The folder a touch up and left of centre, so the pair is centred; the gear over its corner.
        let folder = rect(cx - 9.0, cy - 9.0, 16.0, 16.0);
        let (gx, gy) = (g.snap(cx + 4.5), g.snap(cy + 4.5));
        g.with_hole(folder, gx, gy, 6.0, || g.text(&[0xE8B7], &g.fonts.body_icons, folder, fg, Align::Center));
        g.text(&[0xE713], &g.fonts.caption_icons, rect(gx - 6.0, gy - 6.0, 12.0, 12.0), fg, Align::Center);
    }

    /// The breadcrumb rows: the buttons, then the crumbs under them. Returns where they end.
    /// The tab strip along the top (tabstrip.rs, shared with the text editor). Returns the strip's bottom, and
    /// whether the tabs are still moving.
    fn draw_tabs(&mut self, g: &Gfx, main: D2D_RECT_F, inner: D2D_RECT_F) -> (f32, bool) {
        use super::tabstrip::{StripKind, TabIcon, TabSpec};
        let t = theme::t();
        let n = self.explorer.tabs.len();
        let specs: Vec<TabSpec> = (0..n)
            .map(|k| {
                let place = self.tab_place(k);
                let icon = match &place {
                    Some(Place::Collection(explorer::Collection::Favourites)) => TabIcon::Glyph(0xE735, 0xF2C94C),
                    Some(Place::Collection(explorer::Collection::Colour(c))) => TabIcon::Dot(explorer::COLOURS[*c as usize].1),
                    Some(Place::Computer) | None => TabIcon::Shell(shell_icons::Kind::Computer, 0xE7F8, t.icon),
                    Some(Place::Folder(f)) => TabIcon::Shell(shell_icons::Kind::Path(f.clone()), 0xE8D5, FOLDER_TINT),
                };
                let title = place.as_ref().map_or_else(|| explorer::COMPUTER_NAME.to_string(), Place::name);
                TabSpec { id: self.explorer.tabs[k].id, title, icon, dirty: false }
            })
            .collect();
        let strip = rect(main.left, main.top, main.right - main.left, STRIP_H);
        let (front, focused) = (self.explorer.active_tab, self.explorer.focused);
        self.draw_tab_strip(g, StripKind::Explorer, strip, inner, 9.0, &specs, front, focused, n > 1)
    }

    /// The buttons above the list, and the crumbs. `slide`: how far the sidebar column is in (its button fades out
    /// as it comes).
    fn draw_nav(&mut self, g: &Gfx, main: D2D_RECT_F, slide: f32) -> f32 {
        let t = theme::t();
        let y = main.top + 6.0;
        let can_up = self.explorer.place.as_ref().and_then(Place::parent).is_some();
        let mut x = main.left + 6.0;
        for (id, glyph, enabled) in [
            (Hit::ExplorerBack, 0xE72Bu16, self.explorer.history.can_back()),
            (Hit::ExplorerForward, 0xE72A, self.explorer.history.can_forward()),
            (Hit::ExplorerUp, 0xE74A, can_up),
        ] {
            self.nav_button(g, rect(x, y, NAV, NAV), id, glyph, enabled, false);
            x += NAV + 2.0;
        }
        let hist = rect(x, y, 20.0, NAV);
        self.hits.add(Hit::ExplorerHistory, hist);
        let st = self.state(Hit::ExplorerHistory, true);
        let open = matches!(self.menu, Some((menus::MenuKind::History, _)));
        let fg = ui::button_frame(g, hist, if open { ui::Kind::Toggle(true) } else { ui::Kind::Subtle }, &st);
        g.text(&[0xE70D], &g.fonts.caption_icons, hist, fg, Align::Center);
        let left_end = hist.right + 8.0;

        // From the right: browse mode, the grouping, the sidebar (when it isn't a column).
        let mut rx = main.right - 6.0 - NAV;
        let browsing = self.browsing();
        let glyph = if browsing { 0xE73F } else { 0xE740 };
        // Pulled at the sticky line, the button leans the way it's pulled (Faye, 2026-11-10): its "on" look fades in
        // with the stretch towards browse mode (and out, in browse mode, towards the card), back as it springs.
        let pull = (self.explorer_stretch() / 23.0).clamp(-1.0, 1.0);
        let on = if browsing { 1.0 - (-pull).max(0.0) * 0.8 } else { pull.max(0.0) * 0.8 };
        let br = rect(rx, y, NAV, NAV);
        self.nav_button(g, br, Hit::ExplorerBrowse, glyph, true, on >= 0.999);
        if on > 0.001 && on < 0.999 {
            g.push_fade(on, 1.0, (br.left + br.right) / 2.0, (br.top + br.bottom) / 2.0);
            let fg = ui::button_frame(g, br, ui::Kind::Toggle(true), &ui::State::IDLE);
            g.text(&[glyph], &g.fonts.body_icons, br, fg, Align::Center);
            g.pop_fade();
        }
        rx -= NAV + 2.0;
        let group_open = matches!(self.menu, Some((menus::MenuKind::Group, _)));
        self.folder_settings_button(g, rect(rx, y, NAV, NAV), group_open);
        // The card's Bookmarks and Collections drop-downs, beside it (Faye, 2026-11-10): browse mode has them in its
        // sidebar, so they fade out as that slides in.
        if slide < 1.0 {
            for (id, glyph, collections) in [(Hit::ExplorerCollections, 0xE734u16, true), (Hit::ExplorerBookmarks, 0xE840, false)] {
                // A very narrow card keeps History and the sidebar button clear of them.
                if rx - 2.0 * (NAV + 2.0) < left_end {
                    break;
                }
                rx -= NAV + 2.0;
                let r = rect(rx, y, NAV, NAV);
                let open = matches!(&self.menu, Some((menus::MenuKind::Jump(c, _), _)) if *c == collections);
                g.push_fade(1.0 - slide, 1.0, (r.left + r.right) / 2.0, (r.top + r.bottom) / 2.0);
                if slide < 0.5 {
                    self.nav_button(g, r, id, glyph, true, open);
                } else {
                    let fg = ui::button_frame(g, r, if open { ui::Kind::Toggle(true) } else { ui::Kind::Subtle }, &ui::State::IDLE);
                    g.text(&[glyph], &g.fonts.body_icons, r, fg, Align::Center);
                }
                g.pop_fade();
            }
        }
        // Bookmarks and places: fading out as the sidebar column slides in, back as it goes (Faye, 2026-11-10).
        if slide < 1.0 {
            rx -= NAV + 2.0;
            let open = self.explorer.sidebar_open;
            let r = rect(rx, y, NAV, NAV);
            g.push_fade(1.0 - slide, 1.0, (r.left + r.right) / 2.0, (r.top + r.bottom) / 2.0);
            if slide < 0.5 {
                self.nav_button(g, r, Hit::ExplorerSidebar, 0xE8A0, true, open);
            } else {
                // Mostly gone: drawn, not clickable.
                let st = ui::State::IDLE;
                let fg = ui::button_frame(g, r, if open { ui::Kind::Toggle(true) } else { ui::Kind::Subtle }, &st);
                g.text(&[0xE8A0], &g.fonts.body_icons, r, fg, Align::Center);
            }
            g.pop_fade();
        }

        // The crumbs always have a row of their own under the buttons (Faye, 2026-11-09).
        let _ = left_end;
        let (cx0, cx1, cy) = (main.left + 8.0, main.right - 8.0, y + NAV + 4.0);
        let crumbs = rect(cx0, cy, (cx1 - cx0).max(0.0), NAV);
        self.hits.add(Hit::CrumbArea, crumbs);
        self.draw_crumbs(g, crumbs.left, crumbs.right, crumbs.top, NAV);
        let bottom = cy + NAV + 6.0;
        g.hline(main.left, g.snap(bottom), main.right - main.left, rgb(t.divider));
        bottom + 1.0
    }

    /// Crumbs from `x0` to `x1`: ancestors are links, the last one (where you are) is plain; clicking it edits
    /// the path. The chevrons between them open a crumb's folders. When they don't fit, the leading ones fold
    /// into "…", which opens a menu of them.
    fn draw_crumbs(&mut self, g: &Gfx, x0: f32, x1: f32, y: f32, h: f32) {
        let Some(place) = self.explorer.place.clone() else {
            self.explorer.hidden_crumbs.clear();
            return;
        };
        let t = theme::t();
        let all = explorer::crumbs(&place);
        const PAD: f32 = 6.0;
        const CHEVRON: f32 = 16.0;
        // A long folder name loses its middle, so its start and its end both show (Faye, 2026-11-11).
        const MAX_NAME: f32 = 180.0;
        let names: Vec<String> = all.iter().map(|p| middle_fit(g, &p.name(), MAX_NAME)).collect();
        let widths: Vec<f32> = names.iter().map(|n| g.measure(&wide(n), &g.fonts.body) + 2.0 + 2.0 * PAD).collect();
        let more_w = g.measure(&wide("\u{2026}"), &g.fonts.body) + 2.0 + 2.0 * PAD;
        let avail = (x1 - x0).max(0.0);
        let total = |from: usize, ellipsis: bool| -> f32 {
            let mut t: f32 = widths[from..].iter().sum::<f32>() + CHEVRON * (all.len() - from - 1) as f32;
            if ellipsis {
                t += more_w + CHEVRON;
            }
            t
        };
        let mut from = 0;
        while from + 1 < all.len() && total(from, from > 0) > avail {
            from += 1;
        }
        self.explorer.hidden_crumbs = all[..from].to_vec();
        let mut x = x0;
        if from > 0 {
            let r = rect(x, y, more_w, h);
            self.hits.add(Hit::CrumbMore, r);
            let st = self.state(Hit::CrumbMore, true);
            let open = matches!(self.menu, Some((menus::MenuKind::Crumbs(_), _)));
            let fg = ui::crumb_frame(g, D2D_RECT_F { top: r.top + 2.0, bottom: r.bottom - 2.0, ..r }, &st, open, rgb(t.label));
            g.text(&wide("\u{2026}"), &g.fonts.body, r, fg, Align::Center);
            x += more_w;
            let cr = rect(x, y, CHEVRON, h);
            g.text(&[0xE76C], &g.fonts.caption_icons, cr, rgb(t.placeholder), Align::Center);
            x += CHEVRON;
        }
        for i in from..all.len() {
            let last = i + 1 == all.len();
            let w = if last { widths[i].min((x1 - x).max(0.0)) } else { widths[i] };
            // The last one squeezed by the row's end: shortened in the middle too, to what's left.
            let name = if last && w < widths[i] { middle_fit(g, &names[i], (w - 2.0 - 2.0 * PAD).max(0.0)) } else { names[i].clone() };
            let r = rect(x, y, w, h);
            let id = Hit::Crumb(i);
            self.hits.add(id, r);
            let st = self.state(id, true);
            let fill = D2D_RECT_F { top: r.top + 2.0, bottom: r.bottom - 2.0, ..r };
            let is_target = false;
            let open = self.explorer.path_edit.as_ref().is_some_and(|p| p.crumb == Some(i));
            let colour = ui::crumb_frame(g, fill, &st, is_target || open, if last { rgb(t.text) } else { rgb(t.label) });
            g.text(&wide(&name), &g.fonts.body, r, colour, Align::Center);
            x += w;
            if !last {
                let cr = rect(x, y + 4.0, CHEVRON, h - 8.0);
                let cid = Hit::CrumbChevron(i);
                self.hits.add(cid, cr);
                let st = self.state(cid, true);
                let open = self.explorer.path_edit.as_ref().is_some_and(|p| p.crumb == Some(i + 1));
                let fg = ui::crumb_frame(g, cr, &st, false, rgb(t.placeholder));
                g.text(&[if open { 0xE70D } else { 0xE76C }], &g.fonts.caption_icons, cr, fg, Align::Center);
                x += CHEVRON;
            }
        }
    }

    /// The headers, the rows and the footer. True while the list scrolls.
    fn draw_list(&mut self, g: &Gfx, body: D2D_RECT_F) -> bool {
        let t = theme::t();
        let wide_footer = body.right - body.left >= 420.0;
        let footer_h = FILTER_H + 16.0; // one line always
        let list_left = body.left + 6.0;
        let list_right = body.right - 6.0;
        // The columns stop short of the scrollbar (8 wide now, always: 2 more than its old resting width).
        let (cols, slot, cols_moving) = self.col_layout(list_left, list_right - list_left - 12.0);

        // Headers: click sorts, drag moves (the others ease aside), an edge resizes, right-click customises.
        let hy = body.top + 2.0;
        let sort = self.explorer.look.sort;
        let xsort = self.explorer.look.xsort;
        let resizing = self.explorer.col_resize.as_ref().and_then(|r| r.widths.get(r.pos).map(|&(i, _, _)| i));
        let raised = theme::raised();
        let last_idx = cols.iter().filter(|c| !c.dragged).max_by(|a, b| a.x.total_cmp(&b.x)).map(|c| c.idx);
        let list_bottom = body.bottom - footer_h;
        if let Some((sx, sw)) = slot {
            let ghost = rect(sx + 1.0, hy + 1.0, sw - 2.0, list_bottom - hy - 2.0);
            ui::stroke(g, ghost, 5.0, rgb(t.accent));
        }
        // Each header as Transient's: a box stopping 4 DIPs short of the edges either side; hovered, the hover
        // tint with an accent outline; pressed (or its menu open), solid accent with white text. Titles all sit
        // left, the sort arrow at the column's right edge.
        let hover_tint = theme::mix(t.sidebar, t.accent, 0.2);
        let header_dragging = self.explorer.header_drag.as_ref().is_some_and(|d| d.active);
        for c in &cols {
            let r = rect(c.x, hy, c.w, HEADER);
            let id = Hit::ColHeader(c.idx as u8);
            if !c.dragged {
                self.hits.add(id, r);
            }
            let hb = D2D_RECT_F { left: r.left + 4.0, right: r.right - 4.0, top: r.top + 3.0, bottom: r.bottom - 3.0 };
            let lit = matches!(self.menu, Some((menus::MenuKind::Columns(i), _)) if i == c.idx);
            let hovered = !c.dragged && !header_dragging && self.hover == Some(id);
            let pressed = !c.dragged && !header_dragging && ((self.pressed == Some(id) && hovered) || lit);
            if c.dragged {
                ui::shadow(g, hb, 4.0);
                g.fill_round(hb, 4.0, hover_tint);
                ui::stroke(g, hb, 4.0, rgb(t.accent));
            } else if pressed {
                g.fill_round(hb, 4.0, rgb(t.accent));
            } else if hovered {
                g.fill_round(hb, 4.0, hover_tint);
                ui::stroke(g, hb, 4.0, rgb(t.accent));
            }
            let sorted = match xsort {
                Some((k, d)) => (k == c.kind).then_some(d),
                None => (c.kind.shared_sort() == Some(sort.field)).then_some(sort.descending),
            };
            let colour = rgb(if pressed {
                t.on_accent
            } else if hovered || c.dragged {
                t.text
            } else if sorted.is_some() {
                t.label
            } else {
                t.placeholder
            });
            let arrow = if sorted.is_some() { 14.0 } else { 0.0 };
            let lr = rect(c.x + 8.0, hy, (c.w - 12.0 - arrow).max(0.0), HEADER);
            g.text(&wide(c.kind.label()), &g.fonts.body, lr, colour, Align::Left);
            if let Some(desc) = sorted {
                g.text(&[if desc { 0xE74B } else { 0xE74A }], &g.fonts.small_icons, rect(hb.right - 14.0, hy, 12.0, HEADER), colour, Align::Center);
            }
        }
        // The edges between columns, a generous 12 DIPs to grab (over the headers): every one but the last
        // column's, Name's too.
        if slot.is_none() {
            for c in cols.iter().filter(|c| Some(c.idx) != last_idx) {
                let edge = rect(c.x + c.w - 6.0, hy, 12.0, HEADER);
                self.hits.add(Hit::ColEdge(c.idx as u8), edge);
                let hot = self.hover == Some(Hit::ColEdge(c.idx as u8)) || resizing == Some(c.idx);
                let line = if hot { rgb(t.accent) } else { gfx::rgba(t.divider, 1.0) };
                g.fill(rect(c.x + c.w - if hot { 1.0 } else { 0.5 }, hy + 6.0, if hot { 2.0 } else { g.px() }, HEADER - 12.0), line);
            }
        }

        // Rows: `rh` tall (the view's Height, in rows), the thumbnail growing with it.
        let rh = ROW * self.explorer.look.height as f32;
        let (icon_w, icon_h) = if self.explorer.look.height == 1 { (ICON_W, ICON_H) } else { ((rh - 8.0) * 4.0 / 3.0, rh - 8.0) };
        // The rows stop 4 short of the scrollbar (it starts 12 in from the card's edge): a picked row's outline ran
        // under it (Faye, 2026-11-10).
        let list = D2D_RECT_F { left: list_left, top: hy + HEADER + 2.0, right: list_right - 10.0, bottom: body.bottom - footer_h };
        self.hits.add(Hit::ExplorerList, D2D_RECT_F { left: body.left, right: body.right, ..list });
        self.explorer.cell_tips.clear();
        // For the selection rectangle: where the list is, its rows' height, and the Name column's span.
        let name_span = cols.iter().find(|c| c.kind == ColKind::Name).map_or((list.left, list.right), |c| (c.x, c.x + c.w));
        self.explorer.list_geom = Some((list, rh, name_span));
        let view_h = (list.bottom - list.top).max(1.0);
        let n = self.explorer.view.len();
        let content_h = n as f32 * rh;
        self.explorer.max_scroll = (content_h - view_h).max(0.0);
        if let Some(i) = self.explorer.reveal.take() {
            let (top, bottom) = (i as f32 * rh, i as f32 * rh + rh);
            let (cur, _) = self.explorer.scroll_now();
            let to = if top < cur { top } else if bottom > cur + view_h { bottom - view_h } else { cur };
            self.explorer.anim = None;
            self.explorer.scroll = to.clamp(0.0, self.explorer.max_scroll);
        }
        let (scroll, moving) = self.explorer.scroll_now();
        let scroll = scroll.clamp(0.0, self.explorer.max_scroll);
        let first = (scroll / rh).floor() as usize;
        let last = (((scroll + view_h) / rh).ceil() as usize + 1).min(n);
        let want = ((icon_w * self.scale() / 64.0).ceil() as u32 * 64).clamp(64, 256);
        let focused = self.explorer.focused;
        let target = self.explorer.row_drag.as_ref().filter(|d| d.active).and_then(|d| d.target.clone());
        let lazy_shown = cols.iter().any(|c| c.kind.lazy());
        let mut lazy_wanted = Vec::new();

        g.push_clip(D2D_RECT_F { left: body.left, right: body.right, ..list });
        if n == 0 {
            let msg = if self.explorer.place.is_none() {
                ""
            } else if !self.explorer.filter.edit.units().is_empty() {
                "Nothing matches the filter"
            } else if matches!(self.explorer.place, Some(Place::Collection(_))) {
                "Nothing tagged with this yet"
            } else {
                "This folder is empty"
            };
            g.text(&wide(msg), &g.fonts.body, rect(list.left, list.top + 12.0, list.right - list.left, 24.0), rgb(t.placeholder), Align::Center);
        }
        for vi in first..last {
            let Some(r) = self.explorer.row(vi).cloned() else { break };
            let row = rect(list.left, g.snap(list.top + vi as f32 * rh - scroll), list.right - list.left, rh);
            let visible = D2D_RECT_F { top: row.top.max(list.top), bottom: row.bottom.min(list.bottom), ..row };
            if visible.bottom > visible.top {
                self.hits.add(Hit::ExplorerRow(vi), visible);
            }
            let selected = self.explorer.is_selected(&r.path);
            let cursor = self.explorer.cursor.as_deref().is_some_and(|c| explorer::same_path(c, &r.path));
            let is_target = false;
            // Transient's row states: pressed, solid accent with white text; selected, the accent-tinted fill;
            // hovered, the hover tint with an accent outline; the cursor outlined.
            let hovered = self.hover == Some(Hit::ExplorerRow(vi)) && target.is_none();
            let hover = self.fades.hover(Hit::ExplorerRow(vi), hovered);
            let lit = hovered && self.pressed == Some(Hit::ExplorerRow(vi)) && !self.explorer.row_drag.as_ref().is_some_and(|d| d.active);
            // Several selected (Transient's): each of them solid accent with white text (icons kept: Faye,
            // 2026-11-09), easing in and out, a pixel apart so a run of them still reads as rows.
            let solid = self.fades.get(Hit::RowSolid(vi), selected && self.explorer.selected.len() > 1);
            let fr = D2D_RECT_F { bottom: row.bottom - g.px(), ..row };
            if vi % 2 == 1 {
                g.fill_round(row, 4.0, rgb(t.stripe));
            }
            // The accent fills at 60 %, softer than solid (Faye, 2026-11-10).
            const SOLID: f32 = 0.6;
            if lit {
                g.fill_round(row, 4.0, gfx::rgba(t.accent, SOLID));
            } else if solid > 0.01 {
                if solid < 0.99 && selected {
                    g.fill_round(fr, 4.0, rgb(t.selected));
                }
                g.fill_round(fr, 4.0, gfx::rgba(t.accent, solid * SOLID));
            } else if selected || is_target {
                g.fill_round(row, 4.0, rgb(t.selected));
            } else if hover > 0.0 {
                g.fill_round(row, 4.0, windows::Win32::Graphics::Direct2D::Common::D2D1_COLOR_F { a: hover, ..hover_tint });
            }
            let lit = lit || solid > 0.5;
            if !lit {
                if is_target || (cursor && (selected || focused)) {
                    ui::stroke(g, row, 4.0, rgb(t.accent));
                } else if hover > 0.0 {
                    ui::stroke(g, row, 4.0, gfx::rgba(t.accent, hover));
                }
            }
            let faded = if r.kind == Kind::Other || self.explorer.is_cut(&r.path) { 0.45 } else { 1.0 };
            // On the solid accent everything is white.
            let ink = |c: u32| if lit { rgb(t.on_accent) } else { gfx::rgba(c, faded) };
            let RowLook { badge, is_audio, is_text, .. } = row_look(&r, icon_h);
            if r.kind == Kind::Image && !is_audio {
                if !is_text {
                    self.thumbs.want(&r.path, r.stamp, want, r.cloud, selected);
                }
                if lazy_shown || badge {
                    lazy_wanted.push((r.path.clone(), r.stamp, r.cloud));
                }
            }
            let tag = self.explorer.tags.get(&r.path);
            let meta = self.explorer.meta.get(&explorer::norm(&r.path)).cloned();
            for c in &cols {
                let (x, w) = (c.x, c.w);
                if c.dragged {
                    // Lifted over the others, as its header is.
                    g.fill(rect(x, row.top, w, rh), gfx::rgba(raised.popup, 0.92));
                }
                if c.kind != ColKind::Name {
                    let text = cell_text(&r, c.kind, meta.as_ref());
                    if !text.is_empty() {
                        let right = c.kind.right_aligned();
                        let cr = if right { rect(x, row.top, w - 10.0, rh) } else { rect(x + 8.0, row.top, w - 12.0, rh) };
                        g.text_tabular(&wide(&text), &g.fonts.body, cr, ink(t.placeholder), if right { Align::Right } else { Align::Left });
                        // Cut off, in the row under the pointer: its full text for a tooltip.
                        if hovered && g.measure_tabular(&wide(&text), &g.fonts.body) > cr.right - cr.left + 0.5 {
                            self.explorer.cell_tips.push((rect(x, row.top, w, rh), text));
                        }
                    }
                    continue;
                }
                // Name: the thumbnail or glyph, the name, its tags.
                let ib = rect(x + 6.0, row.top + (rh - icon_h) / 2.0, icon_w, icon_h);
                self.draw_row_icon(g, &r, ib, lit, faded, meta.as_ref());
                let tx = x + icon_w + 14.0;
                let mut name_right = x + w - 6.0;
                if tag.favourite {
                    g.text(&[0xE735], &g.fonts.small_icons, rect(name_right - 14.0, row.top, 14.0, rh), rgb(0xF2C94C), Align::Center);
                    name_right -= 18.0;
                }
                if let Some(ci) = tag.colour {
                    let col = explorer::COLOURS[ci as usize].1;
                    g.fill_round(rect(name_right - 9.0, row.top + (rh - 8.0) / 2.0, 8.0, 8.0), 4.0, rgb(col));
                    name_right -= 14.0;
                }
                let renaming = self.explorer.rename.as_ref().is_some_and(|(p, _)| explorer::same_path(p, &r.path));
                if renaming {
                    let fr = rect(tx - 4.0, row.top + (rh - 22.0) / 2.0, (x + w - 4.0 - (tx - 4.0)).max(40.0), 22.0);
                    self.hits.add(Hit::ExplorerRename, fr);
                    let focused_field = self.explorer.field == Some(EField::Rename);
                    if let Some((_, f)) = self.explorer.rename.as_mut() {
                        let caret = f.draw(g, fr, theme::t(), focused_field, 0.0, None);
                        if focused_field {
                            self.caret_rect = caret;
                        }
                    }
                } else {
                    g.text(&wide(&r.name), &g.fonts.body, rect(tx, row.top, (name_right - tx).max(0.0), rh), ink(t.text), Align::Left);
                    // The name's tooltip whether it's cut off or not, the same for every file (Faye, 2026-11-11).
                    if hovered {
                        self.explorer.cell_tips.push((rect(x, row.top, w, rh), r.name.clone()));
                    }
                }
            }
        }
        g.pop_clip();
        if !lazy_wanted.is_empty() {
            self.want_meta(&lazy_wanted);
        }
        // Filtering by a field: every file's details, nearest the top first.
        if std::mem::take(&mut self.explorer.search_meta) {
            let all: Vec<(PathBuf, u64, bool)> = self.explorer.rows.iter().filter(|r| !r.is_folder()).map(|r| (r.path.clone(), r.stamp, r.cloud)).collect();
            self.want_meta(&all);
        }
        // The selection rectangle, from its press (scrolled with the list) to the pointer, inside the list.
        if let Some(m) = self.explorer.marquee.as_ref().filter(|m| m.active) {
            let (x0, y0) = (m.start.0, m.start.1 - scroll + list.top);
            let (x1, y1) = m.pointer;
            let r = D2D_RECT_F { left: x0.min(x1).max(list.left), top: y0.min(y1).max(list.top), right: x0.max(x1).min(list.right), bottom: y0.max(y1).min(list.bottom) };
            if r.right > r.left && r.bottom > r.top {
                g.fill(r, gfx::rgba(t.accent, 0.14));
                g.outline_round(r, 0.0, rgb(t.accent), g.px());
            }
        }

        // The scrollbar, whenever the list overflows: drag the thumb, or press the track to jump there.
        if self.explorer.max_scroll > 0.0 {
            let track = rect(body.right - 12.0, list.top, 12.0, view_h);
            self.hits.add(Hit::ExplorerThumb, track);
            let thumb_h = (view_h * view_h / content_h).max(28.0);
            let ty = list.top + (view_h - thumb_h) * scroll / self.explorer.max_scroll;
            let held = self.explorer.thumb_drag.is_some();
            ui::scroll_thumb(g, rect(body.right - 4.0 - ui::SCROLL_W, ty, ui::SCROLL_W, thumb_h), held);
        }

        self.draw_footer(g, body, list.bottom, wide_footer);
        moving || cols_moving
    }

    /// A row's picture in `ib`: its thumbnail (with a play badge, and a video's length when there's room), else
    /// Looker's red icon for the type, Windows' icon for a folder, drive or file Looker can't open, else a glyph.
    /// `lit`: on the solid accent (everything white); `faded`: a cut or unsupported file's opacity.
    pub(super) fn draw_row_icon(&mut self, g: &Gfx, r: &explorer::Row, ib: D2D_RECT_F, lit: bool, faded: f32, meta: Option<&Meta>) {
        let t = theme::t();
        let icon_h = ib.bottom - ib.top;
        let ink = |c: u32| if lit { rgb(t.on_accent) } else { gfx::rgba(c, faded) };
        let RowLook { badge, moving_picture, is_audio, is_text } = row_look(r, icon_h);
        let mut drew = false;
        if is_text {
            // Looker's red icon for the type, rather than a picture of the page (Faye, 2026-11-11).
            if let Some(e) = r.path.extension().map(|e| e.to_string_lossy().to_lowercase()) {
                let dip = if icon_h >= 32.0 { icon_h.min(48.0).floor() } else { SHELL_ICON };
                drew = shell_icon_at(g, &shell_icons::Kind::Extension(e), ib, dip, if lit { 1.0 } else { faded.max(0.8) }, lit.then_some(t.on_accent));
            }
            if !drew && is_audio {
                draw_wave_icon(g, ib, ink(t.icon));
                drew = true;
            }
        } else if r.kind == Kind::Image {
            if let Some(th) = self.thumbs.get(&r.path, r.stamp) {
                g.fill_round(ib, 2.0, rgb(t.field));
                th.draw_fit(g, ib);
                drew = true;
                if moving_picture {
                    video::draw_play_badge(g, ib);
                }
                if let Some(d) = meta.and_then(|m| m.duration_us).filter(|_| badge) {
                    video::draw_duration_badge(g, ib, d);
                }
            }
        }
        if !drew && r.kind == Kind::Image {
            // No thumbnail (yet, or ever): Looker's red icon for the type, not a grey photo glyph (Faye,
            // 2026-11-11). The thumbnail takes over when it arrives.
            if let Some(e) = r.path.extension().map(|e| e.to_string_lossy().to_lowercase()) {
                let dip = if icon_h >= 32.0 { icon_h.min(48.0).floor() } else { SHELL_ICON };
                if let Some(icon) = crate::file_icons::icon(&e, (dip * g.dpi / 96.0).round() as u32) {
                    g.draw_icon(&icon, ib, dip, if lit { 1.0 } else { faded.max(0.8) }, lit.then_some(t.on_accent));
                    drew = true;
                }
            }
        }
        if !drew && r.kind == Kind::Other {
            // A file Looker can't open: Windows' icon for its type.
            if let Some(e) = r.path.extension().map(|e| e.to_string_lossy().to_lowercase()) {
                let dip = if icon_h >= 32.0 { icon_h.min(48.0).floor() } else { SHELL_ICON };
                drew = shell_icon_at(g, &shell_icons::Kind::Extension(e), ib, dip, if lit { 1.0 } else { faded.max(0.8) }, lit.then_some(t.on_accent));
            }
        }
        if !drew && r.is_folder() {
            // Windows' icon; on the solid accent recoloured white, as Transient does a picked folder.
            drew = shell_icon(g, &row_icon(r), ib, if lit { 1.0 } else { faded }, lit.then_some(t.on_accent));
        }
        if !drew {
            let (glyph, tint) = match r.kind {
                Kind::Drive => (0xEDA2u16, t.icon),
                Kind::Folder => (0xE8D5, FOLDER_TINT),
                Kind::Image => (0xE91B, t.icon),
                Kind::Other => (0xE7C3, t.icon),
            };
            g.text(&[glyph], &g.fonts.body_icons, ib, ink(tint), Align::Center);
        }
    }

    /// The filter box (a search glyph, the text, a clear button) and the count.
    pub(super) fn draw_footer(&mut self, g: &Gfx, body: D2D_RECT_F, top: f32, wide_footer: bool) {
        let t = theme::t();
        g.hline(body.left, g.snap(top), body.right - body.left, rgb(t.divider));
        let _ = wide_footer;
        let status = self.explorer_status();
        let fy = body.bottom - 8.0 - FILTER_H;
        // One line (Faye, 2026-11-10): the counts at the right as wide as they are, the filter stretching from the
        // left up to them; short of room the counts give way, the filter keeping at least 120.
        let widths: Vec<f32> = status.iter().map(|(glyph, text)| if glyph.is_some() { 20.0 } else { 0.0 } + g.measure_tabular(&wide(text), &g.fonts.body).ceil() + 1.0).collect();
        let total = widths.iter().sum::<f32>() + 16.0 * widths.len().saturating_sub(1) as f32;
        let room = body.right - body.left - 16.0;
        let fw = (room - total - 12.0).max(120.0_f32.min(room));
        let fr = rect(body.left + 8.0, fy, fw, FILTER_H);
        self.hits.add(Hit::ExplorerFilter, fr);
        let focused = self.explorer.field == Some(EField::Filter);
        let hover = self.state(Hit::ExplorerFilter, true).hover;
        g.fill_round(fr, 5.0, rgb(t.field));
        ui::stroke(g, fr, 5.0, theme::mix(t.divider, t.accent, if focused { 1.0 } else { hover * 0.5 }));
        g.text(&[0xE721], &g.fonts.small_icons, rect(fr.left + 4.0, fr.top, 20.0, FILTER_H), rgb(t.placeholder), Align::Center);
        let text_r = D2D_RECT_F { left: fr.left + 18.0, ..fr };
        let empty = self.explorer.filter.edit.units().is_empty();
        let clear = (!empty).then(|| {
            let cr = ui::TextField::clear_rect(fr);
            self.hits.add(Hit::ExplorerFilterClear, cr);
            (cr, self.state(Hit::ExplorerFilterClear, true))
        });
        if empty && !focused {
            let n = self.explorer.rows.len();
            let ph = format!("Filter {}\u{2026}   Ctrl+F", if n == 1 { "1 item".to_string() } else { format!("{n} items") });
            g.text(&wide(&ph), &g.fonts.body, rect(text_r.left + 9.0, fr.top, text_r.right - text_r.left - 12.0, FILTER_H), rgb(t.placeholder), Align::Left);
        } else {
            let caret = self.explorer.filter.draw_text(g, text_r, t, focused, clear);
            if focused {
                self.caret_rect = caret;
            }
        }
        let sr = rect(fr.right + 12.0, fy, (body.right - 8.0 - fr.right - 12.0).max(0.0), FILTER_H);
        // Each part: an icon, then its text, right-aligned beside the filter.
        let mut x = (sr.right - total).max(sr.left);
        g.push_clip(sr);
        for ((glyph, text), w) in status.iter().zip(widths) {
            let mut tx = x;
            if let Some(gl) = glyph {
                g.text(&[*gl], &g.fonts.small_icons, rect(x, sr.top, 14.0, sr.bottom - sr.top), rgb(t.placeholder), Align::Center);
                tx += 20.0;
            }
            g.text_tabular(&wide(text), &g.fonts.body, rect(tx, sr.top, x + w - tx + 40.0, sr.bottom - sr.top), rgb(t.label), Align::Left);
            x += w + 16.0;
        }
        g.pop_clip();
    }

    /// The counts under the list, as Transient's: a file icon and the files, a folder icon and the folders,
    /// then their size; with several selected, those; with a filter on, how many of the folder's show.
    pub(super) fn explorer_status(&self) -> Vec<(Option<u16>, String)> {
        let e = &self.explorer;
        let rows: Vec<&explorer::Row> = e.view.iter().map(|&i| &e.rows[i]).collect();
        let sel: Vec<&&explorer::Row> = rows.iter().filter(|r| e.is_selected(&r.path)).collect();
        let mut out = Vec::new();
        let counted: Vec<&explorer::Row> = if sel.len() > 1 {
            out.push((Some(0xE73Au16), format!("{} selected", sel.len())));
            sel.into_iter().copied().collect()
        } else {
            if rows.len() < e.rows.len() {
                out.push((Some(0xE71Cu16), format!("{} of {}", rows.len(), e.rows.len())));
            }
            // A field filter still reading the files' details.
            if e.search_fields && e.search_unread > 0 {
                out.push((Some(0xE72Cu16), format!("Reading {}", e.search_unread)));
            }
            rows
        };
        let folders = counted.iter().filter(|r| r.is_folder()).count();
        let files = counted.len() - folders;
        let bytes: u64 = counted.iter().map(|r| r.size).sum();
        if files > 0 || folders == 0 {
            out.push((Some(0xE7C3), files.to_string()));
        }
        if folders > 0 {
            out.push((Some(0xE8B7), folders.to_string()));
        }
        if bytes > 0 {
            out.push((None, format_bytes(bytes)));
        }
        out
    }

    /// The sidebar in `r`: section headings and their places, scrolling.
    /// Draws the sidebar; true while its sections are still easing.
    fn draw_sidebar(&mut self, g: &Gfx, r: D2D_RECT_F) -> bool {
        let t = theme::t();
        self.hits.add(Hit::Sidebar, r);
        let items = self.sidebar_items();
        let item_h = |it: &super::explorer::SideItem| if matches!(it.target, SideTarget::Header(_)) { 34.0 } else { ROW };
        let target = self.explorer.row_drag.as_ref().filter(|d| d.active).and_then(|d| d.target.clone());
        let place = self.explorer.place.clone();

        // The sections as blocks (from a heading to the next), each laid out from its eased top. A section
        // dragged by its heading leaves the flow: the others close up around a slot its height, where it would
        // land, and its heading follows the pointer.
        let mut blocks: Vec<(explorer::SideSection, usize, usize, f32)> = Vec::new();
        for (i, it) in items.iter().enumerate() {
            if let SideTarget::Header(sec) = it.target {
                blocks.push((sec, i, i, 0.0));
            }
            if let Some(b) = blocks.last_mut() {
                b.2 = i + 1;
                b.3 += item_h(it);
            }
        }
        // A folded section is its heading alone; folding or opening, its height eases and its rows are cut off
        // at its bottom.
        let now = Instant::now();
        let mut opens: Vec<f32> = vec![1.0; blocks.len()];
        let mut folding = false;
        for (k, b) in blocks.iter_mut().enumerate() {
            let want = if self.settings.sidebar_closed.contains(&b.0) { 0.0 } else { 1.0 };
            let e = self.explorer.section_open.entry(b.0).or_insert_with(|| crate::motion::Ease::at([want]));
            e.set([want], now);
            folding |= e.moving(now);
            opens[k] = e.value(now)[0].clamp(0.0, 1.0);
            b.3 = 34.0 + (b.3 - 34.0) * opens[k];
        }
        let content_h = blocks.iter().map(|b| b.3).sum::<f32>() + 16.0;
        self.explorer.side_max = (content_h - (r.bottom - r.top)).max(0.0);
        // Eased like the list's (it jumped a whole step a notch: Faye, 2026-11-11).
        let (side_scroll, scrolling) = self.explorer.side_scroll_now();
        let side_scroll = side_scroll.clamp(0.0, self.explorer.side_max);
        if !scrolling {
            self.explorer.side_scroll = side_scroll; // a section folded shut: no scrolling past the end
        }
        let origin = r.top + 6.0 - side_scroll;
        let drag = self.explorer.section_drag.as_ref().filter(|d| d.active).map(|d| (d.sec, d.y - d.grab));
        let dragged = drag.and_then(|(sec, _)| blocks.iter().position(|b| b.0 == sec));
        let mut slot = None;
        let mut tops: Vec<f32> = vec![0.0; blocks.len()];
        if let (Some(di), Some((_, head_y))) = (dragged, drag) {
            let others: Vec<usize> = (0..blocks.len()).filter(|&k| k != di).collect();
            // Its heading's middle, against the others' middles as they'd sit without it.
            let mid = head_y + 21.0 - origin;
            let mut y = 0.0;
            let mut insert = 0;
            for &k in &others {
                if y + blocks[k].3 / 2.0 < mid {
                    insert += 1;
                }
                y += blocks[k].3;
            }
            if let Some(d) = self.explorer.section_drag.as_mut() {
                d.insert = insert;
            }
            let mut y = 0.0;
            for (n, &k) in others.iter().enumerate() {
                if n == insert {
                    slot = Some(y);
                    y += blocks[di].3;
                }
                tops[k] = y;
                y += blocks[k].3;
            }
            if slot.is_none() {
                slot = Some(y);
            }
        } else {
            let mut y = 0.0;
            for (k, b) in blocks.iter().enumerate() {
                tops[k] = y;
                y += b.3;
            }
        }
        let animate = drag.is_some() || now < self.explorer.section_anim_until;
        let mut moving = drag.is_some() || folding || scrolling;
        let mut ys: Vec<Option<f32>> = vec![None; items.len()];
        let mut bottoms: Vec<f32> = vec![f32::MAX; items.len()];
        // Where a dragged bookmark or collection would land: its slot's top and height.
        let mut row_slots: Vec<(f32, f32)> = Vec::new();
        for (k, b) in blocks.iter().enumerate() {
            if Some(k) == dragged {
                if let Some((_, head_y)) = drag {
                    self.explorer.section_pos.insert(b.0, crate::motion::Ease::at([head_y - origin]));
                }
                continue;
            }
            let e = self.explorer.section_pos.entry(b.0).or_insert_with(|| crate::motion::Ease::at([tops[k]]));
            if animate {
                e.set([tops[k]], now);
            } else {
                *e = crate::motion::Ease::at([tops[k]]);
            }
            moving |= e.moving(now);
            let top = origin + e.value(now)[0];
            let bottom = top + b.3;
            // Its entries (a top-level row and its open tree beneath it), each at its eased place in the section;
            // a bookmark or collection being dragged follows the pointer and the others make room (Faye,
            // 2026-11-10), as the sections do.
            let mut groups: Vec<(usize, usize, f32, String)> = Vec::new(); // first item, end, height, key
            let mut head_h = 0.0;
            for i in b.1..b.2 {
                let it = &items[i];
                if it.depth == 1 && matches!(it.target, SideTarget::Place(_)) {
                    groups.push((i, i + 1, item_h(it), format!("{:?}|{}", b.0, it.node.clone().unwrap_or_else(|| it.label.clone()))));
                } else if let Some(gr) = groups.last_mut().filter(|_| it.depth > 1) {
                    gr.1 = i + 1;
                    gr.2 += item_h(it);
                } else {
                    ys[i] = Some(top + head_h);
                    bottoms[i] = bottom;
                    head_h += item_h(it);
                }
            }
            let row_drag = self.explorer.side_row_drag.as_ref().filter(|d| d.active && d.sec == b.0).map(|d| (d.from, d.y - d.grab));
            let mut offs: Vec<f32> = vec![0.0; groups.len()];
            let mut row_slot = None;
            match row_drag.filter(|&(from, _)| from < groups.len()) {
                Some((from, row_y)) => {
                    let others: Vec<usize> = (0..groups.len()).filter(|&k| k != from).collect();
                    let mid = row_y + groups[from].2 / 2.0 - (top + head_h);
                    let mut y = 0.0;
                    let mut insert = 0;
                    for &k in &others {
                        if y + groups[k].2 / 2.0 < mid {
                            insert += 1;
                        }
                        y += groups[k].2;
                    }
                    if let Some(d) = self.explorer.side_row_drag.as_mut() {
                        d.insert = insert;
                    }
                    let mut y = 0.0;
                    for (n, &k) in others.iter().enumerate() {
                        if n == insert {
                            row_slot = Some(y);
                            y += groups[from].2;
                        }
                        offs[k] = y;
                        y += groups[k].2;
                    }
                    if row_slot.is_none() {
                        row_slot = Some(y);
                    }
                    offs[from] = row_y - (top + head_h);
                }
                None => {
                    let mut y = 0.0;
                    for (k, gr) in groups.iter().enumerate() {
                        offs[k] = y;
                        y += gr.2;
                    }
                }
            }
            let row_animate = row_drag.is_some() || now < self.explorer.row_anim_until;
            for (k, gr) in groups.iter().enumerate() {
                let dragged_row = row_drag.is_some_and(|(from, _)| from == k);
                let e = self.explorer.row_pos.entry(gr.3.clone()).or_insert_with(|| crate::motion::Ease::at([offs[k]]));
                if dragged_row || !row_animate {
                    *e = crate::motion::Ease::at([offs[k]]);
                } else {
                    e.set([offs[k]], now);
                }
                moving |= e.moving(now) || dragged_row;
                let mut y = top + head_h + e.value(now)[0];
                for i in gr.0..gr.1 {
                    ys[i] = Some(y);
                    // The dragged row isn't cut off by its section's bottom while it's held.
                    bottoms[i] = if dragged_row { f32::MAX } else { bottom };
                    y += item_h(&items[i]);
                }
            }
            if let (Some(sy), Some((from, _))) = (row_slot, row_drag) {
                row_slots.push((top + head_h + sy, groups[from].2));
            }
        }

        let hover_tint = theme::mix(t.sidebar, t.accent, 0.2);
        g.push_clip(r);
        if let (Some(di), Some(sy)) = (dragged, slot) {
            let sr = rect(r.left + 6.0, origin + sy + 4.0, r.right - r.left - 12.0, blocks[di].3 - 6.0);
            g.fill_round(sr, 4.0, gfx::rgba(t.accent, 0.10));
            ui::stroke(g, sr, 4.0, rgb(t.accent));
        }
        for &(sy, sh) in &row_slots {
            let sr = rect(r.left + 6.0, sy, r.right - r.left - 12.0, sh);
            g.fill_round(sr, 4.0, gfx::rgba(t.accent, 0.10));
            ui::stroke(g, sr, 4.0, rgb(t.accent));
        }
        for (i, it) in items.iter().enumerate() {
            let Some(y) = ys[i] else { continue };
            // A row of a section folding away: gone past its bottom, cut off where it crosses it.
            let header = matches!(it.target, SideTarget::Header(_));
            if !header && y >= bottoms[i] {
                continue;
            }
            let cut = !header && y + item_h(it) > bottoms[i];
            if cut {
                g.push_clip(rect(r.left, y, r.right - r.left, bottoms[i] - y));
            }
            match &it.target {
                SideTarget::Header(sec) => {
                    // A button, as Transient's headings are: hover tinted and outlined, pressed solid accent; the
                    // label semibold; a chevron at the right while hovered, and while folded.
                    let hr = rect(r.left + 10.0, y + 8.0, r.right - r.left - 20.0, ROW);
                    let box_ = rect(r.left + 6.0, y + 8.0, r.right - r.left - 12.0, ROW);
                    self.hits.add(Hit::SideHeader(*sec), box_);
                    let hid = Hit::SideHeader(*sec);
                    let hovered = self.hover == Some(hid) && drag.is_none() && target.is_none();
                    let hover = self.fades.hover(hid, hovered);
                    let pressed = hovered && self.pressed == Some(hid);
                    if pressed {
                        g.fill_round(box_, 4.0, rgb(t.accent));
                    } else if hover > 0.0 {
                        g.fill_round(box_, 4.0, windows::Win32::Graphics::Direct2D::Common::D2D1_COLOR_F { a: hover, ..hover_tint });
                        ui::stroke(g, box_, 4.0, gfx::rgba(t.accent, hover));
                    }
                    let (c, chev) = if pressed { (t.on_accent, t.on_accent) } else if hovered { (t.text, t.label) } else { (t.label, t.placeholder) };
                    g.text(&[it.glyph], &g.fonts.small_icons, rect(hr.left, hr.top, 16.0, ROW), rgb(c), Align::Center);
                    g.text(&wide(&it.label), &g.fonts.overline, rect(hr.left + 24.0, hr.top, hr.right - hr.left - 48.0, ROW), rgb(c), Align::Left);
                    let closed = self.settings.sidebar_closed.contains(sec);
                    let open = blocks.iter().position(|b| b.0 == *sec).map_or(1.0, |k| opens[k]);
                    let a = hover.max(1.0 - open);
                    if a > 0.0 {
                        let glyph = if closed { 0xE76C } else { 0xE70D };
                        g.text(&[glyph], &g.fonts.small_icons, rect(box_.right - 24.0, hr.top, 16.0, ROW), gfx::rgba(chev, a), Align::Center);
                    }
                }
                SideTarget::Hint => {
                    g.text(&wide(&it.label), &g.fonts.body, rect(r.left + 20.0, y, r.right - r.left - 28.0, ROW), rgb(t.placeholder), Align::Left);
                }
                SideTarget::Place(p) => {
                    let row = rect(r.left + 6.0, y, r.right - r.left - 12.0, ROW);
                    if row.bottom > r.top && row.top < r.bottom {
                        self.hits.add(Hit::SideRow(i), row);
                    }
                    let current = place.as_ref().is_some_and(|c| c.same(p)) && it.depth >= 1;
                    let is_target = false;
                    let hover = self.fades.hover(Hit::SideRow(i), self.hover == Some(Hit::SideRow(i)) && target.is_none());
                    if current || is_target {
                        g.fill_round(row, 4.0, rgb(t.selected));
                        if is_target {
                            ui::stroke(g, row, 4.0, rgb(t.accent));
                        }
                    } else if hover > 0.0 {
                        g.fill_round(row, 4.0, windows::Win32::Graphics::Direct2D::Common::D2D1_COLOR_F { a: hover, ..hover_tint });
                    }
                    if hover > 0.0 && !is_target {
                        ui::stroke(g, row, 4.0, gfx::rgba(t.accent, hover));
                    }
                    let x = row.left + 4.0 + (it.depth.saturating_sub(1)) as f32 * 14.0;
                    if let Some(node) = &it.node {
                        let cr = rect(x, row.top, 16.0, ROW);
                        self.hits.add(Hit::SideChevron(i), cr);
                        let _ = node;
                        g.text(&[if it.expanded { 0xE70D } else { 0xE76C }], &g.fonts.caption_icons, cr, rgb(t.placeholder), Align::Center);
                    }
                    let ix = x + 18.0;
                    // Windows' icons for places, drives, bookmarks and history (by path: Downloads' arrow, a
                    // drive's own), the one folder icon for the folders opened under them, as Transient's tree.
                    let shell = if it.depth > 1 { Some(shell_icons::Kind::Folder) } else { place_icon(p) };
                    let drew = shell.is_some_and(|k| shell_icon(g, &k, rect(ix, row.top, 16.0, ROW), 1.0, None));
                    if drew {
                        // Drawn.
                    } else if it.glyph == 0xEA3B {
                        // A colour collection: its swatch as a dot.
                        let c = it.tint.unwrap_or(t.icon);
                        g.fill_round(rect(ix + 3.0, row.top + (ROW - 10.0) / 2.0, 10.0, 10.0), 5.0, rgb(c));
                    } else {
                        let glyph = if it.glyph == 0xE8B7 { 0xE8D5 } else { it.glyph };
                        g.text(&[glyph], &g.fonts.body_icons, rect(ix, row.top, 16.0, ROW), rgb(it.tint.unwrap_or(t.icon)), Align::Center);
                    }
                    g.text(&wide(&it.label), &g.fonts.body, rect(ix + 24.0, row.top, (row.right - ix - 28.0).max(0.0), ROW), rgb(t.text), Align::Left);
                }
            }
            if cut {
                g.pop_clip();
            }
        }
        // The dragged heading, lifted over the rest where the pointer holds it.
        if let (Some(di), Some((_, head_y))) = (dragged, drag) {
            let raised = theme::raised();
            let it = &items[blocks[di].1];
            let hr = rect(r.left + 6.0, head_y + 8.0, r.right - r.left - 12.0, ROW);
            ui::shadow(g, hr, 4.0);
            g.fill_round(hr, 4.0, rgb(raised.popup));
            ui::stroke(g, hr, 4.0, rgb(raised.divider));
            g.text(&[it.glyph], &g.fonts.small_icons, rect(hr.left + 4.0, hr.top, 16.0, ROW), rgb(raised.text), Align::Center);
            g.text(&wide(&it.label), &g.fonts.body, rect(hr.left + 28.0, hr.top, hr.right - hr.left - 32.0, ROW), rgb(raised.text), Align::Left);
        }
        g.pop_clip();
        moving
    }

    /// The cover for a file Looker can't open: Windows' own thumbnail of it when there is one (a video's frame,
    /// a document's first page), else the file glyph, with its name under it, centred between the cards.
    pub(super) fn draw_placeholder(&mut self, g: &Gfx) {
        let Some((path, stamp, cloud)) = self.placeholder.as_ref().map(|p| (p.path.clone(), p.stamp, p.cloud)) else { return };
        let a = self.image_area();
        let name = wide(&file_name(&path));
        let w = (a.right - a.left - 32.0).clamp(1.0, 520.0);
        let name_h = g.measure_height(&name, &g.fonts.subtitle_wrap, w);
        let cx = (a.left + a.right) / 2.0;
        // The preview box: up to 640 x 400, leaving room for the name.
        let (bw, bh) = ((a.right - a.left - 64.0).clamp(1.0, 640.0), (a.bottom - a.top - name_h - 84.0).clamp(1.0, 400.0));
        let px = ((bw.max(bh) * self.scale() / 64.0).ceil() as u32 * 64).clamp(128, 1024);
        self.thumbs.want(&path, stamp, px, cloud, true);
        let thumb = self.thumbs.get(&path, stamp);
        let icon_h = if thumb.is_some() { bh } else { 120.0 };
        let top = ((a.top + a.bottom) - (icon_h + 20.0 + name_h)) / 2.0;
        match thumb {
            Some(t) => t.draw_fit(g, rect(cx - bw / 2.0, top, bw, bh)),
            None => g.text(&[0xE7C3], &g.fonts.hero_icons, rect(cx - 80.0, top, 160.0, icon_h), rgb(theme::t().icon), Align::Center),
        }
        g.text(&name, &g.fonts.subtitle_wrap, rect(cx - w / 2.0, top + icon_h + 20.0, w, name_h + 4.0), rgb(theme::t().text), Align::Center);
    }

    /// Transient's crumb popup (explorer.rs has what it does): the path box with Up and close, the folder's
    /// entries, and a footer of keys.
    pub(super) fn draw_crumb_popup(&mut self, g: &Gfx) -> bool {
        let Some(pp) = &self.explorer.path_edit else { return false };
        let e = crate::motion::progress(pp.opened);
        let raised = theme::raised();
        let (ww, wh) = self.size_dip();
        let n = pp.shown.len();
        let anchor = pp.anchor;
        let empty = pp.field.edit.units().is_empty();
        let can_up = pp.listed.is_some();
        let rows = n.clamp(1, POPUP_ROWS);
        let w = 560.0f32.min(ww - 16.0);
        let h = 44.0 + rows as f32 * POPUP_ROW + 8.0 + 34.0;
        let x = g.snap((anchor.left - 6.0).clamp(8.0, (ww - w - 8.0).max(8.0)));
        let y = g.snap((anchor.bottom + 4.0).min(wh - h - 8.0).max(8.0));
        let panel = rect(x, y, w, h);
        self.hits.add(Hit::CrumbPopup, panel);
        g.push_fade(e, 0.94 + 0.06 * e, x, y);
        ui::shadow(g, panel, 8.0);
        g.fill_round(panel, 8.0, rgb(raised.popup));
        ui::stroke(g, panel, 8.0, rgb(raised.divider));

        // The path box (a folder glyph, the text, a clear button), Up, close.
        let close = rect(panel.right - 8.0 - 28.0, y + 8.0, 28.0, 28.0);
        let up = rect(close.left - 2.0 - 28.0, y + 8.0, 28.0, 28.0);
        let fr = rect(x + 8.0, y + 8.0, up.left - 8.0 - (x + 8.0), 28.0);
        self.hits.add(Hit::ExplorerPath, fr);
        let focused = self.explorer.field == Some(EField::Path);
        g.fill_round(fr, 5.0, rgb(raised.field));
        ui::stroke(g, fr, 5.0, theme::mix(raised.divider, raised.accent, if focused { 1.0 } else { 0.0 }));
        g.text(&[0xE8B7], &g.fonts.small_icons, rect(fr.left + 4.0, fr.top, 20.0, 28.0), rgb(raised.placeholder), Align::Center);
        let clear = (!empty).then(|| {
            let cr = ui::TextField::clear_rect(fr);
            self.hits.add(Hit::PathClear, cr);
            (cr, self.state(Hit::PathClear, true))
        });
        let text_r = D2D_RECT_F { left: fr.left + 18.0, ..fr };
        for (id, r, glyph, enabled) in [(Hit::CrumbPopupUp, up, 0xE74Au16, can_up), (Hit::CrumbPopupClose, close, 0xE711, true)] {
            self.hits.add(id, r);
            let st = self.state(id, enabled);
            let fg = ui::button_frame(g, r, ui::Kind::Subtle, &st);
            g.text(&[glyph], &g.fonts.body_icons, r, fg, Align::Center);
        }
        let Some(pp) = self.explorer.path_edit.as_mut() else {
            g.pop_fade();
            return false;
        };
        let caret = pp.field.draw_text(g, text_r, raised, focused, clear);
        if focused {
            self.caret_rect = caret;
        }

        // The entries.
        let list = rect(x + 6.0, y + 44.0, w - 12.0, rows as f32 * POPUP_ROW);
        let pp = self.explorer.path_edit.as_mut().unwrap();
        pp.max_scroll = (n as f32 * POPUP_ROW - list.bottom + list.top).max(0.0);
        pp.scroll = pp.scroll.min(pp.max_scroll);
        let (scroll, cursor) = (pp.scroll, pp.cursor);
        let items: Vec<(usize, PathBuf, bool)> = pp.shown.iter().enumerate().map(|(k, &i)| (k, pp.entries[i].0.clone(), pp.entries[i].1)).collect();
        let pending = pp.entries.is_empty() && pp.listed.is_some();
        g.push_clip(list);
        if items.is_empty() {
            let msg = if pending { "\u{2026}" } else { "Nothing here" };
            g.text(&wide(msg), &g.fonts.body, rect(list.left + 12.0, list.top, list.right - list.left - 24.0, POPUP_ROW), rgb(raised.placeholder), Align::Left);
        }
        let first = (scroll / POPUP_ROW).floor() as usize;
        for (k, path, is_dir) in items.into_iter().skip(first).take(POPUP_ROWS + 1) {
            let row = rect(list.left, list.top + k as f32 * POPUP_ROW - scroll, list.right - list.left, POPUP_ROW);
            self.hits.add(Hit::CrumbPopupRow(k), D2D_RECT_F { top: row.top.max(list.top), bottom: row.bottom.min(list.bottom), ..row });
            let hover = self.fades.hover(Hit::CrumbPopupRow(k), self.hover == Some(Hit::CrumbPopupRow(k)));
            if cursor == Some(k) {
                g.fill_round(row, 4.0, rgb(raised.selected));
                ui::stroke(g, row, 4.0, rgb(raised.accent));
            } else if hover > 0.0 {
                g.fill_round(row, 4.0, gfx::rgba(raised.hover_fill, hover));
            }
            let ir = rect(row.left + 8.0, row.top, 18.0, POPUP_ROW);
            let shell = if path.parent().is_none() {
                Some(shell_icons::Kind::Path(path.clone()))
            } else if is_dir {
                Some(shell_icons::Kind::Folder)
            } else {
                None
            };
            if !shell.is_some_and(|k| shell_icon(g, &k, ir, 1.0, None)) {
                let (glyph, tint) = if path.parent().is_none() {
                    (0xEDA2u16, raised.icon)
                } else if is_dir {
                    (0xE8D5, FOLDER_TINT)
                } else {
                    (0xE91B, raised.icon)
                };
                g.text(&[glyph], &g.fonts.body_icons, ir, rgb(tint), Align::Center);
            }
            g.text(&wide(&explorer::folder_name(&path)), &g.fonts.body, rect(row.left + 34.0, row.top, row.right - row.left - 40.0, POPUP_ROW), rgb(raised.text), Align::Left);
        }
        g.pop_clip();

        // The keys, as chips.
        let fy = panel.bottom - 30.0;
        g.hline(panel.left, g.snap(fy - 4.0), w, rgb(raised.divider));
        let mut cx = x + 10.0;
        for (keys, what) in [(&["Up", "Down"][..], "to move"), (&["Enter"][..], "to go"), (&["Tab"][..], "to step in"), (&["Esc"][..], "to close")] {
            for k in keys {
                let kw = g.measure(&wide(k), &g.fonts.caption) + 12.0;
                let kr = rect(cx, fy + 3.0, kw, 20.0);
                g.fill_round(kr, 4.0, rgb(raised.button));
                g.text(&wide(k), &g.fonts.caption, kr, rgb(raised.label), Align::Center);
                cx += kw + 3.0;
            }
            let tw = g.measure(&wide(what), &g.fonts.caption);
            g.text(&wide(what), &g.fonts.caption, rect(cx + 3.0, fy + 3.0, tw + 2.0, 20.0), rgb(raised.placeholder), Align::Left);
            cx += tw + 18.0;
            if cx > panel.right - 60.0 {
                break;
            }
        }
        g.pop_fade();
        e < 1.0
    }

    /// A drag of rows, as Transient's: an accent box over the folder a drop would go into (a row, a tab, a
    /// crumb, a sidebar folder) that pops in, glides from one to the next and fades out; and a card following
    /// the pointer with what's dragged and, under it, what a drop there does ("Copy to …" / "Move to …": Ctrl
    /// copies, Shift moves, else another drive copies). Returns whether anything is still moving.
    pub(super) fn draw_drag_label(&mut self, g: &Gfx) -> bool {
        let now = Instant::now();
        let Some(d) = self.explorer.row_drag.as_ref().filter(|d| d.active) else {
            self.explorer.drop_box = None;
            self.explorer.drop_shade = crate::motion::Ease::at([0.0]);
            self.explorer.drop_card_h = crate::motion::Ease::at([30.0]);
            return false;
        };
        let (paths, target, trect, opened) = (d.paths.clone(), d.target.clone(), d.target_rect, d.opened);
        let raised = theme::raised();
        let e = &mut self.explorer;
        e.drop_shade.set([if trect.is_some() { 1.0 } else { 0.0 }], now);
        let shade = e.drop_shade.value(now)[0].clamp(0.0, 1.0);
        if let Some(t) = trect {
            let to = [t.left, t.top, t.right - t.left, t.bottom - t.top];
            match e.drop_box.as_mut() {
                Some(b) if shade >= 0.05 => b.set(to, now),
                _ => {
                    // From nothing it pops in, growing from 94 %.
                    let (w, h) = (to[2], to[3]);
                    let mut b = crate::motion::Ease::at([t.left + w * 0.03, t.top + h * 0.03, w * 0.94, h * 0.94]);
                    b.set(to, now);
                    e.drop_box = Some(b);
                }
            }
        }
        let mut moving = e.drop_shade.moving(now) || opened.elapsed().as_secs_f32() < 0.25;
        if let Some(b) = e.drop_box.as_ref() {
            moving |= b.moving(now);
            let v = b.value(now);
            let r = rect(v[0], v[1], v[2], v[3]);
            g.fill_round(r, 4.0, gfx::rgba(raised.accent, 0.22 * shade));
            g.outline_round(r, 4.0, gfx::rgba(raised.accent, shade), 1.0);
            if shade <= 0.0 {
                e.drop_box = None;
            }
        }
        // The card.
        let n = paths.len();
        let one_dir = n == 1 && paths[0].is_dir();
        let what = if n == 1 { explorer::folder_name(&paths[0]) } else { format!("{n} items") };
        let (from_archive, into) = self.explorer.row_drag.as_ref().map_or((false, None), |d| (d.archive.is_some(), d.into.clone()));
        let action = target.as_ref().map(|dest| match (&into, from_archive) {
            // Into an archive: "Add to photos.zip" (or its folder); out of one: "Extract to …".
            (Some(dir), _) => format!("Add to {}{}", explorer::folder_name(dest), if dir.is_empty() { String::new() } else { format!(" \u{203A} {}", dir.replace('/', " \u{203A} ")) }),
            (None, true) => format!("Extract to {}", explorer::folder_name(dest)),
            (None, false) => format!("{} to {}", if self.drop_copies(&paths, dest) { "Copy" } else { "Move" }, explorer::folder_name(dest)),
        });
        // Over an archive that can't take files: why not.
        let action = action.or_else(|| self.archive_drop_refused().filter(|_| !from_archive && self.hover.is_some_and(|h| matches!(h, Hit::ArchiveCard | Hit::ArchiveList | Hit::ArchiveRow(_)))).map(|why| format!("Can't add: {why}")));
        let tw = g.measure(&wide(&what), &g.fonts.body).max(action.as_ref().map_or(0.0, |a| g.measure(&wide(a), &g.fonts.body)));
        let w = (tw + 46.0).clamp(120.0, 340.0).ceil();
        let e = &mut self.explorer;
        e.drop_card_h.set([if action.is_some() { 46.0 } else { 30.0 }], now);
        moving |= e.drop_card_h.moving(now);
        let h = e.drop_card_h.value(now)[0].round();
        let k = crate::motion::progress(opened);
        let (mx, my) = self.mouse;
        let fr = rect(g.snap(mx + 6.0), g.snap(my + 12.0), w, h);
        g.push_fade(k, 0.94 + 0.06 * k, mx, my);
        ui::shadow(g, fr, 7.0);
        g.fill_round(fr, 7.0, rgb(raised.popup));
        ui::stroke(g, fr, 7.0, rgb(raised.divider));
        let ir = rect(fr.left + 10.0, fr.top + 7.0, 16.0, 16.0);
        if one_dir {
            if !shell_icon(g, &shell_icons::Kind::Folder, ir, 1.0, None) {
                g.text(&[0xE8D5], &g.fonts.body_icons, ir, rgb(FOLDER_TINT), Align::Center);
            }
        } else {
            g.text(&[if n == 1 { 0xE91B } else { 0xE8B3 }], &g.fonts.body_icons, ir, rgb(raised.icon), Align::Center);
        }
        g.push_clip(rect(fr.left, fr.top, fr.right - fr.left - 10.0, fr.bottom - fr.top));
        g.text(&wide(&what), &g.fonts.body, rect(fr.left + 34.0, fr.top + 7.0, fr.right - fr.left - 44.0, 16.0), rgb(raised.text), Align::Left);
        if let Some(a) = &action {
            g.text(&wide(a), &g.fonts.body, rect(fr.left + 34.0, fr.top + 25.0, fr.right - fr.left - 44.0, 15.0), rgb(raised.signal), Align::Left);
        }
        g.pop_clip();
        g.pop_fade();
        moving
    }
}

/// A generic waveform: bars of set heights, centred in `r` (an audio file's icon in the explorer's rows).
fn draw_wave_icon(g: &Gfx, r: D2D_RECT_F, colour: windows::Win32::Graphics::Direct2D::Common::D2D1_COLOR_F) {
    const BARS: [f32; 7] = [0.3, 0.62, 1.0, 0.5, 0.82, 0.42, 0.24];
    let side = (r.right - r.left).min(r.bottom - r.top).min(64.0);
    let (cx, cy) = ((r.left + r.right) / 2.0, (r.top + r.bottom) / 2.0);
    let h = side * 0.62;
    let pitch = side * 0.62 / BARS.len() as f32;
    let bar = (pitch * 0.55).max(1.5);
    let x0 = cx - pitch * BARS.len() as f32 / 2.0 + (pitch - bar) / 2.0;
    for (i, k) in BARS.iter().enumerate() {
        let bh = (h * k).max(bar);
        let x = g.snap(x0 + i as f32 * pitch);
        g.fill_round(rect(x, cy - bh / 2.0, bar, bh), bar / 2.0, colour);
    }
}

/// "16:9", "3:2", or "1.85:1" when the sides don't reduce to small numbers.
fn aspect_text(w: u32, h: u32) -> String {
    if w == 0 || h == 0 {
        return String::new();
    }
    fn gcd(a: u32, b: u32) -> u32 {
        if b == 0 { a } else { gcd(b, a % b) }
    }
    let g = gcd(w, h);
    let (a, b) = (w / g, h / g);
    if a <= 32 && b <= 32 {
        return format!("{a}:{b}");
    }
    // Near a common one (a 1920 x 1088 frame, a crop off by a pixel): that one.
    let r = w as f64 / h as f64;
    for (a, b) in [(16, 9), (9, 16), (4, 3), (3, 4), (3, 2), (2, 3), (1, 1), (21, 9), (5, 4), (4, 5), (16, 10), (10, 16)] {
        if (r - a as f64 / b as f64).abs() < 0.01 {
            return format!("{a}:{b}");
        }
    }
    format!("{r:.2}:1")
}

/// `s` as it fits `max_w` in the body font: whole, else its start and end with "…" between them.
fn middle_fit(g: &Gfx, s: &str, max_w: f32) -> String {
    if g.measure(&wide(s), &g.fonts.body) <= max_w {
        return s.to_string();
    }
    let chars: Vec<char> = s.chars().collect();
    let cut = |k: usize| -> String {
        let head = k.div_ceil(2);
        let tail = k / 2;
        let mut out: String = chars[..head].iter().collect();
        out.push('\u{2026}');
        out.extend(&chars[chars.len() - tail..]);
        out
    };
    // The most characters kept that still fit.
    let (mut lo, mut hi) = (0usize, chars.len().saturating_sub(1));
    while lo < hi {
        let mid = (lo + hi + 1) / 2;
        if g.measure(&wide(&cut(mid)), &g.fonts.body) <= max_w { lo = mid } else { hi = mid - 1 }
    }
    cut(lo)
}

/// What kind of picture a row has.
pub(super) struct RowLook {
    /// A video or sound whose length goes on its thumbnail (there's room at this height; the meta thread probes it).
    pub badge: bool,
    /// Videos and GIFs: a play badge in the thumbnail's middle.
    pub moving_picture: bool,
    /// Audio shows a plain waveform icon, not the file's own waveform (Faye, 2026-11-11).
    pub is_audio: bool,
    /// Text, PDFs and audio show Looker's red icon for the type, not a picture (Faye, 2026-11-11).
    pub is_text: bool,
}

pub(super) fn row_look(r: &explorer::Row, icon_h: f32) -> RowLook {
    let f = (r.kind == Kind::Image).then(|| format::of_path(&r.path));
    let is_video = matches!(f, Some(format::Format::Video | format::Format::Audio));
    RowLook {
        badge: is_video && icon_h >= video::BADGE_MIN_H,
        moving_picture: matches!(f, Some(format::Format::Video | format::Format::Gif)),
        is_audio: f == Some(format::Format::Audio),
        is_text: matches!(f, Some(format::Format::Text | format::Format::Pdf | format::Format::Audio)),
    }
}
