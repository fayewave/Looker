//! The file explorer card (Controls/FileExplorer): the info card's left-hand twin. A flat Explorer-style
//! listing of one folder (the open photo's folder until the user navigates): folders first, then images with
//! thumbnails, then other files greyed out, under the toolbar sort. A header with Back / Forward (browser-style
//! history; mouse buttons 4 and 5 do the same) and breadcrumbs headed by "This PC".
//!
//! Click an image to open it; click a folder to put the cursor on it, double-click (or Enter) to list it; a
//! crumb lists an ancestor and puts the cursor on the folder you came from. Up/Down walk the rows, opening
//! files on the way. The listed folder is watched, so it stays live. An 8 px grip on the right border
//! resizes the card (260–640, double-click resets).

use std::time::Instant;

use super::*;
use crate::explorer::{self, History, Kind, Place, Row, Watcher};
use crate::settings::{EXPLORER_WIDTH, INFO_MAX, INFO_MIN};

const MARGIN: f32 = 12.0;
const HEADER_H: f32 = 40.0;
const NAV_W: f32 = 28.0;
const ROW_H: f32 = 28.0;
const ROW_PITCH: f32 = 29.0;
const LIST_PAD: f32 = 8.0;
const ICON_W: f32 = 28.0;
const ICON_H: f32 = 21.0;
const SCROLL_MS: f32 = 150.0;
const SCROLL_STEP: f32 = 48.0;
const MIN_IMAGE_W: f32 = 480.0;
/// Changes on disk often come in bursts (a copy writes, renames, sets times): re-list once they settle.
pub(super) const REFRESH_MS: u32 = 150;

pub(super) struct Listed {
    pub serial: usize,
    pub place: Place,
    pub rows: Vec<Row>,
    /// A re-list of the same place (the folder changed on disk): keep the scroll.
    pub refresh: bool,
}

pub(super) struct Explorer {
    place: Option<Place>,
    rows: Vec<Row>,
    serial: usize,
    history: History,
    /// The open photo's folder the card last followed.
    photo_folder: Option<PathBuf>,
    /// The keyboard cursor (rides along with the selection; reads like a hover).
    cursor: Option<PathBuf>,
    /// After going up, the cursor lands on the folder we came from.
    came_from: Option<PathBuf>,
    /// Bring this row into view on the next frame.
    reveal: Option<usize>,
    scroll: f32,
    anim: Option<(f32, f32, Instant)>,
    max_scroll: f32,
    watcher: Option<Watcher>,
    pub drag: Option<(f32, f32)>,
    /// The crumbs collapsed into "…" on the last frame.
    hidden_crumbs: Vec<Place>,
}

impl Explorer {
    pub fn new() -> Explorer {
        Explorer {
            place: None,
            rows: Vec::new(),
            serial: 0,
            history: History::default(),
            photo_folder: None,
            cursor: None,
            came_from: None,
            reveal: None,
            scroll: 0.0,
            anim: None,
            max_scroll: 0.0,
            watcher: None,
            drag: None,
            hidden_crumbs: Vec::new(),
        }
    }

    fn scroll_now(&mut self) -> (f32, bool) {
        let Some((from, to, t0)) = self.anim else { return (self.scroll, false) };
        let t = (t0.elapsed().as_secs_f32() * 1000.0 / SCROLL_MS).min(1.0);
        if t >= 1.0 {
            self.anim = None;
            self.scroll = to;
            return (to, false);
        }
        (from + (to - from) * (1.0 - (1.0 - t).powi(3)), true)
    }

    fn index_of(&self, path: &Path) -> Option<usize> {
        self.rows.iter().position(|r| explorer::same_path(&r.path, path))
    }

    pub fn resort(&mut self, sort: crate::folder::Sort) {
        explorer::sort_rows(&mut self.rows, sort);
        self.reveal = self.cursor.as_deref().and_then(|c| self.index_of(c));
    }
}

impl App {
    pub(super) fn explorer_shown(&self) -> bool {
        self.settings.explorer_visible && self.panels_allowed()
    }

    pub(super) fn left_inset(&self) -> f32 {
        self.settings.explorer_width * self.slides.explorer.value()
    }

    /// Slid out to the left by however much of it is hidden.
    fn explorer_rect(&self) -> D2D_RECT_F {
        let v = self.viewport();
        let w = self.settings.explorer_width;
        let dx = w * (1.0 - self.slides.explorer.value());
        D2D_RECT_F { left: v.left + MARGIN - dx, top: v.top + MARGIN, right: v.left + w - MARGIN - dx, bottom: v.bottom - MARGIN }
    }

    pub(super) fn toggle_explorer(&mut self) {
        if self.viewer.current.is_none() {
            return;
        }
        self.settings.explorer_visible = !self.settings.explorer_visible;
        settings::save(&self.settings);
        if !self.settings.explorer_visible {
            self.explorer.watcher = None;
        } else {
            self.explorer.photo_folder = None;
            self.explorer_follow();
        }
        self.slide_panels();
    }

    /// After every navigation: moving into another folder lists it; within one folder the listing stays where
    /// the user took it, and the photo's row is highlighted (and scrolled to) when it is listed.
    pub(super) fn explorer_follow(&mut self) {
        if !self.explorer_shown() {
            return;
        }
        let Some(file) = self.current_path().map(Path::to_path_buf) else { return };
        let Some(folder) = file.parent().map(Path::to_path_buf) else { return };
        let moved = self.explorer.photo_folder.as_deref().is_none_or(|f| !explorer::same_path(f, &folder));
        if moved {
            self.explorer.photo_folder = Some(folder.clone());
            let here = Place::Folder(folder);
            if self.explorer.place.as_ref().is_none_or(|p| !p.same(&here)) {
                self.explorer_go(here, true);
                return;
            }
        }
        if let Some(i) = self.explorer.index_of(&file) {
            self.explorer.cursor = Some(file);
            self.explorer.reveal = Some(i);
        }
        self.invalidate();
    }

    /// Lists a place (a new one goes into the history unless Back/Forward are walking it).
    fn explorer_go(&mut self, place: Place, record: bool) {
        let e = &mut self.explorer;
        // Going up: the cursor will land on the folder we came out of.
        e.came_from = match (&e.place, &place) {
            (Some(Place::Folder(cur)), Place::Folder(to)) => cur.ancestors().find(|a| a.parent().is_some_and(|p| explorer::same_path(p, to))).map(Path::to_path_buf),
            (Some(Place::Folder(cur)), Place::Computer) => cur.ancestors().last().map(Path::to_path_buf),
            _ => None,
        };
        if record {
            e.history.record(&place);
        }
        e.watcher = None;
        e.rows.clear();
        e.scroll = 0.0;
        e.anim = None;
        e.place = Some(place.clone());
        self.explorer_list(place, false);
        self.invalidate();
    }

    fn explorer_list(&mut self, place: Place, refresh: bool) {
        self.explorer.serial += 1;
        let serial = self.explorer.serial;
        let sort = self.settings.sort;
        let hwnd = self.hwnd.0 as isize;
        std::thread::Builder::new()
            .name("explorer".into())
            .spawn(move || {
                let rows = explorer::list(&place, sort);
                let ptr = Box::into_raw(Box::new(Listed { serial, place, rows, refresh }));
                unsafe {
                    if PostMessageW(Some(HWND(hwnd as _)), explorer::WM_EXPLORER_LISTED, WPARAM(0), LPARAM(ptr as isize)).is_err() {
                        drop(Box::from_raw(ptr));
                    }
                }
            })
            .ok();
    }

    pub(super) fn on_explorer_listed(&mut self, l: Listed) {
        if l.serial != self.explorer.serial {
            return; // superseded
        }
        // The covering file was renamed or deleted: the photo underneath comes back.
        if self.placeholder.as_ref().is_some_and(|p| !p.path.exists()) {
            self.placeholder = None;
            self.current_changed();
        }
        let e = &mut self.explorer;
        e.rows = l.rows;
        if e.watcher.is_none() {
            if let Place::Folder(f) = &l.place {
                e.watcher = Watcher::start(f, self.hwnd, explorer::WM_EXPLORER_CHANGED);
            }
        }
        e.place = Some(l.place);
        if !l.refresh {
            // The open photo when it is listed here, else (after going up) the folder we came from.
            let came_from = e.came_from.take();
            let current = self.viewer.current.as_ref().map(|(p, _)| p.clone());
            let target = [current, came_from].into_iter().flatten().find(|c| e.index_of(c).is_some());
            e.reveal = target.as_deref().and_then(|t| e.index_of(t));
            e.cursor = target;
        }
        self.invalidate();
    }

    /// The listed folder changed on disk: re-list it once the burst settles. (A late event from a folder
    /// already left only re-lists the current one, which is harmless.)
    pub(super) fn on_explorer_changed(&mut self) {
        if self.explorer.watcher.is_some() {
            unsafe {
                SetTimer(Some(self.hwnd), TIMER_EXPLORER, REFRESH_MS, None);
            }
        }
    }

    pub(super) fn explorer_refresh(&mut self) {
        unsafe {
            let _ = KillTimer(Some(self.hwnd), TIMER_EXPLORER);
        }
        if let Some(p) = self.explorer.place.clone() {
            self.explorer_list(p, true);
        }
    }

    pub(super) fn explorer_back(&mut self) {
        if let Some(p) = self.explorer.history.back() {
            self.explorer_go(p, false);
        }
    }

    pub(super) fn explorer_forward(&mut self) {
        if let Some(p) = self.explorer.history.forward() {
            self.explorer_go(p, false);
        }
    }

    pub(super) fn explorer_crumb(&mut self, place: Place) {
        if self.explorer.place.as_ref().is_none_or(|p| !p.same(&place)) {
            self.explorer_go(place, true);
        }
    }

    /// Opens a file from the card: an index jump when it is in the open folder, else a full open.
    pub(super) fn open_file(&mut self, path: PathBuf) {
        let at = self.viewer.listing.as_ref().and_then(|l| l.images.iter().position(|e| explorer::same_path(&e.path, &path)));
        // Back onto the image the placeholder covered: no navigation happens, so announce it again.
        let reshow = self.placeholder.take().is_some();
        match at {
            Some(i) if Some(i) == self.viewer.index => {
                if reshow {
                    self.current_changed();
                }
            }
            Some(i) => self.go_to(i),
            None => self.open(path),
        }
    }

    /// The explorer landed on a file Looker can't open: a file glyph and its name cover the photo, and the
    /// title, status row and counter describe that file. The photo stays current (←/→ go on from it); any
    /// navigation takes the cover down.
    fn show_placeholder(&mut self, path: PathBuf) {
        let md = std::fs::metadata(&path).ok();
        let modified = md.as_ref().map(|m| {
            let t = m.last_write_time();
            FILETIME { dwLowDateTime: t as u32, dwHighDateTime: (t >> 32) as u32 }
        });
        unsafe {
            let _ = SetWindowTextW(self.hwnd, &HSTRING::from(file_name(&path)));
        }
        self.placeholder = Some(Placeholder { size: md.map(|m| m.len()), modified, path });
        self.invalidate();
    }

    /// The status row for the covering file: "PS1 file · 2.1 KB · Modified …" (no size in pixels, no zoom).
    pub(super) fn placeholder_status(&self) -> Option<String> {
        let p = self.placeholder.as_ref()?;
        let ext = p.path.extension().map(|e| e.to_string_lossy().to_uppercase()).unwrap_or_default();
        let mut parts = vec![if ext.is_empty() { "File".to_string() } else { format!("{ext} file") }];
        if let Some(s) = p.size {
            parts.push(format_bytes(s));
        }
        if let Some(t) = p.modified {
            parts.push(format!("Modified {}", format_time(t)));
        }
        Some(parts.join("   ·   "))
    }

    /// Covers the viewport in the window colour, with the glyph and name centred between the cards.
    pub(super) fn draw_placeholder(&self, g: &Gfx) {
        let Some(p) = &self.placeholder else { return };
        g.fill(self.viewport(), rgb(self.theme().window));
        let a = self.image_area();
        let name = wide(&file_name(&p.path));
        let w = (a.right - a.left - 32.0).clamp(1.0, 520.0);
        let name_h = g.measure_height(&name, &g.fonts.subtitle_wrap, w);
        let icon_h = 120.0;
        let top = ((a.top + a.bottom) - (icon_h + 20.0 + name_h)) / 2.0;
        let cx = (a.left + a.right) / 2.0;
        g.text(&[0xE7C3], &g.fonts.hero_icons, rect(cx - 80.0, top, 160.0, icon_h), white(TEXT_SECONDARY), Align::Center);
        g.text(&name, &g.fonts.subtitle_wrap, rect(cx - w / 2.0, top + icon_h + 20.0, w, name_h + 4.0), white(0xFF), Align::Center);
    }

    /// A row was clicked (or reached with the arrow keys): images open, folders and other files take the cursor.
    fn explorer_pick(&mut self, i: usize) {
        let Some(r) = self.explorer.rows.get(i).cloned() else { return };
        self.explorer.cursor = Some(r.path.clone());
        self.explorer.reveal = Some(i);
        match r.kind {
            Kind::Image => self.open_file(r.path),
            Kind::Other => self.show_placeholder(r.path),
            Kind::Drive | Kind::Folder => {}
        }
        self.invalidate();
    }

    pub(super) fn explorer_click(&mut self, i: usize) {
        self.explorer_pick(i);
    }

    pub(super) fn explorer_double_click(&mut self, i: usize) {
        if let Some(r) = self.explorer.rows.get(i).filter(|r| r.is_folder()) {
            let p = Place::Folder(r.path.clone());
            self.explorer_go(p, true);
        }
    }

    /// Up/Down: the cursor moves a row; a file under it opens.
    pub(super) fn explorer_move(&mut self, delta: isize) {
        let n = self.explorer.rows.len();
        if n == 0 {
            return;
        }
        let at = self.explorer.cursor.as_deref().and_then(|c| self.explorer.index_of(c));
        let i = match at {
            None => if delta > 0 { 0 } else { n - 1 },
            Some(i) => (i as isize + delta).clamp(0, n as isize - 1) as usize,
        };
        self.explorer_pick(i);
    }

    /// Page Up / Page Down: the cursor to the top or bottom of the list.
    pub(super) fn explorer_jump(&mut self, last: bool) {
        let n = self.explorer.rows.len();
        if n > 0 {
            self.explorer_pick(if last { n - 1 } else { 0 });
        }
    }

    /// Enter: list the folder under the cursor. False when there's nothing to do.
    pub(super) fn explorer_enter(&mut self) -> bool {
        let Some(i) = self.explorer.cursor.as_deref().and_then(|c| self.explorer.index_of(c)) else { return false };
        if self.explorer.rows[i].is_folder() {
            self.explorer_double_click(i);
        } else {
            self.explorer_pick(i);
        }
        true
    }

    /// "Open folder" from a folder's menu: open it in the viewer (its first image), as File > Open folder would.
    pub(super) fn open_folder(&mut self, folder: PathBuf) {
        self.pending_folder = Some(folder.clone());
        self.list_folder_async(folder);
    }

    /// The menu for a row (right-click).
    pub(super) fn explorer_menu(&mut self, i: usize, x: f32, y: f32) {
        let Some(r) = self.explorer.rows.get(i).cloned() else { return };
        self.explorer.cursor = Some(r.path.clone());
        self.open_item_menu(r.path, r.kind, x, y);
    }

    pub(super) fn scroll_explorer(&mut self, delta: f32) {
        let e = &mut self.explorer;
        let (now, _) = e.scroll_now();
        let target = e.anim.map_or(e.scroll, |(_, to, _)| to);
        let to = (target - delta / 120.0 * SCROLL_STEP).clamp(0.0, e.max_scroll.max(0.0));
        e.scroll = now;
        e.anim = Some((now, to, Instant::now()));
        self.invalidate();
    }

    // --- Resize grip (dragging right = wider) -------------------------------------------------------

    pub(super) fn explorer_grip_press(&mut self, x: f32) {
        self.explorer.drag = Some((x, self.settings.explorer_width));
    }

    pub(super) fn explorer_grip_drag(&mut self, x: f32) {
        let Some((x0, w0)) = self.explorer.drag else { return };
        let (ww, _) = self.size_dip();
        let cap = INFO_MAX.min((ww - MIN_IMAGE_W).max(INFO_MIN));
        let w = (w0 + (x - x0)).clamp(INFO_MIN, cap).round();
        if w != self.settings.explorer_width {
            self.settings.explorer_width = w;
            self.layout_changed();
        }
    }

    pub(super) fn explorer_grip_release(&mut self) {
        if self.explorer.drag.take().is_some() {
            settings::save(&self.settings);
            self.invalidate();
        }
    }

    pub(super) fn explorer_grip_reset(&mut self) {
        self.explorer.drag = None;
        self.settings.explorer_width = EXPLORER_WIDTH;
        settings::save(&self.settings);
        self.layout_changed();
    }

    pub(super) fn explorer_tooltip(&self, h: Hit) -> Option<String> {
        match h {
            Hit::ExplorerRow(i) => self.explorer.rows.get(i).map(|r| {
                if r.kind == Kind::Other { format!("{}: not an image Looker can open", r.name) } else { r.name.clone() }
            }),
            Hit::Crumb(_) | Hit::CrumbMore => self.explorer.place.as_ref().map(|p| match p {
                Place::Computer => explorer::COMPUTER_NAME.to_string(),
                Place::Folder(f) => f.to_string_lossy().into_owned(),
            }),
            Hit::ExplorerBack => Some("Back".into()),
            Hit::ExplorerForward => Some("Forward".into()),
            _ => None,
        }
    }

    /// The crumbs "…" hides, for its menu.
    pub(super) fn hidden_crumbs(&self) -> Vec<Place> {
        self.explorer.hidden_crumbs.clone()
    }

    // --- Drawing ------------------------------------------------------------------------------------

    pub(super) fn draw_explorer(&mut self, g: &Gfx) -> bool {
        if self.slides.explorer.value() <= 0.0 {
            return false;
        }
        let card = self.explorer_rect();
        if card.bottom - card.top < 80.0 || card.right - card.left < 80.0 {
            return false;
        }
        self.hits.add(Hit::ExplorerCard, card);
        g.fill_round(card, 8.0, rgb(self.theme().window));
        g.outline_round(card, 8.0, white(0x18), g.px());
        let inner = D2D_RECT_F { left: card.left + 1.0, top: card.top + 1.0, right: card.right - 1.0, bottom: card.bottom - 1.0 };

        // Header: Back, Forward, crumbs.
        let hy = inner.top + 6.0;
        let back = rect(inner.left + 6.0, hy, NAV_W, NAV_W);
        let fwd = rect(back.right + 2.0, hy, NAV_W, NAV_W);
        for (id, r, glyph, enabled) in [
            (Hit::ExplorerBack, back, 0xE72Bu16, self.explorer.history.can_back()),
            (Hit::ExplorerForward, fwd, 0xE72A, self.explorer.history.can_forward()),
        ] {
            self.hits.add(id, r);
            let st = self.state(id, enabled);
            let fg = ui::button_frame(g, r, ui::Kind::Subtle, &st);
            g.text(&[glyph], &g.fonts.small_icons, r, fg, Align::Center);
        }
        self.draw_crumbs(g, fwd.right + 6.0, inner.right - 8.0, hy, NAV_W);
        let div_y = inner.top + HEADER_H;
        g.hline(inner.left, g.snap(div_y), inner.right - inner.left, white(0x15));

        // Rows.
        let list = D2D_RECT_F { left: inner.left + LIST_PAD, top: div_y + 1.0 + 6.0, right: inner.right - LIST_PAD, bottom: inner.bottom - LIST_PAD };
        let view_h = (list.bottom - list.top).max(1.0);
        let n = self.explorer.rows.len();
        let content_h = if n == 0 { 0.0 } else { n as f32 * ROW_PITCH - (ROW_PITCH - ROW_H) };
        self.explorer.max_scroll = (content_h - view_h).max(0.0);
        if let Some(i) = self.explorer.reveal.take() {
            // Instantly: after a listing lands, a glide from the top down to the photo reads as lag.
            let (top, bottom) = (i as f32 * ROW_PITCH, i as f32 * ROW_PITCH + ROW_H);
            let (cur, _) = self.explorer.scroll_now();
            let to = if top < cur { top } else if bottom > cur + view_h { bottom - view_h } else { cur };
            self.explorer.anim = None;
            self.explorer.scroll = to.clamp(0.0, self.explorer.max_scroll);
        }
        let (scroll, moving) = self.explorer.scroll_now();
        let scroll = scroll.clamp(0.0, self.explorer.max_scroll);
        let first = (scroll / ROW_PITCH).floor() as usize;
        let last = (((scroll + view_h) / ROW_PITCH).ceil() as usize + 1).min(n);
        let current = self.current_path().map(Path::to_path_buf);
        let want = ((ICON_W * self.scale() / 64.0).ceil() as u32 * 64).clamp(64, 256);

        g.push_clip(D2D_RECT_F { top: list.top, bottom: inner.bottom, ..inner });
        for i in first..last {
            let r = self.explorer.rows[i].clone();
            let row = rect(list.left, g.snap(list.top + i as f32 * ROW_PITCH - scroll), list.right - list.left, ROW_H);
            let visible = D2D_RECT_F { top: row.top.max(list.top), bottom: row.bottom.min(inner.bottom), ..row };
            if visible.bottom > visible.top {
                self.hits.add(Hit::ExplorerRow(i), visible);
            }
            let selected = current.as_deref().is_some_and(|c| explorer::same_path(c, &r.path));
            let hot = self.hover == Some(Hit::ExplorerRow(i)) || self.explorer.cursor.as_deref().is_some_and(|c| explorer::same_path(c, &r.path));
            let pressed = self.pressed == Some(Hit::ExplorerRow(i)) && self.hover == Some(Hit::ExplorerRow(i));
            if selected {
                g.fill_round(row, 4.0, white(if pressed { 0x2E } else { 0x1F }));
                g.outline_round(row, 4.0, gfx::rgba(ui::ACCENT, 1.0), 1.0);
            } else if pressed {
                g.fill_round(row, 4.0, white(0x26));
                g.outline_round(row, 4.0, white(0x80), 1.0);
            } else if hot {
                g.fill_round(row, 4.0, white(0x14));
                g.outline_round(row, 4.0, white(0x55), 1.0);
            }
            let alpha = if r.kind == Kind::Other { 0x73 } else { 0xFF }; // 45% for files Looker can't open
            let ib = rect(row.left + 5.0, row.top + (ROW_H - ICON_H) / 2.0, ICON_W, ICON_H);
            let mut drew = false;
            if r.kind == Kind::Image {
                g.fill_round(ib, 2.0, white(0x1F));
                if let Some(t) = self.thumbs.get(&r.path, r.stamp) {
                    t.draw_fit(g, ib);
                    drew = true;
                }
                self.thumbs.want(&r.path, r.stamp, want, r.cloud, selected);
            }
            if !drew {
                let glyph = match r.kind {
                    Kind::Drive => 0xEDA2u16,
                    Kind::Folder => 0xE8B7,
                    Kind::Image => 0xE91B,
                    Kind::Other => 0xE7C3,
                };
                g.text(&[glyph], &g.fonts.small_icons, ib, white(alpha), Align::Center);
            }
            let tx = ib.right + 6.0;
            g.text(&wide(&r.name), &g.fonts.caption, rect(tx, row.top, (row.right - 5.0 - tx).max(0.0), ROW_H), white(alpha), Align::Left);
        }
        g.pop_clip();

        let over = matches!(self.hover, Some(Hit::ExplorerCard | Hit::ExplorerRow(_)));
        if self.explorer.max_scroll > 0.0 && (over || moving) {
            let thumb_h = (view_h * view_h / content_h).max(24.0);
            let ty = list.top + (view_h - thumb_h) * scroll / self.explorer.max_scroll;
            g.fill_round(rect(inner.right - 5.0, ty, 2.0, thumb_h), 1.0, white(0x8B));
        }

        // The grip straddles the right border.
        let grip = rect(card.right - 4.0, card.top, 8.0, card.bottom - card.top);
        self.hits.add(Hit::ExplorerGrip, grip);
        let dragging = self.explorer.drag.is_some();
        let t = self.fades.get(Hit::ExplorerGrip, self.hover == Some(Hit::ExplorerGrip) || dragging);
        if t > 0.0 {
            let c = if dragging { gfx::rgba(ui::ACCENT, 1.0) } else { gfx::rgba(0xFFFFFF, 0.4 * t) };
            g.fill_round(rect(card.right - 1.0, card.top, 2.0, card.bottom - card.top), 1.0, c);
        }
        moving
    }

    /// Crumbs from `x0` to `x1`: ancestors are links, the last one (where you are) is plain. When they don't
    /// fit, the leading ones fold into "…", which opens a menu of them.
    fn draw_crumbs(&mut self, g: &Gfx, x0: f32, x1: f32, y: f32, h: f32) {
        let Some(place) = self.explorer.place.clone() else {
            self.explorer.hidden_crumbs.clear();
            return;
        };
        let all = explorer::crumbs(&place);
        let names: Vec<String> = all.iter().map(Place::name).collect();
        // Each link is padded for its rounded hover fill; the last crumb (where you are) is plain text, padded on
        // its left only, so the chevron before it sits evenly between the two names.
        const PAD: f32 = 6.0;
        let widths: Vec<f32> = names.iter().enumerate().map(|(i, n)| g.measure(&wide(n), &g.fonts.caption) + 2.0 + if i + 1 < names.len() { 2.0 * PAD } else { PAD }).collect();
        const CHEVRON: f32 = 13.0;
        let more_w = g.measure(&wide("\u{2026}"), &g.fonts.caption) + 2.0 + 2.0 * PAD;
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
            let t = self.fades.get(Hit::CrumbMore, self.hover == Some(Hit::CrumbMore));
            let pressed = self.pressed == Some(Hit::CrumbMore) && self.hover == Some(Hit::CrumbMore);
            ui::link_fill(g, crumb_fill(r), t, pressed);
            let c = if pressed { white(0xFF) } else { white((TEXT_SECONDARY as f32 + (255.0 - TEXT_SECONDARY as f32) * t) as u8) };
            g.text(&wide("\u{2026}"), &g.fonts.caption, r, c, Align::Center);
            x += more_w;
            g.text(&[0xE76C], &g.fonts.caption_icons, rect(x, y, CHEVRON, h), white(TEXT_SECONDARY), Align::Center);
            x += CHEVRON;
        }
        for i in from..all.len() {
            let last = i + 1 == all.len();
            let w = if last { widths[i].min((x1 - x).max(0.0)) } else { widths[i] };
            let r = rect(x, y, w, h);
            if last {
                g.text(&wide(&names[i]), &g.fonts.caption, D2D_RECT_F { left: r.left + PAD, ..r }, white(0xFF), Align::Left);
                self.hits.add(Hit::Crumb(i), r);
            } else {
                self.hits.add(Hit::Crumb(i), r);
                let id = Hit::Crumb(i);
                let t = self.fades.get(id, self.hover == Some(id));
                let pressed = self.pressed == Some(id) && self.hover == Some(id);
                ui::link_fill(g, crumb_fill(r), t, pressed);
                let c = if pressed { white(0xFF) } else { white((TEXT_SECONDARY as f32 + (255.0 - TEXT_SECONDARY as f32) * t) as u8) };
                g.text(&wide(&names[i]), &g.fonts.caption, r, c, Align::Center);
                x += w;
                g.text(&[0xE76C], &g.fonts.caption_icons, rect(x, y, CHEVRON, h), white(TEXT_SECONDARY), Align::Center);
                x += CHEVRON;
            }
        }
    }

    /// A crumb was clicked (not the last: that is where you are).
    pub(super) fn crumb_click(&mut self, i: usize) {
        let Some(place) = self.explorer.place.clone() else { return };
        let all = explorer::crumbs(&place);
        if i + 1 < all.len() {
            self.explorer_crumb(all[i].clone());
        }
    }
}

/// A crumb's hover fill: its padded width, a little inside the header row's height.
fn crumb_fill(r: D2D_RECT_F) -> D2D_RECT_F {
    D2D_RECT_F { top: r.top + 3.0, bottom: r.bottom - 3.0, ..r }
}
