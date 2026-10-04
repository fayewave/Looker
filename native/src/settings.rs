//! Persisted preferences: a plain `key=value` file in `%LOCALAPPDATA%\Looker\native\settings.txt` (inside an
//! MSIX it lands in the package's redirected LocalCache). Read once at startup, before the window exists,
//! because the saved window placement decides the size of the launch photo's first decode.

use std::path::PathBuf;

use crate::folder::Sort;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct SavedWindow {
    /// The restored (not maximized) window rect, screen coordinates.
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
    pub maximized: bool,
}

#[derive(Clone, PartialEq, Debug)]
pub struct Settings {
    pub window: Option<SavedWindow>,
    /// "Remember window size and position" (default on). Off clears the saved placement.
    pub remember_window: bool,
    /// The toolbar sort, applied to every folder.
    pub sort: Sort,
    pub info_visible: bool,
    /// The info card's width in DIPs, its 12 px margins included (what the image fit leaves free).
    pub info_width: f32,
    pub explorer_visible: bool,
    /// The file explorer card's width in DIPs, margins included (the info card's range).
    pub explorer_width: f32,
    /// Remember recently shown photos (the landing page's list). Off records nothing.
    pub recents_enabled: bool,
    /// Most recently shown first, at most [`RECENT_CAPACITY`].
    pub recents: Vec<PathBuf>,
    pub strip_visible: bool,
    /// The thumbnail strip's height in DIPs (the cells are 8 less, 4:3).
    pub strip_height: f32,
}

pub const EXPLORER_WIDTH: f32 = 320.0;
pub const RECENT_CAPACITY: usize = 12;
pub const STRIP_HEIGHT: f32 = 96.0;
pub const STRIP_MIN: f32 = 56.0;
pub const STRIP_MAX: f32 = 480.0;

pub const INFO_WIDTH: f32 = 320.0;
pub const INFO_MIN: f32 = 260.0;
pub const INFO_MAX: f32 = 640.0;

impl Default for Settings {
    fn default() -> Self {
        Settings { window: None, remember_window: true, sort: Sort::default(), info_visible: false, info_width: INFO_WIDTH, explorer_visible: false, explorer_width: EXPLORER_WIDTH, recents_enabled: true, recents: Vec::new(), strip_visible: false, strip_height: STRIP_HEIGHT }
    }
}

fn same_file(a: &std::path::Path, b: &std::path::Path) -> bool {
    a.as_os_str().to_string_lossy().to_lowercase() == b.as_os_str().to_string_lossy().to_lowercase()
}

impl Settings {
    /// A photo was shown: to the front of the recent list (once, case-insensitively), trimmed to capacity.
    /// False when nothing changed (it was already first, or recents are off), so the caller skips the save.
    pub fn push_recent(&mut self, path: &std::path::Path) -> bool {
        if !self.recents_enabled || self.recents.first().is_some_and(|f| same_file(f, path)) {
            return false;
        }
        self.recents.retain(|r| !same_file(r, path));
        self.recents.insert(0, path.to_path_buf());
        self.recents.truncate(RECENT_CAPACITY);
        true
    }

    pub fn remove_recent(&mut self, path: &std::path::Path) {
        self.recents.retain(|r| !same_file(r, path));
    }
}

/// Looker's own data folder (settings, the rendered wallpaper).
pub fn data_dir() -> Option<PathBuf> {
    let base = std::env::var_os("LOCALAPPDATA")?;
    Some(PathBuf::from(base).join("Looker").join("native"))
}

fn file() -> Option<PathBuf> {
    Some(data_dir()?.join("settings.txt"))
}

pub fn parse(text: &str) -> Settings {
    let mut s = Settings::default();
    for line in text.lines() {
        let Some((k, v)) = line.split_once('=') else { continue };
        let v = v.trim();
        match k.trim() {
            "window" => {
                let n: Vec<i32> = v.split_whitespace().filter_map(|x| x.parse().ok()).collect();
                if n.len() == 5 && n[2] > n[0] && n[3] > n[1] {
                    s.window = Some(SavedWindow { left: n[0], top: n[1], right: n[2], bottom: n[3], maximized: n[4] != 0 });
                }
            }
            "remember_window" => s.remember_window = v != "0" && !v.eq_ignore_ascii_case("false"),
            "sort" => s.sort = Sort::parse(v).unwrap_or_default(),
            "info_visible" => s.info_visible = v == "1",
            "info_width" => s.info_width = v.parse::<f32>().map_or(INFO_WIDTH, |w| w.clamp(INFO_MIN, INFO_MAX)),
            "explorer_visible" => s.explorer_visible = v == "1",
            "explorer_width" => s.explorer_width = v.parse::<f32>().map_or(EXPLORER_WIDTH, |w| w.clamp(INFO_MIN, INFO_MAX)),
            "recents_enabled" => s.recents_enabled = v != "0",
            "recent" if !v.is_empty() && s.recents.len() < RECENT_CAPACITY => s.recents.push(PathBuf::from(v)),
            "strip_visible" => s.strip_visible = v == "1",
            "strip_height" => s.strip_height = v.parse::<f32>().map_or(STRIP_HEIGHT, |h| h.clamp(STRIP_MIN, STRIP_MAX)),
            _ => {}
        }
    }
    s
}

pub fn format(s: &Settings) -> String {
    let mut out = String::new();
    if let Some(w) = s.window {
        out.push_str(&format!("window={} {} {} {} {}\n", w.left, w.top, w.right, w.bottom, w.maximized as i32));
    }
    out.push_str(&format!("remember_window={}\n", s.remember_window as i32));
    out.push_str(&format!("sort={}\n", s.sort.to_setting()));
    out.push_str(&format!("info_visible={}\n", s.info_visible as i32));
    out.push_str(&format!("info_width={}\n", s.info_width.round()));
    out.push_str(&format!("explorer_visible={}\n", s.explorer_visible as i32));
    out.push_str(&format!("explorer_width={}\n", s.explorer_width.round()));
    out.push_str(&format!("recents_enabled={}\n", s.recents_enabled as i32));
    for r in &s.recents {
        out.push_str(&format!("recent={}\n", r.display()));
    }
    out.push_str(&format!("strip_visible={}\n", s.strip_visible as i32));
    out.push_str(&format!("strip_height={}\n", s.strip_height.round()));
    out
}

pub fn load() -> Settings {
    file().and_then(|f| std::fs::read_to_string(f).ok()).map(|t| parse(&t)).unwrap_or_default()
}

pub fn save(s: &Settings) {
    let Some(f) = file() else { return };
    if let Some(dir) = f.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    // Write-then-rename so a crash mid-write never leaves a half file.
    let tmp = f.with_extension("tmp");
    if std::fs::write(&tmp, format(s)).is_ok() {
        let _ = std::fs::rename(&tmp, &f);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips() {
        let s = Settings {
            window: Some(SavedWindow { left: -1900, top: 40, right: -100, bottom: 1000, maximized: true }),
            remember_window: true,
            sort: Sort { field: crate::folder::SortField::Date, descending: true },
            info_visible: true,
            info_width: 412.0,
            explorer_visible: true,
            explorer_width: 300.0,
            recents_enabled: true,
            recents: vec![PathBuf::from(r"C:\a b\one.jpg"), PathBuf::from(r"D:\two.png")],
            strip_visible: true,
            strip_height: 160.0,
        };
        assert_eq!(parse(&format(&s)), s);
        assert_eq!(parse(&format(&Settings::default())), Settings::default());
    }

    #[test]
    fn recents_move_to_the_front_and_cap() {
        let mut s = Settings::default();
        for i in 0..15 {
            assert!(s.push_recent(&PathBuf::from(format!(r"C:\p\{i}.jpg"))));
        }
        assert_eq!(s.recents.len(), RECENT_CAPACITY);
        assert!(!s.push_recent(&PathBuf::from(r"c:\P\14.JPG"))); // already first
        assert!(s.push_recent(&PathBuf::from(r"C:\p\10.jpg")));
        assert_eq!(s.recents[0], PathBuf::from(r"C:\p\10.jpg"));
        assert_eq!(s.recents.iter().filter(|r| r.ends_with("10.jpg")).count(), 1);
        s.recents_enabled = false;
        assert!(!s.push_recent(&PathBuf::from(r"C:\p\new.jpg")));
    }

    #[test]
    fn ignores_junk_and_bad_rects() {
        let s = parse("garbage\nwindow=10 10 5 5 0\nremember_window=0\nunknown=1");
        assert_eq!(s.window, None);
        assert!(!s.remember_window);
    }
}
