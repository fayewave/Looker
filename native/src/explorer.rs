//! The file explorer's model, after VEIL Transient's file lists. A *place* is a folder, the virtual "This PC"
//! above the drives, or a collection (the files tagged favourite or with a colour, `tags.rs`). Rows are its
//! entries under the sort (name, date modified or size, Explorer's natural name order breaking ties) and the
//! grouping (folders first, files first, or mixed in by the sort); files Looker can't open are listed too
//! (greyed out by the card). Browser-style history records every place listed. Disk access is confined to
//! [`list`], [`subfolders`], [`dimensions`] and the [`Watcher`]; the rest is pure and unit tested.

use std::cmp::Ordering;
use std::os::windows::fs::MetadataExt;
use std::path::{Path, PathBuf};

use windows::Win32::Foundation::{CloseHandle, HANDLE, HWND, LPARAM, WAIT_OBJECT_0, WPARAM};
use windows::Win32::Storage::FileSystem::{
    FILE_NOTIFY_CHANGE, FILE_NOTIFY_CHANGE_DIR_NAME, FILE_NOTIFY_CHANGE_FILE_NAME, FILE_NOTIFY_CHANGE_LAST_WRITE, FILE_NOTIFY_CHANGE_SIZE,
    FindCloseChangeNotification, FindFirstChangeNotificationW, FindNextChangeNotification, GetDriveTypeW, GetLogicalDrives, GetVolumeInformationW,
};
use windows::Win32::System::Threading::{CreateEventW, INFINITE, SetEvent, WaitForMultipleObjects};
use windows::Win32::UI::Shell::StrCmpLogicalW;
use windows::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_APP};
use windows::core::{HSTRING, PCWSTR};

use crate::folder::{Sort, SortField};
use crate::format::is_supported;

pub const WM_EXPLORER_LISTED: u32 = WM_APP + 7;
pub const WM_EXPLORER_CHANGED: u32 = WM_APP + 8;
/// The open photo's folder changed on disk (the viewer's own watcher).
pub const WM_FOLDER_CHANGED: u32 = WM_APP + 9;
/// A folder's subfolders, listed for a tree or a crumb's menu (boxed `app::explorer::Subfolders`).
pub const WM_SUBFOLDERS: u32 = WM_APP + 20;
/// Image sizes read for the Dimensions column (boxed `Vec<(PathBuf, u64, Option<(u32, u32)>)>`).
pub const WM_DIMENSIONS: u32 = WM_APP + 21;
/// Rows dragged out of the window: Windows' own drag takes them (run after the mouse message that saw it).
pub const WM_DRAG_OUT: u32 = WM_APP + 22;

const FILE_ATTRIBUTE_HIDDEN: u32 = 0x2;
const FILE_ATTRIBUTE_SYSTEM: u32 = 0x4;
const FILE_ATTRIBUTE_OFFLINE: u32 = 0x1000;
const FILE_ATTRIBUTE_RECALL_ON_OPEN: u32 = 0x40000;
const FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS: u32 = 0x400000;
const DRIVE_REMOVABLE: u32 = 2;

pub const COMPUTER_NAME: &str = "This PC";

/// A tag collection: the favourites, or one colour (an index into [`COLOURS`]).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub enum Collection {
    Favourites,
    Colour(u8),
}

/// The colour tags, as Transient offers them: name and swatch.
pub const COLOURS: [(&str, u32); 7] = [
    ("Red", 0xE5484D),
    ("Orange", 0xF08A24),
    ("Yellow", 0xF2C94C),
    ("Green", 0x46A758),
    ("Blue", 0x3E8EED),
    ("Purple", 0x9B59D0),
    ("Grey", 0x8B8B8B),
];

impl Collection {
    pub fn name(self) -> String {
        match self {
            Collection::Favourites => "Favourites".into(),
            Collection::Colour(i) => COLOURS.get(i as usize).map_or("Colour", |c| c.0).into(),
        }
    }
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Place {
    Computer,
    Folder(PathBuf),
    Collection(Collection),
}

impl Place {
    pub fn name(&self) -> String {
        match self {
            Place::Computer => COMPUTER_NAME.into(),
            Place::Folder(p) => folder_name(p),
            Place::Collection(c) => c.name(),
        }
    }

    pub fn same(&self, other: &Place) -> bool {
        match (self, other) {
            (Place::Folder(a), Place::Folder(b)) => same_path(a, b),
            (a, b) => a == b,
        }
    }

    pub fn folder(&self) -> Option<&Path> {
        match self {
            Place::Folder(p) => Some(p),
            _ => None,
        }
    }

    /// One level up: a folder's parent, a drive root's "This PC". Nothing above This PC or a collection.
    pub fn parent(&self) -> Option<Place> {
        match self {
            Place::Folder(p) => Some(p.parent().map_or(Place::Computer, |q| Place::Folder(q.to_path_buf()))),
            _ => None,
        }
    }
}

/// A place as a settings value (a tab): a folder's path, else `::pc`, `::favourites`, `::colour:N`.
pub fn place_key(p: &Place) -> String {
    match p {
        Place::Folder(f) => f.display().to_string(),
        Place::Computer => "::pc".into(),
        Place::Collection(Collection::Favourites) => "::favourites".into(),
        Place::Collection(Collection::Colour(i)) => format!("::colour:{i}"),
    }
}

pub fn place_from_key(s: &str) -> Option<Place> {
    match s {
        "" => None,
        "::pc" => Some(Place::Computer),
        "::favourites" => Some(Place::Collection(Collection::Favourites)),
        _ => match s.strip_prefix("::colour:") {
            Some(n) => n.parse::<u8>().ok().filter(|&i| (i as usize) < COLOURS.len()).map(|i| Place::Collection(Collection::Colour(i))),
            None if s.starts_with("::") => None,
            None => Some(Place::Folder(PathBuf::from(s))),
        },
    }
}

/// A folder's display name: its last segment, or "C:\" for a drive root.
pub fn folder_name(p: &Path) -> String {
    p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| p.to_string_lossy().into_owned())
}

/// Case-insensitive and blind to a trailing separator, like Windows paths.
pub fn same_path(a: &Path, b: &Path) -> bool {
    norm(a) == norm(b)
}

/// A path as a case-folded key, without a trailing separator.
pub fn norm(p: &Path) -> String {
    p.to_string_lossy().trim_end_matches(['\\', '/']).to_lowercase()
}

/// `inner` is `outer` or inside it.
pub fn within(inner: &Path, outer: &Path) -> bool {
    let (i, o) = (norm(inner), norm(outer));
    i == o || i.starts_with(&format!("{o}\\"))
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Drive,
    Folder,
    /// A file Looker can open.
    Image,
    /// Any other file: listed so the folder reads like Explorer, but inert.
    Other,
}

#[derive(Clone, Debug)]
pub struct Row {
    pub path: PathBuf,
    pub name: String,
    name_h: HSTRING,
    pub kind: Kind,
    /// Write time (FILETIME ticks).
    pub stamp: u64,
    /// Creation time (FILETIME ticks).
    pub created: u64,
    pub size: u64,
    pub cloud: bool,
}

impl Row {
    pub fn is_folder(&self) -> bool {
        matches!(self.kind, Kind::Drive | Kind::Folder)
    }

    /// The Type column: "JPG file", "Folder", "Drive".
    pub fn type_name(&self) -> String {
        match self.kind {
            Kind::Drive => "Drive".into(),
            Kind::Folder => "Folder".into(),
            _ => match self.path.extension() {
                Some(e) => format!("{} file", e.to_string_lossy().to_uppercase()),
                None => "File".into(),
            },
        }
    }

    #[cfg(test)]
    fn test(name: &str, kind: Kind, stamp: u64, size: u64) -> Row {
        Row { path: PathBuf::from(name), name: name.into(), name_h: HSTRING::from(name), kind, stamp, created: 0, size, cloud: false }
    }
}

/// The list's columns, as Transient's file lists offer theirs: which show, in what order and how wide is the
/// user's (`Settings::explorer_columns`). Name is always there and takes the width the others leave.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub enum ColKind {
    Name,
    Extension,
    Type,
    Size,
    Parent,
    Path,
    Modified,
    Created,
    Taken,
    Dimensions,
    Megapixels,
    Camera,
    Lens,
}

impl ColKind {
    pub const ALL: [ColKind; 13] = [
        ColKind::Name,
        ColKind::Extension,
        ColKind::Type,
        ColKind::Size,
        ColKind::Parent,
        ColKind::Path,
        ColKind::Modified,
        ColKind::Created,
        ColKind::Taken,
        ColKind::Dimensions,
        ColKind::Megapixels,
        ColKind::Camera,
        ColKind::Lens,
    ];

    /// The header menu's groups, each with its columns.
    pub const GROUPS: [(&'static str, &'static [ColKind]); 3] = [
        ("File", &[ColKind::Name, ColKind::Extension, ColKind::Type, ColKind::Size, ColKind::Parent, ColKind::Path]),
        ("Date and time", &[ColKind::Modified, ColKind::Created, ColKind::Taken]),
        ("Image", &[ColKind::Dimensions, ColKind::Megapixels, ColKind::Camera, ColKind::Lens]),
    ];

    pub fn label(self) -> &'static str {
        match self {
            ColKind::Name => "Name",
            ColKind::Extension => "Extension",
            ColKind::Type => "Type",
            ColKind::Size => "Size",
            ColKind::Parent => "Parent folder",
            ColKind::Path => "Path",
            ColKind::Modified => "Date modified",
            ColKind::Created => "Date created",
            ColKind::Taken => "Date taken",
            ColKind::Dimensions => "Dimensions",
            ColKind::Megapixels => "Megapixels",
            ColKind::Camera => "Camera",
            ColKind::Lens => "Lens",
        }
    }

    pub fn key(self) -> &'static str {
        match self {
            ColKind::Name => "name",
            ColKind::Extension => "ext",
            ColKind::Type => "type",
            ColKind::Size => "size",
            ColKind::Parent => "parent",
            ColKind::Path => "path",
            ColKind::Modified => "modified",
            ColKind::Created => "created",
            ColKind::Taken => "taken",
            ColKind::Dimensions => "dims",
            ColKind::Megapixels => "mp",
            ColKind::Camera => "camera",
            ColKind::Lens => "lens",
        }
    }

    pub fn default_width(self) -> f32 {
        match self {
            ColKind::Name => 0.0,
            ColKind::Extension => 70.0,
            ColKind::Type => 84.0,
            ColKind::Size => 76.0,
            ColKind::Parent => 140.0,
            ColKind::Path => 220.0,
            ColKind::Modified | ColKind::Created | ColKind::Taken => 156.0,
            ColKind::Dimensions => 96.0,
            ColKind::Megapixels => 80.0,
            ColKind::Camera => 140.0,
            ColKind::Lens => 160.0,
        }
    }

    /// Numbers sit at the right of their column.
    pub fn right_aligned(self) -> bool {
        matches!(self, ColKind::Size | ColKind::Megapixels)
    }

    /// Read from the file itself, off the UI thread, as rows come on screen.
    pub fn lazy(self) -> bool {
        matches!(self, ColKind::Taken | ColKind::Dimensions | ColKind::Megapixels | ColKind::Camera | ColKind::Lens)
    }

    /// The toolbar's sort this column drives (the viewer's ←/→ order follows); the others sort the list alone.
    pub fn shared_sort(self) -> Option<SortField> {
        match self {
            ColKind::Name => Some(SortField::Name),
            ColKind::Modified => Some(SortField::Date),
            ColKind::Size => Some(SortField::Size),
            _ => None,
        }
    }

    pub fn parse(s: &str) -> Option<ColKind> {
        ColKind::ALL.into_iter().find(|c| c.key() == s)
    }

    /// The columns Looker starts with.
    pub fn defaults() -> Vec<(ColKind, f32)> {
        [ColKind::Name, ColKind::Modified, ColKind::Size, ColKind::Dimensions, ColKind::Type].into_iter().map(|c| (c, c.default_width())).collect()
    }

    /// "name:0,modified:132,…" for the settings file.
    pub fn format(cols: &[(ColKind, f32)]) -> String {
        cols.iter().map(|(c, w)| format!("{}:{}", c.key(), w.round())).collect::<Vec<_>>().join(",")
    }

    /// The settings' column list; Name is put back at the front if it went missing.
    pub fn parse_list(s: &str) -> Vec<(ColKind, f32)> {
        let mut out: Vec<(ColKind, f32)> = Vec::new();
        for part in s.split(',') {
            let (k, w) = part.split_once(':').unwrap_or((part, ""));
            if let Some(c) = ColKind::parse(k.trim()) {
                if out.iter().all(|(o, _)| *o != c) {
                    let w = w.trim().parse::<f32>().ok().filter(|w| (30.0..=800.0).contains(w)).unwrap_or(c.default_width());
                    out.push((c, if c == ColKind::Name { 0.0 } else { w }));
                }
            }
        }
        if out.iter().all(|(c, _)| *c != ColKind::Name) {
            out.insert(0, (ColKind::Name, 0.0));
        }
        out
    }
}

/// Where folders go among the files (Transient's folders/files toggle).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Group {
    #[default]
    FoldersFirst,
    FilesFirst,
    /// Folders sorted in among the files by the sort.
    Mixed,
}

impl Group {
    pub const ALL: [Group; 3] = [Group::FoldersFirst, Group::FilesFirst, Group::Mixed];

    pub fn label(self) -> &'static str {
        match self {
            Group::FoldersFirst => "Folders first",
            Group::FilesFirst => "Files first",
            Group::Mixed => "Mixed, by the sort",
        }
    }

    pub fn key(self) -> &'static str {
        match self {
            Group::FoldersFirst => "folders",
            Group::FilesFirst => "files",
            Group::Mixed => "mixed",
        }
    }

    pub fn parse(s: &str) -> Option<Group> {
        Group::ALL.into_iter().find(|g| g.key() == s)
    }
}

/// The sidebar's sections, in the order the user dragged them into (Transient's: their headings drag).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub enum SideSection {
    Bookmarks,
    Collections,
    Places,
    History,
}

impl SideSection {
    /// History last (Faye, 2026-11-09).
    pub const DEFAULT: [SideSection; 4] = [SideSection::Bookmarks, SideSection::Collections, SideSection::Places, SideSection::History];

    fn key(self) -> &'static str {
        match self {
            SideSection::Bookmarks => "bookmarks",
            SideSection::Collections => "collections",
            SideSection::Places => "places",
            SideSection::History => "history",
        }
    }

    /// The settings' list: the known sections in its order, once each, any missing appended in the default's.
    pub fn parse_list(s: &str) -> Vec<SideSection> {
        let mut out: Vec<SideSection> = Vec::new();
        for k in s.split(',') {
            if let Some(x) = SideSection::DEFAULT.into_iter().find(|x| x.key() == k.trim()) {
                if !out.contains(&x) {
                    out.push(x);
                }
            }
        }
        for x in SideSection::DEFAULT {
            if !out.contains(&x) {
                out.push(x);
            }
        }
        out
    }

    /// A set of sections (the folded ones): the known keys, once each, nothing added.
    pub fn parse_set(s: &str) -> Vec<SideSection> {
        let mut out: Vec<SideSection> = Vec::new();
        for k in s.split(',') {
            if let Some(x) = SideSection::DEFAULT.into_iter().find(|x| x.key() == k.trim()) {
                if !out.contains(&x) {
                    out.push(x);
                }
            }
        }
        out
    }

    pub fn format(v: &[SideSection]) -> String {
        v.iter().map(|x| x.key()).collect::<Vec<_>>().join(",")
    }
}

/// How the explorer shows a folder (Transient's Folders and files menu): where the folders go, whether the
/// subfolders' files are listed too, whether files Looker can't open show, how tall the rows are, and the
/// columns. The default is the settings'; a folder can keep its own (`SavedView`).
#[derive(Clone, PartialEq, Debug)]
pub struct View {
    pub group: Group,
    pub subfolders: bool,
    pub other_files: bool,
    /// Row height in rows (1-4): taller rows, bigger thumbnails.
    pub height: u8,
    pub columns: Vec<(ColKind, f32)>,
}

pub const HEIGHT_MAX: u8 = 4;

impl Default for View {
    fn default() -> Self {
        View { group: Group::default(), subfolders: false, other_files: true, height: 1, columns: ColKind::defaults() }
    }
}

/// What a saved view covers.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Scope {
    /// The folder alone.
    Folder,
    /// The folder and every folder under it (unless one has its own).
    Tree,
    /// None here, though a parent's covers it: the default view.
    Off,
}

impl Scope {
    fn key(self) -> &'static str {
        match self {
            Scope::Folder => "folder",
            Scope::Tree => "tree",
            Scope::Off => "off",
        }
    }
}

/// A folder's own view (Transient's "Saved view").
#[derive(Clone, PartialEq, Debug)]
pub struct SavedView {
    pub path: PathBuf,
    pub scope: Scope,
    pub view: View,
}

impl SavedView {
    /// `scope;group;subfolders;other;height;columns;path` (the path last: it may hold anything but `|`).
    pub fn format(&self) -> String {
        let v = &self.view;
        format!(
            "{};{};{};{};{};{};{}",
            self.scope.key(),
            v.group.key(),
            v.subfolders as i32,
            v.other_files as i32,
            v.height,
            ColKind::format(&v.columns),
            self.path.display()
        )
    }

    pub fn parse(s: &str) -> Option<SavedView> {
        let f: Vec<&str> = s.splitn(7, ';').collect();
        if f.len() != 7 || f[6].is_empty() {
            return None;
        }
        let scope = [Scope::Folder, Scope::Tree, Scope::Off].into_iter().find(|x| x.key() == f[0])?;
        let view = View {
            group: Group::parse(f[1]).unwrap_or_default(),
            subfolders: f[2] == "1",
            other_files: f[3] != "0",
            height: f[4].parse::<u8>().map_or(1, |h| h.clamp(1, HEIGHT_MAX)),
            columns: ColKind::parse_list(f[5]),
        };
        Some(SavedView { path: PathBuf::from(f[6]), scope, view })
    }
}

/// The saved view that covers `folder`, by index: its own (unless it's `Off`), else the nearest parent's that
/// covers its subfolders. None: the default view.
pub fn saved_for(views: &[SavedView], folder: &Path) -> Option<usize> {
    let key = norm(folder);
    if let Some(i) = views.iter().position(|v| norm(&v.path) == key) {
        return (views[i].scope != Scope::Off).then_some(i);
    }
    let mut best: Option<(usize, usize)> = None;
    for (i, v) in views.iter().enumerate() {
        if v.scope == Scope::Tree && within(folder, &v.path) {
            let len = norm(&v.path).len();
            if best.is_none_or(|(b, _)| len > b) {
                best = Some((len, i));
            }
        }
    }
    best.map(|(_, i)| i)
}

/// Explorer's natural order ("img9" before "img10"), case-insensitive.
pub fn natural(a: &str, b: &str) -> Ordering {
    unsafe { StrCmpLogicalW(&HSTRING::from(a), &HSTRING::from(b)) }.cmp(&0)
}

/// Drives by letter, always first; folders grouped by `group`; then the sort field (folders have no size, so
/// they fall back to name), natural name order breaking ties. Descending flips the sort, not the grouping.
fn compare(a: &Row, b: &Row, sort: Sort, group: Group) -> Ordering {
    if a.kind == Kind::Drive || b.kind == Kind::Drive {
        return match (a.kind == Kind::Drive, b.kind == Kind::Drive) {
            (true, true) => a.path.cmp(&b.path),
            (true, false) => Ordering::Less,
            _ => Ordering::Greater,
        };
    }
    if a.is_folder() != b.is_folder() {
        match group {
            Group::FoldersFirst => return if a.is_folder() { Ordering::Less } else { Ordering::Greater },
            Group::FilesFirst => return if a.is_folder() { Ordering::Greater } else { Ordering::Less },
            Group::Mixed => {}
        }
    }
    // Every key must order all rows alike, or the sort aborts ("does not implement a total order"): by size a
    // folder counts as 0 against files too (it once compared by name against files and by size between files,
    // which Mixed turned into a cycle and a crash, 2026-11-09). Ties go by name, then by the whole path.
    let size = |r: &Row| if r.is_folder() { 0 } else { r.size };
    let c = match sort.field {
        SortField::Date => a.stamp.cmp(&b.stamp),
        SortField::Size => size(a).cmp(&size(b)),
        SortField::Name => Ordering::Equal,
    }
    .then_with(|| unsafe { StrCmpLogicalW(&a.name_h, &b.name_h) }.cmp(&0))
    .then_with(|| a.path.cmp(&b.path));
    if sort.descending { c.reverse() } else { c }
}

pub fn sort_rows(rows: &mut [Row], sort: Sort, group: Group) {
    rows.sort_by(|a, b| compare(a, b, sort, group));
}

/// The filter's words: lower-cased, split on spaces.
pub fn filter_words(text: &str) -> Vec<String> {
    text.split_whitespace().map(str::to_lowercase).collect()
}

/// Every word appears in the row's name (Transient's filter: several words, all must match).
pub fn matches(row: &Row, words: &[String]) -> bool {
    if words.is_empty() {
        return true;
    }
    let name = row.name.to_lowercase();
    words.iter().all(|w| name.contains(w.as_str()))
}

/// The breadcrumb trail: "This PC", then every ancestor from the drive root down to the place. A collection
/// is a trail of its own.
pub fn crumbs(place: &Place) -> Vec<Place> {
    if let Place::Collection(_) = place {
        return vec![place.clone()];
    }
    let mut out = vec![Place::Computer];
    if let Place::Folder(p) = place {
        let mut chain: Vec<PathBuf> = p.ancestors().map(Path::to_path_buf).filter(|a| !a.as_os_str().is_empty()).collect();
        chain.reverse();
        out.extend(chain.into_iter().map(Place::Folder));
    }
    out
}

/// Every place listed, oldest first; Back and Forward walk it like a browser's.
#[derive(Default)]
pub struct History {
    places: Vec<Place>,
    at: Option<usize>,
}

impl History {
    /// A new place was listed: drops the Forward entries. Re-listing the current place is not a move.
    pub fn record(&mut self, place: &Place) {
        if let Some(i) = self.at {
            if self.places[i].same(place) {
                return;
            }
            self.places.truncate(i + 1);
        }
        self.places.push(place.clone());
        self.at = Some(self.places.len() - 1);
    }
    pub fn can_back(&self) -> bool {
        self.at.is_some_and(|i| i > 0)
    }
    pub fn can_forward(&self) -> bool {
        self.at.is_some_and(|i| i + 1 < self.places.len())
    }
    pub fn back(&mut self) -> Option<Place> {
        let i = self.at.filter(|&i| i > 0)? - 1;
        self.at = Some(i);
        Some(self.places[i].clone())
    }
    pub fn forward(&mut self) -> Option<Place> {
        let i = self.at.filter(|&i| i + 1 < self.places.len())? + 1;
        self.at = Some(i);
        Some(self.places[i].clone())
    }
    /// The whole trail, oldest first, and where in it we are (the History drop-down).
    pub fn entries(&self) -> (&[Place], Option<usize>) {
        (&self.places, self.at)
    }
    /// Jumps to entry `i` (a pick in the History drop-down).
    pub fn jump(&mut self, i: usize) -> Option<Place> {
        let p = self.places.get(i)?.clone();
        self.at = Some(i);
        Some(p)
    }
    /// A place was renamed or moved: entries at or under `from` follow it.
    pub fn moved(&mut self, from: &Path, to: &Path) {
        for p in &mut self.places {
            if let Place::Folder(f) = p {
                if let Some(n) = rebase(f, from, to) {
                    *f = n;
                }
            }
        }
    }
}

/// `p` with its `from` prefix swapped for `to`, when `p` is `from` or under it.
pub fn rebase(p: &Path, from: &Path, to: &Path) -> Option<PathBuf> {
    if !within(p, from) {
        return None;
    }
    let rest = &p.to_string_lossy()[from.to_string_lossy().trim_end_matches(['\\', '/']).len()..];
    Some(PathBuf::from(format!("{}{}", to.to_string_lossy().trim_end_matches(['\\', '/']), rest)))
}

/// A free name for `name` in `folder`: `name`, else "name (2).ext", "name (3).ext"…
pub fn free_name(folder: &Path, name: &str) -> PathBuf {
    let first = folder.join(name);
    if std::fs::symlink_metadata(&first).is_err() {
        return first;
    }
    let p = Path::new(name);
    let (stem, ext) = match (p.file_stem(), p.extension()) {
        (Some(s), Some(e)) if !s.is_empty() => (s.to_string_lossy().into_owned(), format!(".{}", e.to_string_lossy())),
        _ => (name.to_string(), String::new()),
    };
    (2..10_000).map(|n| folder.join(format!("{stem} ({n}){ext}"))).find(|c| std::fs::symlink_metadata(c).is_err()).unwrap_or(first)
}

// --- Disk ----------------------------------------------------------------------------------------------

fn row_for(path: PathBuf, md: &std::fs::Metadata) -> Row {
    let attrs = md.file_attributes();
    let name = folder_name(&path);
    let kind = if md.is_dir() {
        Kind::Folder
    } else if is_supported(&path) {
        Kind::Image
    } else {
        Kind::Other
    };
    Row {
        name_h: HSTRING::from(name.as_str()),
        name,
        kind,
        stamp: md.last_write_time(),
        created: md.creation_time(),
        size: if md.is_dir() { 0 } else { md.len() },
        cloud: attrs & (FILE_ATTRIBUTE_OFFLINE | FILE_ATTRIBUTE_RECALL_ON_OPEN | FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS) != 0,
        path,
    }
}

/// Lists a place, sorted. A folder that can't be read lists as empty; a collection lists the `tagged` paths
/// that still exist.
/// A place's rows, sorted. `deep` (Include subfolders) adds the files of every folder under a folder, its
/// own subfolders still listed but theirs not; links (junctions) aren't followed, and it stops at
/// [`DEEP_CAP`] rows.
pub fn list(place: &Place, sort: Sort, group: Group, tagged: &[PathBuf], deep: bool) -> Vec<Row> {
    let mut rows = match place {
        Place::Computer => drives(),
        Place::Folder(dir) => list_folder(dir, deep),
        Place::Collection(_) => tagged.iter().filter_map(|p| std::fs::metadata(p).ok().map(|md| row_for(p.clone(), &md))).collect(),
    };
    sort_rows(&mut rows, sort, group);
    rows
}

pub const DEEP_CAP: usize = 100_000;
const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;

fn list_folder(dir: &Path, deep: bool) -> Vec<Row> {
    let mut rows = Vec::new();
    let mut stack = vec![(dir.to_path_buf(), true)];
    while let Some((d, top)) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&d) else { continue };
        for e in rd.flatten() {
            let Ok(md) = e.metadata() else { continue };
            let attrs = md.file_attributes();
            if attrs & (FILE_ATTRIBUTE_HIDDEN | FILE_ATTRIBUTE_SYSTEM) != 0 {
                continue; // desktop.ini, Thumbs.db, $RECYCLE.BIN: Explorer hides them too
            }
            if md.is_dir() {
                if deep && attrs & FILE_ATTRIBUTE_REPARSE_POINT == 0 {
                    stack.push((e.path(), false));
                }
                if !top {
                    continue;
                }
            }
            rows.push(row_for(e.path(), &md));
            if rows.len() >= DEEP_CAP {
                return rows;
            }
        }
    }
    rows
}

/// A folder's visible subfolders, in natural order (a tree's children, a crumb's menu).
pub fn subfolders(dir: &Path) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = match std::fs::read_dir(dir) {
        Ok(rd) => rd
            .flatten()
            .filter(|e| e.metadata().is_ok_and(|m| m.is_dir() && m.file_attributes() & (FILE_ATTRIBUTE_HIDDEN | FILE_ATTRIBUTE_SYSTEM) == 0))
            .map(|e| e.path())
            .collect(),
        Err(_) => Vec::new(),
    };
    out.sort_by(|a, b| natural(&folder_name(a), &folder_name(b)));
    out
}

/// A folder's visible subfolders, then the files Looker can open, each in natural order (the crumb popup).
pub fn entries(dir: &Path) -> (Vec<PathBuf>, Vec<PathBuf>) {
    let (mut folders, mut files) = (Vec::new(), Vec::new());
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            let Ok(md) = e.metadata() else { continue };
            if md.file_attributes() & (FILE_ATTRIBUTE_HIDDEN | FILE_ATTRIBUTE_SYSTEM) != 0 {
                continue;
            }
            let p = e.path();
            if md.is_dir() {
                folders.push(p);
            } else if is_supported(&p) {
                files.push(p);
            }
        }
    }
    folders.sort_by(|a, b| natural(&folder_name(a), &folder_name(b)));
    files.sort_by(|a, b| natural(&folder_name(a), &folder_name(b)));
    (folders, files)
}

/// The popup's typed text split into the folder to list (up to and with the last separator) and what's
/// typed after it, which filters that folder's entries.
pub fn split_typed(text: &str) -> (String, String) {
    match text.rfind(['\\', '/']) {
        Some(i) => (text[..=i].to_string(), text[i + 1..].to_string()),
        None => (String::new(), text.to_string()),
    }
}

/// "Local Disk (C:)" and friends, by letter. Not-ready drives (an empty card reader) keep the generic label.
pub fn drives() -> Vec<Row> {
    let mask = unsafe { GetLogicalDrives() };
    let mut rows = Vec::new();
    for i in 0..26u32 {
        if mask & (1 << i) == 0 {
            continue;
        }
        let letter = (b'A' + i as u8) as char;
        let root = format!("{letter}:\\");
        let root_h = HSTRING::from(root.as_str());
        let kind = unsafe { GetDriveTypeW(&root_h) };
        let mut label = [0u16; 261];
        let ok = unsafe { GetVolumeInformationW(&root_h, Some(&mut label), None, None, None, None).is_ok() };
        let label = if ok { String::from_utf16_lossy(&label[..label.iter().position(|&c| c == 0).unwrap_or(0)]) } else { String::new() };
        let label = if label.is_empty() { if kind == DRIVE_REMOVABLE { "Removable Disk".into() } else { "Local Disk".into() } } else { label };
        let name = format!("{label} ({letter}:)");
        rows.push(Row { name_h: HSTRING::from(name.as_str()), name, path: PathBuf::from(root), kind: Kind::Drive, stamp: 0, created: 0, size: 0, cloud: false });
    }
    rows
}

/// The user's own folders for the sidebar's Places: name, path, glyph.
pub fn known_places() -> Vec<(String, PathBuf, u16)> {
    use windows::Win32::UI::Shell::{FOLDERID_Desktop, FOLDERID_Documents, FOLDERID_Downloads, FOLDERID_Pictures, FOLDERID_Videos, KF_FLAG_DEFAULT, SHGetKnownFolderPath};
    let mut out = Vec::new();
    for (name, id, glyph) in [
        ("Desktop", FOLDERID_Desktop, 0xE7F4u16),
        ("Downloads", FOLDERID_Downloads, 0xE896),
        ("Documents", FOLDERID_Documents, 0xE8A5),
        ("Pictures", FOLDERID_Pictures, 0xE91B),
        ("Videos", FOLDERID_Videos, 0xE714),
    ] {
        unsafe {
            if let Ok(p) = SHGetKnownFolderPath(&id, KF_FLAG_DEFAULT, None) {
                let s = p.to_string().unwrap_or_default();
                windows::Win32::System::Com::CoTaskMemFree(Some(p.0 as _));
                if !s.is_empty() {
                    out.push((name.to_string(), PathBuf::from(s), glyph));
                }
            }
        }
    }
    out
}

/// An image's pixel size from its header (WIC reads only that), for the Dimensions column. COM must be up
/// on the calling thread.
pub fn dimensions(f: &windows::Win32::Graphics::Imaging::IWICImagingFactory, path: &Path) -> Option<(u32, u32)> {
    use windows::Win32::Graphics::Imaging::WICDecodeMetadataCacheOnDemand;
    use windows::Win32::Foundation::GENERIC_READ;
    unsafe {
        let d = f.CreateDecoderFromFilename(&HSTRING::from(path.as_os_str()), None, GENERIC_READ, WICDecodeMetadataCacheOnDemand).ok()?;
        let frame = d.GetFrame(0).ok()?;
        let (mut w, mut h) = (0u32, 0u32);
        frame.GetSize(&mut w, &mut h).ok()?;
        (w > 0 && h > 0).then_some((w, h))
    }
}

/// Posts `msg` whenever anything in one folder changes: files
/// or folders appearing, vanishing, renamed, rewritten. Not recursive. Stops when dropped.
pub struct Watcher {
    stop: HANDLE,
    thread: Option<std::thread::JoinHandle<()>>,
}

unsafe impl Send for Watcher {}

impl Watcher {
    /// Watches `folder` (and everything under it with `subtree`), posting `msg` on each change.
    pub fn start(folder: &Path, hwnd: HWND, msg: u32, subtree: bool) -> Option<Watcher> {
        let filter = FILE_NOTIFY_CHANGE(
            FILE_NOTIFY_CHANGE_FILE_NAME.0 | FILE_NOTIFY_CHANGE_DIR_NAME.0 | FILE_NOTIFY_CHANGE_LAST_WRITE.0 | FILE_NOTIFY_CHANGE_SIZE.0,
        );
        let change = unsafe { FindFirstChangeNotificationW(&HSTRING::from(folder.as_os_str()), subtree, filter).ok()? };
        let Ok(stop) = (unsafe { CreateEventW(None, true, false, PCWSTR::null()) }) else {
            unsafe {
                let _ = FindCloseChangeNotification(change);
            }
            return None;
        };
        let (change_raw, stop_raw, hwnd_raw) = (change.0 as isize, stop.0 as isize, hwnd.0 as isize);
        let thread = std::thread::Builder::new()
            .name("watch".into())
            .spawn(move || unsafe {
                let handles = [HANDLE(change_raw as _), HANDLE(stop_raw as _)];
                loop {
                    let r = WaitForMultipleObjects(&handles, false, INFINITE);
                    if r != WAIT_OBJECT_0 {
                        break; // stop, or an error
                    }
                    let _ = PostMessageW(Some(HWND(hwnd_raw as _)), msg, WPARAM(0), LPARAM(0));
                    if FindNextChangeNotification(handles[0]).is_err() {
                        break;
                    }
                }
                let _ = FindCloseChangeNotification(handles[0]);
            })
            .ok()?;
        Some(Watcher { stop, thread: Some(thread) })
    }
}

impl Drop for Watcher {
    fn drop(&mut self) {
        unsafe {
            let _ = SetEvent(self.stop);
        }
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
        unsafe {
            let _ = CloseHandle(self.stop);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(rows: &[Row]) -> Vec<&str> {
        rows.iter().map(|r| r.name.as_str()).collect()
    }

    fn sample() -> Vec<Row> {
        vec![
            Row::test("b.jpg", Kind::Image, 1, 30),
            Row::test("Zebra", Kind::Folder, 5, 0),
            Row::test("a10.jpg", Kind::Image, 3, 10),
            Row::test("a9.txt", Kind::Other, 2, 20),
            Row::test("Apple", Kind::Folder, 9, 0),
        ]
    }

    #[test]
    fn folders_first_in_either_direction() {
        let mut rows = sample();
        sort_rows(&mut rows, Sort::default(), Group::FoldersFirst);
        assert_eq!(names(&rows), ["Apple", "Zebra", "a9.txt", "a10.jpg", "b.jpg"]);
        sort_rows(&mut rows, Sort { field: SortField::Size, descending: true }, Group::FoldersFirst);
        // Folders have no size: they fall back to name (reversed with the direction), still first.
        assert_eq!(names(&rows), ["Zebra", "Apple", "b.jpg", "a9.txt", "a10.jpg"]);
    }

    #[test]
    fn files_first_and_mixed() {
        let mut rows = sample();
        sort_rows(&mut rows, Sort::default(), Group::FilesFirst);
        assert_eq!(names(&rows), ["a9.txt", "a10.jpg", "b.jpg", "Apple", "Zebra"]);
        sort_rows(&mut rows, Sort { field: SortField::Date, descending: false }, Group::Mixed);
        assert_eq!(names(&rows), ["b.jpg", "a9.txt", "a10.jpg", "Zebra", "Apple"]);
    }

    #[test]
    fn typed_paths_split_at_the_last_separator() {
        assert_eq!(split_typed(r"F:\TMC Dropbox\fa"), (r"F:\TMC Dropbox\".to_string(), "fa".to_string()));
        assert_eq!(split_typed(r"F:\"), (r"F:\".to_string(), String::new()));
        assert_eq!(split_typed("loo"), (String::new(), "loo".to_string()));
    }

    #[test]
    fn columns_round_trip() {
        let cols = vec![(ColKind::Size, 90.0), (ColKind::Name, 0.0), (ColKind::Taken, 140.0)];
        assert_eq!(ColKind::parse_list(&ColKind::format(&cols)), cols);
        assert_eq!(ColKind::parse_list("size:90,bogus:5,size:4"), vec![(ColKind::Name, 0.0), (ColKind::Size, 90.0)]);
        assert_eq!(ColKind::parse_list(&ColKind::format(&ColKind::defaults())), ColKind::defaults());
    }

    #[test]
    fn the_filter_needs_every_word() {
        let r = Row::test("Beach Sunset 04.jpg", Kind::Image, 0, 0);
        assert!(matches(&r, &filter_words("sun beach")));
        assert!(!matches(&r, &filter_words("sun dog")));
        assert!(matches(&r, &[]));
    }

    #[test]
    fn crumbs_run_from_this_pc_to_the_folder() {
        let c = crumbs(&Place::Folder(PathBuf::from(r"C:\Users\me\Pictures")));
        let n: Vec<String> = c.iter().map(Place::name).collect();
        assert_eq!(n, ["This PC", r"C:\", "Users", "me", "Pictures"]);
        assert_eq!(crumbs(&Place::Computer), [Place::Computer]);
        assert_eq!(crumbs(&Place::Collection(Collection::Favourites)).len(), 1);
    }

    #[test]
    fn history_walks_like_a_browser() {
        let a = Place::Folder(PathBuf::from(r"C:\a"));
        let b = Place::Folder(PathBuf::from(r"C:\b"));
        let c = Place::Folder(PathBuf::from(r"C:\c"));
        let mut h = History::default();
        h.record(&a);
        h.record(&b);
        h.record(&Place::Folder(PathBuf::from(r"c:\B\"))); // the same place again: not a move
        assert!(h.can_back() && !h.can_forward());
        assert_eq!(h.back(), Some(a.clone()));
        assert!(h.can_forward());
        h.record(&c); // a new branch drops Forward
        assert!(!h.can_forward());
        assert_eq!(h.back(), Some(a.clone()));
        assert_eq!(h.jump(1), Some(c));
        h.moved(Path::new(r"C:\a"), Path::new(r"C:\z"));
        assert_eq!(h.entries().0[0], Place::Folder(PathBuf::from(r"C:\z")));
    }

    #[test]
    fn paths_rebase_and_nest() {
        assert!(within(Path::new(r"C:\a\b"), Path::new(r"c:\A")));
        assert!(!within(Path::new(r"C:\ab"), Path::new(r"C:\a")));
        assert_eq!(rebase(Path::new(r"C:\a\b\c.jpg"), Path::new(r"C:\a"), Path::new(r"D:\x")), Some(PathBuf::from(r"D:\x\b\c.jpg")));
        assert_eq!(Place::Folder(PathBuf::from(r"C:\")).parent(), Some(Place::Computer));
    }

    #[test]
    fn free_names_count_up() {
        let dir = std::env::temp_dir().join(format!("looker-free-name-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        std::fs::write(dir.join("a.jpg"), b"x").unwrap();
        std::fs::write(dir.join("a (2).jpg"), b"x").unwrap();
        assert_eq!(free_name(&dir, "a.jpg"), dir.join("a (3).jpg"));
        assert_eq!(free_name(&dir, "b.jpg"), dir.join("b.jpg"));
        std::fs::create_dir(dir.join("New folder")).unwrap();
        assert_eq!(free_name(&dir, "New folder"), dir.join("New folder (2)"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn saved_views_cover_their_folders() {
        let sv = |p: &str, scope| SavedView { path: PathBuf::from(p), scope, view: View::default() };
        let views = vec![sv(r"C:\Photos", Scope::Tree), sv(r"C:\Photos\2026", Scope::Folder), sv(r"C:\Photos\Raw", Scope::Off), sv(r"C:\Photos\2026\May", Scope::Tree)];
        assert_eq!(saved_for(&views, Path::new(r"c:\photos")), Some(0));
        assert_eq!(saved_for(&views, Path::new(r"C:\Photos\Trips\Rome")), Some(0));
        assert_eq!(saved_for(&views, Path::new(r"C:\Photos\2026")), Some(1));
        // "This folder" doesn't reach its subfolders: the nearest "and subfolders" above does.
        assert_eq!(saved_for(&views, Path::new(r"C:\Photos\2026\June")), Some(0));
        assert_eq!(saved_for(&views, Path::new(r"C:\Photos\2026\May\1")), Some(3));
        assert_eq!(saved_for(&views, Path::new(r"C:\Photos\Raw")), None);
        assert_eq!(saved_for(&views, Path::new(r"C:\Photoshop")), None);
        let one = &views[3];
        assert_eq!(SavedView::parse(&one.format()).as_ref(), Some(one));
    }

    #[test]
    fn every_sort_is_a_total_order() {
        // Mixed by size once ran in a circle: small "z" file < big "a" file (size) < folder "m" (name) < "z" (name).
        let rows = [
            Row::test("z.jpg", Kind::Image, 3, 1),
            Row::test("m", Kind::Folder, 2, 0),
            Row::test("a.jpg", Kind::Image, 1, 2),
            Row::test("b", Kind::Folder, 5, 0),
            Row::test("c.txt", Kind::Other, 4, 2),
        ];
        for field in [SortField::Name, SortField::Date, SortField::Size] {
            for descending in [false, true] {
                for group in Group::ALL {
                    let sort = Sort { field, descending };
                    let cmp = |x: &Row, y: &Row| compare(x, y, sort, group);
                    for x in &rows {
                        for y in &rows {
                            assert_eq!(cmp(x, y), cmp(y, x).reverse(), "{field:?} {group:?}: antisymmetry");
                            for z in &rows {
                                if cmp(x, y) != Ordering::Greater && cmp(y, z) != Ordering::Greater {
                                    assert_ne!(cmp(x, z), Ordering::Greater, "{field:?} {descending} {group:?}: {} {} {}", x.name, y.name, z.name);
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}
