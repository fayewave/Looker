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
    /// The mouse wheel over the image steps through the folder instead of zooming (Ctrl+wheel still zooms).
    pub wheel_navigates: bool,
    /// Pointer zooms (wheel, double-click) aim at the middle of the view rather than the pointer.
    pub zoom_center: bool,
    /// The dark grey (#1F1F1F) theme instead of black.
    pub dark_grey: bool,
    /// How much memory decoded photos may take, in MB.
    pub cache_mb: u32,
    /// Remember recently shown photos (the landing page's list). Off records nothing.
    pub recents_enabled: bool,
    /// Most recently shown first, at most [`RECENT_CAPACITY`].
    pub recents: Vec<PathBuf>,
    pub strip_visible: bool,
    /// The thumbnail strip's height in DIPs (the cells are 8 less, 4:3).
    pub strip_height: f32,
    /// How long the slideshow dwells on each image (1-120).
    pub slideshow_seconds: u32,
    /// The landing page's "Set Looker as your default photo viewer" was dismissed with its X, for good.
    pub default_hint_dismissed: bool,
}

pub const EXPLORER_WIDTH: f32 = 320.0;
pub const RECENT_CAPACITY: usize = 12;
pub const CACHE_MB: u32 = 512;
/// The choices the Settings page offers for the decode cache.
pub const CACHE_CHOICES: [u32; 5] = [128, 256, 512, 1024, 2048];
pub const STRIP_HEIGHT: f32 = 96.0;
pub const SLIDESHOW_SECONDS: u32 = 4;
pub const STRIP_MIN: f32 = 56.0;
pub const STRIP_MAX: f32 = 480.0;

pub const INFO_WIDTH: f32 = 320.0;
pub const INFO_MIN: f32 = 260.0;
pub const INFO_MAX: f32 = 640.0;

impl Default for Settings {
    fn default() -> Self {
        Settings { window: None, remember_window: true, sort: Sort::default(), info_visible: false, info_width: INFO_WIDTH, explorer_visible: false, explorer_width: EXPLORER_WIDTH, wheel_navigates: false, zoom_center: false, dark_grey: false, cache_mb: CACHE_MB, recents_enabled: true, recents: Vec::new(), strip_visible: false, strip_height: STRIP_HEIGHT, slideshow_seconds: SLIDESHOW_SECONDS, default_hint_dismissed: false }
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

/// Looker's own data folder (settings, the rendered wallpaper): the package's LocalState when packaged,
/// else `%LOCALAPPDATA%\Looker\native`. Not the shared path when packaged: the package's file system view
/// would read a dev build's settings there and the C# app's settings would never be migrated.
pub fn data_dir() -> Option<PathBuf> {
    static DIR: std::sync::OnceLock<Option<PathBuf>> = std::sync::OnceLock::new();
    DIR.get_or_init(|| {
        let base = PathBuf::from(std::env::var_os("LOCALAPPDATA")?);
        Some(match crate::store::family_name() {
            Some(family) => base.join("Packages").join(family).join("LocalState"),
            None => base.join("Looker").join("native"),
        })
    })
    .clone()
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
            "wheel" => s.wheel_navigates = v == "navigate",
            "zoom_anchor" => s.zoom_center = v == "center",
            "theme" => s.dark_grey = v == "dark_grey",
            "cache_mb" => s.cache_mb = v.parse::<u32>().map_or(CACHE_MB, |m| m.clamp(128, 2048)),
            "recents_enabled" => s.recents_enabled = v != "0",
            "recent" if !v.is_empty() && s.recents.len() < RECENT_CAPACITY => s.recents.push(PathBuf::from(v)),
            "strip_visible" => s.strip_visible = v == "1",
            "strip_height" => s.strip_height = v.parse::<f32>().map_or(STRIP_HEIGHT, |h| h.clamp(STRIP_MIN, STRIP_MAX)),
            "default_hint_dismissed" => s.default_hint_dismissed = v == "1",
            "slideshow_seconds" => s.slideshow_seconds = v.parse::<u32>().map_or(SLIDESHOW_SECONDS, |n| n.clamp(1, 120)),
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
    out.push_str(&format!("wheel={}\n", if s.wheel_navigates { "navigate" } else { "zoom" }));
    out.push_str(&format!("zoom_anchor={}\n", if s.zoom_center { "center" } else { "pointer" }));
    out.push_str(&format!("theme={}\n", if s.dark_grey { "dark_grey" } else { "black" }));
    out.push_str(&format!("cache_mb={}\n", s.cache_mb));
    out.push_str(&format!("recents_enabled={}\n", s.recents_enabled as i32));
    for r in &s.recents {
        out.push_str(&format!("recent={}\n", r.display()));
    }
    out.push_str(&format!("strip_visible={}\n", s.strip_visible as i32));
    out.push_str(&format!("strip_height={}\n", s.strip_height.round()));
    out.push_str(&format!("slideshow_seconds={}\n", s.slideshow_seconds));
    out.push_str(&format!("default_hint_dismissed={}\n", s.default_hint_dismissed as i32));
    out
}

pub fn load() -> Settings {
    let Some(f) = file() else { return Settings::default() };
    match std::fs::read_to_string(&f) {
        Ok(t) => parse(&t),
        // First launch: carry the C# app's preferences over when running as its package (same identity).
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => match legacy_store() {
            Some(s) => {
                crate::trace::mark("settings migrated from the C# app");
                save(&s);
                s
            }
            None => Settings::default(),
        },
        Err(_) => Settings::default(),
    }
}

// --- Migration from the C# app ----------------------------------------------------------------------

/// A value in the C# app's LocalSettings.
#[derive(Clone, Debug, PartialEq)]
pub enum Legacy {
    Int(i32),
    Bool(bool),
    Str(String),
}

/// The C# app's LocalSettings (`ApplicationData.Current`), readable only with its package identity: `None`
/// when unpackaged, or when the store is empty (a fresh install has nothing to carry over).
fn legacy_store() -> Option<Settings> {
    use windows::Foundation::IPropertyValue;
    use windows::Foundation::PropertyType;
    use windows::core::{HSTRING, Interface};
    let values = windows::Storage::ApplicationData::Current().ok()?.LocalSettings().ok()?.Values().ok()?;
    if values.Size().ok()? == 0 {
        return None;
    }
    let get = |key: &str| -> Option<Legacy> {
        let v = values.Lookup(&HSTRING::from(key)).ok()?;
        let v: IPropertyValue = v.cast().ok()?;
        match v.Type().ok()? {
            PropertyType::Int32 => v.GetInt32().ok().map(Legacy::Int),
            PropertyType::Boolean => v.GetBoolean().ok().map(Legacy::Bool),
            PropertyType::String => v.GetString().ok().map(|s| Legacy::Str(s.to_string_lossy())),
            _ => None,
        }
    };
    Some(from_legacy(get))
}

/// Maps the C# app's keys (`SettingsService.cs`) onto these settings; anything missing or of the wrong type
/// keeps its default, and the ranges are clamped as the C# app does.
pub fn from_legacy(get: impl Fn(&str) -> Option<Legacy>) -> Settings {
    use crate::folder::SortField;
    let int = |k: &str| match get(k) {
        Some(Legacy::Int(i)) => Some(i),
        _ => None,
    };
    let flag = |k: &str| match get(k) {
        Some(Legacy::Bool(b)) => Some(b),
        _ => None,
    };
    let mut s = Settings::default();
    s.sort = Sort {
        field: match int("SortField") {
            Some(1) => SortField::Date,
            Some(2) => SortField::Size,
            _ => SortField::Name,
        },
        descending: int("SortDirection") == Some(1),
    };
    s.strip_visible = flag("StripVisible").unwrap_or(false);
    s.strip_height = int("StripHeight").map_or(STRIP_HEIGHT, |h| (h as f32).clamp(STRIP_MIN, STRIP_MAX));
    s.info_visible = flag("InfoVisible").unwrap_or(false);
    s.info_width = int("InfoWidth").map_or(INFO_WIDTH, |w| (w as f32).clamp(INFO_MIN, INFO_MAX));
    s.explorer_visible = flag("ExplorerVisible").unwrap_or(false);
    s.explorer_width = int("ExplorerWidth").map_or(EXPLORER_WIDTH, |w| (w as f32).clamp(INFO_MIN, INFO_MAX));
    s.slideshow_seconds = int("SlideshowSeconds").map_or(SLIDESHOW_SECONDS, |n| n.clamp(1, 120) as u32);
    s.cache_mb = int("CacheBudgetMB").map_or(CACHE_MB, |m| m.clamp(128, 2048) as u32);
    s.wheel_navigates = int("WheelMode") == Some(1);
    s.zoom_center = int("ZoomAnchor") == Some(1);
    s.dark_grey = int("Theme") == Some(1);
    s.remember_window = flag("RememberWindow").unwrap_or(true);
    s.default_hint_dismissed = flag("DefaultHintDismissed").unwrap_or(false);
    s.recents_enabled = flag("RecentsEnabled").unwrap_or(true);
    if s.recents_enabled {
        if let Some(Legacy::Str(list)) = get("RecentFiles") {
            s.recents = list.split('\n').filter(|l| !l.is_empty()).take(RECENT_CAPACITY).map(PathBuf::from).collect();
        }
    }
    // Physical pixels, outer bounds of the restored window.
    if s.remember_window {
        if let (Some(x), Some(y), Some(w), Some(h)) = (int("WindowX"), int("WindowY"), int("WindowW"), int("WindowH")) {
            if w > 0 && h > 0 {
                s.window = Some(SavedWindow { left: x, top: y, right: x + w, bottom: y + h, maximized: flag("WasMaximized").unwrap_or(false) });
            }
        }
    }
    s
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
            wheel_navigates: true,
            zoom_center: true,
            dark_grey: true,
            cache_mb: 1024,
            recents_enabled: true,
            recents: vec![PathBuf::from(r"C:\a b\one.jpg"), PathBuf::from(r"D:\two.png")],
            strip_visible: true,
            strip_height: 160.0,
            slideshow_seconds: 9,
            default_hint_dismissed: true,
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
    fn carries_the_csharp_settings_over() {
        use std::collections::HashMap;
        let m: HashMap<&str, Legacy> = [
            ("SortField", Legacy::Int(1)),
            ("SortDirection", Legacy::Int(1)),
            ("StripVisible", Legacy::Bool(true)),
            ("StripHeight", Legacy::Int(9999)),
            ("InfoWidth", Legacy::Int(400)),
            ("Theme", Legacy::Int(1)),
            ("WheelMode", Legacy::Int(1)),
            ("CacheBudgetMB", Legacy::Int(1024)),
            ("RecentFiles", Legacy::Str("C:\\a.jpg\nD:\\b.png\n".into())),
            ("WindowX", Legacy::Int(-1900)),
            ("WindowY", Legacy::Int(40)),
            ("WindowW", Legacy::Int(1600)),
            ("WindowH", Legacy::Int(900)),
            ("WasMaximized", Legacy::Bool(true)),
            ("ExplorerVisible", Legacy::Str("not a bool".into())),
        ]
        .into_iter()
        .collect();
        let s = from_legacy(|k| m.get(k).cloned());
        assert_eq!(s.sort, Sort { field: crate::folder::SortField::Date, descending: true });
        assert!(s.strip_visible && s.dark_grey && s.wheel_navigates && !s.zoom_center);
        assert_eq!(s.strip_height, STRIP_MAX);
        assert_eq!(s.info_width, 400.0);
        assert_eq!(s.cache_mb, 1024);
        assert!(!s.explorer_visible, "a wrongly typed value keeps its default");
        assert_eq!(s.recents, vec![PathBuf::from("C:\\a.jpg"), PathBuf::from("D:\\b.png")]);
        assert_eq!(s.window, Some(SavedWindow { left: -1900, top: 40, right: -300, bottom: 940, maximized: true }));
        assert_eq!(from_legacy(|_| None), Settings::default());
    }

    #[test]
    fn ignores_junk_and_bad_rects() {
        let s = parse("garbage\nwindow=10 10 5 5 0\nremember_window=0\nunknown=1");
        assert_eq!(s.window, None);
        assert!(!s.remember_window);
    }
}
