//! The open photo's folder: every file in Explorer's natural name order, and the subset Looker can show.

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

pub struct Entry {
    pub path: PathBuf,
    /// Online-only cloud file (Dropbox/OneDrive placeholder): reading even its header downloads it, so it
    /// is never preloaded.
    pub cloud: bool,
}

pub struct Listing {
    pub folder: PathBuf,
    /// Images Looker can show, in order.
    pub images: Vec<Entry>,
    /// Rank of each image among *all* visible files, for the "3 / 128" counter.
    pub rank: Vec<usize>,
    pub total_files: usize,
}

fn natural(a: &HSTRING, b: &HSTRING) -> Ordering {
    unsafe { StrCmpLogicalW(a, b).cmp(&0) }
}

pub fn list(folder: &Path) -> Listing {
    let mut files: Vec<(HSTRING, PathBuf, bool, bool)> = Vec::new();
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
            let cloud = attrs & (FILE_ATTRIBUTE_OFFLINE | FILE_ATTRIBUTE_RECALL_ON_OPEN | FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS) != 0;
            let path = e.path();
            let name = HSTRING::from(e.file_name().as_os_str());
            let supported = is_supported(&path);
            files.push((name, path, supported, cloud));
        }
    }
    files.sort_by(|a, b| natural(&a.0, &b.0));
    let mut images = Vec::new();
    let mut rank = Vec::new();
    for (i, (_, path, supported, cloud)) in files.iter().enumerate() {
        if *supported {
            images.push(Entry { path: path.clone(), cloud: *cloud });
            rank.push(i);
        }
    }
    Listing { folder: folder.to_path_buf(), images, rank, total_files: files.len() }
}
