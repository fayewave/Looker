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

const FILE_ATTRIBUTE_READONLY: u32 = 0x1;
const FILE_ATTRIBUTE_HIDDEN: u32 = 0x2;
const FILE_ATTRIBUTE_SYSTEM: u32 = 0x4;
const FILE_ATTRIBUTE_OFFLINE: u32 = 0x1000;
const FILE_ATTRIBUTE_RECALL_ON_OPEN: u32 = 0x40000;
const FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS: u32 = 0x400000;
const DRIVE_REMOVABLE: u32 = 2;
const DRIVE_REMOTE: u32 = 4;

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
            // A share's root goes up to its computer, the computer to This PC.
            Place::Folder(p) if is_server(p) => Some(Place::Computer),
            Place::Folder(p) => Some(match p.parent() {
                Some(q) => Place::Folder(q.to_path_buf()),
                None => unc_server(p).map_or(Place::Computer, Place::Folder),
            }),
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
    /// A folder Windows marks as customised (read-only, as Desktop, Downloads and any folder with a
    /// `desktop.ini` are) and not a cloud placeholder: its icon is looked up by path (`shell_icons`).
    pub custom_icon: bool,
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
        Row { path: PathBuf::from(name), name: name.into(), name_h: HSTRING::from(name), kind, stamp, created: 0, size, cloud: false, custom_icon: false }
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
    Width,
    Height,
    AspectRatio,
    Make,
    Aperture,
    Exposure,
    Iso,
    FocalLength,
    ExposureBias,
    Flash,
    Metering,
    WhiteBalance,
    Gps,
    Altitude,
    Duration,
    FrameRate,
    VideoCodec,
    VideoFormat,
    Colour,
    BitRate,
    AudioCodec,
    Channels,
    SampleRate,
    BitDepth,
    Title,
    Artist,
    Album,
    AlbumArtist,
    Genre,
    Year,
    Track,
    Composer,
    Bpm,
    Key,
}

impl ColKind {
    pub const ALL: [ColKind; 47] = [
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
        ColKind::Width,
        ColKind::Height,
        ColKind::AspectRatio,
        ColKind::Make,
        ColKind::Aperture,
        ColKind::Exposure,
        ColKind::Iso,
        ColKind::FocalLength,
        ColKind::ExposureBias,
        ColKind::Flash,
        ColKind::Metering,
        ColKind::WhiteBalance,
        ColKind::Gps,
        ColKind::Altitude,
        ColKind::Duration,
        ColKind::FrameRate,
        ColKind::VideoCodec,
        ColKind::VideoFormat,
        ColKind::Colour,
        ColKind::BitRate,
        ColKind::AudioCodec,
        ColKind::Channels,
        ColKind::SampleRate,
        ColKind::BitDepth,
        ColKind::Title,
        ColKind::Artist,
        ColKind::Album,
        ColKind::AlbumArtist,
        ColKind::Genre,
        ColKind::Year,
        ColKind::Track,
        ColKind::Composer,
        ColKind::Bpm,
        ColKind::Key,
    ];

    /// The header menu's categories (Transient's: each a submenu), each with its columns.
    pub const GROUPS: [(&'static str, &'static [ColKind]); 8] = [
        ("Standard", &[ColKind::Name, ColKind::Extension, ColKind::Type, ColKind::Size, ColKind::Parent, ColKind::Path]),
        ("Date and Time", &[ColKind::Modified, ColKind::Created, ColKind::Taken]),
        ("Image", &[ColKind::Dimensions, ColKind::Width, ColKind::Height, ColKind::Megapixels, ColKind::AspectRatio]),
        (
            "Camera",
            &[
                ColKind::Camera,
                ColKind::Make,
                ColKind::Lens,
                ColKind::Aperture,
                ColKind::Exposure,
                ColKind::Iso,
                ColKind::FocalLength,
                ColKind::ExposureBias,
                ColKind::Flash,
                ColKind::Metering,
                ColKind::WhiteBalance,
            ],
        ),
        ("Location", &[ColKind::Gps, ColKind::Altitude]),
        ("Video", &[ColKind::Duration, ColKind::FrameRate, ColKind::VideoCodec, ColKind::VideoFormat, ColKind::Colour, ColKind::BitRate]),
        ("Audio", &[ColKind::AudioCodec, ColKind::Channels, ColKind::SampleRate, ColKind::BitDepth]),
        (
            "Music",
            &[ColKind::Title, ColKind::Artist, ColKind::Album, ColKind::AlbumArtist, ColKind::Genre, ColKind::Year, ColKind::Track, ColKind::Composer, ColKind::Bpm, ColKind::Key],
        ),
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
            ColKind::Width => "Width",
            ColKind::Height => "Height",
            ColKind::AspectRatio => "Aspect ratio",
            ColKind::Make => "Camera maker",
            ColKind::Aperture => "F-stop",
            ColKind::Exposure => "Exposure time",
            ColKind::Iso => "ISO speed",
            ColKind::FocalLength => "Focal length",
            ColKind::ExposureBias => "Exposure bias",
            ColKind::Flash => "Flash",
            ColKind::Metering => "Metering mode",
            ColKind::WhiteBalance => "White balance",
            ColKind::Gps => "Coordinates",
            ColKind::Altitude => "Altitude",
            ColKind::Duration => "Duration",
            ColKind::FrameRate => "Frame rate",
            ColKind::VideoCodec => "Video codec",
            ColKind::VideoFormat => "Video format",
            ColKind::Colour => "Colour",
            ColKind::BitRate => "Bit rate",
            ColKind::AudioCodec => "Audio codec",
            ColKind::Channels => "Channels",
            ColKind::SampleRate => "Sample rate",
            ColKind::BitDepth => "Bit depth",
            ColKind::Title => "Title",
            ColKind::Artist => "Artist",
            ColKind::Album => "Album",
            ColKind::AlbumArtist => "Album artist",
            ColKind::Genre => "Genre",
            ColKind::Year => "Year",
            ColKind::Track => "Track",
            ColKind::Composer => "Composer",
            ColKind::Bpm => "BPM",
            ColKind::Key => "Key",
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
            ColKind::Width => "width",
            ColKind::Height => "height",
            ColKind::AspectRatio => "aspect",
            ColKind::Make => "make",
            ColKind::Aperture => "aperture",
            ColKind::Exposure => "exposure",
            ColKind::Iso => "iso",
            ColKind::FocalLength => "focal",
            ColKind::ExposureBias => "bias",
            ColKind::Flash => "flash",
            ColKind::Metering => "metering",
            ColKind::WhiteBalance => "wb",
            ColKind::Gps => "gps",
            ColKind::Altitude => "altitude",
            ColKind::Duration => "duration",
            ColKind::FrameRate => "fps",
            ColKind::VideoCodec => "vcodec",
            ColKind::VideoFormat => "vformat",
            ColKind::Colour => "colour",
            ColKind::BitRate => "bitrate",
            ColKind::AudioCodec => "acodec",
            ColKind::Channels => "channels",
            ColKind::SampleRate => "rate",
            ColKind::BitDepth => "bits",
            ColKind::Title => "title",
            ColKind::Artist => "artist",
            ColKind::Album => "album",
            ColKind::AlbumArtist => "albumartist",
            ColKind::Genre => "genre",
            ColKind::Year => "year",
            ColKind::Track => "track",
            ColKind::Composer => "composer",
            ColKind::Bpm => "bpm",
            ColKind::Key => "key",
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
            ColKind::Width => 70.0,
            ColKind::Height => 70.0,
            ColKind::AspectRatio => 90.0,
            ColKind::Make => 120.0,
            ColKind::Aperture => 70.0,
            ColKind::Exposure => 100.0,
            ColKind::Iso => 76.0,
            ColKind::FocalLength => 90.0,
            ColKind::ExposureBias => 96.0,
            ColKind::Flash => 140.0,
            ColKind::Metering => 120.0,
            ColKind::WhiteBalance => 100.0,
            ColKind::Gps => 170.0,
            ColKind::Altitude => 96.0,
            ColKind::Duration => 90.0,
            ColKind::FrameRate => 84.0,
            ColKind::VideoCodec => 130.0,
            ColKind::VideoFormat => 110.0,
            ColKind::Colour => 140.0,
            ColKind::BitRate => 96.0,
            ColKind::AudioCodec => 110.0,
            ColKind::Channels => 90.0,
            ColKind::SampleRate => 90.0,
            ColKind::BitDepth => 76.0,
            ColKind::Title => 180.0,
            ColKind::Artist => 160.0,
            ColKind::Album => 160.0,
            ColKind::AlbumArtist => 160.0,
            ColKind::Genre => 110.0,
            ColKind::Year => 60.0,
            ColKind::Track => 60.0,
            ColKind::Composer => 140.0,
            ColKind::Bpm => 60.0,
            ColKind::Key => 60.0,
        }
    }

    /// Numbers sit at the right of their column.
    pub fn right_aligned(self) -> bool {
        matches!(self, ColKind::Size | ColKind::Megapixels | ColKind::Width | ColKind::Height | ColKind::Aperture | ColKind::Exposure | ColKind::Iso | ColKind::FocalLength | ColKind::ExposureBias | ColKind::Altitude | ColKind::Duration | ColKind::FrameRate | ColKind::BitRate | ColKind::SampleRate | ColKind::BitDepth | ColKind::Year | ColKind::Track | ColKind::Bpm)
    }

    /// Read from the file itself, off the UI thread, as rows come on screen.
    pub fn lazy(self) -> bool {
        !matches!(self, ColKind::Name | ColKind::Extension | ColKind::Type | ColKind::Size | ColKind::Parent | ColKind::Path | ColKind::Modified | ColKind::Created)
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
        // Dimensions isn't a default (Faye, 2026-11-10); it's one tick away in the header menu.
        [ColKind::Name, ColKind::Modified, ColKind::Size, ColKind::Type].into_iter().map(|c| (c, c.default_width())).collect()
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
    Network,
    History,
}

impl SideSection {
    /// History last (Faye, 2026-11-09); Network after Places, as Explorer's (2026-11-10).
    pub const DEFAULT: [SideSection; 5] = [SideSection::Bookmarks, SideSection::Collections, SideSection::Places, SideSection::Network, SideSection::History];

    fn key(self) -> &'static str {
        match self {
            SideSection::Bookmarks => "bookmarks",
            SideSection::Collections => "collections",
            SideSection::Places => "places",
            SideSection::Network => "network",
            SideSection::History => "history",
        }
    }

    /// The settings' list: the known sections in its order, once each. Any missing (one added since the list was
    /// saved) goes in after the section it follows by default, so Network lands under Places, not at the end.
    pub fn parse_list(s: &str) -> Vec<SideSection> {
        let mut out: Vec<SideSection> = Vec::new();
        for k in s.split(',') {
            if let Some(x) = SideSection::DEFAULT.into_iter().find(|x| x.key() == k.trim()) {
                if !out.contains(&x) {
                    out.push(x);
                }
            }
        }
        for (i, x) in SideSection::DEFAULT.into_iter().enumerate() {
            if !out.contains(&x) {
                let at = SideSection::DEFAULT[..i].iter().rev().find_map(|p| out.iter().position(|o| o == p)).map_or(0, |j| j + 1);
                out.insert(at, x);
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
/// subfolders' files are listed too, whether files Looker can't open show, how tall the rows are, the columns,
/// and the sort. The default is the settings'; any change in a folder is kept for that folder (`SavedView`;
/// Faye, 2026-11-11), and only Save as default changes the default.
#[derive(Clone, PartialEq, Debug)]
pub struct View {
    pub group: Group,
    pub subfolders: bool,
    pub other_files: bool,
    /// Row height in rows (1-4): taller rows, bigger thumbnails.
    pub height: u8,
    pub columns: Vec<(ColKind, f32)>,
    /// Name, date or size, as the toolbar sorts (the viewer steps through the folder in this order).
    pub sort: Sort,
    /// A column the list alone is sorted by instead (Camera, Dimensions, …), and descending.
    pub xsort: Option<(ColKind, bool)>,
}

pub const HEIGHT_MAX: u8 = 4;

impl Default for View {
    fn default() -> Self {
        View { group: Group::default(), subfolders: false, other_files: true, height: 1, columns: ColKind::defaults(), sort: Sort::default(), xsort: None }
    }
}

/// A column sort for the settings file: "camera desc", or "-" for none.
pub fn format_xsort(x: Option<(ColKind, bool)>) -> String {
    x.map_or("-".to_string(), |(c, d)| format!("{} {}", c.key(), if d { "desc" } else { "asc" }))
}

pub fn parse_xsort(s: &str) -> Option<(ColKind, bool)> {
    s.split_once(' ').and_then(|(c, d)| Some((ColKind::parse(c)?, d == "desc")))
}

/// The key a place's saved view is kept under: a folder's path, or a name for the places that aren't folders.
pub fn view_key(place: &Place) -> PathBuf {
    match place {
        Place::Folder(f) => f.clone(),
        Place::Computer => PathBuf::from("::computer"),
        Place::Collection(Collection::Favourites) => PathBuf::from("::favourites"),
        Place::Collection(Collection::Colour(c)) => PathBuf::from(format!("::colour{c}")),
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
    /// `scope;group;subfolders;other;height;columns;sort;xsort;path` (a `view2=` line; the path last: it may hold
    /// anything). `xsort` is `-` or "column asc|desc".
    pub fn format(&self) -> String {
        let v = &self.view;
        let xsort = format_xsort(v.xsort);
        format!(
            "{};{};{};{};{};{};{};{};{}",
            self.scope.key(),
            v.group.key(),
            v.subfolders as i32,
            v.other_files as i32,
            v.height,
            ColKind::format(&v.columns),
            v.sort.to_setting(),
            xsort,
            self.path.display()
        )
    }

    /// A `view2=` line (`format`).
    pub fn parse(s: &str) -> Option<SavedView> {
        let f: Vec<&str> = s.splitn(9, ';').collect();
        if f.len() != 9 || f[8].is_empty() {
            return None;
        }
        let head = f[..6].join(";");
        let mut sv = SavedView::parse_v1(&format!("{head};{}", f[8]), Sort::parse(f[6]).unwrap_or_default())?;
        sv.view.xsort = parse_xsort(f[7]);
        Some(sv)
    }

    /// A `view=` line from before views kept their sort: `scope;group;subfolders;other;height;columns;path`, its
    /// sort the default's then (`sort`).
    pub fn parse_v1(s: &str, sort: Sort) -> Option<SavedView> {
        let f: Vec<&str> = s.splitn(7, ';').collect();
        if f.len() != 7 || f[6].is_empty() {
            return None;
        }
        let scope = [Scope::Folder, Scope::Tree, Scope::Off].into_iter().find(|x| x.key() == f[0])?;
        let view = View {
            group: Group::parse(f[1]).unwrap_or_default(),
            subfolders: f[2] == "1",
            // Always on since 2026-11-10 (the toggle went): an old "off" reads as on.
            other_files: true,
            height: f[4].parse::<u8>().map_or(1, |h| h.clamp(1, HEIGHT_MAX)),
            columns: ColKind::parse_list(f[5]),
            sort,
            xsort: None,
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

/// A field the filter can search by name ("keyword:beach"), in what the lazy columns read from the files.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SearchField {
    Title,
    Description,
    Keywords,
    Rating,
    Author,
    Copyright,
    Label,
    Camera,
    Lens,
    Iso,
    Aperture,
    Exposure,
    Focal,
    Taken,
    Ext,
    Artist,
    Album,
    Genre,
}

/// The field a filter word names before its colon. (No one-letter names: "c:" and "f:" are drives.)
pub fn search_field(name: &str) -> Option<SearchField> {
    use SearchField::*;
    Some(match name {
        "title" => Title,
        "description" | "desc" | "caption" | "comment" => Description,
        "keyword" | "keywords" | "kw" => Keywords,
        "rating" | "stars" => Rating,
        "author" | "creator" | "by" => Author,
        "copyright" => Copyright,
        "label" => Label,
        "camera" | "make" | "model" => Camera,
        "lens" => Lens,
        "iso" => Iso,
        "aperture" | "fstop" => Aperture,
        "exposure" | "shutter" => Exposure,
        "focal" => Focal,
        "taken" | "date" | "year" => Taken,
        "ext" | "type" => Ext,
        "artist" => Artist,
        "album" => Album,
        "genre" => Genre,
        _ => return None,
    })
}

/// One word of the filter: in the name, or in a field (an empty value: the field is there at all).
#[derive(Clone, PartialEq, Debug)]
pub enum Term {
    Name(String),
    Field(SearchField, String),
}

/// The filter's words, lower-cased: split on spaces outside double quotes (`keyword:"new york"`); a word
/// whose part before its colon names a field searches that field.
pub fn filter_terms(text: &str) -> Vec<Term> {
    let mut out = Vec::new();
    let mut chars = text.chars().peekable();
    loop {
        while chars.peek().is_some_and(|c| c.is_whitespace()) {
            chars.next();
        }
        if chars.peek().is_none() {
            break;
        }
        let mut tok = String::new();
        let mut quoted = false;
        while let Some(&c) = chars.peek() {
            if c == '"' {
                quoted = !quoted;
            } else if c.is_whitespace() && !quoted {
                break;
            } else {
                tok.push(c);
            }
            chars.next();
        }
        let tok = tok.to_lowercase();
        if tok.is_empty() {
            continue;
        }
        match tok.split_once(':').and_then(|(f, v)| search_field(f).map(|f| (f, v))) {
            Some((f, v)) => out.push(Term::Field(f, v.trim().to_string())),
            None => out.push(Term::Name(tok)),
        }
    }
    out
}

/// Whether a field's values (lower-cased) satisfy a filter word: contains it; a rating is a count of stars
/// ("4", or "3+" for at least three; "0": unrated); a type is the extension, dot or not.
pub fn field_matches(f: SearchField, values: &[String], want: &str) -> bool {
    if want.is_empty() {
        return !values.is_empty();
    }
    match f {
        SearchField::Rating => {
            let k = values.first().and_then(|v| crate::metawrite::parse_rating(v)).unwrap_or(0);
            match want.strip_suffix('+') {
                Some(n) => n.parse::<u32>().is_ok_and(|n| k >= n),
                None => want.parse::<u32>().is_ok_and(|n| k == n),
            }
        }
        SearchField::Ext => values.iter().any(|v| v == want.trim_start_matches('.')),
        _ => values.iter().any(|v| v.contains(want)),
    }
}

/// The breadcrumb trail: "This PC", then every ancestor from the drive root down to the place. A collection
/// is a trail of its own.
pub fn crumbs(place: &Place) -> Vec<Place> {
    if let Place::Collection(_) = place {
        return vec![place.clone()];
    }
    let mut out = vec![Place::Computer];
    if let Place::Folder(p) = place {
        if is_server(p) {
            out.push(place.clone());
            return out;
        }
        let mut chain: Vec<PathBuf> = p.ancestors().map(Path::to_path_buf).filter(|a| !a.as_os_str().is_empty()).collect();
        // On a network share: its computer before the share.
        if let Some(server) = unc_server(p) {
            chain.push(server);
        }
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
    let cloud = attrs & (FILE_ATTRIBUTE_OFFLINE | FILE_ATTRIBUTE_RECALL_ON_OPEN | FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS) != 0;
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
        cloud,
        custom_icon: md.is_dir() && attrs & FILE_ATTRIBUTE_READONLY != 0 && !cloud,
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
    if is_server(dir) {
        return server_shares(dir).into_iter().map(share_row).collect();
    }
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

/// A computer's share as a folder row (nothing read from it: it may be slow or need signing in to).
fn share_row(path: PathBuf) -> Row {
    let name = folder_name(&path);
    Row { name_h: HSTRING::from(name.as_str()), name, path, kind: Kind::Folder, stamp: 0, created: 0, size: 0, cloud: false, custom_icon: false }
}

/// A folder's visible subfolders, in natural order (a tree's children, a crumb's menu); a computer's shares.
pub fn subfolders(dir: &Path) -> Vec<PathBuf> {
    if is_server(dir) {
        return server_shares(dir);
    }
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
    if is_server(dir) {
        return (server_shares(dir), Vec::new());
    }
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
        if kind == DRIVE_REMOTE {
            // Explorer's "share (\\server) (Z:)", from the mapping (asking the volume could hang on a host
            // that's gone).
            let name = match mapped_to(letter) {
                Some(unc) => match unc.trim_start_matches('\\').split_once('\\') {
                    Some((server, share)) => format!("{share} (\\\\{server}) ({letter}:)"),
                    None => format!("{unc} ({letter}:)"),
                },
                None => format!("Network Drive ({letter}:)"),
            };
            rows.push(Row { name_h: HSTRING::from(name.as_str()), name, path: PathBuf::from(root), kind: Kind::Drive, stamp: 0, created: 0, size: 0, cloud: false, custom_icon: false });
            continue;
        }
        let mut label = [0u16; 261];
        let ok = unsafe { GetVolumeInformationW(&root_h, Some(&mut label), None, None, None, None).is_ok() };
        let label = if ok { String::from_utf16_lossy(&label[..label.iter().position(|&c| c == 0).unwrap_or(0)]) } else { String::new() };
        let label = if label.is_empty() { if kind == DRIVE_REMOVABLE { "Removable Disk".into() } else { "Local Disk".into() } } else { label };
        let name = format!("{label} ({letter}:)");
        rows.push(Row { name_h: HSTRING::from(name.as_str()), name, path: PathBuf::from(root), kind: Kind::Drive, stamp: 0, created: 0, size: 0, cloud: false, custom_icon: false });
    }
    rows
}

/// The share a drive letter is mapped to (`\\server\share`).
fn mapped_to(letter: char) -> Option<String> {
    use windows::Win32::NetworkManagement::WNet::WNetGetConnectionW;
    let mut buf = [0u16; 520];
    let mut len = buf.len() as u32;
    let r = unsafe { WNetGetConnectionW(&HSTRING::from(format!("{letter}:")), Some(windows::core::PWSTR(buf.as_mut_ptr())), &mut len) };
    (r == windows::Win32::Foundation::NO_ERROR).then(|| String::from_utf16_lossy(&buf[..buf.iter().position(|&c| c == 0).unwrap_or(0)])).filter(|s| !s.is_empty())
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

// --- Network ----------------------------------------------------------------------------------------

/// `\\server` alone (a computer on the network, no share): listed by its shares, not read as a folder.
pub fn is_server(p: &Path) -> bool {
    let s = p.to_string_lossy();
    let s = s.trim_end_matches(['\\', '/']);
    s.strip_prefix("\\\\").is_some_and(|rest| !rest.is_empty() && !rest.contains(['\\', '/']) && !rest.starts_with(['?', '.']))
}

/// The computer a UNC path is on (`\\server` of `\\server\share\…`), when it's below that computer.
pub fn unc_server(p: &Path) -> Option<PathBuf> {
    let s = p.to_string_lossy();
    let rest = s.strip_prefix("\\\\")?;
    if rest.starts_with(['?', '.']) {
        return None;
    }
    let (server, share) = rest.split_once(['\\', '/'])?;
    (!server.is_empty() && !share.trim_matches(['\\', '/']).is_empty()).then(|| PathBuf::from(format!("\\\\{server}")))
}

/// A computer's shared folders (`\\server\share`), as Explorer lists them: the disk shares, not the admin
/// ones (`C$`). Empty when it can't be reached. Slow on an unreachable host: never on the UI thread.
pub fn server_shares(server: &Path) -> Vec<PathBuf> {
    use windows::Win32::NetworkManagement::WNet::{NETRESOURCEW, RESOURCE_GLOBALNET, RESOURCETYPE_DISK, WNET_OPEN_ENUM_USAGE};
    let name = server.to_string_lossy().trim_end_matches(['\\', '/']).to_string();
    let mut remote: Vec<u16> = name.encode_utf16().chain(Some(0)).collect();
    let res = NETRESOURCEW { dwScope: RESOURCE_GLOBALNET, dwType: RESOURCETYPE_DISK, lpRemoteName: windows::core::PWSTR(remote.as_mut_ptr()), ..Default::default() };
    let mut out: Vec<PathBuf> = wnet_enum(RESOURCE_GLOBALNET, WNET_OPEN_ENUM_USAGE(0), Some(&res))
        .into_iter()
        .filter(|r| !r.ends_with('$'))
        .map(PathBuf::from)
        .collect();
    out.sort_by(|a, b| natural(&folder_name(a), &folder_name(b)));
    out.dedup_by(|a, b| same_path(a, b));
    out
}

/// The remote names a WNet enumeration returns.
fn wnet_enum(scope: windows::Win32::NetworkManagement::WNet::NET_RESOURCE_SCOPE, usage: windows::Win32::NetworkManagement::WNet::WNET_OPEN_ENUM_USAGE, under: Option<&windows::Win32::NetworkManagement::WNet::NETRESOURCEW>) -> Vec<String> {
    use windows::Win32::Foundation::{ERROR_MORE_DATA, NO_ERROR};
    use windows::Win32::NetworkManagement::WNet::{NETRESOURCEW, RESOURCETYPE_DISK, WNetCloseEnum, WNetEnumResourceW, WNetOpenEnumW};
    let mut out = Vec::new();
    unsafe {
        let mut h = HANDLE::default();
        if WNetOpenEnumW(scope, RESOURCETYPE_DISK, usage, under.map(|r| r as *const NETRESOURCEW), &mut h) != NO_ERROR {
            return out;
        }
        // 16 KB, as the docs suggest; u64s keep the NETRESOURCEW array aligned.
        let mut buf = vec![0u64; 2048];
        loop {
            let mut count = u32::MAX;
            let mut size = (buf.len() * 8) as u32;
            let r = WNetEnumResourceW(h, &mut count, buf.as_mut_ptr() as _, &mut size);
            if r == ERROR_MORE_DATA {
                buf = vec![0u64; size as usize / 8 + 1];
                continue;
            }
            if r != NO_ERROR {
                break;
            }
            let items = std::slice::from_raw_parts(buf.as_ptr() as *const NETRESOURCEW, count as usize);
            for it in items {
                if !it.lpRemoteName.is_null() {
                    if let Ok(s) = it.lpRemoteName.to_string() {
                        out.push(s);
                    }
                }
            }
        }
        let _ = WNetCloseEnum(h);
    }
    out
}

/// The computers Explorer's Network folder finds (Windows' own discovery: WSD, NetBIOS), `\\NAME` each,
/// handed to `found` one by one as the enumeration turns them up. Takes seconds (~14 here): never on the UI
/// thread. COM must be up on the calling thread.
pub fn shell_computers(mut found: impl FnMut(PathBuf)) {
    use windows::Win32::UI::Shell::{BHID_EnumItems, FOLDERID_NetworkFolder, IEnumShellItems, IShellItem, KF_FLAG_DEFAULT, SHGetKnownFolderItem, SIGDN_DESKTOPABSOLUTEPARSING};
    unsafe {
        let Ok(net) = SHGetKnownFolderItem::<IShellItem>(&FOLDERID_NetworkFolder, KF_FLAG_DEFAULT, None) else { return };
        let Ok(en) = net.BindToHandler::<_, IEnumShellItems>(None, &BHID_EnumItems) else { return };
        loop {
            let mut got = [None];
            let mut n = 0u32;
            if en.Next(&mut got, Some(&mut n)).is_err() || n == 0 {
                break;
            }
            let Some(item) = got[0].take() else { break };
            if let Ok(p) = item.GetDisplayName(SIGDN_DESKTOPABSOLUTEPARSING) {
                let s = p.to_string().unwrap_or_default();
                windows::Win32::System::Com::CoTaskMemFree(Some(p.0 as _));
                // Computers parse as \\NAME; media servers and devices as ::{…} (not folders).
                if is_server(Path::new(&s)) {
                    found(PathBuf::from(s));
                }
            }
        }
    }
}

/// The computers the old browser service lists (the workgroup's), `\\NAME` each. Takes seconds.
pub fn browser_computers() -> Vec<PathBuf> {
    use windows::Win32::NetworkManagement::WNet::{RESOURCE_CONTEXT, WNET_OPEN_ENUM_USAGE};
    wnet_enum(RESOURCE_CONTEXT, WNET_OPEN_ENUM_USAGE(0), None).into_iter().map(PathBuf::from).filter(|p| is_server(p)).collect()
}

/// Adds computers to a sorted list, by name, once each.
pub fn merge_computers(list: &mut Vec<PathBuf>, more: Vec<PathBuf>) {
    list.extend(more);
    list.sort_by(|a, b| natural(&a.to_string_lossy().to_lowercase(), &b.to_string_lossy().to_lowercase()));
    list.dedup_by(|a, b| same_path(a, b));
}

/// Of `drives`, those mapped to network shares (Explorer's "Network locations").
pub fn network_drives_of(drives: &[Row]) -> Vec<Row> {
    drives.iter().filter(|d| d.kind == Kind::Drive && unsafe { GetDriveTypeW(&HSTRING::from(d.path.as_os_str())) } == DRIVE_REMOTE).cloned().collect()
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
    fn new_sections_land_by_their_default_neighbour() {
        use SideSection::*;
        // Saved before Network existed: it goes under Places, wherever Places was dragged.
        assert_eq!(SideSection::parse_list("bookmarks,collections,places,history"), vec![Bookmarks, Collections, Places, Network, History]);
        assert_eq!(SideSection::parse_list("places,history,bookmarks,collections"), vec![Places, Network, History, Bookmarks, Collections]);
        assert_eq!(SideSection::parse_list(""), SideSection::DEFAULT.to_vec());
        assert_eq!(SideSection::parse_list("history"), vec![Bookmarks, Collections, Places, Network, History]);
    }

    #[test]
    fn network_paths() {
        assert!(is_server(Path::new(r"\\nas")));
        assert!(is_server(Path::new(r"\\nas\")));
        assert!(!is_server(Path::new(r"\\nas\photos")));
        assert!(!is_server(Path::new(r"\\?\C:")));
        assert!(!is_server(Path::new(r"C:\")));
        assert_eq!(unc_server(Path::new(r"\\nas\photos\2024")), Some(PathBuf::from(r"\\nas")));
        assert_eq!(unc_server(Path::new(r"\\nas")), None);
        let up = Place::Folder(PathBuf::from(r"\\nas\photos\")).parent();
        assert_eq!(up, Some(Place::Folder(PathBuf::from(r"\\nas"))));
        assert_eq!(Place::Folder(PathBuf::from(r"\\nas")).parent(), Some(Place::Computer));
        let c = crumbs(&Place::Folder(PathBuf::from(r"\\nas\photos\2024")));
        assert_eq!(c[0], Place::Computer);
        assert_eq!(c[1], Place::Folder(PathBuf::from(r"\\nas")));
        assert_eq!(c.len(), 4);
        assert_eq!(crumbs(&Place::Folder(PathBuf::from(r"\\nas"))).len(), 2);
    }

    /// What the sidebar's Network finds on this machine's network.
    #[test]
    #[ignore]
    fn finds_network() {
        unsafe {
            let _ = windows::Win32::System::Com::CoInitializeEx(None, windows::Win32::System::Com::COINIT_MULTITHREADED);
        }
        let t = std::time::Instant::now();
        let mut computers = Vec::new();
        shell_computers(|c| {
            println!("shell found {} at {:?}", c.display(), t.elapsed());
            computers.push(c);
        });
        let b = browser_computers();
        println!("browser: {b:?} at {:?}", t.elapsed());
        merge_computers(&mut computers, b);
        println!("{} computers in {:?}: {computers:?}", computers.len(), t.elapsed());
        for c in &computers {
            println!("{}: {:?}", c.display(), server_shares(c));
        }
        println!("mapped: {:?}", network_drives_of(&drives()).iter().map(|d| &d.name).collect::<Vec<_>>());
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
    fn filter_terms_name_fields() {
        use SearchField::*;
        assert_eq!(
            filter_terms(r#"Sun  Keyword:Beach camera:"X100 V" c:\photos rating:"#),
            vec![
                Term::Name("sun".into()),
                Term::Field(Keywords, "beach".into()),
                Term::Field(Camera, "x100 v".into()),
                Term::Name(r"c:\photos".into()),
                Term::Field(Rating, String::new()),
            ]
        );
        let v = |s: &[&str]| s.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        assert!(field_matches(Rating, &v(&["\u{2605}\u{2605}\u{2605}\u{2606}\u{2606}"]), "3"));
        assert!(field_matches(Rating, &v(&["\u{2605}\u{2605}\u{2605}\u{2605}\u{2606}"]), "3+"));
        assert!(!field_matches(Rating, &v(&["\u{2605}\u{2605}\u{2606}\u{2606}\u{2606}"]), "3+"));
        assert!(field_matches(Rating, &[], "0"));
        assert!(field_matches(Ext, &v(&["jpg"]), ".jpg"));
        assert!(!field_matches(Ext, &v(&["jpeg"]), "jpg"));
        assert!(field_matches(Keywords, &v(&["beach", "sun"]), "bea"));
        assert!(!field_matches(Title, &[], ""));
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
        let mut one = views[3].clone();
        one.view.sort = Sort { field: SortField::Size, descending: true };
        one.view.xsort = Some((ColKind::Camera, true));
        assert_eq!(SavedView::parse(&one.format()).as_ref(), Some(&one));
        // A path with semicolons in it, and a place that isn't a folder.
        one.path = PathBuf::from(r"C:\a;b\c");
        assert_eq!(SavedView::parse(&one.format()).as_ref(), Some(&one));
        one.path = view_key(&Place::Collection(Collection::Colour(3)));
        assert_eq!(SavedView::parse(&one.format()).as_ref(), Some(&one));
        // An old line keeps the sort it's given (the default's when it was written).
        let old = SavedView::parse_v1(r"folder;mixed;0;1;2;name:0;C:\x", Sort { field: SortField::Date, descending: true }).unwrap();
        assert_eq!(old.view.sort, Sort { field: SortField::Date, descending: true });
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
