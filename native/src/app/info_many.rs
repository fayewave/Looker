//! The info card with several files selected in the explorer (Transient's Metadata panel with a selection): how
//! many, their total size, kinds and dates, then every property any of them has, with its value where they all
//! share it, "Canon (in 4 of 6)" where only some have it, or "Mixed values (3 different)". Dates show their
//! range.
//!
//! The files' metadata is read on one background thread per selection (`metadata::read`, the single card's
//! reader), at most `MAX_READ` of them, never an online-only file; results are kept by path and write time, so
//! going back to a selection is instant. The summary is worked out again only when the selection or the
//! results change, not per frame.

use std::collections::{BTreeMap, HashMap};
use std::hash::{Hash, Hasher};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use super::info::{Block, format_exif_time, grouped, size_text};
use super::*;
use crate::metadata::{self, Exif, ExifTime};

pub(super) const WM_EXIF_MANY: u32 = WM_APP + 14;
/// Files read per selection; past this the summary says it covers the first ones.
const MAX_READ: usize = 1000;
/// The groups in the single card's order: a group first met in a later file still lands in its place.
const GROUP_ORDER: [&str; 9] = ["Image", "Description", "Video", "Audio", "Tags", "Camera", "Exposure", "Date", "Location"];

pub(super) struct ManyResult {
    pub generation: u64,
    pub items: Vec<(PathBuf, u64, Option<Exif>)>,
}

/// One selected file, as the explorer lists it.
#[derive(Clone, PartialEq)]
pub(super) struct Picked {
    pub path: PathBuf,
    pub stamp: u64,
    pub size: u64,
    pub cloud: bool,
}

struct Current {
    key: u64,
    files: Vec<Picked>,
    folders: usize,
    blocks: Option<Vec<Block>>,
}

#[derive(Default)]
pub(super) struct ManyState {
    current: Option<Current>,
    /// Every file read so far: (write time, what it had).
    cache: HashMap<PathBuf, (u64, Option<Exif>)>,
    /// Bumped per selection: a reader for an older one stops.
    generation: Arc<AtomicU64>,
}

impl ManyState {
    pub(super) fn key(&self) -> Option<u64> {
        self.current.as_ref().map(|c| c.key)
    }
}

impl App {
    /// The explorer's selection when it holds more than one file (and the explorer is out): those files, and how
    /// many folders are selected with them.
    fn explorer_picked(&self) -> Option<(Vec<Picked>, usize)> {
        if !self.explorer_shown() || self.explorer.selected.len() < 2 {
            return None;
        }
        let mut files = Vec::new();
        let mut folders = 0;
        for &i in &self.explorer.view {
            let r = &self.explorer.rows[i];
            if !self.explorer.is_selected(&r.path) {
                continue;
            }
            if r.is_folder() {
                folders += 1;
            } else {
                files.push(Picked { path: r.path.clone(), stamp: r.stamp, size: r.size, cloud: r.cloud });
            }
        }
        (files.len() > 1).then_some((files, folders))
    }

    /// Whether the card is showing a selection's summary.
    pub(super) fn info_many(&self) -> bool {
        self.info_card.many.current.is_some()
    }

    /// The summary's blocks, while it's showing.
    pub(super) fn many_blocks(&self) -> Option<Vec<Block>> {
        self.info_card.many.current.as_ref().map(|c| c.blocks.clone().unwrap_or_default())
    }

    /// The selection's files as read, each with what it had: all of them, or None while some are still being
    /// read (online-only ones and those past `MAX_READ` aren't counted).
    pub(super) fn many_read(&self) -> Option<Vec<(&Path, Option<&Exif>)>> {
        let c = self.info_card.many.current.as_ref()?;
        let cache = &self.info_card.many.cache;
        let mut out = Vec::new();
        for f in c.files.iter().take(MAX_READ).filter(|f| !f.cloud) {
            let (stamp, exif) = cache.get(&f.path)?;
            if *stamp != f.stamp {
                return None;
            }
            out.push((f.path.as_path(), exif.as_ref()));
        }
        Some(out)
    }

    /// What a selected file had, as last read.
    pub(super) fn many_exif(&self, path: &Path) -> Option<&Exif> {
        self.info_card.many.cache.get(path).and_then(|(_, e)| e.as_ref())
    }

    /// Every file of the selection showing.
    pub(super) fn many_paths(&self) -> Vec<PathBuf> {
        self.info_card.many.current.as_ref().map(|c| c.files.iter().map(|f| f.path.clone()).collect()).unwrap_or_default()
    }

    /// Reads `paths` of the selection again (just written): the cache keeps the old reading until then, so the
    /// summary doesn't flash "Reading".
    pub(super) fn reread_many(&self, paths: &[PathBuf]) {
        let Some(c) = &self.info_card.many.current else { return };
        let todo: Vec<(PathBuf, u64)> = c.files.iter().filter(|f| paths.contains(&f.path)).map(|f| (f.path.clone(), f.stamp)).collect();
        if !todo.is_empty() {
            self.read_many(todo, self.info_card.many.generation.load(Ordering::SeqCst));
        }
    }

    /// Follows the explorer's selection: a new one starts its reader; the summary is (re)built when stale.
    pub(super) fn sync_many(&mut self) {
        let picked = self.explorer_picked();
        let key = picked.as_ref().map(|(files, folders)| {
            let mut h = std::collections::hash_map::DefaultHasher::new();
            for f in files {
                f.path.hash(&mut h);
                f.stamp.hash(&mut h);
            }
            folders.hash(&mut h);
            h.finish()
        });
        if key != self.info_card.many.key() {
            let generation = self.info_card.many.generation.fetch_add(1, Ordering::SeqCst) + 1;
            self.info_card.many.current = picked.map(|(files, folders)| Current { key: key.unwrap_or(0), files, folders, blocks: None });
            if let Some(c) = &self.info_card.many.current {
                let cache = &self.info_card.many.cache;
                let todo: Vec<(PathBuf, u64)> = c.files.iter().take(MAX_READ).filter(|f| !f.cloud && cache.get(&f.path).is_none_or(|(s, _)| *s != f.stamp)).map(|f| (f.path.clone(), f.stamp)).collect();
                if !todo.is_empty() {
                    self.read_many(todo, generation);
                }
            }
        }
        if let Some(c) = &self.info_card.many.current {
            if c.blocks.is_none() {
                let blocks = summary(&c.files, c.folders, &self.info_card.many.cache);
                if let Some(c) = self.info_card.many.current.as_mut() {
                    c.blocks = Some(blocks);
                }
            }
        }
    }

    /// Reads `todo` on a thread, posting what it has every 16 files (or at the end); stops when the selection
    /// moves on.
    fn read_many(&self, todo: Vec<(PathBuf, u64)>, generation: u64) {
        let hwnd = self.hwnd.0 as isize;
        let current = self.info_card.many.generation.clone();
        let post = move |items: Vec<(PathBuf, u64, Option<Exif>)>| {
            let ptr = Box::into_raw(Box::new(ManyResult { generation, items }));
            unsafe {
                if PostMessageW(Some(HWND(hwnd as _)), WM_EXIF_MANY, WPARAM(0), LPARAM(ptr as isize)).is_err() {
                    drop(Box::from_raw(ptr));
                }
            }
        };
        std::thread::Builder::new()
            .name("exif-many".into())
            .spawn(move || {
                let mut batch = Vec::new();
                for (path, stamp) in todo {
                    if current.load(Ordering::SeqCst) != generation {
                        return;
                    }
                    let exif = metadata::read(&path);
                    batch.push((path, stamp, exif));
                    if batch.len() >= 16 {
                        post(std::mem::take(&mut batch));
                    }
                }
                if !batch.is_empty() {
                    post(batch);
                }
            })
            .ok();
    }

    pub(super) fn on_exif_many(&mut self, r: ManyResult) {
        let m = &mut self.info_card.many;
        // Kept whichever selection asked: the files are read either way.
        if m.cache.len() > 20_000 {
            m.cache.clear();
        }
        for (path, stamp, exif) in r.items {
            m.cache.insert(path, (stamp, exif));
        }
        if r.generation == m.generation.load(Ordering::SeqCst) {
            if let Some(c) = m.current.as_mut() {
                c.blocks = None;
            }
            self.invalidate();
        }
    }
}

/// A file's rows by (group, label), the Image group led by its dimensions.
fn rows_of(e: &Exif) -> Vec<(&'static str, &'static str, String)> {
    let mut out = Vec::new();
    if let Some((w, h)) = e.dims {
        out.push(("Image", "Dimensions", format!("{w} \u{d7} {h}")));
    }
    for g in metadata::groups(e, format_exif_time) {
        // Dates are summed up as ranges from the raw times instead.
        if g.title == "Date" {
            continue;
        }
        for (l, v) in g.rows {
            out.push((g.title, l, v));
        }
    }
    out
}

/// "a to b", or one of them when they read the same.
fn range(a: String, b: String) -> String {
    if a == b { a } else { format!("{a} to {b}") }
}

/// The selection's blocks: the heading, the File group, then each property any file has.
fn summary(files: &[Picked], folders: usize, cache: &HashMap<PathBuf, (u64, Option<Exif>)>) -> Vec<Block> {
    let row = |label: &'static str, value: String| Block::Row { label, value, path: false, copy: None, dim: false };
    let mut out = Vec::new();
    let names: Vec<String> = files.iter().map(|f| file_name(&f.path)).collect();
    out.push(Block::Heading(format!("{} files selected", grouped(files.len() as u64)), Some(names.join("\r\n"))));
    out.push(Block::Title("Selection"));
    out.push(row("Total size", size_text(files.iter().map(|f| f.size).sum())));
    // Kinds by extension, most common first.
    let mut kinds: BTreeMap<String, usize> = BTreeMap::new();
    for f in files {
        let ext = f.path.extension().map(|e| e.to_string_lossy().to_uppercase()).unwrap_or_else(|| "No extension".into());
        *kinds.entry(ext).or_default() += 1;
    }
    let mut kinds: Vec<(String, usize)> = kinds.into_iter().collect();
    kinds.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    out.push(row(if kinds.len() == 1 { "Type" } else { "Types" }, kinds.iter().map(|(k, n)| format!("{k} ({})", grouped(*n as u64))).collect::<Vec<_>>().join(", ")));
    let ft = |t: u64| FILETIME { dwLowDateTime: t as u32, dwHighDateTime: (t >> 32) as u32 };
    if let (Some(lo), Some(hi)) = (files.iter().map(|f| f.stamp).filter(|&s| s > 0).min(), files.iter().map(|f| f.stamp).max()) {
        out.push(row("Modified", range(format_time(ft(lo)), format_time(ft(hi)))));
    }
    if folders > 0 {
        out.push(row("Folders", format!("{} selected too, not included", grouped(folders as u64))));
    }
    let considered: Vec<&Picked> = files.iter().take(MAX_READ).filter(|f| !f.cloud).collect();
    let online = files.iter().take(MAX_READ).filter(|f| f.cloud).count();
    if online > 0 {
        out.push(row("Online only", format!("{} of them: their details come once they're downloaded", grouped(online as u64))));
    }
    if files.len() > MAX_READ {
        out.push(row("Details", format!("From the first {} files", grouped(MAX_READ as u64))));
    }
    let read: Vec<&Option<Exif>> = considered.iter().filter_map(|f| cache.get(&f.path).filter(|(s, _)| *s == f.stamp).map(|(_, e)| e)).collect();
    if read.len() < considered.len() {
        out.push(row("Reading", format!("{} of {} files\u{2026}", grouped(read.len() as u64), grouped(considered.len() as u64))));
    }
    if read.is_empty() {
        return out;
    }
    // Each file's rows; the (group, label) pairs in the card's group order, first met first within a group.
    let per_file: Vec<Vec<(&'static str, &'static str, String)>> = read.iter().map(|e| e.as_ref().map(rows_of).unwrap_or_default()).collect();
    let mut keys: Vec<(&'static str, &'static str)> = Vec::new();
    for rows in &per_file {
        for (g, l, _) in rows {
            if !keys.contains(&(g, l)) {
                keys.push((g, l));
            }
        }
    }
    // Within a group, the order of the file with the most of its rows (one file's Software first shouldn't put
    // Software above Title); what that file lacks after, as first met.
    let template = |g: &str| -> Vec<&'static str> { per_file.iter().map(|rows| rows.iter().filter(|r| r.0 == g).map(|r| r.1).collect::<Vec<_>>()).max_by_key(|v| v.len()).unwrap_or_default() };
    let templates: HashMap<&str, Vec<&'static str>> = keys.iter().map(|(g, _)| (*g, template(g))).collect();
    let met: Vec<(&'static str, &'static str)> = keys.clone();
    keys.sort_by_key(|k| {
        let t = &templates[k.0];
        t.iter().position(|l| *l == k.1).unwrap_or(t.len() + met.iter().position(|m| m == k).unwrap_or(0))
    });
    let n = read.len();
    // The groups in the card's order (any other after them); Date is the files' time ranges.
    let mut order: Vec<&'static str> = GROUP_ORDER.to_vec();
    for (g, _) in &keys {
        if !order.contains(g) {
            order.push(g);
        }
    }
    for g in order {
        let rows: Vec<Block> = if g == "Date" {
            dates(&read)
        } else {
            keys.iter()
                .filter(|(kg, _)| *kg == g)
                .map(|&(_, l)| {
                    let values: Vec<Option<&String>> = per_file.iter().map(|rows| rows.iter().find(|(rg, rl, _)| *rg == g && *rl == l).map(|r| &r.2)).collect();
                    shared(l, &values, n)
                })
                .collect()
        };
        if !rows.is_empty() {
            out.push(Block::Title(g));
            out.extend(rows);
        }
    }
    out
}

/// One property across the files: shared, shared by some, or mixed.
pub(super) fn shared(label: &'static str, values: &[Option<&String>], n: usize) -> Block {
    let have: Vec<&String> = values.iter().flatten().copied().collect();
    let mut counts: Vec<(&String, usize)> = Vec::new();
    for v in &have {
        match counts.iter_mut().find(|(c, _)| c == v) {
            Some(c) => c.1 += 1,
            None => counts.push((v, 1)),
        }
    }
    counts.sort_by(|a, b| b.1.cmp(&a.1));
    let row = |value: String, copy: Option<String>, dim: bool| Block::Row { label, value, path: false, copy, dim };
    match counts.len() {
        1 if have.len() == n => row(counts[0].0.clone(), None, false),
        1 => row(format!("{} (in {} of {})", counts[0].0, grouped(have.len() as u64), grouped(n as u64)), Some(counts[0].0.clone()), false),
        k => {
            // Missing counts as one more kind of value, as Transient's does.
            let kinds = k + usize::from(have.len() < n);
            let copy = counts.iter().map(|(v, _)| v.as_str()).collect::<Vec<_>>().join("\r\n");
            row(format!("Mixed values ({kinds} different)"), Some(copy), true)
        }
    }
}

/// Taken (or Recorded) and Digitized as ranges over the files that have them.
fn dates(read: &[&Option<Exif>]) -> Vec<Block> {
    let span = |pick: &dyn Fn(&Exif) -> Option<ExifTime>| -> Option<(String, usize)> {
        let mut ts: Vec<ExifTime> = read.iter().filter_map(|e| e.as_ref().and_then(pick)).collect();
        if ts.is_empty() {
            return None;
        }
        ts.sort_by_key(|t| (t.year, t.month, t.day, t.hour, t.minute, t.second));
        Some((range(format_exif_time(ts[0]), format_exif_time(*ts.last().unwrap())), ts.len()))
    };
    let n = read.len();
    let video = read.iter().any(|e| e.as_ref().is_some_and(|e| !e.video.is_empty()));
    let mut rows = Vec::new();
    for (label, pick) in [(if video { "Recorded" } else { "Taken" }, &(|e: &Exif| e.taken) as &dyn Fn(&Exif) -> Option<ExifTime>), ("Digitized", &|e: &Exif| e.digitized)] {
        if let Some((text, k)) = span(pick) {
            let value = if k < n { format!("{text} (in {} of {})", grouped(k as u64), grouped(n as u64)) } else { text };
            rows.push(Block::Row { label, value, path: false, copy: None, dim: false });
        }
    }
    rows
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shared_some_and_mixed() {
        let (a, b) = ("Canon".to_string(), "Sony".to_string());
        let value = |blk: Block| match blk {
            Block::Row { value, dim, .. } => (value, dim),
            _ => unreachable!(),
        };
        assert_eq!(value(shared("Make", &[Some(&a), Some(&a)], 2)), ("Canon".to_string(), false));
        assert_eq!(value(shared("Make", &[Some(&a), None, Some(&a)], 3)), ("Canon (in 2 of 3)".to_string(), false));
        assert_eq!(value(shared("Make", &[Some(&a), Some(&b)], 2)), ("Mixed values (2 different)".to_string(), true));
        assert_eq!(value(shared("Make", &[Some(&a), Some(&b), None], 3)), ("Mixed values (3 different)".to_string(), true));
    }

    #[test]
    fn ranges_collapse_when_equal() {
        assert_eq!(range("a".into(), "a".into()), "a");
        assert_eq!(range("a".into(), "b".into()), "a to b");
    }
}
