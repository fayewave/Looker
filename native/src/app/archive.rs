//! An archive on screen (zip, 7z, RAR, tar..., imaging/archive.rs): a card in the photo area, the explorer card's
//! height and look, listing what's inside one folder at a time: Back, Forward and Up (and the mouse's Back and
//! Forward buttons) with the folder's path beside them, the explorer's rows (its row height, stripes, hover, selection
//! and cursor), thumbnails for its pictures. Rows are selected as in the explorer (click, Ctrl, Shift, the selection
//! rectangle from beside the names, Ctrl+A, the arrows); double-clicking a folder goes in (a file: nothing). Rows drag
//! out onto the explorer's folders (extracted there) or out of the window (extracted to a temporary folder first);
//! files dragged in from the explorer are added (the archive written anew, for the formats libarchive writes).
//! Extract all unpacks beside the archive into a folder named after it, Extract to… into a folder picked; with rows
//! selected, both take only those. (The explorer's row menu extracts an archive without opening it: app/extract.rs.)

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use super::*;
use crate::imaging::archive::{self as arc, Entry};

/// The explorer's own row and header heights (explorer_view.rs: row_h, HEADER), and the footer's.
const ROW_BASE: f32 = 26.0;
const COLS_H: f32 = 26.0;
const HEADER_H: f32 = 46.0;
const FOOT_H: f32 = 44.0;
/// A press moved this far is a drag or a rectangle, not a click (the explorer's DRAG_START).
const DRAG_START: f32 = 5.0;

/// What an extraction or an add is doing, for the footer, and what to say once it's done.
struct Work {
    state: Arc<Mutex<WorkState>>,
    cancel: Arc<AtomicBool>,
    total: u64,
    kind: WorkKind,
}

enum WorkKind {
    /// Extracting into this folder.
    Extract(PathBuf),
    /// Adding files into this folder of the archive (it's listed again after).
    Add(String),
}

#[derive(Default)]
struct WorkState {
    written: u64,
    name: String,
    /// Files done, or what went wrong.
    result: Option<Result<(usize, usize, usize), String>>,
}

type Thumbs = Arc<Mutex<Vec<(String, u32, u32, Vec<u8>)>>>;

/// The selection rectangle: where it was pressed (x, and y in the list's own scrolled space), the pointer, whether
/// it has moved far enough to be one, whether it adds to the selection (Ctrl), and what was selected before.
struct Rubber {
    start: (f32, f32),
    pointer: (f32, f32),
    active: bool,
    add: bool,
    before: BTreeSet<usize>,
}

pub(super) struct ArchiveView {
    pub path: PathBuf,
    listing: Arc<Mutex<Option<Result<Vec<Entry>, String>>>>,
    entries: Option<Result<Vec<Entry>, String>>,
    /// The folder of the archive showing ("" its top), and its rows: indexes into `entries`, folders first.
    dir: String,
    rows: Vec<usize>,
    selected: BTreeSet<usize>,
    /// The cursor (the last row clicked or stepped to) and where a Shift-click reaches from.
    anchor: Option<usize>,
    scroll: f32,
    /// Folders visited, for Back and Forward.
    back: Vec<String>,
    forward: Vec<String>,
    rubber: Option<Rubber>,
    /// A press on a selected row: a click (no drag) picks it alone when the button comes up.
    pending: Option<usize>,
    thumbs: HashMap<String, ID2D1Bitmap1>,
    thumb_px: Thumbs,
    /// Bumped when the folder changes: the thumbnail thread for an older one stops.
    thumb_gen: Arc<AtomicU64>,
    work: Option<Work>,
    /// Whether files can be added (asked once, when something is first dragged over it).
    writable: Option<Result<(), String>>,
    /// Whether it has the arrows (clicked into, or nothing else to have them): arrowing through the explorer onto
    /// an archive mustn't stop there (Faye, 2026-11-12).
    focused: bool,
}

impl ArchiveView {
    pub(super) fn device_lost(&mut self) {
        self.thumbs.clear();
        self.thumb_gen.fetch_add(1, Ordering::Relaxed);
        self.start_thumbs();
    }

    fn list(&self) -> Option<&Vec<Entry>> {
        self.entries.as_ref()?.as_ref().ok()
    }

    fn entry(&self, row: usize) -> Option<&Entry> {
        self.list()?.get(*self.rows.get(row)?)
    }

    /// The rows of folder `dir`: its direct children, folders first, then by name (as Explorer sorts).
    fn show_dir(&mut self, dir: String) {
        self.dir = dir;
        self.selected.clear();
        self.anchor = None;
        self.pending = None;
        self.rubber = None;
        self.scroll = 0.0;
        let Some(list) = self.list() else { return };
        let prefix = if self.dir.is_empty() { String::new() } else { format!("{}/", self.dir) };
        let mut rows: Vec<usize> = list
            .iter()
            .enumerate()
            .filter(|(_, e)| e.path.strip_prefix(&prefix).is_some_and(|rest| !rest.is_empty() && !rest.contains('/')))
            .map(|(i, _)| i)
            .collect();
        rows.sort_by(|&a, &b| {
            let (a, b) = (&list[a], &list[b]);
            b.dir.cmp(&a.dir).then_with(|| natural(a.name(), b.name()))
        });
        self.rows = rows;
        self.thumb_gen.fetch_add(1, Ordering::Relaxed);
        self.start_thumbs();
    }

    /// Goes to folder `dir`, remembered for Back.
    fn go(&mut self, dir: String) {
        if dir == self.dir {
            return;
        }
        self.back.push(std::mem::take(&mut self.dir));
        self.forward.clear();
        self.show_dir(dir);
    }

    /// Thumbnails of this folder's pictures, read in the archive's order on a thread of its own (a solid 7z or RAR
    /// can only be read through), each handed over as it's decoded.
    fn start_thumbs(&mut self) {
        let Some(list) = self.list() else { return };
        let want: std::collections::HashSet<String> =
            self.rows.iter().map(|&i| &list[i]).filter(|e| !e.dir && arc::is_picture(&e.path) && e.size <= 40 << 20).map(|e| e.path.clone()).collect();
        if want.is_empty() {
            return;
        }
        let (path, out, generation) = (self.path.clone(), self.thumb_px.clone(), self.thumb_gen.clone());
        let mine = generation.load(Ordering::Relaxed);
        std::thread::Builder::new()
            .name("archive-thumbs".into())
            .spawn(move || {
                unsafe {
                    let _ = windows::Win32::System::Com::CoInitializeEx(None, windows::Win32::System::Com::COINIT_MULTITHREADED);
                }
                let Ok(f) = (unsafe {
                    windows::Win32::System::Com::CoCreateInstance::<_, windows::Win32::Graphics::Imaging::IWICImagingFactory>(
                        &windows::Win32::Graphics::Imaging::CLSID_WICImagingFactory2,
                        None,
                        windows::Win32::System::Com::CLSCTX_INPROC_SERVER,
                    )
                }) else {
                    return;
                };
                let _ = arc::read_files(&path, 40 << 20, &|e| want.contains(&e.path), &mut |e, bytes| {
                    if generation.load(Ordering::Relaxed) != mine {
                        return false;
                    }
                    if let Ok(d) = crate::imaging::wic::decode(&f, crate::imaging::wic::Source::Memory(&bytes), 256, 256) {
                        if let Some(fr) = d.frames.into_iter().next() {
                            if let Ok(mut o) = out.lock() {
                                o.push((e.path.clone(), d.width, d.height, fr.pixels));
                            }
                        }
                    }
                    true
                });
            })
            .ok();
    }

    /// Reads the listing again (after files were added), staying in folder `dir`.
    fn relist(&mut self, dir: String) {
        let listing = Arc::new(Mutex::new(None));
        let (l, p) = (listing.clone(), self.path.clone());
        std::thread::Builder::new()
            .name("archive-list".into())
            .spawn(move || {
                let r = arc::list(&p);
                if let Ok(mut l) = l.lock() {
                    *l = Some(r);
                }
            })
            .ok();
        self.listing = listing;
        self.entries = None;
        self.dir = dir;
        self.thumbs.clear();
        self.writable = None;
    }
}

/// Names compared as Explorer does: case ignored, runs of digits by value ("2" before "10").
fn natural(a: &str, b: &str) -> std::cmp::Ordering {
    let (mut x, mut y) = (a.chars().peekable(), b.chars().peekable());
    loop {
        match (x.peek(), y.peek()) {
            (None, None) => return std::cmp::Ordering::Equal,
            (None, _) => return std::cmp::Ordering::Less,
            (_, None) => return std::cmp::Ordering::Greater,
            (Some(c), Some(d)) if c.is_ascii_digit() && d.is_ascii_digit() => {
                let take = |it: &mut std::iter::Peekable<std::str::Chars>| {
                    let mut s = String::new();
                    while let Some(c) = it.peek().filter(|c| c.is_ascii_digit()) {
                        s.push(*c);
                        it.next();
                    }
                    s
                };
                let (m, n) = (take(&mut x), take(&mut y));
                let (m2, n2) = (m.trim_start_matches('0'), n.trim_start_matches('0'));
                let o = m2.len().cmp(&n2.len()).then_with(|| m2.cmp(n2));
                if o != std::cmp::Ordering::Equal {
                    return o;
                }
            }
            (Some(c), Some(d)) => {
                let o = c.to_lowercase().cmp(d.to_lowercase());
                if o != std::cmp::Ordering::Equal {
                    return o;
                }
                x.next();
                y.next();
            }
        }
    }
}

/// Unix seconds as the explorer's date column shows them.
fn date_text(secs: i64) -> String {
    if secs <= 0 {
        return String::new();
    }
    let t = ((secs + 11_644_473_600) as u64) * 10_000_000;
    format_column_time(windows::Win32::Foundation::FILETIME { dwLowDateTime: t as u32, dwHighDateTime: (t >> 32) as u32 })
}

/// The folder an archive extracts into beside it: its name without the archive's extensions ("photos.tar.gz" ->
/// "photos").
pub(super) fn base_name(path: &Path) -> String {
    let mut name = path.file_name().and_then(|n| n.to_str()).unwrap_or("archive").to_string();
    for _ in 0..2 {
        if let Some((stem, ext)) = name.rsplit_once('.') {
            let ext = ext.to_ascii_lowercase();
            if crate::format::from_extension(Some(&ext)) == crate::format::Format::Archive || ext == "tar" {
                name = stem.to_string();
                continue;
            }
        }
        break;
    }
    if name.is_empty() { "archive".into() } else { name }
}

/// A new folder in `parent` named `name`, " (2)" and so on after it when that's taken.
pub(super) fn unique_folder(parent: &Path, name: &str) -> PathBuf {
    let mut dest = parent.join(name);
    let mut n = 2;
    while dest.exists() {
        dest = parent.join(format!("{name} ({n})"));
        n += 1;
    }
    dest
}

/// Where the list's pieces are: the card, the list's rect, the row height, the Name column's span (a press beside
/// it starts a rectangle), the thumbnail's size.
struct Geom {
    card: D2D_RECT_F,
    list: D2D_RECT_F,
    rh: f32,
    name: (f32, f32),
    icon: (f32, f32),
}

impl App {
    /// The archive's file count and unpacked size, once listed.
    pub(super) fn archive_facts(&self) -> Option<(usize, u64)> {
        let list = self.archive.as_ref()?.list()?;
        Some((list.iter().filter(|e| !e.dir).count(), list.iter().map(|e| e.size.max(0) as u64).sum()))
    }

    pub(super) fn archive_shown(&self) -> bool {
        self.archive.is_some() && self.placeholder.is_none()
    }

    /// Before each frame: an archive on screen gets its view (and its listing, read on a thread); what the threads
    /// have made comes in.
    pub(super) fn prepare_archive(&mut self, g: &Gfx) {
        let want = self.current_path().filter(|p| format::of_path(p) == format::Format::Archive && self.placeholder.is_none()).map(Path::to_path_buf);
        if self.archive.as_ref().map(|a| &a.path) != want.as_ref() {
            let focused = !self.explorer_shown();
            self.archive = want.map(|path| {
                let mut a = ArchiveView {
                    path,
                    listing: Arc::new(Mutex::new(None)),
                    entries: None,
                    dir: String::new(),
                    rows: Vec::new(),
                    selected: BTreeSet::new(),
                    anchor: None,
                    scroll: 0.0,
                    back: Vec::new(),
                    forward: Vec::new(),
                    rubber: None,
                    pending: None,
                    thumbs: HashMap::new(),
                    thumb_px: Arc::new(Mutex::new(Vec::new())),
                    thumb_gen: Arc::new(AtomicU64::new(0)),
                    work: None,
                    writable: None,
                    focused,
                };
                a.relist(String::new());
                a
            });
        }
        let Some(a) = self.archive.as_mut() else { return };
        if a.entries.is_none() {
            if let Some(r) = a.listing.lock().ok().and_then(|mut l| l.take()) {
                a.entries = Some(r);
                let dir = std::mem::take(&mut a.dir);
                // Still there after a relist? Else its top.
                let keep = dir.is_empty() || a.list().is_some_and(|l| l.iter().any(|e| e.dir && e.path == dir));
                a.show_dir(if keep { dir } else { String::new() });
            }
        }
        let made: Vec<_> = a.thumb_px.lock().map(|mut v| std::mem::take(&mut *v)).unwrap_or_default();
        for (p, w, h, px) in made {
            if let Ok(b) = g.bitmap(w, h, &px) {
                a.thumbs.insert(p, b);
            }
        }
        // An extraction or an add finished.
        let finished = a.work.as_ref().and_then(|w| w.state.lock().ok().and_then(|mut s| s.result.take()));
        if let Some(result) = finished {
            let w = a.work.take().unwrap();
            match (result, w.kind) {
                (Ok((files, enc, failed)), WorkKind::Extract(dest)) => {
                    let name = dest.file_name().and_then(|n| n.to_str()).unwrap_or("").to_string();
                    let mut msg = format!("Extracted {files} file{} to \u{201c}{name}\u{201d}", if files == 1 { "" } else { "s" });
                    if enc > 0 {
                        msg.push_str(&format!(", {enc} encrypted left out"));
                    }
                    if failed > 0 {
                        msg.push_str(&format!(", {failed} couldn't be written"));
                    }
                    self.show_toast(msg, false);
                }
                (Ok((files, _, _)), WorkKind::Add(dir)) => {
                    a.relist(dir);
                    self.show_toast(format!("Added {files} file{}", if files == 1 { "" } else { "s" }), false);
                }
                (Err(e), WorkKind::Add(dir)) => {
                    a.relist(dir);
                    self.show_toast(format!("Couldn't add: {e}"), false);
                }
                (Err(e), WorkKind::Extract(_)) => self.show_toast(format!("Couldn't extract: {e}"), false),
            }
            self.invalidate();
        }
    }

    /// Still reading the listing or thumbnails, or working: draw again next vsync.
    pub(super) fn archive_animating(&self) -> bool {
        self.archive.as_ref().is_some_and(|a| a.entries.is_none() || a.work.is_some() || a.thumb_px.lock().is_ok_and(|v| !v.is_empty()) || Arc::strong_count(&a.thumb_px) > 1)
    }

    /// The card: the photo area's width, the explorer card's top and bottom.
    fn archive_geom(&self) -> Geom {
        let a = self.image_area();
        let v = self.viewport();
        let m = super::explorer::MARGIN;
        let (ml, mr) = ((m - self.left_inset()).max(0.0), (m - self.right_inset()).max(0.0));
        let card = D2D_RECT_F { left: a.left + ml, top: v.top + m, right: a.right - mr, bottom: v.bottom - m };
        let list = D2D_RECT_F { left: card.left + 6.0, top: card.top + HEADER_H + COLS_H + 2.0, right: card.right - 16.0, bottom: card.bottom - FOOT_H };
        let height = self.explorer.look.height.max(1) as f32;
        let rh = crate::gfx::tsr(ROW_BASE) * height;
        let icon = if height <= 1.0 { (24.0, 18.0) } else { ((rh - 8.0) * 4.0 / 3.0, rh - 8.0) };
        let size_w = 100.0;
        let date_w = if card.right - card.left > 560.0 { 150.0 } else { 0.0 };
        let name_end = list.right - date_w - size_w - 8.0;
        Geom { card, list, rh, name: (list.left, name_end.max(list.left + 80.0)), icon }
    }

    /// The row at y on the list (None past the last).
    fn archive_row_at(&self, y: f32) -> Option<usize> {
        let g = self.archive_geom();
        let a = self.archive.as_ref()?;
        let i = ((y - g.list.top + a.scroll) / g.rh).floor();
        (i >= 0.0 && (i as usize) < a.rows.len()).then_some(i as usize)
    }

    pub(super) fn draw_archive(&mut self, g: &Gfx) {
        if !self.archive_shown() {
            return;
        }
        let card = self.archive_geom().card;
        if card.right - card.left < 240.0 || card.bottom - card.top < 160.0 {
            return;
        }
        ui::card(g, card);
        self.hits.add(Hit::ArchiveCard, card);
        // Without the keyboard, what's in it dimmed, its selection grey and its cursor hidden (Faye, 2026-11-12).
        let focused = self.archive.as_ref().is_some_and(|a| a.focused) && !self.explorer.focused && !self.text_focused();
        let keys = self.fades.get(Hit::ArchiveCard, focused);
        g.push_fade(ui::panel_opacity(keys), 1.0, 0.0, 0.0);
        self.draw_archive_content(g, keys);
        g.pop_fade();
    }

    fn draw_archive_content(&mut self, g: &Gfx, keys: f32) {
        let geo = self.archive_geom();
        let (card, list, rh) = (geo.card, geo.list, geo.rh);
        let t = theme::t();
        let Some(a) = self.archive.as_ref() else { return };
        let name = a.path.file_name().and_then(|n| n.to_str()).unwrap_or("").to_string();
        let dir = a.dir.clone();
        let nsel = a.selected.len();
        let working = a.work.is_some();
        let (can_back, can_fwd, can_up) = (!a.back.is_empty(), !a.forward.is_empty(), !a.dir.is_empty());
        let ready = matches!(a.entries, Some(Ok(_))) && !working;

        // The header: Back, Forward, Up; where in the archive (each part a step up); the actions.
        let pad = 12.0;
        let btn = 28.0;
        let by = card.top + (HEADER_H - btn) / 2.0;
        let mut x = card.left + pad;
        for (id, glyph, on) in [(Hit::ArchiveBack, 0xE72Bu16, can_back), (Hit::ArchiveForward, 0xE72A, can_fwd), (Hit::ArchiveUp, 0xE74A, can_up)] {
            let r = rect(x, by, btn, btn);
            if on {
                self.hits.add(id, r);
            }
            let st = self.state(id, on);
            let fg = ui::quiet_frame(g, r, ui::Kind::Standard, &st);
            g.text(&[glyph], &g.fonts.icons, r, fg, Align::Center);
            x += btn + 2.0;
        }
        x += 6.0;
        let all_label = if nsel > 0 { format!("Extract {nsel} item{}", if nsel == 1 { "" } else { "s" }) } else { "Extract all".to_string() };
        let to_label = "Extract to\u{2026}";
        let w_all = g.measure(&wide(&all_label), &g.fonts.body).ceil() + 28.0;
        let w_to = g.measure(&wide(to_label), &g.fonts.body).ceil() + 28.0;
        let r_all = rect(card.right - pad - w_all, by, w_all, btn);
        let r_to = rect(r_all.left - 8.0 - w_to, by, w_to, btn);
        for (id, r, label, kind) in [(Hit::ArchiveExtractTo, r_to, to_label.to_string(), ui::Kind::Standard), (Hit::ArchiveExtractAll, r_all, all_label, ui::Kind::Accent)] {
            if ready {
                self.hits.add(id, r);
            }
            let st = self.state(id, ready);
            let fg = ui::button_frame(g, r, kind, &st);
            g.text(&wide(&label), &g.fonts.body, r, fg, Align::Center);
        }
        let mut parts = vec![name.clone()];
        parts.extend(dir.split('/').filter(|s| !s.is_empty()).map(str::to_string));
        let crumb_right = r_to.left - 12.0;
        let n = parts.len();
        for (i, p) in parts.iter().enumerate() {
            if x >= crumb_right {
                break;
            }
            let last = i + 1 == n;
            let w = (g.measure(&wide(p), &g.fonts.body).ceil() + 14.0).min(crumb_right - x);
            let r = rect(x, by, w, btn);
            if !last {
                self.hits.add(Hit::ArchiveCrumb(i as u32), r);
                let st = self.state(Hit::ArchiveCrumb(i as u32), true);
                ui::quiet_frame(g, r, ui::Kind::Standard, &st);
            }
            g.text(&wide(p), &g.fonts.body, rect(x + 7.0, by, w - 14.0, btn), rgb(if last { t.text } else { t.label }), Align::Left);
            x += w;
            if !last {
                g.text(&[0xE76C], &g.fonts.small_icons, rect(x, by, 14.0, btn), rgb(t.placeholder), Align::Center);
                x += 14.0;
            }
        }

        // Column titles, the explorer's (its font and placeholder ink).
        let hy = card.top + HEADER_H;
        g.fill(rect(card.left + 1.0, hy - 0.5, card.right - card.left - 2.0, g.px()), rgb(t.divider));
        let date_w = if card.right - card.left > 560.0 { 150.0 } else { 0.0 };
        let size_r = list.right - date_w - 4.0;
        let size_w = 100.0;
        let name_x = list.left + 6.0 + geo.icon.0 + 8.0;
        let col_y = rect(0.0, hy + 2.0, 0.0, COLS_H);
        g.text(&wide("Name"), &g.fonts.body, D2D_RECT_F { left: list.left + 8.0, right: size_r - size_w, ..col_y }, rgb(t.placeholder), Align::Left);
        g.text(&wide("Size"), &g.fonts.body, D2D_RECT_F { left: size_r - size_w, right: size_r - 10.0, ..col_y }, rgb(t.placeholder), Align::Right);
        if date_w > 0.0 {
            g.text(&wide("Date modified"), &g.fonts.body, D2D_RECT_F { left: size_r + 8.0, right: list.right, ..col_y }, rgb(t.placeholder), Align::Left);
        }

        // The rows, as the explorer draws them.
        let Some(a) = self.archive.as_ref() else { return };
        let total = a.rows.len();
        let view_h = (list.bottom - list.top).max(1.0);
        let max_scroll = (total as f32 * rh - view_h).max(0.0);
        let scroll = a.scroll.clamp(0.0, max_scroll);
        self.hits.add(Hit::ArchiveList, D2D_RECT_F { left: card.left, right: card.right, ..list });
        g.push_clip(D2D_RECT_F { left: card.left, right: card.right, ..list });
        let first = (scroll / rh).floor() as usize;
        let last = (((scroll + view_h) / rh).ceil() as usize + 1).min(total);
        let hover_tint = theme::mix(t.sidebar, t.accent, 0.2);
        let dragging = self.explorer.row_drag.as_ref().is_some_and(|d| d.active);
        let drop_dir = self.explorer.row_drag.as_ref().filter(|d| d.active).and_then(|d| d.into.clone());
        let many = a.selected.len() > 1;
        for vi in first..last {
            let row_r = rect(list.left, g.snap(list.top + vi as f32 * rh - scroll), list.right - list.left, rh);
            let visible = D2D_RECT_F { top: row_r.top.max(list.top), bottom: row_r.bottom.min(list.bottom), ..row_r };
            if visible.bottom > visible.top {
                self.hits.add(Hit::ArchiveRow(vi as u32), visible);
            }
            let a = self.archive.as_ref().unwrap();
            let Some(e) = a.entry(vi).cloned() else { break };
            let selected = a.selected.contains(&vi);
            let cursor = a.anchor == Some(vi);
            let is_target = drop_dir.as_deref().is_some_and(|d| e.dir && d == e.path);
            let hovered = self.hover == Some(Hit::ArchiveRow(vi as u32)) && !dragging;
            let hover = self.fades.hover(Hit::ArchiveRow(vi as u32), hovered);
            let lit = hovered && self.pressed == Some(Hit::ArchiveRow(vi as u32)) && self.archive.as_ref().is_some_and(|a| a.rubber.is_none());
            let solid = self.fades.get(Hit::ArchiveSolid(vi as u32), selected && many) * keys;
            let fr = D2D_RECT_F { bottom: row_r.bottom - g.px(), ..row_r };
            if vi % 2 == 1 {
                g.fill_round(row_r, 4.0, rgb(t.stripe));
            }
            const SOLID: f32 = 0.6;
            if lit {
                g.fill_round(row_r, 4.0, gfx::rgba(t.accent, SOLID));
            } else if solid > 0.01 {
                if solid < 0.99 && selected {
                    g.fill_round(fr, 4.0, ui::selected_fill(keys));
                }
                g.fill_round(fr, 4.0, gfx::rgba(t.accent, solid * SOLID));
            } else if selected || is_target {
                g.fill_round(row_r, 4.0, if is_target { rgb(t.selected) } else { ui::selected_fill(keys) });
            } else if hover > 0.0 {
                g.fill_round(row_r, 4.0, windows::Win32::Graphics::Direct2D::Common::D2D1_COLOR_F { a: hover, ..hover_tint });
            }
            let lit = lit || solid > 0.5;
            if !lit {
                if is_target {
                    ui::stroke(g, row_r, 4.0, rgb(t.accent));
                } else if cursor && selected && keys > hover {
                    ui::stroke(g, row_r, 4.0, gfx::rgba(t.accent, keys));
                } else if hover > 0.0 {
                    ui::stroke(g, row_r, 4.0, gfx::rgba(t.accent, hover));
                }
            }
            let ink = |c: u32| if lit { rgb(t.on_accent) } else { rgb(c) };
            let fade: f32 = if e.encrypted { 0.45 } else { 1.0 };
            // The picture: its thumbnail, else Looker's (or Windows') icon for the type, a folder's.
            let ib = rect(list.left + 6.0, row_r.top + (rh - geo.icon.1) / 2.0, geo.icon.0, geo.icon.1);
            let thumb = self.archive.as_ref().unwrap().thumbs.get(&e.path).cloned();
            let dip = if geo.icon.1 >= 32.0 { geo.icon.1.min(48.0).floor() } else { 16.0 };
            if let Some(b) = thumb {
                let s = unsafe { b.GetSize() };
                let k = (geo.icon.0 / s.width.max(1.0)).min(geo.icon.1 / s.height.max(1.0));
                let (w, h) = (s.width * k, s.height * k);
                g.fill_round(ib, 2.0, rgb(t.field));
                g.draw_bitmap(&b, rect(ib.left + (geo.icon.0 - w) / 2.0, ib.top + (geo.icon.1 - h) / 2.0, w, h), 1.0);
            } else if e.dir {
                super::explorer_view::shell_icon(g, &crate::shell_icons::Kind::Folder, ib, 1.0, lit.then_some(t.on_accent));
            } else {
                let ext = Path::new(e.name()).extension().and_then(|x| x.to_str()).map(str::to_ascii_lowercase).unwrap_or_default();
                let mine = crate::file_icons::icon(&ext, (dip * g.dpi / 96.0).round() as u32);
                if let Some(icon) = mine {
                    g.draw_icon(&icon, ib, dip, fade.max(0.8), lit.then_some(t.on_accent));
                } else if !super::explorer_view::shell_icon_at(g, &crate::shell_icons::Kind::Extension(ext), ib, dip, fade.max(0.8), lit.then_some(t.on_accent)) {
                    g.text(&[0xE7C3], &g.fonts.body_icons, ib, ink(t.icon), Align::Center);
                }
            }
            let row_y = D2D_RECT_F { top: row_r.top, bottom: row_r.bottom, left: 0.0, right: 0.0 };
            let label = if e.encrypted { format!("{}  \u{1f512}", e.name()) } else { e.name().to_string() };
            g.text(&wide(&label), &g.fonts.body, D2D_RECT_F { left: name_x, right: size_r - size_w - 8.0, ..row_y }, ink(t.text), Align::Left);
            if !e.dir {
                g.text_tabular(&wide(&format_bytes(e.size as u64)), &g.fonts.body, D2D_RECT_F { left: size_r - size_w, right: size_r - 10.0, ..row_y }, ink(t.placeholder), Align::Right);
            }
            if date_w > 0.0 {
                g.text_tabular(&wide(&date_text(e.mtime)), &g.fonts.body, D2D_RECT_F { left: size_r + 8.0, right: list.right, ..row_y }, ink(t.placeholder), Align::Left);
            }
        }
        // The selection rectangle, inside the list.
        let a = self.archive.as_ref().unwrap();
        if let Some(m) = a.rubber.as_ref().filter(|m| m.active) {
            let (x0, y0) = (m.start.0, m.start.1 - scroll + list.top);
            let (x1, y1) = m.pointer;
            let r = D2D_RECT_F { left: x0.min(x1).max(list.left), top: y0.min(y1).max(list.top), right: x0.max(x1).min(list.right), bottom: y0.max(y1).min(list.bottom) };
            if r.right > r.left && r.bottom > r.top {
                g.fill(r, gfx::rgba(t.accent, 0.14));
                g.outline_round(r, 0.0, rgb(t.accent), g.px());
            }
        }
        // A drop into this folder (the list itself, not a folder row): the list outlined.
        if drop_dir.as_deref().is_some_and(|d| d == a.dir) {
            ui::stroke(g, D2D_RECT_F { top: list.top + 1.0, bottom: list.bottom - 1.0, ..list }, 6.0, rgb(t.accent));
        }
        let note = match &a.entries {
            None => Some("Reading\u{2026}".to_string()),
            Some(Err(e)) => Some(format!("Can't read this archive: {e}")),
            Some(Ok(_)) if a.rows.is_empty() => Some(if a.dir.is_empty() { "This archive is empty" } else { "This folder is empty" }.to_string()),
            _ => None,
        };
        g.pop_clip();
        if let Some(n) = note {
            g.text(&wide(&n), &g.fonts.body, rect(list.left, list.top + 12.0, list.right - list.left, 24.0), rgb(t.placeholder), Align::Center);
        }
        // The scrollbar, when the list overflows (the explorer's thumb).
        if max_scroll > 0.0 {
            let thumb_h = (view_h * view_h / (total as f32 * rh)).max(28.0);
            let ty = list.top + (view_h - thumb_h) * scroll / max_scroll;
            ui::scroll_thumb(g, rect(card.right - 4.0 - ui::SCROLL_W, ty, ui::SCROLL_W, thumb_h), false);
        }

        // The footer: what's here, or the work's progress.
        let foot = D2D_RECT_F { top: card.bottom - FOOT_H, ..card };
        let text_r = D2D_RECT_F { left: card.left + 16.0, right: card.right - 16.0, ..foot };
        if let Some(w) = &a.work {
            let (written, file) = w.state.lock().map(|s| (s.written, s.name.clone())).unwrap_or_default();
            let k = if w.total > 0 { (written as f64 / w.total as f64).clamp(0.0, 1.0) as f32 } else { 0.0 };
            let what = match w.kind {
                WorkKind::Extract(_) => "Extracting",
                WorkKind::Add(_) => "Adding",
            };
            let cancel_w = 76.0;
            let r_cancel = rect(card.right - 12.0 - cancel_w, foot.top + 8.0, cancel_w, FOOT_H - 16.0);
            let bar = rect(text_r.left, foot.bottom - 9.0, r_cancel.left - 12.0 - text_r.left, 2.0);
            g.fill_round(bar, 1.0, rgb(t.track));
            g.fill_round(D2D_RECT_F { right: bar.left + (bar.right - bar.left) * k, ..bar }, 1.0, rgb(t.accent));
            g.text(&wide(&format!("{what} {file}\u{2026} {:.0} %", k * 100.0)), &g.fonts.body, D2D_RECT_F { right: r_cancel.left - 12.0, bottom: foot.bottom - 10.0, ..text_r }, rgb(t.label), Align::Left);
            self.hits.add(Hit::ArchiveCancel, r_cancel);
            let st = self.state(Hit::ArchiveCancel, true);
            let fg = ui::quiet_frame(g, r_cancel, ui::Kind::Standard, &st);
            g.text(&wide("Cancel"), &g.fonts.body, r_cancel, fg, Align::Center);
        } else if let Some(all) = a.list() {
            let files = all.iter().filter(|e| !e.dir).count();
            let bytes: i64 = all.iter().map(|e| e.size).sum();
            let enc = all.iter().filter(|e| e.encrypted).count();
            let mut s = format!("{files} file{} \u{b7} {} unpacked", if files == 1 { "" } else { "s" }, format_bytes(bytes as u64));
            if enc > 0 {
                s.push_str(&format!(" \u{b7} {enc} encrypted (can't be opened)"));
            }
            if nsel > 0 {
                s = format!("{nsel} selected \u{b7} {s}");
            }
            g.text(&wide(&s), &g.fonts.body, text_r, rgb(t.label), Align::Left);
        }
    }

    // --- Pointer -----------------------------------------------------------------------------------------------

    /// A press on the list at (x, y): on a row's name, it selects (Ctrl adds or takes away, Shift reaches from the
    /// cursor) and may become a drag; beside the names or under the rows, the selection rectangle.
    pub(super) fn archive_press(&mut self, x: f32, y: f32) {
        let (ctrl, shift) = unsafe { (GetKeyState(VK_CONTROL.0 as i32) < 0, GetKeyState(VK_SHIFT.0 as i32) < 0) };
        let geo = self.archive_geom();
        let row = self.archive_row_at(y);
        let on_name = x >= geo.name.0 && x <= geo.name.1;
        // A click inside takes the keys from the explorer.
        self.explorer.focused = false;
        let Some(a) = self.archive.as_mut() else { return };
        a.focused = true;
        let scroll = a.scroll;
        match row {
            Some(r) if on_name => {
                if shift {
                    let from = a.anchor.unwrap_or(r);
                    if !ctrl {
                        a.selected.clear();
                    }
                    a.selected.extend(from.min(r)..=from.max(r));
                } else if ctrl {
                    if !a.selected.remove(&r) {
                        a.selected.insert(r);
                    }
                    a.anchor = Some(r);
                } else if a.selected.contains(&r) {
                    // Maybe a drag of all of them: a plain click picks it alone on release.
                    a.pending = Some(r);
                    a.anchor = Some(r);
                } else {
                    a.selected.clear();
                    a.selected.insert(r);
                    a.anchor = Some(r);
                }
                if a.selected.contains(&r) {
                    self.archive_drag_start(x, y);
                }
            }
            _ => {
                let before = if ctrl { a.selected.clone() } else { BTreeSet::new() };
                a.rubber = Some(Rubber { start: (x, y - geo.list.top + scroll), pointer: (x, y), active: false, add: ctrl, before });
            }
        }
        self.invalidate();
    }

    /// The pointer moved with the button down: the rectangle selects what it crosses (scrolling past the list's
    /// top or bottom).
    pub(super) fn archive_pointer(&mut self, x: f32, y: f32) {
        let geo = self.archive_geom();
        let Some(a) = self.archive.as_mut() else { return };
        let Some(m) = a.rubber.as_mut() else { return };
        m.pointer = (x, y);
        if !m.active {
            let py = y - geo.list.top + a.scroll;
            if (x - m.start.0).abs() + (py - m.start.1).abs() <= DRAG_START {
                return;
            }
            m.active = true;
        }
        let out = if y < geo.list.top { y - geo.list.top } else if y > geo.list.bottom { y - geo.list.bottom } else { 0.0 };
        let max = (a.rows.len() as f32 * geo.rh - (geo.list.bottom - geo.list.top)).max(0.0);
        if out != 0.0 {
            a.scroll = (a.scroll + out * 0.5).clamp(0.0, max);
        }
        let m = a.rubber.as_ref().unwrap();
        let py = y.clamp(geo.list.top, geo.list.bottom) - geo.list.top + a.scroll;
        let (top, bottom) = (m.start.1.min(py), m.start.1.max(py));
        let n = a.rows.len();
        let mut picked = if m.add { m.before.clone() } else { BTreeSet::new() };
        if n > 0 && bottom >= 0.0 {
            let first = (top / geo.rh).floor().max(0.0) as usize;
            let last = ((bottom / geo.rh).floor().max(0.0) as usize).min(n - 1);
            if first < n {
                picked.extend(first..=last);
                a.anchor = Some(if py >= m.start.1 { last } else { first });
            }
        }
        a.selected = picked;
        self.invalidate();
    }

    /// The button came up: a rectangle ends; a press on a selected row that didn't drag picks it alone; a click
    /// on empty space (no rectangle) clears the selection.
    pub(super) fn archive_release(&mut self, dragged: bool) {
        let Some(a) = self.archive.as_mut() else { return };
        if let Some(m) = a.rubber.take() {
            if !m.active && !m.add {
                a.selected.clear();
                a.anchor = None;
            }
        }
        if let Some(r) = a.pending.take().filter(|_| !dragged) {
            a.selected.clear();
            a.selected.insert(r);
        }
        self.invalidate();
    }

    /// A double-click: a folder goes in. A file does nothing (Faye, 2026-11-12).
    pub(super) fn archive_double(&mut self, y: f32) {
        let Some(r) = self.archive_row_at(y) else { return };
        let Some(a) = self.archive.as_mut() else { return };
        if let Some(e) = a.entry(r).filter(|e| e.dir).cloned() {
            a.go(e.path);
            self.invalidate();
        }
    }

    /// Up a folder (Up, Backspace).
    pub(super) fn archive_up(&mut self) {
        let Some(a) = self.archive.as_mut() else { return };
        if a.dir.is_empty() {
            return;
        }
        let up = a.dir.rsplit_once('/').map_or(String::new(), |(p, _)| p.to_string());
        a.go(up);
        self.invalidate();
    }

    /// Back (and the mouse's Back button) / Forward: the folders visited. False when there's nowhere to go, so the
    /// button falls through to the files viewed.
    pub(super) fn archive_history(&mut self, back: bool) -> bool {
        let Some(a) = self.archive.as_mut() else { return false };
        let to = if back { a.back.pop() } else { a.forward.pop() };
        let Some(to) = to else { return false };
        let cur = std::mem::take(&mut a.dir);
        if back {
            a.forward.push(cur);
        } else {
            a.back.push(cur);
        }
        a.show_dir(to);
        self.invalidate();
        true
    }

    /// The header's path: part `i` (0, the archive's top).
    pub(super) fn archive_crumb(&mut self, i: usize) {
        let Some(a) = self.archive.as_mut() else { return };
        let to: Vec<&str> = a.dir.split('/').filter(|s| !s.is_empty()).take(i).collect();
        let to = to.join("/");
        a.go(to);
        self.invalidate();
    }

    // --- Dragging ----------------------------------------------------------------------------------------------

    /// What's selected, as the explorer's row drag carries it: stand-in paths under the archive (for its card's
    /// name and count) and the entries themselves.
    fn archive_drag_start(&mut self, x: f32, y: f32) {
        let Some(a) = self.archive.as_ref() else { return };
        let entries: Vec<String> = a.selected.iter().filter_map(|&r| a.entry(r)).map(|e| e.path.clone()).collect();
        if entries.is_empty() {
            return;
        }
        let paths = entries.iter().map(|p| a.path.join(p.replace('/', "\\"))).collect();
        let src = super::explorer::ArchiveSource { archive: a.path.clone(), entries, strip: a.dir.clone() };
        self.explorer.row_drag = Some(super::explorer::RowDrag {
            start: (x, y),
            paths,
            active: false,
            target: None,
            target_rect: None,
            opened: std::time::Instant::now(),
            archive: Some(src),
            into: None,
        });
    }

    /// While the explorer's rows are dragged over the card: the folder of the archive a drop would add them to (a
    /// folder row's, else the one showing) and what stands for it. None over anything else, for archive rows
    /// themselves, or when the archive can't take files (RAR, CAB, ISO, encrypted...).
    /// The rect is None for the folder showing (the list outlines itself; a filled drop box over the whole list was a
    /// wall of red).
    pub(super) fn archive_drop_at(&mut self, x: f32, y: f32) -> Option<(String, Option<D2D_RECT_F>)> {
        if !self.archive_shown() || self.explorer.row_drag.as_ref().is_some_and(|d| d.archive.is_some()) {
            return None;
        }
        let hit = self.hit(x, y)?;
        if !matches!(hit, Hit::ArchiveCard | Hit::ArchiveList | Hit::ArchiveRow(_)) {
            return None;
        }
        let geo = self.archive_geom();
        let a = self.archive.as_mut()?;
        a.list()?;
        if a.writable.is_none() {
            a.writable = Some(arc::can_add(&a.path));
        }
        if a.writable.as_ref().is_some_and(|w| w.is_err()) {
            return None;
        }
        if let Hit::ArchiveRow(i) = hit {
            if let Some(e) = a.entry(i as usize).filter(|e| e.dir) {
                return Some((e.path.clone(), Some(self.hits.rect_of(hit)?)));
            }
        }
        let _ = geo;
        Some((a.dir.clone(), None))
    }

    /// Why files can't be dropped in, when that's known (for the drag card).
    pub(super) fn archive_drop_refused(&self) -> Option<String> {
        self.archive.as_ref()?.writable.as_ref()?.as_ref().err().cloned()
    }

    /// Archive rows dropped onto a folder: extracted there.
    pub(super) fn archive_drop_out(&mut self, src: super::explorer::ArchiveSource, dest: PathBuf) {
        let chosen = src.entries.clone();
        let pick = move |en: &Entry| chosen.iter().any(|c| en.path == *c || en.path.starts_with(&format!("{c}/")));
        let total = self.archive.as_ref().and_then(|a| a.list()).map_or(0, |l| l.iter().filter(|e| !e.dir && pick(e)).map(|e| e.size.max(0) as u64).sum());
        self.archive_run(Box::new(pick), src.strip, dest, total);
    }

    /// Files dropped onto the card: added to the archive, in folder `dir`.
    pub(super) fn archive_drop_in(&mut self, paths: Vec<PathBuf>, dir: String) {
        let Some(a) = self.archive.as_mut() else { return };
        if a.work.is_some() || paths.is_empty() {
            return;
        }
        let total = paths.iter().map(|p| dir_size(p)).sum();
        let state = Arc::new(Mutex::new(WorkState::default()));
        let cancel = Arc::new(AtomicBool::new(false));
        let (s, c, path, d) = (state.clone(), cancel.clone(), a.path.clone(), dir.clone());
        // The thumbnail thread may still be reading it: stop that first.
        a.thumb_gen.fetch_add(1, Ordering::Relaxed);
        std::thread::Builder::new()
            .name("archive-add".into())
            .spawn(move || {
                let r = arc::add(&path, &paths, &d, &mut |written, name| {
                    if let Ok(mut st) = s.lock() {
                        st.written = written;
                        st.name = name.to_string();
                    }
                    !c.load(Ordering::Relaxed)
                });
                if let Ok(mut st) = s.lock() {
                    st.result = Some(r.map(|n| (n, 0, 0)));
                }
            })
            .ok();
        a.work = Some(Work { state, cancel, total, kind: WorkKind::Add(dir) });
        self.invalidate();
    }

    /// Archive rows dragged out of the window: extracted to a temporary folder (they're small, or the wait is the
    /// price), then handed to Windows' drag as those files.
    pub(super) fn archive_drag_files(&mut self, src: &super::explorer::ArchiveSource) -> Vec<PathBuf> {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        src.archive.hash(&mut h);
        src.entries.hash(&mut h);
        let dest = std::env::temp_dir().join("looker-archive-drag").join(format!("{:016x}", h.finish()));
        let _ = std::fs::remove_dir_all(&dest);
        let chosen = src.entries.clone();
        let pick = |en: &Entry| chosen.iter().any(|c| en.path == *c || en.path.starts_with(&format!("{c}/")));
        if arc::extract(&src.archive, &dest, &src.strip, &pick, &mut |_, _| true).is_err() {
            return Vec::new();
        }
        src.entries.iter().filter_map(|e| arc::safe_target(&dest, e, &src.strip)).filter(|p| p.exists()).collect()
    }

    /// What Extract takes: the rows selected (folders with all inside), from the folder showing; else all of it.
    fn archive_pick(&self) -> Option<(Box<dyn Fn(&Entry) -> bool + Send>, String, u64)> {
        let a = self.archive.as_ref()?;
        let list = a.list()?;
        if a.selected.is_empty() {
            let total = list.iter().filter(|e| !e.dir).map(|e| e.size.max(0) as u64).sum();
            return Some((Box::new(|_: &Entry| true), String::new(), total));
        }
        let chosen: Vec<String> = a.selected.iter().filter_map(|&r| a.entry(r)).map(|e| e.path.clone()).collect();
        let picks = |en: &Entry, chosen: &[String]| chosen.iter().any(|c| en.path == *c || en.path.starts_with(&format!("{c}/")));
        let total = list.iter().filter(|e| !e.dir && picks(e, &chosen)).map(|e| e.size.max(0) as u64).sum();
        let strip = a.dir.clone();
        Some((Box::new(move |en: &Entry| picks(en, &chosen)), strip, total))
    }

    /// Extract all (or the selection): beside the archive, into a new folder named after it.
    pub(super) fn archive_extract_beside(&mut self) {
        let Some(a) = self.archive.as_ref() else { return };
        let Some(parent) = a.path.parent() else { return };
        let dest = unique_folder(parent, &base_name(&a.path));
        let Some((pick, strip, total)) = self.archive_pick() else { return };
        self.archive_run(pick, strip, dest, total);
    }

    /// Extract to…: into a folder picked.
    pub(super) fn archive_extract_to(&mut self) {
        let Some(start) = self.archive.as_ref().and_then(|a| a.path.parent().map(Path::to_path_buf)) else { return };
        let Some(dest) = (unsafe { self.pick_folder(&start) }) else { return };
        let Some((pick, strip, total)) = self.archive_pick() else { return };
        self.archive_run(pick, strip, dest, total);
    }

    /// A folder picker opening at `start`.
    pub(super) unsafe fn pick_folder(&self, start: &Path) -> Option<PathBuf> {
        unsafe {
            let dlg = CoCreateInstance::<_, IFileOpenDialog>(&FileOpenDialog, None, CLSCTX_INPROC_SERVER).ok()?;
            if let Ok(o) = dlg.GetOptions() {
                let _ = dlg.SetOptions(o | FOS_PICKFOLDERS);
            }
            if let Ok(item) = SHCreateItemFromParsingName::<_, _, IShellItem>(&HSTRING::from(start.as_os_str()), None) {
                let _ = dlg.SetFolder(&item);
            }
            dlg.Show(Some(self.hwnd)).ok()?;
            let item = dlg.GetResult().ok()?;
            let name = item.GetDisplayName(SIGDN_FILESYSPATH).ok()?;
            let path = PathBuf::from(name.to_string().unwrap_or_default());
            CoTaskMemFree(Some(name.0 as _));
            Some(path)
        }
    }

    /// Runs an extraction on a thread of its own: the footer shows it, Cancel stops it.
    fn archive_run(&mut self, pick: Box<dyn Fn(&Entry) -> bool + Send>, strip: String, dest: PathBuf, total: u64) {
        let Some(a) = self.archive.as_mut() else { return };
        if a.work.is_some() {
            return;
        }
        let state = Arc::new(Mutex::new(WorkState::default()));
        let cancel = Arc::new(AtomicBool::new(false));
        let (s, c, path, d) = (state.clone(), cancel.clone(), a.path.clone(), dest.clone());
        std::thread::Builder::new()
            .name("archive-extract".into())
            .spawn(move || {
                let r = arc::extract(&path, &d, &strip, &*pick, &mut |written, name| {
                    if let Ok(mut st) = s.lock() {
                        st.written = written;
                        if st.name != name {
                            st.name = name.to_string();
                        }
                    }
                    !c.load(Ordering::Relaxed)
                });
                let r = if c.load(Ordering::Relaxed) { Err("cancelled".to_string()) } else { r };
                if let Ok(mut st) = s.lock() {
                    st.result = Some(r.map(|x| (x.files, x.skipped_encrypted, x.failed)));
                }
            })
            .ok();
        a.work = Some(Work { state, cancel, total, kind: WorkKind::Extract(dest) });
        self.invalidate();
    }

    // --- The right-click menu --------------------------------------------------------------------------------

    /// Right-click: on a row, its menu (the selection's, when it's in it, else it alone); elsewhere on the card,
    /// the whole archive's.
    pub(super) fn archive_menu(&mut self, x: f32, y: f32, hit: Hit) {
        self.explorer.focused = false;
        let row = if matches!(hit, Hit::ArchiveRow(_)) { self.archive_row_at(y) } else { None };
        let Some(a) = self.archive.as_mut() else { return };
        if a.list().is_none() {
            return;
        }
        a.focused = true;
        a.rubber = None;
        a.pending = None;
        match row {
            Some(r) if !a.selected.contains(&r) => {
                a.selected = [r].into();
                a.anchor = Some(r);
            }
            Some(r) => a.anchor = Some(r),
            None => {
                a.selected.clear();
                a.anchor = None;
            }
        }
        let n = a.selected.len();
        let folder = (n == 1).then(|| a.selected.first().and_then(|&r| a.entry(r))).flatten().is_some_and(|e| e.dir);
        let busy = a.work.is_some();
        let here = base_name(&a.path);
        let all = n == 0;
        let mut e = Vec::new();
        if folder {
            e.push(menus::item(menus::Action::ArchiveOpen, 0xE8B7, "Open", Some("Enter"), true));
            e.push(ui::Entry::Separator);
        }
        let to_here = if all { format!("Extract all to \u{201c}{here}\u{201d}") } else { format!("Extract to \u{201c}{here}\u{201d}") };
        e.push(menus::item(menus::Action::ArchiveExtractHere, 0xE8B7, &to_here, None, !busy));
        e.push(menus::item(menus::Action::ArchiveExtractTo, 0xE8DE, if all { "Extract all to\u{2026}" } else { "Extract to\u{2026}" }, None, !busy));
        e.push(ui::Entry::Separator);
        let copy = if n > 1 { format!("Copy {n} items") } else { "Copy".to_string() };
        e.push(menus::item(menus::Action::ArchiveCopy, 0xE8C8, &copy, Some("Ctrl+C"), !all));
        e.push(menus::item(menus::Action::ArchiveSelectAll, 0xE8B3, "Select all", Some("Ctrl+A"), true));
        let Some(g) = &self.gfx else { return };
        let (w, h) = self.size_dip();
        self.menu = Some((menus::MenuKind::Archive, ui::Menu::open(g, e, x, y, rect(0.0, 0.0, w, h))));
        self.hide_tooltip();
        self.invalidate();
    }

    pub(super) fn archive_action(&mut self, action: menus::Action) {
        match action {
            menus::Action::ArchiveOpen => {
                if let Some(a) = self.archive.as_mut() {
                    if let Some(e) = a.selected.first().and_then(|&r| a.entry(r)).filter(|e| e.dir).cloned() {
                        a.go(e.path);
                    }
                }
            }
            menus::Action::ArchiveExtractHere => self.archive_extract_beside(),
            menus::Action::ArchiveExtractTo => self.archive_extract_to(),
            menus::Action::ArchiveCopy => self.archive_copy(),
            menus::Action::ArchiveSelectAll => {
                if let Some(a) = self.archive.as_mut() {
                    a.selected = (0..a.rows.len()).collect();
                }
            }
            _ => {}
        }
        self.invalidate();
    }

    /// Copy (Ctrl+C): the rows selected, extracted to a temporary folder, on the clipboard as files (Explorer's
    /// Paste takes them, and so does the explorer's).
    fn archive_copy(&mut self) {
        let Some(a) = self.archive.as_ref() else { return };
        let entries: Vec<String> = a.selected.iter().filter_map(|&r| a.entry(r)).map(|e| e.path.clone()).collect();
        if entries.is_empty() {
            return;
        }
        let src = super::explorer::ArchiveSource { archive: a.path.clone(), entries, strip: a.dir.clone() };
        let paths = self.archive_drag_files(&src);
        if paths.is_empty() {
            self.show_toast("Couldn't extract them to copy", false);
            return;
        }
        match crate::clipboard::set_files(self.hwnd, &paths, false) {
            Some(_) => self.show_toast(if paths.len() == 1 { "Copied 1 item".to_string() } else { format!("Copied {} items", paths.len()) }, false),
            None => self.show_toast("Couldn't use the clipboard", false),
        }
    }

    pub(super) fn archive_cancel(&mut self) {
        if let Some(w) = self.archive.as_ref().and_then(|a| a.work.as_ref()) {
            w.cancel.store(true, Ordering::Relaxed);
        }
    }

    /// The wheel over the card: the explorer's step (52 DIPs a notch).
    pub(super) fn archive_wheel(&mut self, delta: f32) {
        let geo = self.archive_geom();
        let Some(a) = self.archive.as_mut() else { return };
        let max = (a.rows.len() as f32 * geo.rh - (geo.list.bottom - geo.list.top)).max(0.0);
        a.scroll = (a.scroll - delta / 120.0 * 52.0).clamp(0.0, max);
        self.invalidate();
    }

    /// Keys while an archive shows: ↑/↓ move (Shift extends), Enter goes into a folder, Backspace goes up,
    /// Alt+←/→ back and forward, Ctrl+A selects all.
    pub(super) fn archive_key(&mut self, vk: VIRTUAL_KEY, ctrl: bool, shift: bool) -> bool {
        if !self.archive_shown() || self.page_open() || self.menu.is_some() || self.explorer.focused || !self.archive.as_ref().is_some_and(|a| a.focused) {
            return false;
        }
        let geo = self.archive_geom();
        let Some(a) = self.archive.as_mut() else { return false };
        let n = a.rows.len();
        match vk {
            VK_A if ctrl => {
                a.selected = (0..n).collect();
            }
            VK_UP | VK_DOWN if n > 0 => {
                let cur = a.anchor.unwrap_or(if vk == VK_DOWN { usize::MAX } else { n });
                let next = if vk == VK_DOWN { cur.wrapping_add(1).min(n - 1) } else { cur.saturating_sub(1) };
                if !shift {
                    a.selected.clear();
                }
                a.selected.insert(next);
                a.anchor = Some(next);
                let y = next as f32 * geo.rh;
                let h = geo.list.bottom - geo.list.top;
                if y < a.scroll {
                    a.scroll = y;
                } else if y + geo.rh > a.scroll + h {
                    a.scroll = y + geo.rh - h;
                }
            }
            VK_RETURN => {
                if let Some(e) = a.anchor.filter(|r| a.selected.contains(r)).and_then(|r| a.entry(r)).filter(|e| e.dir).cloned() {
                    a.go(e.path);
                }
            }
            VK_BACK => {
                self.archive_up();
                return true;
            }
            VK_C if ctrl => {
                self.archive_copy();
                return true;
            }
            _ => return false,
        }
        self.invalidate();
        true
    }
}

/// A file's size, or everything in a folder.
fn dir_size(p: &Path) -> u64 {
    if p.is_dir() {
        std::fs::read_dir(p).map(|rd| rd.flatten().map(|e| dir_size(&e.path())).sum()).unwrap_or(0)
    } else {
        std::fs::metadata(p).map(|m| m.len()).unwrap_or(0)
    }
}
