//! Column view, after Finder's (Faye, 2026-11-12): the folder gone to is the first column, and every folder
//! picked in a column opens in a column of its own to its right, so the whole way down shows at once; going back
//! up is a click (or Left) away. Each column's right edge resizes that column alone (macOS 27's), and its menu
//! sizes it to fit, makes every column its width (kept in the view), or puts them back.
//!
//! The column with the keys is the explorer itself: its rows, filter, selection, cursor, scroll and watcher are
//! `Explorer`'s as in the list, so everything a list does (rename, drag, the clipboard, menus, type-ahead) works
//! there unchanged. The other columns keep their rows in `Columns::panes`; focusing one swaps it in
//! (`column_focus`). The view (its layout, sort and column width) is the first column's place's: the columns
//! after it show as it does, and a change is kept for it (`App::view_place`).

use std::path::{Path, PathBuf};
use std::time::Instant;

use super::explorer::Listed;
use super::explorer_view::{FILTER_H, ICON_H, ICON_W, ROW};
use super::*;
use crate::explorer::{self, Kind, Layout, Place, Row, Watcher};

const HSCROLL_MS: f32 = 190.0;
/// The horizontal scrollbar's height, under the columns when they don't all fit.
const HBAR: f32 = 12.0;

/// A column: a folder (the first may be This PC or a collection), its rows while it hasn't the keys, its scroll,
/// and its own width while it's been dragged to one.
pub(super) struct Pane {
    pub id: u64,
    pub place: Place,
    pub rows: Vec<Row>,
    pub listed: bool,
    pub scroll: f32,
    pub max_scroll: f32,
    pub width: Option<f32>,
    /// The wheel's easing (from, to, start), as the list's.
    pub anim: Option<(f32, f32, Instant)>,
}

impl Pane {
    /// Its scroll this frame, and whether it's still easing.
    pub fn scroll_now(&mut self) -> (f32, bool) {
        let Some((from, to, t0)) = self.anim else { return (self.scroll, false) };
        let t = (t0.elapsed().as_secs_f32() * 1000.0 / PANE_SCROLL_MS).min(1.0);
        if t >= 1.0 || !crate::motion::enabled() {
            self.anim = None;
            self.scroll = to;
            return (to, false);
        }
        (from + (to - from) * crate::motion::ease_out(t), true)
    }
}

/// The wheel's easing in a column, the list's (explorer.rs SCROLL_MS).
const PANE_SCROLL_MS: f32 = 150.0;

#[derive(Default)]
pub(super) struct Columns {
    /// Left to right; empty while the list shows. `panes[active]` has the keys: its rows are `Explorer::rows`.
    pub panes: Vec<Pane>,
    pub active: usize,
    /// The active column's rows have come (until then its selection isn't the user's).
    pub active_listed: bool,
    pub hscroll: f32,
    pub hanim: Option<(f32, f32, Instant)>,
    pub max_h: f32,
    /// Scroll sideways to show the last column (and the active one) on the next frame.
    pub reveal: bool,
    /// A column's edge being dragged: which column, where the press was, its width then.
    pub resize: Option<(usize, f32, f32)>,
    /// The horizontal scrollbar's thumb being dragged: the pointer's offset from its left.
    pub hthumb: Option<f32>,
    /// The column of the place gone to (those left of it are its folders up to This PC): its view shows.
    pub base: usize,
    /// Show the last column at once on the next frame, without the sideways glide (the columns were just made).
    pub jump: bool,
    next_id: u64,
}

impl Columns {
    fn pane(&mut self, place: Place) -> Pane {
        self.next_id += 1;
        Pane { id: self.next_id, place, rows: Vec::new(), listed: false, scroll: 0.0, max_scroll: 0.0, width: None, anim: None }
    }

    pub fn clear(&mut self) {
        self.panes.clear();
        self.active = 0;
        self.base = 0;
        self.hscroll = 0.0;
        self.hanim = None;
        self.resize = None;
        self.hthumb = None;
    }

    /// The sideways scroll this frame, and whether it's still easing.
    pub fn hscroll_now(&mut self) -> (f32, bool) {
        let Some((from, to, t0)) = self.hanim else { return (self.hscroll, false) };
        let t = (t0.elapsed().as_secs_f32() * 1000.0 / HSCROLL_MS).min(1.0);
        if t >= 1.0 || !crate::motion::enabled() {
            self.hanim = None;
            self.hscroll = to;
            return (to, false);
        }
        (from + (to - from) * crate::motion::ease_out(t), true)
    }

    fn scroll_to(&mut self, to: f32) {
        let (now, _) = self.hscroll_now();
        let to = to.clamp(0.0, self.max_h.max(0.0));
        if (to - now).abs() > 0.5 {
            self.hscroll = to;
            self.hanim = Some((now, to, Instant::now()));
        }
    }
}

impl App {
    /// Column view is showing.
    pub(super) fn columns_on(&self) -> bool {
        self.explorer.look.layout == Layout::Columns && !self.explorer.cols.panes.is_empty()
    }

    /// Column `i`'s width: its own while it's been dragged, else the view's.
    pub(super) fn column_w(&self, i: usize) -> f32 {
        self.explorer.cols.panes.get(i).and_then(|p| p.width).unwrap_or(self.explorer.look.column_w)
    }

    /// The place whose saved view shows: column view's place gone to, else the place listed.
    pub(super) fn view_place(&self) -> Option<Place> {
        let c = &self.explorer.cols;
        if self.columns_on() { c.panes.get(c.base.min(c.panes.len().saturating_sub(1))).map(|p| p.place.clone()) } else { self.explorer.place.clone() }
    }

    /// The columns start again at the place listed (it was gone to, or column view was turned on), with its
    /// folders up to This PC (a drive's) or the path's root (a network share's) in columns left of it, so they
    /// can always be scrolled back to (Faye, 2026-11-12). `listed`: its rows are there already.
    pub(super) fn columns_reset(&mut self, listed: bool) {
        let e = &mut self.explorer;
        e.cols.clear();
        if e.look.layout != Layout::Columns {
            return;
        }
        let Some(p) = e.place.clone() else { return };
        let mut chain: Vec<Place> = Vec::new();
        if let Place::Folder(f) = &p {
            let mut up: Vec<PathBuf> = f.ancestors().skip(1).map(Path::to_path_buf).collect();
            up.reverse();
            let drive = f.to_string_lossy().as_bytes().get(1) == Some(&b':');
            if drive {
                chain.push(Place::Computer);
            }
            chain.extend(up.into_iter().map(Place::Folder));
        }
        let mut list = Vec::new();
        for place in chain {
            let pane = e.cols.pane(place.clone());
            list.push((pane.id, place));
            e.cols.panes.push(pane);
        }
        let pane = e.cols.pane(p);
        e.cols.panes.push(pane);
        let n = e.cols.panes.len();
        e.cols.active = n - 1;
        e.cols.base = n - 1;
        e.cols.active_listed = listed;
        e.cols.jump = true;
        for (id, place) in list {
            self.column_list(id, place);
        }
    }

    /// Going to `place` in column view: a column already showing it takes the keys; a folder under one of the
    /// columns opens the columns down to it (as the open photo's folder does); anything else isn't for the
    /// columns (false: the explorer goes there, its first column).
    pub(super) fn columns_go(&mut self, place: &Place, record: bool) -> bool {
        if !self.columns_on() {
            return false;
        }
        let panes = &self.explorer.cols.panes;
        if let Some(j) = panes.iter().position(|p| p.place.same(place)) {
            if record {
                self.explorer.history.record(place);
            }
            self.column_focus(j);
            return true;
        }
        let Place::Folder(to) = place else { return false };
        let holds = |p: &Pane| match &p.place {
            Place::Computer => true,
            Place::Folder(f) => explorer::within(to, f),
            Place::Collection(_) => false,
        };
        let Some(i) = panes.iter().rposition(holds) else { return false };
        self.explorer.cols.base = self.explorer.cols.base.min(i);
        // The folders from under that column's down to `to`.
        let base = panes[i].place.folder().map(Path::to_path_buf);
        let mut chain: Vec<PathBuf> = to.ancestors().take_while(|a| base.as_deref().is_none_or(|b| !explorer::same_path(a, b))).map(Path::to_path_buf).collect();
        chain.reverse();
        if chain.is_empty() {
            return false;
        }
        self.column_focus(i);
        let e = &mut self.explorer;
        e.cols.panes.truncate(i + 1);
        for f in chain {
            let pane = e.cols.pane(Place::Folder(f));
            e.cols.panes.push(pane);
        }
        let n = e.cols.panes.len();
        let between: Vec<(u64, Place)> = e.cols.panes[i + 1..n - 1].iter().map(|p| (p.id, p.place.clone())).collect();
        for (id, p) in between {
            self.column_list(id, p);
        }
        if record {
            self.explorer.history.record(place);
        }
        self.column_focus(n - 1);
        true
    }

    /// Column `j` takes the keys: the active one's rows go back into its pane and `j`'s come out, its folder's
    /// row (the column after it) picked, as Finder's Left leaves it.
    pub(super) fn column_focus(&mut self, j: usize) {
        let hwnd = self.hwnd;
        let e = &mut self.explorer;
        let a = e.cols.active;
        if j == a || j >= e.cols.panes.len() {
            return;
        }
        let (scroll, _) = e.scroll_now();
        let listed = e.cols.active_listed;
        if let Some(p) = e.cols.panes.get_mut(a) {
            p.rows = std::mem::take(&mut e.rows);
            p.scroll = scroll;
            p.anim = None;
            p.max_scroll = e.max_scroll;
            p.listed = listed;
        }
        e.cols.active = j;
        let next = e.cols.panes.get(j + 1).and_then(|p| p.place.folder()).map(Path::to_path_buf);
        let p = &mut e.cols.panes[j];
        let place = p.place.clone();
        e.rows = std::mem::take(&mut p.rows);
        e.scroll = p.scroll_now().0;
        p.anim = None;
        e.max_scroll = p.max_scroll;
        let listed = p.listed;
        e.cols.active_listed = listed;
        e.anim = None;
        // A listing still on its way for the column that had the keys isn't this one's.
        e.serial += 1;
        e.watcher = None;
        e.rename = None;
        e.pending_rename = None;
        e.filter.edit.clear();
        e.place = Some(place.clone());
        e.selected = next.iter().map(|n| explorer::norm(n)).collect();
        e.cursor = next.clone();
        e.anchor = next.clone();
        e.refilter();
        e.cols.reveal = true;
        if listed {
            if let Place::Folder(f) = &place {
                e.watcher = Watcher::start(f, hwnd, explorer::WM_EXPLORER_CHANGED, false);
            }
        } else {
            // Kept as picked when the column after it is showing; else as a new listing (the open photo).
            self.explorer_list(place, next.is_some());
        }
        self.save_tabs();
        self.invalidate();
    }

    /// The columns after the active one follow its selection: one folder picked opens it in the next column; a file,
    /// several rows or none close them.
    pub(super) fn columns_sync(&mut self) {
        if !self.columns_on() || !self.explorer.cols.active_listed {
            return;
        }
        let e = &mut self.explorer;
        let want = if e.selected.len() == 1 {
            e.rows.iter().find(|r| r.is_folder() && e.selected.contains(&explorer::norm(&r.path))).map(|r| r.path.clone())
        } else {
            None
        };
        let a = e.cols.active;
        let next = e.cols.panes.get(a + 1).and_then(|p| p.place.folder()).map(Path::to_path_buf);
        match (want, next) {
            (Some(w), Some(n)) if explorer::same_path(&w, &n) => {}
            (Some(w), _) => {
                e.cols.panes.truncate(a + 1);
                e.cols.base = e.cols.base.min(a);
                let pane = e.cols.pane(Place::Folder(w.clone()));
                let id = pane.id;
                e.cols.panes.push(pane);
                e.cols.reveal = true;
                self.column_list(id, Place::Folder(w));
            }
            (None, Some(_)) => {
                e.cols.panes.truncate(a + 1);
                e.cols.base = e.cols.base.min(a);
                e.cols.reveal = true;
            }
            (None, None) => {}
        }
    }

    /// Lists a column that hasn't the keys, off the UI thread (`on_column_listed`).
    fn column_list(&self, id: u64, place: Place) {
        let (sort, group) = (self.explorer.look.sort, self.explorer.look.group);
        let tagged = match &place {
            Place::Collection(c) => self.explorer.tags.in_collection(*c),
            _ => Vec::new(),
        };
        let hwnd = self.hwnd.0 as isize;
        std::thread::Builder::new()
            .name("explorer-column".into())
            .spawn(move || {
                let rows = explorer::list(&place, sort, group, &tagged, false);
                let ptr = Box::into_raw(Box::new(Listed { serial: 0, place, rows, refresh: false, pane: Some(id) }));
                unsafe {
                    if PostMessageW(Some(HWND(hwnd as _)), explorer::WM_EXPLORER_LISTED, WPARAM(0), LPARAM(ptr as isize)).is_err() {
                        drop(Box::from_raw(ptr));
                    }
                }
            })
            .ok();
    }

    pub(super) fn on_column_listed(&mut self, id: u64, mut rows: Vec<Row>) {
        if !self.explorer.look.other_files {
            rows.retain(|r| r.kind != Kind::Other);
        }
        let c = &mut self.explorer.cols;
        // The column that has the keys was listed as the explorer's own since.
        let Some(i) = c.panes.iter().position(|p| p.id == id).filter(|&i| i != c.active) else { return };
        let p = &mut c.panes[i];
        p.rows = rows;
        p.listed = true;
        self.invalidate();
    }

    /// F5 (or a change on disk shown in another column): every column but the active one lists again.
    pub(super) fn columns_refresh(&mut self) {
        let a = self.explorer.cols.active;
        let jobs: Vec<(u64, Place)> = self.explorer.cols.panes.iter().enumerate().filter(|(i, _)| *i != a).map(|(_, p)| (p.id, p.place.clone())).collect();
        for (id, p) in jobs {
            self.column_list(id, p);
        }
    }

    /// Right (or Enter / a double-click on a folder): into the column after, its first row picked.
    pub(super) fn column_right(&mut self) {
        self.columns_sync();
        let a = self.explorer.cols.active;
        if a + 1 < self.explorer.cols.panes.len() {
            self.column_focus(a + 1);
            if self.explorer.cursor.is_none() && !self.explorer.view.is_empty() {
                self.explorer_pick(0);
            }
        }
    }

    /// Left: back to the column before, its folder's row still picked.
    pub(super) fn column_left(&mut self) {
        let a = self.explorer.cols.active;
        if a > 0 {
            self.column_focus(a - 1);
        }
    }

    /// A press (any button) on a column that hasn't the keys gives it them first, and the hits drawn last frame
    /// are renamed to match, so the press then lands as it would in the list.
    pub(super) fn column_press_focus(&mut self, x: f32, y: f32) {
        let p = match self.hit(x, y) {
            Some(Hit::ColumnRow(p, _) | Hit::ColumnPane(p)) => p as usize,
            _ => return,
        };
        let old = self.explorer.cols.active;
        if p == old {
            return;
        }
        self.column_focus(p);
        let (p, old) = (p as u16, old as u16);
        self.hits.remap(|h| match h {
            Hit::ColumnRow(q, i) if q == p => Hit::ExplorerRow(i),
            Hit::ColumnPane(q) if q == p => Hit::ExplorerList,
            Hit::ExplorerRow(i) => Hit::ColumnRow(old, i),
            Hit::ExplorerList => Hit::ColumnPane(old),
            h => h,
        });
    }

    /// The wheel over a column that hasn't the keys scrolls it, easing as the list does (Faye, 2026-11-12: it
    /// jumped).
    pub(super) fn scroll_column(&mut self, p: usize, delta: f32) {
        if let Some(pane) = self.explorer.cols.panes.get_mut(p) {
            let (now, _) = pane.scroll_now();
            let target = pane.anim.map_or(pane.scroll, |(_, to, _)| to);
            let to = (target - delta / 120.0 * super::explorer::SCROLL_STEP).clamp(0.0, pane.max_scroll.max(0.0));
            pane.scroll = now;
            pane.anim = Some((now, to, Instant::now()));
            self.invalidate();
        }
    }

    /// A sideways wheel (or Shift + the wheel) over the columns.
    pub(super) fn scroll_columns_sideways(&mut self, delta: f32) {
        let c = &mut self.explorer.cols;
        let target = c.hanim.map_or(c.hscroll, |(_, to, _)| to);
        c.scroll_to(target + delta / 120.0 * 80.0);
        self.invalidate();
    }

    // --- Widths ----------------------------------------------------------------------------------------

    pub(super) fn column_edge_press(&mut self, i: usize, x: f32) {
        let w = self.column_w(i);
        self.explorer.cols.resize = Some((i, x, w));
    }

    pub(super) fn column_edge_drag(&mut self, x: f32) {
        let Some((i, x0, w0)) = self.explorer.cols.resize else { return };
        if let Some(p) = self.explorer.cols.panes.get_mut(i) {
            p.width = Some((w0 + x - x0).round().clamp(explorer::COLUMN_MIN, explorer::COLUMN_MAX));
        }
        self.invalidate();
    }

    pub(super) fn column_edge_release(&mut self) {
        if self.explorer.cols.resize.take().is_some() {
            self.invalidate();
        }
    }

    /// The width that shows column `i`'s longest name whole.
    fn column_fit_width(&self, i: usize) -> Option<f32> {
        let g = self.gfx.as_ref()?;
        let c = &self.explorer.cols;
        let rows = if i == c.active { &self.explorer.rows } else { &c.panes.get(i)?.rows };
        let icon_w = self.column_icon().0;
        let widest = rows.iter().map(|r| g.measure(&wide(&r.name), &g.fonts.body)).fold(0.0f32, f32::max);
        // Icon, gaps, the tags' room and the chevron.
        Some((widest + icon_w + 14.0 + 30.0 + 22.0).ceil().clamp(explorer::COLUMN_MIN, explorer::COLUMN_MAX))
    }

    /// Double-click an edge (or its menu's Size to fit): the column just wide enough for its names.
    pub(super) fn column_fit(&mut self, i: usize) {
        if let Some(w) = self.column_fit_width(i) {
            if let Some(p) = self.explorer.cols.panes.get_mut(i) {
                p.width = Some(w);
            }
            self.invalidate();
        }
    }

    pub(super) fn column_fit_all(&mut self) {
        for i in 0..self.explorer.cols.panes.len() {
            self.column_fit(i);
        }
    }

    /// Every column column `i`'s width, from now on (kept in the view).
    pub(super) fn column_all_this(&mut self, i: usize) {
        let w = self.column_w(i);
        self.set_column_widths(w);
        self.show_toast("Every column this width", false);
    }

    /// Column `i` back to the view's width.
    pub(super) fn column_reset(&mut self, i: usize) {
        if let Some(p) = self.explorer.cols.panes.get_mut(i) {
            p.width = None;
        }
        self.invalidate();
    }

    /// Every column back to the default width (and the view with it).
    pub(super) fn column_reset_all(&mut self) {
        self.set_column_widths(explorer::COLUMN_W);
        self.show_toast("Columns back to the default width", false);
    }

    fn set_column_widths(&mut self, w: f32) {
        for p in &mut self.explorer.cols.panes {
            p.width = None;
        }
        self.explorer.look.column_w = w;
        self.store_view();
        self.invalidate();
    }

    /// An edge's menu (right-click), as Finder's: size it, size them all, one width for all, back to defaults.
    pub(super) fn column_edge_menu(&mut self, i: usize, x: f32, y: f32) {
        use super::menus::{Action, MenuKind};
        let own = self.explorer.cols.panes.get(i).is_some_and(|p| p.width.is_some());
        let any = self.explorer.cols.panes.iter().any(|p| p.width.is_some());
        let default = (self.explorer.look.column_w - explorer::COLUMN_W).abs() < 0.5;
        let e = vec![
            super::explorer_ops::item(Action::ColumnFit, 0xE9A6, "Size column to fit", Some("Double-click edge"), true),
            super::explorer_ops::item(Action::ColumnFitAll, 0xE9A6, "Size all columns to fit", None, true),
            crate::ui::Entry::Separator,
            super::explorer_ops::item(Action::ColumnAllThis, 0xE8A9, "Make all columns this width", None, true),
            super::explorer_ops::item(Action::ColumnReset, 0xE7A7, "Reset this column", None, own),
            super::explorer_ops::item(Action::ColumnResetAll, 0xE7A7, "Reset all columns to default width", None, any || !default),
        ];
        self.show_menu(MenuKind::ColumnEdge(i), e, x, y);
    }

    // --- Layout ----------------------------------------------------------------------------------------

    /// The Layout menu's List / Columns: this tab's, and what new tabs open in (Faye, 2026-11-12: no longer kept
    /// per folder).
    pub(super) fn set_layout(&mut self, layout: Layout) {
        if self.settings.explorer_layout != layout {
            self.settings.explorer_layout = layout;
            settings::save(&self.settings);
        }
        if self.explorer.look.layout == layout {
            return;
        }
        // Leaving column view: the column with the keys then lists as it was.
        self.explorer.look.layout = layout;
        match layout {
            Layout::Columns => {
                // Column view lists each folder alone.
                if self.explorer.look.subfolders {
                    self.explorer.look.subfolders = false;
                    self.explorer.watcher = None;
                    self.store_view();
                    let p = self.explorer.place.clone();
                    self.columns_reset(false);
                    if let Some(p) = p {
                        self.explorer_list(p, true);
                    }
                } else {
                    self.store_view();
                    self.columns_reset(true);
                }
            }
            Layout::List => {
                self.explorer.cols.clear();
                self.sync_viewer_sort();
            }
        }
        self.save_tabs();
        self.invalidate();
    }

    // --- Drawing ---------------------------------------------------------------------------------------

    /// The rows' height and their pictures' size (the view's Height, as the list's).
    fn column_icon(&self) -> (f32, f32, f32) {
        let h = self.explorer.look.height;
        let rh = ROW * h as f32;
        if h == 1 { (ICON_W, ICON_H, rh) } else { ((rh - 8.0) * 4.0 / 3.0, rh - 8.0, rh) }
    }

    /// The columns side by side under the crumbs, scrolling sideways when they don't fit, then the filter and
    /// count. True while anything moves.
    pub(super) fn draw_columns(&mut self, g: &Gfx, body: D2D_RECT_F) -> bool {
        let t = theme::t();
        self.columns_sync();
        let footer_h = FILTER_H + 16.0;
        let area = D2D_RECT_F { left: body.left, top: body.top, right: body.right, bottom: body.bottom - footer_h };
        let n = self.explorer.cols.panes.len();
        let widths: Vec<f32> = (0..n).map(|i| self.column_w(i)).collect();
        let total: f32 = widths.iter().sum();
        let avail = (area.right - area.left).max(1.0);
        let c = &mut self.explorer.cols;
        c.max_h = (total - avail).max(0.0);
        if c.hanim.is_none() {
            c.hscroll = c.hscroll.clamp(0.0, c.max_h);
        }
        let jump = std::mem::take(&mut c.jump);
        if std::mem::take(&mut c.reveal) || jump {
            // The last column in view, but never the active one's left edge out of it.
            let active_x: f32 = widths[..c.active.min(n)].iter().sum();
            let (now, _) = c.hscroll_now();
            let mut to = now;
            if total - to > avail {
                to = total - avail;
            }
            to = to.min(active_x).max(0.0);
            if to > total - avail {
                to = (total - avail).max(0.0);
            }
            if jump {
                c.hanim = None;
                c.hscroll = to;
            } else {
                c.scroll_to(to);
            }
        }
        let (hs, mut moving) = c.hscroll_now();
        let rows_bottom = area.bottom - if c.max_h > 0.0 { HBAR } else { 0.0 };
        let active = c.active;

        g.push_clip(area);
        let mut x = area.left - hs;
        let mut edges = Vec::new();
        for (i, &w) in widths.iter().enumerate() {
            let pr = rect(x, area.top, w, rows_bottom - area.top);
            if pr.right > area.left && pr.left < area.right {
                moving |= self.draw_column(g, i, i == active, pr, area);
            }
            edges.push(x + w);
            x += w;
        }
        // The edges over every column (a column's rows mustn't take its left neighbour's edge).
        let resizing = self.explorer.cols.resize.map(|r| r.0);
        for (i, &ex) in edges.iter().enumerate() {
            if ex < area.left - 6.0 || ex > area.right + 6.0 {
                continue;
            }
            let id = Hit::ColumnEdge(i as u16);
            self.hits.add(id, rect(ex - ui::GRIP / 2.0, area.top, ui::GRIP, rows_bottom - area.top));
            let hot = self.hover == Some(id) || resizing == Some(i);
            let k = self.fades.hover(id, hot);
            g.fill(rect(g.snap(ex) - g.px(), area.top, g.px(), rows_bottom - area.top), rgb(t.divider));
            if k > 0.0 {
                ui::grip_line(g, rect(ex - 1.0, area.top + 10.0, 2.0, (rows_bottom - area.top - 20.0).max(0.0)), k, resizing == Some(i));
            }
        }
        g.pop_clip();

        // The sideways scrollbar, whenever the columns overflow.
        let max_h = self.explorer.cols.max_h;
        if max_h > 0.0 {
            let track = rect(area.left, rows_bottom, avail, HBAR);
            self.hits.add(Hit::ColumnHScroll, track);
            let thumb_w = (avail * avail / total).max(28.0);
            let tx = area.left + (avail - thumb_w) * hs / max_h;
            let held = self.explorer.cols.hthumb.is_some();
            ui::scroll_thumb(g, rect(tx, rows_bottom + (HBAR - ui::SCROLL_W) / 2.0, thumb_w, ui::SCROLL_W), held);
        }

        self.draw_footer(g, body, area.bottom, body.right - body.left >= 420.0);
        moving
    }

    /// The scrollbar's thumb: where it is and how wide, and the track.
    fn hthumb_geometry(&self) -> Option<(f32, f32, D2D_RECT_F)> {
        let track = self.hits.rect_of(Hit::ColumnHScroll)?;
        let c = &self.explorer.cols;
        let avail = track.right - track.left;
        let total = avail + c.max_h;
        let w = (avail * avail / total.max(1.0)).max(28.0);
        let left = track.left + (avail - w) * c.hscroll / c.max_h.max(1.0);
        Some((left, w, track))
    }

    pub(super) fn hthumb_press(&mut self, x: f32) {
        let _ = self.explorer.cols.hscroll_now();
        self.explorer.cols.hanim = None;
        let Some((left, w, _)) = self.hthumb_geometry() else { return };
        let offset = if x >= left && x < left + w { x - left } else { w / 2.0 };
        self.explorer.cols.hthumb = Some(offset);
        self.hthumb_move(x);
    }

    pub(super) fn hthumb_move(&mut self, x: f32) {
        let Some(offset) = self.explorer.cols.hthumb else { return };
        let Some((_, w, track)) = self.hthumb_geometry() else { return };
        let travel = (track.right - track.left - w).max(1.0);
        let k = ((x - offset - track.left) / travel).clamp(0.0, 1.0);
        let c = &mut self.explorer.cols;
        c.hscroll = k * c.max_h;
        c.hanim = None;
        self.invalidate();
    }

    /// One column's rows in `pr` (clipped to `area`). The active one is the explorer's list: its hits are the
    /// list's rows (`Hit::ExplorerRow`), so a click, drag, menu or rename works as it does there; the others show
    /// their rows with the folder open in the next column marked. True while it scrolls.
    fn draw_column(&mut self, g: &Gfx, i: usize, active: bool, pr: D2D_RECT_F, area: D2D_RECT_F) -> bool {
        let t = theme::t();
        let (icon_w, icon_h, rh) = self.column_icon();
        let list = D2D_RECT_F { left: pr.left + 4.0, top: pr.top + 4.0, right: pr.right - 6.0, bottom: pr.bottom };
        let clip = D2D_RECT_F { left: pr.left.max(area.left), right: pr.right.min(area.right), top: pr.top, bottom: pr.bottom };
        let view_h = (list.bottom - list.top).max(1.0);
        let next = self.explorer.cols.panes.get(i + 1).and_then(|p| p.place.folder()).map(Path::to_path_buf);
        let (n, scroll, moving) = if active {
            self.hits.add(Hit::ExplorerList, clip);
            self.explorer.list_geom = Some((list, rh, (list.left, list.right)));
            let n = self.explorer.view.len();
            self.explorer.max_scroll = (n as f32 * rh - view_h).max(0.0);
            if let Some(vi) = self.explorer.reveal.take() {
                let (top, bottom) = (vi as f32 * rh, vi as f32 * rh + rh);
                let (cur, _) = self.explorer.scroll_now();
                let to = if top < cur { top } else if bottom > cur + view_h { bottom - view_h } else { cur };
                self.explorer.anim = None;
                self.explorer.scroll = to.clamp(0.0, self.explorer.max_scroll);
            }
            let (s, m) = self.explorer.scroll_now();
            (n, s.clamp(0.0, self.explorer.max_scroll), m)
        } else {
            self.hits.add(Hit::ColumnPane(i as u16), clip);
            let p = &mut self.explorer.cols.panes[i];
            let n = p.rows.len();
            p.max_scroll = (n as f32 * rh - view_h).max(0.0);
            if p.anim.is_none() {
                p.scroll = p.scroll.clamp(0.0, p.max_scroll);
            }
            let (s, m) = p.scroll_now();
            (n, s.clamp(0.0, p.max_scroll), m)
        };
        if active {
            self.explorer.cell_tips.clear();
        }
        let listed = if active { self.explorer.cols.active_listed } else { self.explorer.cols.panes[i].listed };
        g.push_clip(clip);
        if n == 0 && listed {
            let msg = if active && !self.explorer.filter.edit.units().is_empty() { "Nothing matches" } else { "Empty" };
            g.text(&wide(msg), &g.fonts.body, rect(list.left, list.top + 12.0, list.right - list.left, 24.0), rgb(t.placeholder), Align::Center);
        }
        let first = (scroll / rh).floor() as usize;
        let last = (((scroll + view_h) / rh).ceil() as usize + 1).min(n);
        let want = ((icon_w * self.scale() / 64.0).ceil() as u32 * 64).clamp(64, 256);
        let focused = self.explorer.focused;
        let target = self.explorer.row_drag.as_ref().filter(|d| d.active).and_then(|d| d.target.clone());
        let hover_tint = theme::mix(t.sidebar, t.accent, 0.2);
        let many = self.explorer.selected.len() > 1;
        let mut wanted_meta = Vec::new();
        for k in first..last {
            let r = if active { self.explorer.row(k).cloned() } else { self.explorer.cols.panes[i].rows.get(k).cloned() };
            let Some(r) = r else { break };
            let row = rect(list.left, g.snap(list.top + k as f32 * rh - scroll), list.right - list.left, rh);
            let visible = D2D_RECT_F { top: row.top.max(list.top), bottom: row.bottom.min(list.bottom), left: row.left.max(clip.left), right: row.right.min(clip.right) };
            let id = if active { Hit::ExplorerRow(k) } else { Hit::ColumnRow(i as u16, k) };
            if visible.bottom > visible.top && visible.right > visible.left {
                self.hits.add(id, visible);
            }
            let hovered = self.hover == Some(id) && target.is_none();
            let hover = self.fades.hover(id, hovered);
            let (selected, cursor) = if active {
                (self.explorer.is_selected(&r.path), self.explorer.cursor.as_deref().is_some_and(|c| explorer::same_path(c, &r.path)))
            } else {
                (next.as_deref().is_some_and(|nx| explorer::same_path(nx, &r.path)), false)
            };
            let is_target = target.as_deref().is_some_and(|tp| explorer::same_path(tp, &r.path));
            let lit = active && hovered && self.pressed == Some(id) && !self.explorer.row_drag.as_ref().is_some_and(|d| d.active);
            // Several picked: solid accent, as the list's.
            let solid = active && selected && many;
            const SOLID: f32 = 0.6;
            if lit || solid {
                g.fill_round(row, 4.0, gfx::rgba(t.accent, SOLID));
            } else if selected || is_target {
                g.fill_round(row, 4.0, rgb(t.selected));
            } else if hover > 0.0 {
                g.fill_round(row, 4.0, windows::Win32::Graphics::Direct2D::Common::D2D1_COLOR_F { a: hover, ..hover_tint });
            }
            let lit = lit || solid;
            if !lit {
                if is_target || (cursor && (selected || focused)) {
                    ui::stroke(g, row, 4.0, rgb(t.accent));
                } else if hover > 0.0 {
                    ui::stroke(g, row, 4.0, gfx::rgba(t.accent, hover));
                }
            }
            let faded = if r.kind == Kind::Other || self.explorer.is_cut(&r.path) { 0.45 } else { 1.0 };
            let ink = |c: u32| if lit { rgb(t.on_accent) } else { gfx::rgba(c, faded) };
            let look = super::explorer_view::row_look(&r, icon_h);
            if r.kind == Kind::Image && !look.is_audio {
                if !look.is_text {
                    self.thumbs.want(&r.path, r.stamp, want, r.cloud, selected);
                }
                if look.badge {
                    wanted_meta.push((r.path.clone(), r.stamp, r.cloud));
                }
            }
            let meta = self.explorer.meta.get(&explorer::norm(&r.path)).cloned();
            let ib = rect(row.left + 6.0, row.top + (rh - icon_h) / 2.0, icon_w, icon_h);
            self.draw_row_icon(g, &r, ib, lit, faded, meta.as_ref());
            // A folder's chevron at the end, as Finder's.
            let mut name_right = row.right - 6.0;
            if r.is_folder() {
                g.text(&[0xE76C], &g.fonts.caption_icons, rect(name_right - 14.0, row.top, 14.0, rh), ink(t.placeholder), Align::Center);
                name_right -= 18.0;
            }
            let tag = self.explorer.tags.get(&r.path);
            if tag.favourite {
                g.text(&[0xE735], &g.fonts.small_icons, rect(name_right - 14.0, row.top, 14.0, rh), rgb(0xF2C94C), Align::Center);
                name_right -= 18.0;
            }
            if let Some(ci) = tag.colour {
                let col = explorer::COLOURS[ci as usize].1;
                g.fill_round(rect(name_right - 9.0, row.top + (rh - 8.0) / 2.0, 8.0, 8.0), 4.0, rgb(col));
                name_right -= 14.0;
            }
            let tx = row.left + icon_w + 14.0;
            let renaming = active && self.explorer.rename.as_ref().is_some_and(|(p, _)| explorer::same_path(p, &r.path));
            if renaming {
                let fr = rect(tx - 4.0, row.top + (rh - 22.0) / 2.0, (row.right - 4.0 - (tx - 4.0)).max(40.0), 22.0);
                self.hits.add(Hit::ExplorerRename, fr);
                let focused_field = self.explorer.field == Some(super::explorer::EField::Rename);
                if let Some((_, f)) = self.explorer.rename.as_mut() {
                    let caret = f.draw(g, fr, theme::t(), focused_field, 0.0, None);
                    if focused_field {
                        self.caret_rect = caret;
                    }
                }
            } else {
                g.text(&wide(&r.name), &g.fonts.body, rect(tx, row.top, (name_right - tx).max(0.0), rh), ink(t.text), Align::Left);
                if hovered && active {
                    self.explorer.cell_tips.push((row, r.name.clone()));
                }
            }
        }
        // The selection rectangle, in the active column.
        if active {
            if let Some(m) = self.explorer.marquee.as_ref().filter(|m| m.active) {
                let (x0, y0) = (m.start.0, m.start.1 - scroll + list.top);
                let (x1, y1) = m.pointer;
                let r = D2D_RECT_F { left: x0.min(x1).max(list.left), top: y0.min(y1).max(list.top), right: x0.max(x1).min(list.right), bottom: y0.max(y1).min(list.bottom) };
                if r.right > r.left && r.bottom > r.top {
                    g.fill(r, gfx::rgba(t.accent, 0.14));
                    g.outline_round(r, 0.0, rgb(t.accent), g.px());
                }
            }
        }
        g.pop_clip();
        if !wanted_meta.is_empty() {
            self.want_meta(&wanted_meta);
        }
        // Its scrollbar when it overflows (the active one's can be dragged, as the list's).
        let max_scroll = if active { self.explorer.max_scroll } else { self.explorer.cols.panes[i].max_scroll };
        if max_scroll > 0.0 {
            let content_h = view_h + max_scroll;
            let thumb_h = (view_h * view_h / content_h).max(28.0);
            let ty = list.top + (view_h - thumb_h) * scroll / max_scroll;
            if active {
                self.hits.add(Hit::ExplorerThumb, rect(pr.right - 11.0, list.top, 8.0, view_h));
            }
            let held = active && self.explorer.thumb_drag.is_some();
            ui::scroll_thumb(g, rect(pr.right - 5.0 - ui::SCROLL_W, ty, ui::SCROLL_W, thumb_h), held);
        }
        moving
    }
}
