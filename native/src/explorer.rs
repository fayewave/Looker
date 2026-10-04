//! The file explorer card's model (port of `Navigation/ExplorerTree.cs` as the C# control uses it: one folder,
//! listed flat). A *place* is a folder or the virtual "This PC" above the drives. Rows are its sub-folders
//! (always first), then its files under the toolbar sort, with Explorer's natural name order as the tiebreak;
//! files Looker can't open are listed too (greyed out by the card). Browser-style history records every place
//! listed. Disk access is confined to [`list`] and the [`Watcher`]; the rest is pure and unit tested.

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

const FILE_ATTRIBUTE_HIDDEN: u32 = 0x2;
const FILE_ATTRIBUTE_SYSTEM: u32 = 0x4;
const FILE_ATTRIBUTE_OFFLINE: u32 = 0x1000;
const FILE_ATTRIBUTE_RECALL_ON_OPEN: u32 = 0x40000;
const FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS: u32 = 0x400000;
const DRIVE_REMOVABLE: u32 = 2;

pub const COMPUTER_NAME: &str = "This PC";

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Place {
    Computer,
    Folder(PathBuf),
}

impl Place {
    pub fn name(&self) -> String {
        match self {
            Place::Computer => COMPUTER_NAME.into(),
            Place::Folder(p) => folder_name(p),
        }
    }

    pub fn same(&self, other: &Place) -> bool {
        match (self, other) {
            (Place::Computer, Place::Computer) => true,
            (Place::Folder(a), Place::Folder(b)) => same_path(a, b),
            _ => false,
        }
    }
}

/// A folder's display name: its last segment, or "C:\" for a drive root.
pub fn folder_name(p: &Path) -> String {
    p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| p.to_string_lossy().into_owned())
}

/// Case-insensitive and blind to a trailing separator, like Windows paths.
pub fn same_path(a: &Path, b: &Path) -> bool {
    let norm = |p: &Path| p.to_string_lossy().trim_end_matches(['\\', '/']).to_lowercase();
    norm(a) == norm(b)
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
    pub stamp: u64,
    pub size: u64,
    pub cloud: bool,
}

impl Row {
    pub fn is_folder(&self) -> bool {
        matches!(self.kind, Kind::Drive | Kind::Folder)
    }

    #[cfg(test)]
    fn test(name: &str, kind: Kind, stamp: u64, size: u64) -> Row {
        Row { path: PathBuf::from(name), name: name.into(), name_h: HSTRING::from(name), kind, stamp, size, cloud: false }
    }
}

/// Folders first in either direction; drives by letter; then the sort field (folders have no size, so they
/// fall back to name), natural name order breaking ties; descending flips everything but the folders-first.
fn compare(a: &Row, b: &Row, sort: Sort) -> Ordering {
    if a.is_folder() != b.is_folder() {
        return if a.is_folder() { Ordering::Less } else { Ordering::Greater };
    }
    if a.kind == Kind::Drive && b.kind == Kind::Drive {
        return a.path.cmp(&b.path);
    }
    let c = match sort.field {
        SortField::Date => a.stamp.cmp(&b.stamp),
        SortField::Size if !a.is_folder() => a.size.cmp(&b.size),
        _ => Ordering::Equal,
    }
    .then_with(|| unsafe { StrCmpLogicalW(&a.name_h, &b.name_h) }.cmp(&0));
    if sort.descending { c.reverse() } else { c }
}

pub fn sort_rows(rows: &mut [Row], sort: Sort) {
    rows.sort_by(|a, b| compare(a, b, sort));
}

/// The breadcrumb trail: "This PC", then every ancestor from the drive root down to the place.
pub fn crumbs(place: &Place) -> Vec<Place> {
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
}

// --- Disk ----------------------------------------------------------------------------------------------

/// Lists a place, sorted. A folder that can't be read lists as empty.
pub fn list(place: &Place, sort: Sort) -> Vec<Row> {
    let mut rows = match place {
        Place::Computer => drives(),
        Place::Folder(dir) => {
            let mut rows = Vec::new();
            if let Ok(rd) = std::fs::read_dir(dir) {
                for e in rd.flatten() {
                    let Ok(md) = e.metadata() else { continue };
                    let attrs = md.file_attributes();
                    if attrs & (FILE_ATTRIBUTE_HIDDEN | FILE_ATTRIBUTE_SYSTEM) != 0 {
                        continue; // desktop.ini, Thumbs.db, $RECYCLE.BIN: Explorer hides them too
                    }
                    let path = e.path();
                    let name = e.file_name().to_string_lossy().into_owned();
                    let kind = if md.is_dir() {
                        Kind::Folder
                    } else if is_supported(&path) {
                        Kind::Image
                    } else {
                        Kind::Other
                    };
                    rows.push(Row {
                        name_h: HSTRING::from(name.as_str()),
                        name,
                        kind,
                        stamp: md.last_write_time(),
                        size: if md.is_dir() { 0 } else { md.len() },
                        cloud: attrs & (FILE_ATTRIBUTE_OFFLINE | FILE_ATTRIBUTE_RECALL_ON_OPEN | FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS) != 0,
                        path,
                    });
                }
            }
            rows
        }
    };
    sort_rows(&mut rows, sort);
    rows
}

/// "Local Disk (C:)" and friends, by letter. Not-ready drives (an empty card reader) keep the generic label.
fn drives() -> Vec<Row> {
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
        rows.push(Row { name_h: HSTRING::from(name.as_str()), name, path: PathBuf::from(root), kind: Kind::Drive, stamp: 0, size: 0, cloud: false });
    }
    rows
}

/// Posts `msg` whenever anything in one folder changes: files
/// or folders appearing, vanishing, renamed, rewritten. Not recursive. Stops when dropped.
pub struct Watcher {
    stop: HANDLE,
    thread: Option<std::thread::JoinHandle<()>>,
}

unsafe impl Send for Watcher {}

impl Watcher {
    pub fn start(folder: &Path, hwnd: HWND, msg: u32) -> Option<Watcher> {
        let filter = FILE_NOTIFY_CHANGE(
            FILE_NOTIFY_CHANGE_FILE_NAME.0 | FILE_NOTIFY_CHANGE_DIR_NAME.0 | FILE_NOTIFY_CHANGE_LAST_WRITE.0 | FILE_NOTIFY_CHANGE_SIZE.0,
        );
        let change = unsafe { FindFirstChangeNotificationW(&HSTRING::from(folder.as_os_str()), false, filter).ok()? };
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

    #[test]
    fn folders_come_first_in_either_direction() {
        let mut rows = vec![
            Row::test("b.jpg", Kind::Image, 1, 30),
            Row::test("Zebra", Kind::Folder, 5, 0),
            Row::test("a10.jpg", Kind::Image, 3, 10),
            Row::test("a9.txt", Kind::Other, 2, 20),
            Row::test("Apple", Kind::Folder, 9, 0),
        ];
        sort_rows(&mut rows, Sort::default());
        assert_eq!(names(&rows), ["Apple", "Zebra", "a9.txt", "a10.jpg", "b.jpg"]);
        sort_rows(&mut rows, Sort { field: SortField::Size, descending: true });
        // Folders have no size: they fall back to name (reversed with the direction), still first.
        assert_eq!(names(&rows), ["Zebra", "Apple", "b.jpg", "a9.txt", "a10.jpg"]);
    }

    #[test]
    fn crumbs_run_from_this_pc_to_the_folder() {
        let c = crumbs(&Place::Folder(PathBuf::from(r"C:\Users\me\Pictures")));
        let n: Vec<String> = c.iter().map(Place::name).collect();
        assert_eq!(n, ["This PC", r"C:\", "Users", "me", "Pictures"]);
        assert_eq!(crumbs(&Place::Computer), [Place::Computer]);
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
        assert_eq!(h.back(), Some(a));
    }
}
