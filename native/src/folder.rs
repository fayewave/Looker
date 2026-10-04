//! The open photo's folder: every visible file under the toolbar sort, and the subset Looker can show.
//! The position counter counts every file, not only images (as the C# app does since Sept 2026).

use std::cmp::Ordering;
use std::os::windows::fs::MetadataExt;
use std::path::{Path, PathBuf};

use windows::Win32::UI::Shell::StrCmpLogicalW;
use windows::core::HSTRING;

use crate::format::is_supported;

const FILE_ATTRIBUTE_HIDDEN: u32 = 0x2;
const FILE_ATTRIBUTE_SYSTEM: u32 = 0x4;
const FILE_ATTRIBUTE_OFFLINE: u32 = 0x1000;
const FILE_ATTRIBUTE_RECALL_ON_OPEN: u32 = 0x40000;
const FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS: u32 = 0x400000;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SortField {
    /// Explorer's natural order ("img9" before "img10").
    Name,
    /// Date modified.
    Date,
    Size,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Sort {
    pub field: SortField,
    pub descending: bool,
}

impl Default for Sort {
    fn default() -> Self {
        Sort { field: SortField::Name, descending: false }
    }
}

impl Sort {
    pub fn parse(s: &str) -> Option<Sort> {
        let mut it = s.split_whitespace();
        let field = match it.next()? {
            "name" => SortField::Name,
            "date" => SortField::Date,
            "size" => SortField::Size,
            _ => return None,
        };
        let descending = it.next() == Some("desc");
        Some(Sort { field, descending })
    }

    pub fn to_setting(self) -> String {
        let f = match self.field {
            SortField::Name => "name",
            SortField::Date => "date",
            SortField::Size => "size",
        };
        format!("{f} {}", if self.descending { "desc" } else { "asc" })
    }
}

pub struct File {
    pub path: PathBuf,
    name: HSTRING,
    pub stamp: u64,
    pub size: u64,
    pub supported: bool,
    pub cloud: bool,
}

pub struct Entry {
    pub path: PathBuf,
    /// Last-write time, part of the cache key.
    pub stamp: u64,
    /// Online-only cloud file (Dropbox/OneDrive placeholder): reading even its header downloads it, so it
    /// is never preloaded.
    pub cloud: bool,
}

pub struct Listing {
    pub folder: PathBuf,
    /// Every visible file, sorted.
    files: Vec<File>,
    /// Images Looker can show, in order.
    pub images: Vec<Entry>,
    /// Rank of each image among *all* visible files, for the "3 / 128" counter.
    pub rank: Vec<usize>,
    pub total_files: usize,
    pub sort: Sort,
}

fn natural(a: &HSTRING, b: &HSTRING) -> Ordering {
    unsafe { StrCmpLogicalW(a, b).cmp(&0) }
}

fn compare(a: &File, b: &File, sort: Sort) -> Ordering {
    let o = match sort.field {
        SortField::Name => natural(&a.name, &b.name),
        SortField::Date => a.stamp.cmp(&b.stamp).then_with(|| natural(&a.name, &b.name)),
        SortField::Size => a.size.cmp(&b.size).then_with(|| natural(&a.name, &b.name)),
    };
    if sort.descending { o.reverse() } else { o }
}

impl Listing {
    pub fn resort(&mut self, sort: Sort) {
        self.sort = sort;
        self.files.sort_by(|a, b| compare(a, b, sort));
        self.images.clear();
        self.rank.clear();
        for (i, f) in self.files.iter().enumerate() {
            if f.supported {
                self.images.push(Entry { path: f.path.clone(), stamp: f.stamp, cloud: f.cloud });
                self.rank.push(i);
            }
        }
        self.total_files = self.files.len();
    }
}

pub fn list(folder: &Path, sort: Sort) -> Listing {
    let mut files: Vec<File> = Vec::new();
    if let Ok(rd) = std::fs::read_dir(folder) {
        for e in rd.flatten() {
            let Ok(md) = e.metadata() else { continue };
            if !md.is_file() {
                continue;
            }
            let attrs = md.file_attributes();
            if attrs & (FILE_ATTRIBUTE_HIDDEN | FILE_ATTRIBUTE_SYSTEM) != 0 {
                continue;
            }
            let path = e.path();
            files.push(File {
                name: HSTRING::from(e.file_name().as_os_str()),
                supported: is_supported(&path),
                cloud: attrs & (FILE_ATTRIBUTE_OFFLINE | FILE_ATTRIBUTE_RECALL_ON_OPEN | FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS) != 0,
                stamp: md.last_write_time(),
                size: md.len(),
                path,
            });
        }
    }
    let mut l = Listing { folder: folder.to_path_buf(), files, images: Vec::new(), rank: Vec::new(), total_files: 0, sort };
    l.resort(sort);
    l
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(name: &str, stamp: u64, size: u64) -> File {
        File { path: PathBuf::from(name), name: HSTRING::from(name), stamp, size, supported: !name.ends_with(".txt"), cloud: false }
    }

    fn listing(sort: Sort) -> Listing {
        let mut l = Listing {
            folder: PathBuf::new(),
            files: vec![file("img10.jpg", 1, 30), file("img9.jpg", 3, 10), file("notes.txt", 2, 20), file("img1.jpg", 2, 10)],
            images: Vec::new(),
            rank: Vec::new(),
            total_files: 0,
            sort,
        };
        l.resort(sort);
        l
    }

    fn names(l: &Listing) -> Vec<String> {
        l.images.iter().map(|e| e.path.to_string_lossy().into_owned()).collect()
    }

    #[test]
    fn natural_name_order_and_counter_ranks() {
        let l = listing(Sort::default());
        assert_eq!(names(&l), ["img1.jpg", "img9.jpg", "img10.jpg"]);
        // notes.txt sorts last by name, so the images rank 0, 1, 2 among all 4 files.
        assert_eq!(l.rank, [0, 1, 2]);
        assert_eq!(l.total_files, 4);
    }

    #[test]
    fn date_and_size_sorts_break_ties_by_name() {
        let by_date = listing(Sort { field: SortField::Date, descending: false });
        assert_eq!(names(&by_date), ["img10.jpg", "img1.jpg", "img9.jpg"]);
        let by_size_desc = listing(Sort { field: SortField::Size, descending: true });
        assert_eq!(names(&by_size_desc), ["img10.jpg", "img9.jpg", "img1.jpg"]);
    }

    #[test]
    fn sort_setting_round_trips() {
        for s in [Sort::default(), Sort { field: SortField::Size, descending: true }] {
            assert_eq!(Sort::parse(&s.to_setting()), Some(s));
        }
    }
}
