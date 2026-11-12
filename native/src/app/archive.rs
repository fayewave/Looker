//! An archive on screen (zip, 7z, RAR, tar..., imaging/archive.rs): a card over the photo area listing what's inside,
//! one folder of it at a time (the path in the header, `..` to go up), with thumbnails for its pictures. A file opens
//! in Looker (its folder of the archive is unpacked into a cache folder first, so ←/→ go through its neighbours);
//! Extract all unpacks beside the archive into a folder named after it, Extract to… into a folder picked; with
//! rows selected, both take only those.

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use super::*;
use crate::imaging::archive::{self as arc, Entry, Extracted};

const HEADER_H: f32 = 52.0;
const COLS_H: f32 = 28.0;
const ROW_H: f32 = 34.0;
const FOOT_H: f32 = 34.0;
const THUMB: f32 = 26.0;

/// An extraction under way: for the footer, and what to do once it's done.
struct Work {
    state: Arc<Mutex<WorkState>>,
    cancel: Arc<AtomicBool>,
    total: u64,
    /// Open this file when it's done (a row opened), else say where it went.
    open: Option<PathBuf>,
    dest: PathBuf,
}

#[derive(Default)]
struct WorkState {
    written: u64,
    name: String,
    result: Option<Result<Extracted, String>>,
}

type Thumbs = Arc<Mutex<Vec<(String, u32, u32, Vec<u8>)>>>;

pub(super) struct ArchiveView {
    pub path: PathBuf,
    listing: Arc<Mutex<Option<Result<Vec<Entry>, String>>>>,
    entries: Option<Result<Vec<Entry>, String>>,
    /// The folder of the archive showing ("" its top), and its rows: indexes into `entries`, folders first.
    dir: String,
    rows: Vec<usize>,
    selected: BTreeSet<usize>,
    anchor: Option<usize>,
    scroll: f32,
    thumbs: HashMap<String, ID2D1Bitmap1>,
    thumb_px: Thumbs,
    /// Bumped when the folder changes: the thumbnail thread for an older one stops.
    thumb_gen: Arc<AtomicU64>,
    work: Option<Work>,
}

impl ArchiveView {
    pub(super) fn device_lost(&mut self) {
        self.thumbs.clear();
        self.thumb_gen.fetch_add(1, Ordering::Relaxed);
        self.start_thumbs();
    }

    fn entry(&self, row: usize) -> Option<&Entry> {
        let i = *self.rows.get(row)?;
        self.entries.as_ref()?.as_ref().ok()?.get(i)
    }

    /// The rows of folder `dir`: its direct children, folders first, then by name (as Explorer sorts: case
    /// ignored).
    fn show_dir(&mut self, dir: String) {
        self.dir = dir;
        self.selected.clear();
        self.anchor = None;
        self.scroll = 0.0;
        let Some(Ok(list)) = &self.entries else { return };
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

    /// Thumbnails of this folder's pictures, read in the archive's order on a thread of its own (a solid 7z or RAR
    /// can only be read through), each handed over as it's decoded.
    fn start_thumbs(&mut self) {
        let Some(Ok(list)) = &self.entries else { return };
        let want: std::collections::HashSet<String> = self.rows.iter().map(|&i| &list[i]).filter(|e| !e.dir && arc::is_picture(&e.path) && e.size <= 40 << 20).map(|e| e.path.clone()).collect();
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
                let px = (THUMB * 2.0) as u32; // up to 200 % displays
                let _ = arc::read_files(&path, 40 << 20, &|e| want.contains(&e.path), &mut |e, bytes| {
                    if generation.load(Ordering::Relaxed) != mine {
                        return false;
                    }
                    if let Ok(d) = crate::imaging::wic::decode(&f, crate::imaging::wic::Source::Memory(&bytes), px, px) {
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
fn base_name(path: &Path) -> String {
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

impl App {
    /// The archive's file count and unpacked size, once listed.
    pub(super) fn archive_facts(&self) -> Option<(usize, u64)> {
        let list = self.archive.as_ref()?.entries.as_ref()?.as_ref().ok()?;
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
            self.archive = want.map(|path| {
                let listing = Arc::new(Mutex::new(None));
                let (l, p) = (listing.clone(), path.clone());
                std::thread::Builder::new()
                    .name("archive-list".into())
                    .spawn(move || {
                        let r = arc::list(&p);
                        if let Ok(mut l) = l.lock() {
                            *l = Some(r);
                        }
                    })
                    .ok();
                ArchiveView {
                    path,
                    listing,
                    entries: None,
                    dir: String::new(),
                    rows: Vec::new(),
                    selected: BTreeSet::new(),
                    anchor: None,
                    scroll: 0.0,
                    thumbs: HashMap::new(),
                    thumb_px: Arc::new(Mutex::new(Vec::new())),
                    thumb_gen: Arc::new(AtomicU64::new(0)),
                    work: None,
                }
            });
        }
        let Some(a) = self.archive.as_mut() else { return };
        if a.entries.is_none() {
            if let Some(r) = a.listing.lock().ok().and_then(|mut l| l.take()) {
                a.entries = Some(r);
                a.show_dir(String::new());
            }
        }
        let made: Vec<_> = a.thumb_px.lock().map(|mut v| std::mem::take(&mut *v)).unwrap_or_default();
        for (p, w, h, px) in made {
            if let Ok(b) = g.bitmap(w, h, &px) {
                a.thumbs.insert(p, b);
            }
        }
        // An extraction finished.
        let finished = a.work.as_ref().and_then(|w| w.state.lock().ok().and_then(|mut s| s.result.take()));
        if let Some(result) = finished {
            let w = a.work.take().unwrap();
            match (result, w.open) {
                (Ok(_), Some(file)) => self.open(file),
                (Ok(d), None) => {
                    let name = w.dest.file_name().and_then(|n| n.to_str()).unwrap_or("").to_string();
                    let mut msg = format!("Extracted {} file{} to \u{201c}{name}\u{201d}", d.files, if d.files == 1 { "" } else { "s" });
                    if d.skipped_encrypted > 0 {
                        msg.push_str(&format!(", {} encrypted left out", d.skipped_encrypted));
                    }
                    if d.failed > 0 {
                        msg.push_str(&format!(", {} couldn't be written", d.failed));
                    }
                    self.show_toast(msg, false);
                }
                (Err(e), _) => self.show_toast(format!("Couldn't extract: {e}"), false),
            }
            self.invalidate();
        }
    }

    /// Still reading the listing or thumbnails, or extracting: draw again next vsync.
    pub(super) fn archive_animating(&self) -> bool {
        self.archive.as_ref().is_some_and(|a| a.entries.is_none() || a.work.is_some() || a.thumb_px.lock().is_ok_and(|v| !v.is_empty()) || (Arc::strong_count(&a.thumb_px) > 1))
    }

    /// The card's rect, and the list's (under the header and column titles, over the footer).
    fn archive_rects(&self) -> (D2D_RECT_F, D2D_RECT_F) {
        let a = self.image_area();
        let (ml, mr) = ((12.0 - self.left_inset()).max(0.0), (12.0 - self.right_inset()).max(0.0));
        let card = D2D_RECT_F { left: a.left + ml, top: a.top + 12.0, right: a.right - mr, bottom: a.bottom - 12.0 };
        let list = D2D_RECT_F { top: card.top + HEADER_H + COLS_H, bottom: card.bottom - FOOT_H, ..card };
        (card, list)
    }

    /// The rows above the first: `..` when inside a folder.
    fn archive_up_row(&self) -> usize {
        self.archive.as_ref().is_some_and(|a| !a.dir.is_empty()) as usize
    }

    pub(super) fn draw_archive(&mut self, g: &Gfx) {
        if !self.archive_shown() {
            return;
        }
        let (card, list) = self.archive_rects();
        if card.right - card.left < 200.0 || card.bottom - card.top < 160.0 {
            return;
        }
        let t = theme::t();
        ui::shadow(g, card, 12.0);
        g.fill_round(card, 12.0, rgb(t.sidebar));
        ui::stroke(g, card, 12.0, rgb(t.divider));
        self.hits.add(Hit::ArchiveCard, card);
        let Some(a) = self.archive.as_ref() else { return };
        let name = a.path.file_name().and_then(|n| n.to_str()).unwrap_or("").to_string();
        let dir = a.dir.clone();
        let nsel = a.selected.len();
        let working = a.work.is_some();

        // The header: where in the archive (each part a step back up), and the actions.
        let pad = 16.0;
        let btn_h = 30.0;
        let by = card.top + (HEADER_H - btn_h) / 2.0;
        let all_label = if nsel > 0 { format!("Extract {nsel} item{}", if nsel == 1 { "" } else { "s" }) } else { "Extract all".to_string() };
        let to_label = "Extract to\u{2026}";
        let w_all = g.measure(&wide(&all_label), &g.fonts.caption).ceil() + 28.0;
        let w_to = g.measure(&wide(to_label), &g.fonts.caption).ceil() + 28.0;
        let r_all = rect(card.right - pad - w_all, by, w_all, btn_h);
        let r_to = rect(r_all.left - 8.0 - w_to, by, w_to, btn_h);
        let ready = matches!(a.entries, Some(Ok(_))) && !working;
        for (id, r, label, kind) in [(Hit::ArchiveExtractTo, r_to, to_label.to_string(), ui::Kind::Standard), (Hit::ArchiveExtractAll, r_all, all_label, ui::Kind::Accent)] {
            if ready {
                self.hits.add(id, r);
            }
            let st = self.state(id, ready);
            let fg = ui::button_frame(g, r, kind, &st);
            g.text(&wide(&label), &g.fonts.caption, r, fg, Align::Center);
        }
        let mut parts = vec![name.clone()];
        parts.extend(dir.split('/').filter(|s| !s.is_empty()).map(str::to_string));
        let mut x = card.left + pad;
        let crumb_right = r_to.left - 12.0;
        let n = parts.len();
        for (i, p) in parts.iter().enumerate() {
            if x >= crumb_right {
                break;
            }
            let last = i + 1 == n;
            let font = if i == 0 { &g.fonts.title } else { &g.fonts.caption };
            let w = (g.measure(&wide(p), font).ceil() + 12.0).min(crumb_right - x);
            let r = rect(x, by, w, btn_h);
            if !last {
                self.hits.add(Hit::ArchiveCrumb(i as u32), r);
                let st = self.state(Hit::ArchiveCrumb(i as u32), true);
                ui::quiet_frame(g, r, ui::Kind::Standard, &st);
            }
            g.text(&wide(p), font, rect(x + 6.0, by, w - 12.0, btn_h), rgb(if last { t.text } else { t.label }), Align::Left);
            x += w;
            if !last {
                g.text(&wide("\u{203a}"), &g.fonts.caption, rect(x, by, 14.0, btn_h), rgb(t.label), Align::Center);
                x += 14.0;
            }
        }

        // Column titles.
        let cols_top = card.top + HEADER_H;
        g.fill(rect(card.left + 1.0, cols_top - 0.5, card.right - card.left - 2.0, 1.0), rgb(t.divider));
        let size_w = 96.0;
        let date_w = if card.right - card.left > 560.0 { 150.0 } else { 0.0 };
        let name_x = card.left + pad + THUMB + 10.0;
        let size_r = card.right - pad - date_w - 12.0;
        let col_y = rect(0.0, cols_top, 0.0, COLS_H);
        g.text(&wide("Name"), &g.fonts.caption, D2D_RECT_F { left: name_x, right: size_r - size_w, ..col_y }, rgb(t.label), Align::Left);
        g.text(&wide("Size"), &g.fonts.caption, D2D_RECT_F { left: size_r - size_w, right: size_r, ..col_y }, rgb(t.label), Align::Right);
        if date_w > 0.0 {
            g.text(&wide("Modified"), &g.fonts.caption, D2D_RECT_F { left: card.right - pad - date_w, right: card.right - pad, ..col_y }, rgb(t.label), Align::Left);
        }

        // The rows.
        let Some(a) = self.archive.as_ref() else { return };
        let up = !a.dir.is_empty() as usize;
        let total_rows = a.rows.len() + up;
        let view_h = list.bottom - list.top;
        let max_scroll = (total_rows as f32 * ROW_H - view_h).max(0.0);
        let scroll = a.scroll.clamp(0.0, max_scroll);
        g.push_clip(list);
        let first = (scroll / ROW_H) as usize;
        let last = ((scroll + view_h) / ROW_H).ceil() as usize;
        let mut draws = Vec::new();
        for i in first..last.min(total_rows) {
            let y = list.top + i as f32 * ROW_H - scroll;
            let r = D2D_RECT_F { left: card.left + 6.0, top: y + 1.0, right: card.right - 6.0, bottom: y + ROW_H - 1.0 };
            draws.push((i, r));
        }
        for (i, r) in draws {
            let (hit, entry, selected) = if i < up {
                (Hit::ArchiveUp, None, false)
            } else {
                let row = i - up;
                let a = self.archive.as_ref().unwrap();
                (Hit::ArchiveRow(row as u32), a.entry(row).cloned(), a.selected.contains(&row))
            };
            let clipped = D2D_RECT_F { top: r.top.max(list.top), bottom: r.bottom.min(list.bottom), ..r };
            if clipped.bottom > clipped.top {
                self.hits.add(hit, clipped);
            }
            let hover = self.hover == Some(hit);
            if selected {
                g.fill_round(r, 6.0, rgb(t.row_select));
            } else if hover {
                g.fill_round(r, 6.0, rgb(t.hover_fill));
            }
            let icon_r = rect(card.left + pad, r.top + (r.bottom - r.top - THUMB) / 2.0, THUMB, THUMB);
            let (name, size, date, dir) = match &entry {
                None => ("..".to_string(), String::new(), String::new(), true),
                Some(e) => (
                    e.name().to_string() + if e.encrypted { "  \u{1f512}" } else { "" },
                    if e.dir { String::new() } else { format_bytes(e.size as u64) },
                    date_text(e.mtime),
                    e.dir,
                ),
            };
            let thumb = entry.as_ref().and_then(|e| self.archive.as_ref().unwrap().thumbs.get(&e.path).cloned());
            if let Some(b) = thumb {
                let s = unsafe { b.GetSize() };
                let k = (THUMB / s.width.max(1.0)).min(THUMB / s.height.max(1.0));
                let (w, h) = (s.width * k, s.height * k);
                g.draw_bitmap(&b, rect(icon_r.left + (THUMB - w) / 2.0, icon_r.top + (THUMB - h) / 2.0, w, h), 1.0);
            } else if dir {
                super::explorer_view::shell_icon_at(g, &crate::shell_icons::Kind::Folder, icon_r, 20.0, 1.0, None);
            } else {
                let ext = entry.as_ref().and_then(|e| Path::new(e.name()).extension().and_then(|x| x.to_str()).map(str::to_ascii_lowercase)).unwrap_or_default();
                if !super::explorer_view::shell_icon_at(g, &crate::shell_icons::Kind::Extension(ext), icon_r, 20.0, 1.0, None) {
                    g.text(&[0xE7C3], &g.fonts.icons, icon_r, rgb(t.label), Align::Center);
                }
            }
            let row_y = D2D_RECT_F { top: r.top, bottom: r.bottom, left: 0.0, right: 0.0 };
            g.text(&wide(&name), &g.fonts.caption, D2D_RECT_F { left: name_x, right: size_r - size_w - 8.0, ..row_y }, rgb(t.text), Align::Left);
            g.text_tabular(&wide(&size), &g.fonts.caption, D2D_RECT_F { left: size_r - size_w, right: size_r, ..row_y }, rgb(t.label), Align::Right);
            if date_w > 0.0 {
                g.text_tabular(&wide(&date), &g.fonts.caption, D2D_RECT_F { left: card.right - pad - date_w, right: card.right - pad, ..row_y }, rgb(t.label), Align::Left);
            }
        }
        // Reading, or nothing to read.
        let a = self.archive.as_ref().unwrap();
        let note = match &a.entries {
            None => Some("Reading\u{2026}".to_string()),
            Some(Err(e)) => Some(format!("Can't read this archive: {e}")),
            Some(Ok(_)) if a.rows.is_empty() && a.dir.is_empty() => Some("This archive is empty".to_string()),
            _ => None,
        };
        g.pop_clip();
        if let Some(n) = note {
            g.text(&wide(&n), &g.fonts.caption, list, rgb(t.label), Align::Center);
        }

        // The footer: what's here, or the extraction's progress.
        let foot = D2D_RECT_F { top: card.bottom - FOOT_H, ..card };
        g.fill(rect(card.left + 1.0, foot.top, card.right - card.left - 2.0, 1.0), rgb(t.divider));
        let text_r = D2D_RECT_F { left: card.left + pad, right: card.right - pad, ..foot };
        if let Some(w) = &a.work {
            let (written, file) = w.state.lock().map(|s| (s.written, s.name.clone())).unwrap_or_default();
            let k = if w.total > 0 { (written as f64 / w.total as f64).clamp(0.0, 1.0) as f32 } else { 0.0 };
            let what = if w.open.is_some() { "Opening" } else { "Extracting" };
            let cancel_w = 70.0;
            let r_cancel = rect(card.right - pad - cancel_w, foot.top + 4.0, cancel_w, FOOT_H - 8.0);
            let bar = rect(text_r.left, foot.bottom - 6.0, r_cancel.left - 12.0 - text_r.left, 2.0);
            g.fill_round(bar, 1.0, rgb(t.track));
            g.fill_round(D2D_RECT_F { right: bar.left + (bar.right - bar.left) * k, ..bar }, 1.0, rgb(t.accent));
            g.text(&wide(&format!("{what} {file}\u{2026} {:.0} %", k * 100.0)), &g.fonts.caption, D2D_RECT_F { right: r_cancel.left - 12.0, bottom: foot.bottom - 6.0, ..text_r }, rgb(t.text), Align::Left);
            self.hits.add(Hit::ArchiveCancel, r_cancel);
            let st = self.state(Hit::ArchiveCancel, true);
            let fg = ui::quiet_frame(g, r_cancel, ui::Kind::Standard, &st);
            g.text(&wide("Cancel"), &g.fonts.caption, r_cancel, fg, Align::Center);
        } else if let Some(Ok(list)) = &a.entries {
            let files = list.iter().filter(|e| !e.dir).count();
            let bytes: i64 = list.iter().map(|e| e.size).sum();
            let enc = list.iter().filter(|e| e.encrypted).count();
            let mut s = format!("{files} file{} \u{b7} {} unpacked", if files == 1 { "" } else { "s" }, format_bytes(bytes as u64));
            if enc > 0 {
                s.push_str(&format!(" \u{b7} {enc} encrypted (can't be opened)"));
            }
            if nsel > 0 {
                s = format!("{nsel} selected \u{b7} {s}");
            }
            g.text(&wide(&s), &g.fonts.caption, text_r, rgb(t.label), Align::Left);
        }
    }

    /// A press on row `row`: selects it (Ctrl adds or takes it away, Shift reaches from the last one).
    pub(super) fn archive_press(&mut self, row: usize) {
        let (ctrl, shift) = unsafe { (GetKeyState(VK_CONTROL.0 as i32) < 0, GetKeyState(VK_SHIFT.0 as i32) < 0) };
        let Some(a) = self.archive.as_mut() else { return };
        if shift {
            let from = a.anchor.unwrap_or(row);
            if !ctrl {
                a.selected.clear();
            }
            a.selected.extend(from.min(row)..=from.max(row));
        } else if ctrl {
            if !a.selected.remove(&row) {
                a.selected.insert(row);
            }
            a.anchor = Some(row);
        } else {
            a.selected.clear();
            a.selected.insert(row);
            a.anchor = Some(row);
        }
        self.invalidate();
    }

    /// A press on the list away from the rows: nothing selected.
    pub(super) fn archive_press_empty(&mut self) {
        if let Some(a) = self.archive.as_mut() {
            a.selected.clear();
            a.anchor = None;
        }
        self.invalidate();
    }

    /// Opens row `row`: a folder goes in; a file is unpacked with its folder's other files (into the cache) and
    /// opened in Looker.
    pub(super) fn archive_open(&mut self, row: usize) {
        let Some(a) = self.archive.as_mut() else { return };
        let Some(e) = a.entry(row).cloned() else { return };
        if e.dir {
            a.show_dir(e.path);
            self.invalidate();
            return;
        }
        if e.encrypted {
            self.show_toast("That file is encrypted: Looker can't open it", false);
            return;
        }
        if a.work.is_some() {
            return;
        }
        // The cache folder: one per archive (by its path, size and time), the folder's files unpacked there once.
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        a.path.hash(&mut h);
        if let Ok(m) = std::fs::metadata(&a.path) {
            m.len().hash(&mut h);
            m.modified().ok().hash(&mut h);
        }
        let cache = std::env::temp_dir().join("looker-archive").join(format!("{:016x}", h.finish()));
        let dest = if a.dir.is_empty() { cache.clone() } else { arc::safe_target(&cache, &a.dir, "").unwrap_or(cache.clone()) };
        let Some(target) = arc::safe_target(&dest, e.name(), "") else { return };
        if target.metadata().is_ok_and(|m| m.len() as i64 == e.size) {
            self.open(target);
            return;
        }
        let prefix = if a.dir.is_empty() { String::new() } else { format!("{}/", a.dir) };
        let siblings: std::collections::HashSet<String> = a.rows.iter().filter_map(|&i| a.entries.as_ref()?.as_ref().ok()?.get(i)).filter(|s| !s.dir).map(|s| s.path.clone()).collect();
        let total = a.rows.iter().filter_map(|&i| a.entries.as_ref()?.as_ref().ok()?.get(i)).filter(|s| !s.dir).map(|s| s.size.max(0) as u64).sum();
        let strip = prefix.trim_end_matches('/').to_string();
        self.archive_run(Box::new(move |en: &Entry| siblings.contains(&en.path)), strip, dest, total, Some(target));
    }

    /// Up a folder (`..`, Backspace).
    pub(super) fn archive_up(&mut self) {
        let Some(a) = self.archive.as_mut() else { return };
        if a.dir.is_empty() {
            return;
        }
        let up = a.dir.rsplit_once('/').map_or(String::new(), |(p, _)| p.to_string());
        a.show_dir(up);
        self.invalidate();
    }

    /// The header's path: part `i` (0, the archive's top).
    pub(super) fn archive_crumb(&mut self, i: usize) {
        let Some(a) = self.archive.as_mut() else { return };
        let to: Vec<&str> = a.dir.split('/').filter(|s| !s.is_empty()).take(i).collect();
        a.show_dir(to.join("/"));
        self.invalidate();
    }

    /// What Extract takes: the rows selected (folders with all inside), from the folder showing; else all of it.
    fn archive_pick(&self) -> Option<(Box<dyn Fn(&Entry) -> bool + Send>, String, u64)> {
        let a = self.archive.as_ref()?;
        let list = a.entries.as_ref()?.as_ref().ok()?;
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
        let mut dest = parent.join(base_name(&a.path));
        let mut n = 2;
        while dest.exists() {
            dest = parent.join(format!("{} ({n})", base_name(&a.path)));
            n += 1;
        }
        let Some((pick, strip, total)) = self.archive_pick() else { return };
        self.archive_run(pick, strip, dest, total, None);
    }

    /// Extract to…: into a folder picked.
    pub(super) fn archive_extract_to(&mut self) {
        let Some(start) = self.archive.as_ref().and_then(|a| a.path.parent().map(Path::to_path_buf)) else { return };
        let Some(dest) = (unsafe { self.pick_folder(&start) }) else { return };
        let Some((pick, strip, total)) = self.archive_pick() else { return };
        self.archive_run(pick, strip, dest, total, None);
    }

    /// A folder picker opening at `start`.
    unsafe fn pick_folder(&self, start: &Path) -> Option<PathBuf> {
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
    fn archive_run(&mut self, pick: Box<dyn Fn(&Entry) -> bool + Send>, strip: String, dest: PathBuf, total: u64, open: Option<PathBuf>) {
        let Some(a) = self.archive.as_mut() else { return };
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
                    st.result = Some(r);
                }
            })
            .ok();
        a.work = Some(Work { state, cancel, total, open, dest });
        self.invalidate();
    }

    pub(super) fn archive_cancel(&mut self) {
        if let Some(w) = self.archive.as_ref().and_then(|a| a.work.as_ref()) {
            w.cancel.store(true, Ordering::Relaxed);
        }
    }

    /// The wheel over the card: three rows a notch.
    pub(super) fn archive_wheel(&mut self, delta: f32) {
        let (_, list) = self.archive_rects();
        let up = self.archive_up_row();
        let Some(a) = self.archive.as_mut() else { return };
        let max = ((a.rows.len() + up) as f32 * ROW_H - (list.bottom - list.top)).max(0.0);
        a.scroll = (a.scroll - delta / 120.0 * ROW_H * 3.0).clamp(0.0, max);
        self.invalidate();
    }

    /// Keys while an archive shows: ↑/↓ move (Shift extends), Enter opens, Backspace goes up, Ctrl+A selects all.
    pub(super) fn archive_key(&mut self, vk: VIRTUAL_KEY, ctrl: bool, shift: bool) -> bool {
        if !self.archive_shown() || self.page_open() || self.menu.is_some() {
            return false;
        }
        let (_, list) = self.archive_rects();
        let up = self.archive_up_row();
        let Some(a) = self.archive.as_mut() else { return false };
        let n = a.rows.len();
        match vk {
            VK_A if ctrl => {
                a.selected = (0..n).collect();
            }
            VK_UP | VK_DOWN if n > 0 => {
                let cur = a.anchor.unwrap_or(if vk == VK_DOWN { usize::MAX } else { n });
                let next = if vk == VK_DOWN { cur.wrapping_add(1).min(n - 1) } else { cur.saturating_sub(1) };
                if shift {
                    a.selected.insert(next);
                } else {
                    a.selected.clear();
                    a.selected.insert(next);
                }
                a.anchor = Some(next);
                // Keep it in view.
                let y = (next + up) as f32 * ROW_H;
                let h = list.bottom - list.top;
                if y < a.scroll {
                    a.scroll = y;
                } else if y + ROW_H > a.scroll + h {
                    a.scroll = y + ROW_H - h;
                }
            }
            VK_RETURN => {
                if let Some(r) = a.anchor.filter(|r| a.selected.contains(r)) {
                    self.archive_open(r);
                }
                return true;
            }
            VK_BACK => {
                self.archive_up();
                return true;
            }
            _ => return false,
        }
        self.invalidate();
        true
    }
}
