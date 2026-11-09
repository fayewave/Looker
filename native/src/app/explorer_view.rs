//! Drawing the explorer (explorer.rs has what it does): the sidebar, the breadcrumb row, the column headers,
//! the rows, the filter box and count, and the label that follows a drag. Laid out from the card's rect each
//! frame, recording hits as it goes.
//!
//! Looks after Transient's file lists: tight 26 px rows with faint stripes, metadata much dimmer than names,
//! folders in yellow, the picked rows in the accent tint with the cursor's outlined, stars and colour dots
//! at the end of the name.

use super::explorer::{EField, FOLDER_TINT, Meta, SideTarget};
use super::*;
use crate::explorer::{self, ColKind, Kind, Place};

const ROW: f32 = 26.0;
const NAV: f32 = 28.0;
const SIDEBAR_W: f32 = 210.0;
/// From this inner width the sidebar is a column of its own; narrower, it's behind a toggle.
const SIDEBAR_AT: f32 = 600.0;
/// From this width the crumbs share the buttons' row.
const ONE_ROW_AT: f32 = 560.0;
const HEADER: f32 = 26.0;
const FILTER_H: f32 = 28.0;
const ICON_W: f32 = 24.0;
/// The crumb popup's rows, and how many show before it scrolls.
pub(super) const POPUP_ROW: f32 = 25.0;
pub(super) const POPUP_ROWS: usize = 12;
const ICON_H: f32 = 18.0;

/// A cell's text (not Name's, which draws its own way).
pub(super) fn cell_text(r: &explorer::Row, c: ColKind, m: Option<&Meta>) -> String {
    let ft = |t: u64| format_time(FILETIME { dwLowDateTime: t as u32, dwHighDateTime: (t >> 32) as u32 });
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
            format_local(&st)
        }),
        ColKind::Dimensions => m.and_then(|m| m.dims).map_or(String::new(), |(w, h)| format!("{w} \u{00D7} {h}")),
        ColKind::Megapixels => m.and_then(|m| m.dims).map_or(String::new(), |(w, h)| format!("{:.1} MP", w as f64 * h as f64 / 1_000_000.0)),
        ColKind::Camera => m.and_then(|m| m.camera.clone()).unwrap_or_default(),
        ColKind::Lens => m.and_then(|m| m.lens.clone()).unwrap_or_default(),
        _ => String::new(),
    }
}

/// The columns that fit `w` in the user's order, Name taking what the others leave; columns that would leave
/// Name narrower than 160 DIPs drop off from the end. Each: its index in the settings, kind, x and width.
fn columns(cols: &[(ColKind, f32)], left: f32, w: f32) -> Vec<(usize, ColKind, f32, f32)> {
    const NAME_MIN: f32 = 160.0;
    let mut shown: Vec<usize> = (0..cols.len()).collect();
    let others = |shown: &[usize]| shown.iter().filter(|&&i| cols[i].0 != ColKind::Name).map(|&i| cols[i].1).sum::<f32>();
    while others(&shown) > w - NAME_MIN {
        let Some(pos) = shown.iter().rposition(|&i| cols[i].0 != ColKind::Name) else { break };
        shown.remove(pos);
    }
    let name_w = (w - others(&shown)).max(NAME_MIN.min(w));
    let mut out = Vec::new();
    let mut x = left;
    for i in shown {
        let (c, cw) = cols[i];
        let cw = if c == ColKind::Name { name_w } else { cw };
        out.push((i, c, x, cw));
        x += cw;
    }
    out
}

impl App {
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
        let column_sidebar = w >= SIDEBAR_AT;
        g.push_clip(inner);
        let mut moving = false;
        let main = if column_sidebar {
            let side = D2D_RECT_F { right: inner.left + SIDEBAR_W, ..inner };
            self.draw_sidebar(g, side);
            g.fill(rect(side.right, inner.top, g.px(), inner.bottom - inner.top), rgb(theme::t().divider));
            D2D_RECT_F { left: side.right + 1.0, ..inner }
        } else {
            inner
        };
        let top = self.draw_nav(g, main, column_sidebar);
        let body = D2D_RECT_F { top, ..main };
        if !column_sidebar && self.explorer.sidebar_open {
            self.draw_sidebar(g, body);
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
    fn nav_button(&mut self, g: &Gfx, r: D2D_RECT_F, id: Hit, glyph: u16, enabled: bool, on: bool) {
        self.hits.add(id, r);
        let st = self.state(id, enabled);
        let fg = ui::button_frame(g, r, if on { ui::Kind::Toggle(true) } else { ui::Kind::Subtle }, &st);
        g.text(&[glyph], &g.fonts.body_icons, r, fg, Align::Center);
    }

    /// The breadcrumb row (two rows when narrow: buttons, then crumbs). Returns where it ends.
    fn draw_nav(&mut self, g: &Gfx, main: D2D_RECT_F, column_sidebar: bool) -> f32 {
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

        // From the right: browse mode, the bookmark, the grouping, the sidebar (when it isn't a column).
        let mut rx = main.right - 6.0 - NAV;
        let browsing = self.browsing();
        self.nav_button(g, rect(rx, y, NAV, NAV), Hit::ExplorerBrowse, if browsing { 0xE73F } else { 0xE740 }, true, browsing);
        if let Some(f) = self.explorer.folder() {
            rx -= NAV + 2.0;
            let marked = self.settings.is_bookmarked(&f);
            let r = rect(rx, y, NAV, NAV);
            self.hits.add(Hit::ExplorerBookmark, r);
            let st = self.state(Hit::ExplorerBookmark, true);
            let fg = ui::button_frame(g, r, ui::Kind::Subtle, &st);
            g.text(&[if marked { 0xE840 } else { 0xE718 }], &g.fonts.body_icons, r, if marked { rgb(t.accent) } else { fg }, Align::Center);
        }
        rx -= NAV + 2.0;
        let group_glyph = match self.settings.explorer_group {
            explorer::Group::FoldersFirst => 0xE8B7u16,
            explorer::Group::FilesFirst => 0xE7C3,
            explorer::Group::Mixed => 0xE8FD,
        };
        let group_open = matches!(self.menu, Some((menus::MenuKind::Group, _)));
        self.nav_button(g, rect(rx, y, NAV, NAV), Hit::ExplorerGroup, group_glyph, true, group_open);
        if !column_sidebar {
            rx -= NAV + 2.0;
            let open = self.explorer.sidebar_open;
            self.nav_button(g, rect(rx, y, NAV, NAV), Hit::ExplorerSidebar, 0xE8A0, true, open);
        }

        let one_row = main.right - main.left >= ONE_ROW_AT;
        let (cx0, cx1, cy) = if one_row { (left_end, rx - 8.0, y) } else { (main.left + 8.0, main.right - 8.0, y + NAV + 4.0) };
        let crumbs = rect(cx0, cy, (cx1 - cx0).max(0.0), NAV);
        self.hits.add(Hit::CrumbArea, crumbs);
        self.draw_crumbs(g, crumbs.left, crumbs.right, crumbs.top, NAV);
        let bottom = if one_row { y + NAV + 6.0 } else { cy + NAV + 6.0 };
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
        let names: Vec<String> = all.iter().map(Place::name).collect();
        const PAD: f32 = 6.0;
        const CHEVRON: f32 = 16.0;
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
        let target = self.explorer.row_drag.as_ref().filter(|d| d.active).and_then(|d| d.target.clone());
        let mut x = x0;
        if from > 0 {
            let r = rect(x, y, more_w, h);
            self.hits.add(Hit::CrumbMore, r);
            let st = self.state(Hit::CrumbMore, true);
            ui::link_fill(g, D2D_RECT_F { top: r.top + 2.0, bottom: r.bottom - 2.0, ..r }, st.hover, st.pressed);
            g.text(&wide("\u{2026}"), &g.fonts.body, r, ui::link_color(st.hover), Align::Center);
            x += more_w;
            let cr = rect(x, y, CHEVRON, h);
            g.text(&[0xE76C], &g.fonts.caption_icons, cr, rgb(t.placeholder), Align::Center);
            x += CHEVRON;
        }
        for i in from..all.len() {
            let last = i + 1 == all.len();
            let w = if last { widths[i].min((x1 - x).max(0.0)) } else { widths[i] };
            let r = rect(x, y, w, h);
            let id = Hit::Crumb(i);
            self.hits.add(id, r);
            let st = self.state(id, true);
            let fill = D2D_RECT_F { top: r.top + 2.0, bottom: r.bottom - 2.0, ..r };
            if target.is_some() && all[i].folder().is_some_and(|f| target.as_deref().is_some_and(|tg| explorer::same_path(f, tg))) {
                g.fill_round(fill, 4.0, rgb(t.selected));
                ui::stroke(g, fill, 4.0, rgb(t.accent));
            } else {
                ui::link_fill(g, fill, st.hover, st.pressed);
            }
            let colour = if last { rgb(t.text) } else { ui::link_color(st.hover) };
            g.text(&wide(&names[i]), &g.fonts.body, r, colour, Align::Center);
            x += w;
            if !last {
                let cr = rect(x, y + 4.0, CHEVRON, h - 8.0);
                let cid = Hit::CrumbChevron(i);
                self.hits.add(cid, cr);
                let st = self.state(cid, true);
                ui::link_fill(g, cr, st.hover, st.pressed);
                g.text(&[0xE76C], &g.fonts.caption_icons, cr, if st.hover > 0.0 { rgb(t.text) } else { rgb(t.placeholder) }, Align::Center);
                x += CHEVRON;
            }
        }
    }

    /// The headers, the rows and the footer. True while the list scrolls.
    fn draw_list(&mut self, g: &Gfx, body: D2D_RECT_F) -> bool {
        let t = theme::t();
        let wide_footer = body.right - body.left >= 420.0;
        let footer_h = if wide_footer { FILTER_H + 16.0 } else { FILTER_H + 16.0 + 20.0 };
        let list_left = body.left + 6.0;
        let list_right = body.right - 6.0;
        let cols = columns(&self.settings.explorer_columns, list_left, list_right - list_left - 10.0);

        // Headers: click sorts, drag moves, the right edge resizes, right-click customises.
        let hy = body.top + 2.0;
        let sort = self.settings.sort;
        let xsort = self.explorer.xsort;
        let header_drag = self.explorer.header_drag.as_ref().filter(|d| d.active).map(|d| (d.idx, d.x, d.insert));
        for &(i, c, x, w) in &cols {
            let r = rect(x, hy, w, HEADER);
            let id = Hit::ColHeader(i as u8);
            self.hits.add(id, r);
            let st = self.state(id, true);
            let lifted = header_drag.is_some_and(|(di, _, _)| di == i);
            if !lifted {
                ui::link_fill(g, D2D_RECT_F { left: r.left + 2.0, right: r.right - 2.0, top: r.top + 2.0, bottom: r.bottom - 2.0 }, st.hover, st.pressed);
            }
            let right = c.right_aligned();
            let pad = if c == ColKind::Name { ICON_W + 14.0 } else { 8.0 };
            let lr = if right { rect(x, hy, w - 10.0, HEADER) } else { rect(x + pad, hy, w - pad - 4.0, HEADER) };
            let colour = if lifted { gfx::rgba(t.label, 0.35) } else { rgb(t.label) };
            g.text(&wide(c.label()), &g.fonts.body, lr, colour, if right { Align::Right } else { Align::Left });
            let sorted = match xsort {
                Some((k, d)) => (k == c).then_some(d),
                None => (c.shared_sort() == Some(sort.field)).then_some(sort.descending),
            };
            if let Some(desc) = sorted {
                let glyph = if desc { 0xE70D } else { 0xE70E };
                let lw = g.measure(&wide(c.label()), &g.fonts.body);
                let ax = if right { lr.right - lw - 16.0 } else { lr.left + lw + 6.0 };
                g.text(&[glyph], &g.fonts.caption_icons, rect(ax, hy, 12.0, HEADER), colour, Align::Center);
            }
            if c != ColKind::Name {
                let edge = rect(x + w - 4.0, hy, 8.0, HEADER);
                self.hits.add(Hit::ColEdge(i as u8), edge);
                let hot = self.hover == Some(Hit::ColEdge(i as u8)) || self.explorer.col_resize.is_some_and(|(ri, _, _)| ri == i);
                if hot {
                    g.fill(rect(x + w - 1.0, hy + 5.0, 2.0, HEADER - 10.0), rgb(t.accent));
                }
            }
        }
        // A header being dragged: where it would go, and the header itself following the pointer.
        if let Some((di, dx, insert)) = header_drag {
            let edge_x = cols.iter().find(|&&(i, _, _, _)| i >= insert).map(|&(_, _, x, _)| x).or_else(|| cols.last().map(|&(_, _, x, w)| x + w));
            if let Some(ex) = edge_x {
                g.fill(rect(ex - 1.0, hy + 2.0, 2.0, HEADER - 4.0), rgb(t.accent));
            }
            if let Some(&(c, _)) = self.settings.explorer_columns.get(di) {
                let lw = g.measure(&wide(c.label()), &g.fonts.body) + 24.0;
                let gr = rect(dx - lw / 2.0, hy + 1.0, lw, HEADER - 2.0);
                let raised = theme::raised();
                g.fill_round(gr, 5.0, rgb(raised.popup));
                ui::stroke(g, gr, 5.0, rgb(raised.accent));
                g.text(&wide(c.label()), &g.fonts.body, gr, rgb(raised.text), Align::Center);
            }
        }

        // Rows.
        let list = D2D_RECT_F { left: list_left, top: hy + HEADER + 2.0, right: list_right - 6.0, bottom: body.bottom - footer_h };
        self.hits.add(Hit::ExplorerList, D2D_RECT_F { left: body.left, right: body.right, ..list });
        let view_h = (list.bottom - list.top).max(1.0);
        let n = self.explorer.view.len();
        let content_h = n as f32 * ROW;
        self.explorer.max_scroll = (content_h - view_h).max(0.0);
        if let Some(i) = self.explorer.reveal.take() {
            let (top, bottom) = (i as f32 * ROW, i as f32 * ROW + ROW);
            let (cur, _) = self.explorer.scroll_now();
            let to = if top < cur { top } else if bottom > cur + view_h { bottom - view_h } else { cur };
            self.explorer.anim = None;
            self.explorer.scroll = to.clamp(0.0, self.explorer.max_scroll);
        }
        let (scroll, moving) = self.explorer.scroll_now();
        let scroll = scroll.clamp(0.0, self.explorer.max_scroll);
        let first = (scroll / ROW).floor() as usize;
        let last = (((scroll + view_h) / ROW).ceil() as usize + 1).min(n);
        let want = ((ICON_W * self.scale() / 64.0).ceil() as u32 * 64).clamp(64, 256);
        let focused = self.explorer.focused;
        let target = self.explorer.row_drag.as_ref().filter(|d| d.active).and_then(|d| d.target.clone());
        let lazy_shown = cols.iter().any(|&(_, c, _, _)| c.lazy());
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
            let row = rect(list.left, g.snap(list.top + vi as f32 * ROW - scroll), list.right - list.left, ROW);
            let visible = D2D_RECT_F { top: row.top.max(list.top), bottom: row.bottom.min(list.bottom), ..row };
            if visible.bottom > visible.top {
                self.hits.add(Hit::ExplorerRow(vi), visible);
            }
            let selected = self.explorer.is_selected(&r.path);
            let cursor = self.explorer.cursor.as_deref().is_some_and(|c| explorer::same_path(c, &r.path));
            let is_target = target.as_deref().is_some_and(|tg| r.is_folder() && explorer::same_path(tg, &r.path));
            let hover = self.fades.hover(Hit::ExplorerRow(vi), self.hover == Some(Hit::ExplorerRow(vi)) && target.is_none());
            if vi % 2 == 1 {
                g.fill(row, gfx::rgba(t.ink, if t.dark { 0.022 } else { 0.03 }));
            }
            if selected || is_target {
                g.fill_round(row, 4.0, rgb(t.selected));
            } else if hover > 0.0 {
                g.fill_round(row, 4.0, gfx::rgba(t.hover_fill, hover));
            }
            if is_target || (cursor && (selected || focused)) {
                ui::stroke(g, row, 4.0, rgb(t.accent));
            }
            let faded = if r.kind == Kind::Other || self.explorer.is_cut(&r.path) { 0.45 } else { 1.0 };
            let ib = rect(row.left + 6.0, row.top + (ROW - ICON_H) / 2.0, ICON_W, ICON_H);
            let mut drew = false;
            if r.kind == Kind::Image {
                if let Some(th) = self.thumbs.get(&r.path, r.stamp) {
                    g.fill_round(ib, 2.0, rgb(t.field));
                    th.draw_fit(g, ib);
                    drew = true;
                }
                self.thumbs.want(&r.path, r.stamp, want, r.cloud, selected);
                if lazy_shown {
                    lazy_wanted.push((r.path.clone(), r.stamp, r.cloud));
                }
            }
            if !drew {
                let (glyph, tint) = match r.kind {
                    Kind::Drive => (0xEDA2u16, t.icon),
                    Kind::Folder => (0xE8D5, FOLDER_TINT),
                    Kind::Image => (0xE91B, t.icon),
                    Kind::Other => (0xE7C3, t.icon),
                };
                g.text(&[glyph], &g.fonts.body_icons, ib, gfx::rgba(tint, faded), Align::Center);
            }

            let tag = self.explorer.tags.get(&r.path);
            let meta = self.explorer.meta.get(&explorer::norm(&r.path)).cloned();
            for &(_, c, x, w) in &cols {
                if c != ColKind::Name {
                    let text = cell_text(&r, c, meta.as_ref());
                    if !text.is_empty() {
                        let right = c.right_aligned();
                        let cr = if right { rect(x, row.top, w - 10.0, ROW) } else { rect(x + 8.0, row.top, w - 12.0, ROW) };
                        g.text_tabular(&wide(&text), &g.fonts.body, cr, gfx::rgba(t.placeholder, faded), if right { Align::Right } else { Align::Left });
                    }
                    continue;
                }
                let tx = x + ICON_W + 14.0;
                let mut name_right = x + w - 6.0;
                if tag.favourite {
                    g.text(&[0xE735], &g.fonts.small_icons, rect(name_right - 14.0, row.top, 14.0, ROW), rgb(0xF2C94C), Align::Center);
                    name_right -= 18.0;
                }
                if let Some(ci) = tag.colour {
                    let c = explorer::COLOURS[ci as usize].1;
                    g.fill_round(rect(name_right - 9.0, row.top + (ROW - 8.0) / 2.0, 8.0, 8.0), 4.0, rgb(c));
                    name_right -= 14.0;
                }
                let renaming = self.explorer.rename.as_ref().is_some_and(|(p, _)| explorer::same_path(p, &r.path));
                if renaming {
                    let fr = rect(tx - 4.0, row.top + 2.0, (x + w - 4.0 - (tx - 4.0)).max(40.0), ROW - 4.0);
                    self.hits.add(Hit::ExplorerRename, fr);
                    let focused_field = self.explorer.field == Some(EField::Rename);
                    if let Some((_, f)) = self.explorer.rename.as_mut() {
                        let caret = f.draw(g, fr, theme::t(), focused_field, 0.0, None);
                        if focused_field {
                            self.caret_rect = caret;
                        }
                    }
                } else {
                    g.text(&wide(&r.name), &g.fonts.body, rect(tx, row.top, (name_right - tx).max(0.0), ROW), gfx::rgba(t.text, faded), Align::Left);
                }
            }
        }
        g.pop_clip();
        if !lazy_wanted.is_empty() {
            self.want_meta(&lazy_wanted);
        }

        // The scrollbar, whenever the list overflows: drag the thumb, or press the track to jump there.
        if self.explorer.max_scroll > 0.0 {
            let track = rect(body.right - 12.0, list.top, 12.0, view_h);
            self.hits.add(Hit::ExplorerThumb, track);
            let thumb_h = (view_h * view_h / content_h).max(28.0);
            let ty = list.top + (view_h - thumb_h) * scroll / self.explorer.max_scroll;
            let held = self.explorer.thumb_drag.is_some();
            let hot = held || self.hover == Some(Hit::ExplorerThumb);
            let tw = if hot { 8.0 } else { 6.0 };
            ui::scroll_thumb(g, rect(body.right - 4.0 - tw, ty, tw, thumb_h), held);
        }

        self.draw_footer(g, body, list.bottom, wide_footer);
        moving
    }

    /// The filter box (a search glyph, the text, a clear button) and the count.
    fn draw_footer(&mut self, g: &Gfx, body: D2D_RECT_F, top: f32, wide_footer: bool) {
        let t = theme::t();
        g.hline(body.left, g.snap(top), body.right - body.left, rgb(t.divider));
        let (status, _) = self.explorer_status();
        let fy = body.bottom - 8.0 - FILTER_H;
        let fw = if wide_footer { (body.right - body.left - 16.0) * 0.5 } else { body.right - body.left - 16.0 };
        let fr = rect(body.left + 8.0, fy, fw.min(320.0).max(fw.min(180.0)), FILTER_H);
        let fr = if wide_footer { fr } else { rect(body.left + 8.0, fy, fw, FILTER_H) };
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
        let sr = if wide_footer { rect(fr.right + 12.0, fy, body.right - 10.0 - fr.right - 12.0, FILTER_H) } else { rect(body.left + 10.0, fy - 22.0, body.right - body.left - 20.0, 20.0) };
        g.text_tabular(&wide(&status), &g.fonts.body, sr, rgb(t.label), if wide_footer { Align::Right } else { Align::Left });
    }

    /// The count under the list: "4 folders, 61 files, 120 MB", "3 selected, 12 MB", "12 of 61 shown".
    pub(super) fn explorer_status(&self) -> (String, u64) {
        let e = &self.explorer;
        let rows: Vec<&explorer::Row> = e.view.iter().map(|&i| &e.rows[i]).collect();
        let sel: Vec<&&explorer::Row> = rows.iter().filter(|r| e.is_selected(&r.path)).collect();
        let plural = |n: usize, one: &str, many: &str| if n == 1 { format!("1 {one}") } else { format!("{n} {many}") };
        if sel.len() > 1 {
            let bytes: u64 = sel.iter().map(|r| r.size).sum();
            return (format!("{} selected, {}", sel.len(), format_bytes(bytes)), bytes);
        }
        let folders = rows.iter().filter(|r| r.is_folder()).count();
        let files = rows.len() - folders;
        let bytes: u64 = rows.iter().map(|r| r.size).sum();
        let mut parts = Vec::new();
        if folders > 0 {
            parts.push(plural(folders, "folder", "folders"));
        }
        if files > 0 || folders == 0 {
            parts.push(plural(files, "file", "files"));
        }
        let mut s = parts.join(", ");
        if bytes > 0 {
            s.push_str(&format!(", {}", format_bytes(bytes)));
        }
        if rows.len() < e.rows.len() {
            s = format!("{} of {} shown", rows.len(), e.rows.len());
        }
        (s, bytes)
    }

    /// The sidebar in `r`: section headings and their places, scrolling.
    fn draw_sidebar(&mut self, g: &Gfx, r: D2D_RECT_F) {
        let t = theme::t();
        self.hits.add(Hit::Sidebar, r);
        let items = self.sidebar_items();
        let content_h = items.iter().map(|it| if it.target == SideTarget::Header { 34.0 } else { ROW }).sum::<f32>() + 16.0;
        self.explorer.side_max = (content_h - (r.bottom - r.top)).max(0.0);
        self.explorer.side_scroll = self.explorer.side_scroll.min(self.explorer.side_max);
        let target = self.explorer.row_drag.as_ref().filter(|d| d.active).and_then(|d| d.target.clone());
        let place = self.explorer.place.clone();
        g.push_clip(r);
        let mut y = r.top + 6.0 - self.explorer.side_scroll;
        for (i, it) in items.iter().enumerate() {
            match &it.target {
                SideTarget::Header => {
                    let hr = rect(r.left + 10.0, y + 8.0, r.right - r.left - 20.0, ROW);
                    g.text(&[it.glyph], &g.fonts.small_icons, rect(hr.left, hr.top, 16.0, ROW), rgb(t.label), Align::Center);
                    g.text(&wide(&it.label), &g.fonts.body, rect(hr.left + 24.0, hr.top, hr.right - hr.left - 24.0, ROW), rgb(t.label), Align::Left);
                    y += 34.0;
                }
                SideTarget::Hint => {
                    g.text(&wide(&it.label), &g.fonts.body, rect(r.left + 20.0, y, r.right - r.left - 28.0, ROW), rgb(t.placeholder), Align::Left);
                    y += ROW;
                }
                SideTarget::Place(p) => {
                    let row = rect(r.left + 6.0, y, r.right - r.left - 12.0, ROW);
                    if row.bottom > r.top && row.top < r.bottom {
                        self.hits.add(Hit::SideRow(i), row);
                    }
                    let current = place.as_ref().is_some_and(|c| c.same(p)) && it.depth >= 1;
                    let is_target = target.as_deref().is_some_and(|tg| p.folder().is_some_and(|f| explorer::same_path(f, tg)));
                    let hover = self.fades.hover(Hit::SideRow(i), self.hover == Some(Hit::SideRow(i)) && target.is_none());
                    if current || is_target {
                        g.fill_round(row, 4.0, rgb(t.selected));
                        if is_target {
                            ui::stroke(g, row, 4.0, rgb(t.accent));
                        }
                    } else if hover > 0.0 {
                        g.fill_round(row, 4.0, gfx::rgba(t.hover_fill, hover));
                    }
                    let x = row.left + 4.0 + (it.depth.saturating_sub(1)) as f32 * 14.0;
                    if let Some(node) = &it.node {
                        let cr = rect(x, row.top, 16.0, ROW);
                        self.hits.add(Hit::SideChevron(i), cr);
                        let _ = node;
                        g.text(&[if it.expanded { 0xE70D } else { 0xE76C }], &g.fonts.caption_icons, cr, rgb(t.placeholder), Align::Center);
                    }
                    let ix = x + 18.0;
                    if it.glyph == 0xEA3B {
                        // A colour collection: its swatch as a dot.
                        let c = it.tint.unwrap_or(t.icon);
                        g.fill_round(rect(ix + 3.0, row.top + (ROW - 10.0) / 2.0, 10.0, 10.0), 5.0, rgb(c));
                    } else {
                        let glyph = if it.glyph == 0xE8B7 { 0xE8D5 } else { it.glyph };
                        g.text(&[glyph], &g.fonts.body_icons, rect(ix, row.top, 16.0, ROW), rgb(it.tint.unwrap_or(t.icon)), Align::Center);
                    }
                    g.text(&wide(&it.label), &g.fonts.body, rect(ix + 24.0, row.top, (row.right - ix - 28.0).max(0.0), ROW), rgb(t.text), Align::Left);
                    y += ROW;
                }
            }
        }
        g.pop_clip();
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
    pub(super) fn draw_crumb_popup(&mut self, g: &Gfx) {
        let Some(pp) = &self.explorer.path_edit else { return };
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
        let Some(pp) = self.explorer.path_edit.as_mut() else { return };
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
            let (glyph, tint) = if path.parent().is_none() {
                (0xEDA2u16, raised.icon)
            } else if is_dir {
                (0xE8D5, FOLDER_TINT)
            } else {
                (0xE91B, raised.icon)
            };
            g.text(&[glyph], &g.fonts.body_icons, rect(row.left + 8.0, row.top, 18.0, POPUP_ROW), rgb(tint), Align::Center);
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
    }

    /// The label that follows a drag of rows: what a drop does there.
    pub(super) fn draw_drag_label(&self, g: &Gfx) {
        let Some(d) = self.explorer.row_drag.as_ref().filter(|d| d.active) else { return };
        let raised = theme::raised();
        let n = d.paths.len();
        let what = if n == 1 { explorer::folder_name(&d.paths[0]) } else { format!("{n} items") };
        let text = match &d.target {
            Some(dest) => format!("{} to {}", if self.drop_copies(&d.paths, dest) { "Copy" } else { "Move" }, explorer::folder_name(dest)),
            None => what,
        };
        let w = (g.measure(&wide(&text), &g.fonts.body) + 24.0).ceil();
        let (mx, my) = self.mouse;
        let r = rect(g.snap(mx + 14.0), g.snap(my + 18.0), w, 26.0);
        ui::shadow(g, r, 5.0);
        g.fill_round(r, 5.0, rgb(raised.popup));
        ui::stroke(g, r, 5.0, rgb(if d.target.is_some() { raised.accent } else { raised.divider }));
        g.text(&wide(&text), &g.fonts.body, r, rgb(raised.text), Align::Center);
    }
}
