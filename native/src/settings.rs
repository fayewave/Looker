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
    /// The file explorer card's width in DIPs, margins included, as its edge was dragged; 0 for the default, a
    /// quarter of the window (Faye, 2026-11-10; the old default, 320, reads as 0).
    pub explorer_width: f32,
    /// The mouse wheel over the image steps through the folder instead of zooming (Ctrl+wheel still zooms).
    pub wheel_navigates: bool,
    /// Pointer zooms (wheel, double-click) aim at the middle of the view rather than the pointer.
    pub zoom_center: bool,
    /// The theme in use: Black (default), Dark, Paper or Light (theme.rs). While `theme_follow`, whichever of
    /// `theme_dark` and `theme_light` matches Windows' app mode (`follow_windows`).
    pub theme: crate::theme::ThemeId,
    /// Match Windows: dark or light as Windows' app mode is, in the themes picked for each (Snapper's).
    pub theme_follow: bool,
    pub theme_dark: crate::theme::ThemeId,
    pub theme_light: crate::theme::ThemeId,
    /// The status row filled with the theme's bar colour instead of floating see-through on the photo (the key
    /// keeps its first name: it covered the title bar too at first).
    pub bars_opaque: bool,
    /// The accent colour (Snapper's set; Crimson, Looker's red, the default) and Custom's colour.
    pub accent: crate::theme::Accent,
    pub accent_custom: u32,
    /// The checkerboard behind the image (default on); off, the viewport is the theme's plain background.
    pub checkerboard: bool,
    /// The same behind a video (default off: a video is never transparent; Faye, 2026-11-11).
    pub checkerboard_video: bool,
    /// How much memory decoded photos may take, in MB.
    pub cache_mb: u32,
    /// Remember recently shown photos (the landing page's list). Off records nothing.
    pub recents_enabled: bool,
    /// Most recently shown first, at most [`RECENT_CAPACITY`].
    pub recents: Vec<PathBuf>,
    pub strip_visible: bool,
    /// The thumbnail strip's height in DIPs (the cells are 8 less, 4:3).
    pub strip_height: f32,
    /// The strip lists the folder's other files too, greyed out (as the explorer card does), and ←/→ step
    /// through them.
    pub strip_other_files: bool,
    /// The explorer card lists files Looker can't open (greyed out).
    pub explorer_other_files: bool,
    /// How long the slideshow dwells on each image (1-120).
    pub slideshow_seconds: u32,
    /// The landing page's "Set Looker as your default photo viewer" was dismissed with its X, for good.
    pub default_hint_dismissed: bool,
    /// The explorer's bookmarked folders, in the order they were added.
    pub bookmarks: Vec<PathBuf>,
    /// Folders the explorer listed, most recent first, at most [`VISITED_CAPACITY`] (its sidebar's History).
    pub visited: Vec<PathBuf>,
    /// Where the explorer puts folders among the files.
    pub explorer_group: crate::explorer::Group,
    /// Browse mode's width as a share of the window's, as its edge was last dragged (0: the default, all but
    /// a preview of 360 DIPs or 30 %).
    pub browse_share: f32,
    /// The explorer's columns, in order, with their widths (Name's is 0: it takes what's left).
    pub explorer_columns: Vec<(crate::explorer::ColKind, f32)>,
    /// The explorer's sidebar column's width, as its edge was last dragged.
    pub sidebar_width: f32,
    /// The default view lists the subfolders' files too.
    pub explorer_subfolders: bool,
    /// The default view's row height, in rows (1-4).
    pub explorer_height: u8,
    /// The default view's sort by a column the list alone sorts by (`sort` is its name/date/size one).
    pub explorer_xsort: Option<(crate::explorer::ColKind, bool)>,
    /// The default view's layout (list or columns) and column view's column width.
    pub explorer_layout: crate::explorer::Layout,
    pub explorer_column_w: f32,
    /// Folders (and the places that aren't folders, `explorer::view_key`) with a view of their own.
    pub views: Vec<crate::explorer::SavedView>,
    /// The explorer sidebar's sections, top to bottom.
    pub sidebar_order: Vec<crate::explorer::SideSection>,
    /// The sections folded away to their headings.
    pub sidebar_closed: Vec<crate::explorer::SideSection>,
    /// The Collections section's order as dragged: 0 Favourites, 1 + a colour tag's index (empty: the default).
    pub collection_order: Vec<u8>,
    /// The explorer's tabs, left to right (`explorer::place_key`), and the one in front.
    pub tabs: Vec<String>,
    pub tab_active: usize,
    /// Each tab's layout, in the tabs' order (missing ones take `explorer_layout`, the last one picked).
    pub tab_layouts: Vec<crate::explorer::Layout>,
    /// How the explorer's date columns read (Transient's Date and Time options): the date, the time, and the
    /// year in two digits.
    pub date_show_date: bool,
    pub date_show_time: bool,
    pub date_two_digit_year: bool,
    /// Day and month padded to two digits, 01/01/2026 (default on; saved as `u` in `explorer_dates` when off).
    pub date_pad: bool,
    /// A one-digit hour padded to line up with two (Transient's Align times; default on, saved as `n` when off).
    pub date_align: bool,
    /// Video sound: 0..1, and muted.
    pub video_volume: f32,
    pub video_muted: bool,
    /// A video starts playing when it's opened.
    pub video_autoplay: bool,
    /// The video bar counts pictures ("372 / 732") instead of showing timecode.
    pub video_frames: bool,
    /// Videos' AC-3/E-AC-3/DTS/TrueHD sound goes undecoded to a receiver (when the output device takes it).
    pub video_passthrough: bool,
    /// Videos shorter than this many seconds loop (0: none, `u32::MAX`: all).
    pub video_loop_secs: u32,
    /// Every video starts muted (M unmutes it).
    pub video_start_muted: bool,
    /// Long videos (over `RESUME_MIN_SECS`) open where they were left.
    pub video_resume: bool,
    /// Audio files show their spectrogram instead of the waveform.
    pub audio_spectrum: bool,
    /// The text editor wraps long lines to its width (Word wrap).
    pub text_wrap: bool,
    /// The text editor's font (family, weight, italic, points), and View's switches: the status bar, line
    /// numbers, spell check, autocorrect, formatted Markdown. Print margins in thousandths of an inch.
    pub text_font: String,
    pub text_font_weight: i32,
    pub text_font_italic: bool,
    pub text_font_pt: f32,
    pub text_status: bool,
    pub text_numbers: bool,
    pub text_spell: bool,
    pub text_autocorrect: bool,
    pub text_markdown: bool,
    pub text_margins: [i32; 4],
    /// Subtitles' text size, percent of normal (`SUBTITLE_SIZES`).
    pub subtitle_size: u32,
}

/// The sizes the Subtitles menu offers, percent.
pub const SUBTITLE_SIZES: [u32; 5] = [75, 100, 125, 150, 200];

/// The loop lengths the Settings page offers, in seconds.
pub const LOOP_CHOICES: [u32; 6] = [0, 5, 10, 30, 60, u32::MAX];
/// Videos at least this long are resumed where they were left.
pub const RESUME_MIN_SECS: u32 = 300;

pub const EXPLORER_WIDTH: f32 = 320.0;
/// The home page lists this many recent files (Faye, 2026-11-11: it was 12).
pub const RECENT_CAPACITY: usize = 50;
pub const VISITED_CAPACITY: usize = 10;
pub const SIDEBAR_WIDTH: f32 = 210.0;
pub const CACHE_MB: u32 = 512;
/// The choices the Settings page offers for the decode cache.
pub const CACHE_CHOICES: [u32; 5] = [128, 256, 512, 1024, 2048];
pub const STRIP_HEIGHT: f32 = 96.0;
pub const SLIDESHOW_SECONDS: u32 = 4;
pub const STRIP_MIN: f32 = 56.0;
pub const STRIP_MAX: f32 = 480.0;

pub const INFO_WIDTH: f32 = 320.0;
pub const INFO_MIN: f32 = 260.0;
/// The explorer card's narrowest: a tad wider than the info card's (Faye, 2026-11-10), so its columns and footer fit.
pub const EXPLORER_MIN: f32 = 300.0;
pub const INFO_MAX: f32 = 640.0;

impl Default for Settings {
    fn default() -> Self {
        Settings { window: None, remember_window: true, sort: Sort { field: crate::folder::SortField::Date, descending: true }, info_visible: false, info_width: INFO_WIDTH, explorer_visible: false, explorer_width: 0.0, wheel_navigates: false, zoom_center: false, theme: crate::theme::ThemeId::Black, theme_follow: false, theme_dark: crate::theme::ThemeId::Black, theme_light: crate::theme::ThemeId::Light, bars_opaque: false, accent: crate::theme::Accent::DEFAULT, accent_custom: crate::theme::DEFAULT_CUSTOM, checkerboard: true, checkerboard_video: false, cache_mb: CACHE_MB, recents_enabled: true, recents: Vec::new(), strip_visible: false, strip_height: STRIP_HEIGHT, strip_other_files: true, explorer_other_files: true, slideshow_seconds: SLIDESHOW_SECONDS, default_hint_dismissed: false, bookmarks: Vec::new(), visited: Vec::new(), explorer_group: crate::explorer::Group::Mixed, browse_share: 0.0, explorer_columns: crate::explorer::ColKind::defaults(), sidebar_width: SIDEBAR_WIDTH, explorer_subfolders: false, explorer_height: 1, explorer_xsort: None, explorer_layout: crate::explorer::Layout::List, explorer_column_w: crate::explorer::COLUMN_W, views: Vec::new(), sidebar_order: crate::explorer::SideSection::DEFAULT.to_vec(), sidebar_closed: Vec::new(), collection_order: Vec::new(), tabs: Vec::new(), tab_active: 0, tab_layouts: Vec::new(), date_show_date: true, date_show_time: true, date_two_digit_year: false, date_pad: true, date_align: true, video_volume: 0.8, video_muted: false, video_autoplay: true, video_frames: false, video_passthrough: false, video_loop_secs: 10, video_start_muted: false, video_resume: true, audio_spectrum: false, text_wrap: true, text_font: "Consolas".into(), text_font_weight: 400, text_font_italic: false, text_font_pt: 10.5, text_status: true, text_numbers: true, text_spell: true, text_autocorrect: true, text_markdown: true, text_margins: [750, 1000, 750, 1000], subtitle_size: 100 }
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

    /// The explorer listed a folder: to the front of its History. False when it was already first.
    pub fn push_visited(&mut self, path: &std::path::Path) -> bool {
        if self.visited.first().is_some_and(|f| same_file(f, path)) {
            return false;
        }
        self.visited.retain(|r| !same_file(r, path));
        self.visited.insert(0, path.to_path_buf());
        self.visited.truncate(VISITED_CAPACITY);
        true
    }

    pub fn is_bookmarked(&self, path: &std::path::Path) -> bool {
        self.bookmarks.iter().any(|b| same_file(b, path))
    }

    /// Adds or removes a bookmark; returns whether it's bookmarked now.
    pub fn toggle_bookmark(&mut self, path: &std::path::Path) -> bool {
        if self.is_bookmarked(path) {
            self.bookmarks.retain(|b| !same_file(b, path));
            false
        } else {
            self.bookmarks.push(path.to_path_buf());
            true
        }
    }

    /// While the theme follows Windows, picks the dark or light one to match its app mode. True if that changed
    /// the theme (Snapper's).
    pub fn follow_windows(&mut self) -> bool {
        if !self.theme_follow {
            return false;
        }
        let t = if crate::theme::windows_light() { self.theme_light } else { self.theme_dark };
        std::mem::replace(&mut self.theme, t) != t
    }

    /// The explorer's default view (a folder without a saved view of its own shows it).
    pub fn default_view(&self) -> crate::explorer::View {
        crate::explorer::View {
            group: self.explorer_group,
            subfolders: self.explorer_subfolders,
            other_files: self.explorer_other_files,
            height: self.explorer_height,
            columns: self.explorer_columns.clone(),
            sort: self.sort,
            xsort: self.explorer_xsort,
            layout: self.explorer_layout,
            column_w: self.explorer_column_w,
        }
    }

    pub fn set_default_view(&mut self, v: &crate::explorer::View) {
        self.explorer_group = v.group;
        self.explorer_subfolders = v.subfolders;
        self.explorer_other_files = v.other_files;
        self.explorer_height = v.height;
        self.explorer_columns = v.columns.clone();
        self.sort = v.sort;
        self.explorer_xsort = v.xsort;
        self.explorer_layout = v.layout;
        self.explorer_column_w = v.column_w;
    }

    /// A folder was renamed or moved through Looker: bookmarks, History and saved views at or under it follow.
    pub fn folder_moved(&mut self, from: &std::path::Path, to: &std::path::Path) -> bool {
        let mut changed = false;
        for p in self.bookmarks.iter_mut().chain(self.visited.iter_mut()).chain(self.views.iter_mut().map(|v| &mut v.path)) {
            if let Some(n) = crate::explorer::rebase(p, from, to) {
                *p = n;
                changed = true;
            }
        }
        for t in &mut self.tabs {
            if let Some(n) = crate::explorer::rebase(std::path::Path::new(t.as_str()), from, to) {
                *t = n.display().to_string();
                changed = true;
            }
        }
        changed
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
            "sort" => s.sort = Sort::parse(v).unwrap_or(Settings::default().sort),
            "info_visible" => s.info_visible = v == "1",
            "info_width" => s.info_width = v.parse::<f32>().map_or(INFO_WIDTH, |w| w.clamp(INFO_MIN, INFO_MAX)),
            "explorer_visible" => s.explorer_visible = v == "1",
            "explorer_width" => s.explorer_width = v.parse::<f32>().map_or(0.0, |w| if w <= 0.0 || w == EXPLORER_WIDTH { 0.0 } else { w.clamp(EXPLORER_MIN, INFO_MAX) }),
            "wheel" => s.wheel_navigates = v == "navigate",
            "zoom_anchor" => s.zoom_center = v == "center",
            "video_volume" => s.video_volume = v.parse::<f32>().map_or(0.8, |x| x.clamp(0.0, 1.0)),
            "video_muted" => s.video_muted = v == "1",
            "audio_spectrum" => s.audio_spectrum = v == "1",
            "text_wrap" => s.text_wrap = v != "0",
            "text_font" if !v.trim().is_empty() => s.text_font = v.trim().to_string(),
            "text_font_weight" => s.text_font_weight = v.parse::<i32>().map_or(400, |w| w.clamp(100, 900)),
            "text_font_italic" => s.text_font_italic = v == "1",
            "text_font_pt" => s.text_font_pt = v.parse::<f32>().map_or(10.5, |p| p.clamp(6.0, 96.0)),
            "text_status" => s.text_status = v != "0",
            "text_numbers" => s.text_numbers = v != "0",
            "text_spell" => s.text_spell = v != "0",
            "text_autocorrect" => s.text_autocorrect = v != "0",
            "text_markdown" => s.text_markdown = v != "0",
            "text_margins" => {
                let m: Vec<i32> = v.split(',').filter_map(|x| x.trim().parse().ok()).collect();
                if m.len() == 4 {
                    s.text_margins = [m[0], m[1], m[2], m[3]];
                }
            }
            "video_autoplay" => s.video_autoplay = v != "0",
            "video_frames" => s.video_frames = v != "0",
            "video_passthrough" => s.video_passthrough = v != "0",
            "video_loop" => s.video_loop_secs = if v == "always" { u32::MAX } else { v.parse().unwrap_or(10) },
            "video_start_muted" => s.video_start_muted = v == "1",
            "video_resume" => s.video_resume = v != "0",
            "subtitle_size" => s.subtitle_size = v.parse().unwrap_or(100).clamp(25, 400),
            "theme" => s.theme = crate::theme::ThemeId::parse(v).unwrap_or(crate::theme::ThemeId::Black),
            "theme_follow" => s.theme_follow = v != "0",
            "theme_dark" => s.theme_dark = crate::theme::ThemeId::parse(v).filter(|t| t.theme().dark).unwrap_or(crate::theme::ThemeId::Black),
            "theme_light" => s.theme_light = crate::theme::ThemeId::parse(v).filter(|t| !t.theme().dark).unwrap_or(crate::theme::ThemeId::Light),
            "bars_opaque" => s.bars_opaque = v != "0",
            "accent" => s.accent = crate::theme::Accent::parse(v).unwrap_or(crate::theme::Accent::DEFAULT),
            "accent_custom" => s.accent_custom = u32::from_str_radix(v.trim_start_matches('#'), 16).map_or(crate::theme::DEFAULT_CUSTOM, |c| c & 0xFF_FFFF),
            "checkerboard" => s.checkerboard = v != "0",
            "checkerboard_video" => s.checkerboard_video = v != "0",
            "cache_mb" => s.cache_mb = v.parse::<u32>().map_or(CACHE_MB, |m| m.clamp(128, 2048)),
            "recents_enabled" => s.recents_enabled = v != "0",
            "recent" if !v.is_empty() && s.recents.len() < RECENT_CAPACITY => s.recents.push(PathBuf::from(v)),
            "strip_visible" => s.strip_visible = v == "1",
            "strip_height" => s.strip_height = v.parse::<f32>().map_or(STRIP_HEIGHT, |h| h.clamp(STRIP_MIN, STRIP_MAX)),
            // Both always on since 2026-11-10 (their toggles went): an old "off" reads as on.
            "strip_other_files" | "explorer_other_files" => {}
            "default_hint_dismissed" => s.default_hint_dismissed = v == "1",
            "slideshow_seconds" => s.slideshow_seconds = v.parse::<u32>().map_or(SLIDESHOW_SECONDS, |n| n.clamp(1, 120)),
            "bookmark" if !v.is_empty() => s.bookmarks.push(PathBuf::from(v)),
            "visited" if !v.is_empty() && s.visited.len() < VISITED_CAPACITY => s.visited.push(PathBuf::from(v)),
            "explorer_group" => s.explorer_group = crate::explorer::Group::parse(v).unwrap_or_default(),
            "sidebar_width" => s.sidebar_width = v.parse::<f32>().map_or(SIDEBAR_WIDTH, |w| w.clamp(150.0, 420.0)),
            "explorer_columns" => s.explorer_columns = crate::explorer::ColKind::parse_list(v),
            "explorer_subfolders" => s.explorer_subfolders = v == "1",
            "explorer_height" => s.explorer_height = v.parse::<u8>().map_or(1, |h| h.clamp(1, crate::explorer::HEIGHT_MAX)),
            "explorer_xsort" => s.explorer_xsort = crate::explorer::parse_xsort(v),
            // Before views kept their sort: the default's at the time (its `sort=` line comes first).
            "view" => s.views.extend(crate::explorer::SavedView::parse_v1(v, s.sort)),
            "view2" => s.views.extend(crate::explorer::SavedView::parse(v)),
            "view3" => s.views.extend(crate::explorer::SavedView::parse3(v)),
            "explorer_layout" => (s.explorer_layout, s.explorer_column_w) = crate::explorer::Layout::parse(v),
            "sidebar_order" => s.sidebar_order = crate::explorer::SideSection::parse_list(v),
            "sidebar_closed" => s.sidebar_closed = crate::explorer::SideSection::parse_set(v),
            "collection_order" => s.collection_order = v.split(',').filter_map(|k| k.trim().parse::<u8>().ok()).collect(),
            "tab" if !v.is_empty() && s.tabs.len() < 64 => s.tabs.push(v.to_string()),
            "tab_active" => s.tab_active = v.parse().unwrap_or(0),
            "tab_layouts" => s.tab_layouts = v.split(',').filter(|k| !k.is_empty()).take(64).map(|k| crate::explorer::Layout::parse(k).0).collect(),
            "explorer_dates" => {
                s.date_show_date = v.contains('d');
                s.date_show_time = v.contains('t');
                s.date_two_digit_year = v.contains('y');
                s.date_pad = !v.contains('u');
                s.date_align = !v.contains('n');
            }
            "browse_share" => s.browse_share = v.parse::<f32>().map_or(0.0, |f| if (0.2..=0.95).contains(&f) { f } else { 0.0 }),
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
    out.push_str(&format!("video_volume={:.2}\n", s.video_volume));
    out.push_str(&format!("video_muted={}\n", s.video_muted as i32));
    out.push_str(&format!("audio_spectrum={}\n", s.audio_spectrum as i32));
    out.push_str(&format!("text_wrap={}\n", s.text_wrap as i32));
    out.push_str(&format!("text_font={}\n", s.text_font));
    out.push_str(&format!("text_font_weight={}\n", s.text_font_weight));
    out.push_str(&format!("text_font_italic={}\n", s.text_font_italic as i32));
    out.push_str(&format!("text_font_pt={}\n", s.text_font_pt));
    out.push_str(&format!("text_status={}\n", s.text_status as i32));
    out.push_str(&format!("text_numbers={}\n", s.text_numbers as i32));
    out.push_str(&format!("text_spell={}\n", s.text_spell as i32));
    out.push_str(&format!("text_autocorrect={}\n", s.text_autocorrect as i32));
    out.push_str(&format!("text_markdown={}\n", s.text_markdown as i32));
    let m = s.text_margins;
    out.push_str(&format!("text_margins={},{},{},{}\n", m[0], m[1], m[2], m[3]));
    out.push_str(&format!("video_autoplay={}\n", s.video_autoplay as i32));
    out.push_str(&format!("video_frames={}\n", s.video_frames as i32));
    out.push_str(&format!("video_passthrough={}\n", s.video_passthrough as i32));
    if s.video_loop_secs == u32::MAX {
        out.push_str("video_loop=always\n");
    } else {
        out.push_str(&format!("video_loop={}\n", s.video_loop_secs));
    }
    out.push_str(&format!("video_start_muted={}\n", s.video_start_muted as i32));
    out.push_str(&format!("video_resume={}\n", s.video_resume as i32));
    out.push_str(&format!("subtitle_size={}\n", s.subtitle_size));
    out.push_str(&format!("theme={}\n", s.theme.name().to_lowercase()));
    out.push_str(&format!("theme_follow={}\n", s.theme_follow as i32));
    out.push_str(&format!("theme_dark={}\n", s.theme_dark.name().to_lowercase()));
    out.push_str(&format!("theme_light={}\n", s.theme_light.name().to_lowercase()));
    out.push_str(&format!("bars_opaque={}\n", s.bars_opaque as i32));
    out.push_str(&format!("accent={}\n", s.accent.name().to_lowercase()));
    out.push_str(&format!("accent_custom={:06X}\n", s.accent_custom));
    out.push_str(&format!("checkerboard={}\n", s.checkerboard as i32));
    out.push_str(&format!("checkerboard_video={}\n", s.checkerboard_video as i32));
    out.push_str(&format!("cache_mb={}\n", s.cache_mb));
    out.push_str(&format!("recents_enabled={}\n", s.recents_enabled as i32));
    for r in &s.recents {
        out.push_str(&format!("recent={}\n", r.display()));
    }
    out.push_str(&format!("strip_visible={}\n", s.strip_visible as i32));
    out.push_str(&format!("strip_height={}\n", s.strip_height.round()));
    out.push_str(&format!("strip_other_files={}\n", s.strip_other_files as i32));
    out.push_str(&format!("explorer_other_files={}\n", s.explorer_other_files as i32));
    out.push_str(&format!("slideshow_seconds={}\n", s.slideshow_seconds));
    out.push_str(&format!("default_hint_dismissed={}\n", s.default_hint_dismissed as i32));
    for b in &s.bookmarks {
        out.push_str(&format!("bookmark={}\n", b.display()));
    }
    for v in &s.visited {
        out.push_str(&format!("visited={}\n", v.display()));
    }
    out.push_str(&format!("explorer_group={}\n", s.explorer_group.key()));
    out.push_str(&format!("browse_share={:.3}\n", s.browse_share));
    out.push_str(&format!("explorer_columns={}\n", crate::explorer::ColKind::format(&s.explorer_columns)));
    out.push_str(&format!("sidebar_width={}\n", s.sidebar_width.round()));
    out.push_str(&format!("explorer_subfolders={}\n", s.explorer_subfolders as i32));
    out.push_str(&format!("explorer_height={}\n", s.explorer_height));
    out.push_str(&format!("explorer_xsort={}\n", crate::explorer::format_xsort(s.explorer_xsort)));
    out.push_str(&format!("explorer_layout={}\n", s.explorer_layout.format(s.explorer_column_w)));
    for v in &s.views {
        out.push_str(&format!("view3={}\n", v.format()));
    }
    out.push_str(&format!("sidebar_order={}\n", crate::explorer::SideSection::format(&s.sidebar_order)));
    out.push_str(&format!("sidebar_closed={}\n", crate::explorer::SideSection::format(&s.sidebar_closed)));
    out.push_str(&format!("collection_order={}\n", s.collection_order.iter().map(|k| k.to_string()).collect::<Vec<_>>().join(",")));
    for t in &s.tabs {
        out.push_str(&format!("tab={t}\n"));
    }
    out.push_str(&format!("tab_active={}\n", s.tab_active));
    out.push_str(&format!("tab_layouts={}\n", s.tab_layouts.iter().map(|l| if *l == crate::explorer::Layout::Columns { "columns" } else { "list" }).collect::<Vec<_>>().join(",")));
    out.push_str(&format!(
        "explorer_dates={}{}{}{}{}\n",
        if s.date_show_date { "d" } else { "" },
        if s.date_show_time { "t" } else { "" },
        if s.date_two_digit_year { "y" } else { "" },
        if s.date_pad { "" } else { "u" },
        if s.date_align { "" } else { "n" }
    ));
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
    // The C# app's sort if it saved one; else the default (newest first).
    if let Some(f) = int("SortField") {
        s.sort = Sort {
            field: match f {
                1 => SortField::Date,
                2 => SortField::Size,
                _ => SortField::Name,
            },
            descending: int("SortDirection") == Some(1),
        };
    }
    s.strip_visible = flag("StripVisible").unwrap_or(false);
    s.strip_height = int("StripHeight").map_or(STRIP_HEIGHT, |h| (h as f32).clamp(STRIP_MIN, STRIP_MAX));
    s.info_visible = flag("InfoVisible").unwrap_or(false);
    s.info_width = int("InfoWidth").map_or(INFO_WIDTH, |w| (w as f32).clamp(INFO_MIN, INFO_MAX));
    s.explorer_visible = flag("ExplorerVisible").unwrap_or(false);
    s.explorer_width = int("ExplorerWidth").map_or(0.0, |w| if w as f32 == EXPLORER_WIDTH { 0.0 } else { (w as f32).clamp(EXPLORER_MIN, INFO_MAX) });
    s.slideshow_seconds = int("SlideshowSeconds").map_or(SLIDESHOW_SECONDS, |n| n.clamp(1, 120) as u32);
    s.cache_mb = int("CacheBudgetMB").map_or(CACHE_MB, |m| m.clamp(128, 2048) as u32);
    s.wheel_navigates = int("WheelMode") == Some(1);
    s.zoom_center = int("ZoomAnchor") == Some(1);
    s.theme = if int("Theme") == Some(1) { crate::theme::ThemeId::Dark } else { crate::theme::ThemeId::Black };
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
            theme: crate::theme::ThemeId::Dark,
            theme_follow: true,
            theme_dark: crate::theme::ThemeId::Dark,
            theme_light: crate::theme::ThemeId::Paper,
            bars_opaque: true,
            accent: crate::theme::Accent::Sky,
            accent_custom: 0x123456,
            checkerboard: false,
            checkerboard_video: true,
            cache_mb: 1024,
            recents_enabled: true,
            recents: vec![PathBuf::from(r"C:\a b\one.jpg"), PathBuf::from(r"D:\two.png")],
            strip_visible: true,
            strip_height: 160.0,
            strip_other_files: true,
            explorer_other_files: true,
            slideshow_seconds: 9,
            default_hint_dismissed: true,
            bookmarks: vec![PathBuf::from(r"S:\SFX Library"), PathBuf::from(r"C:\Users\me\Pictures")],
            visited: vec![PathBuf::from(r"C:\Users\me\Pictures\2026")],
            explorer_group: crate::explorer::Group::Mixed,
            browse_share: 0.625,
            explorer_columns: vec![(crate::explorer::ColKind::Name, 0.0), (crate::explorer::ColKind::Taken, 150.0)],
            sidebar_width: 260.0,
            explorer_subfolders: true,
            explorer_height: 3,
            explorer_xsort: None,
            explorer_layout: crate::explorer::Layout::Columns,
            explorer_column_w: 300.0,
            views: vec![
                crate::explorer::SavedView {
                    path: PathBuf::from(r"C:\Users\me\Pictures; odd"),
                    scope: crate::explorer::Scope::Tree,
                    view: crate::explorer::View { group: crate::explorer::Group::FilesFirst, subfolders: true, other_files: true, height: 2, columns: crate::explorer::ColKind::defaults(), sort: Sort { field: crate::folder::SortField::Size, descending: true }, xsort: Some((crate::explorer::ColKind::Camera, false)), layout: crate::explorer::Layout::Columns, column_w: 280.0 },
                },
                crate::explorer::SavedView { path: PathBuf::from(r"D:\"), scope: crate::explorer::Scope::Off, view: crate::explorer::View::default() },
            ],
            sidebar_order: vec![crate::explorer::SideSection::History, crate::explorer::SideSection::Places, crate::explorer::SideSection::Bookmarks, crate::explorer::SideSection::Network, crate::explorer::SideSection::Collections],
            sidebar_closed: vec![crate::explorer::SideSection::Places],
            collection_order: vec![3, 0, 1],
            tabs: vec![r"C:\Users\me\Pictures".into(), "::favourites".into()],
            tab_active: 1,
            tab_layouts: vec![crate::explorer::Layout::Columns, crate::explorer::Layout::List],
            date_show_date: false,
            date_show_time: true,
            date_two_digit_year: true,
            date_pad: false,
            date_align: false,
            video_volume: 0.35,
            video_muted: true,
            video_autoplay: false,
            video_frames: false,
            video_passthrough: false,
            video_loop_secs: u32::MAX,
            video_start_muted: true,
            video_resume: false,
            audio_spectrum: true,
            text_wrap: false,
            text_font: "Cascadia Mono".into(),
            text_font_weight: 700,
            text_font_italic: true,
            text_font_pt: 12.0,
            text_status: false,
            text_numbers: false,
            text_spell: false,
            text_autocorrect: false,
            text_markdown: false,
            text_margins: [500, 600, 700, 800],
            subtitle_size: 150,
        };
        assert_eq!(parse(&format(&s)), s);
        assert_eq!(parse(&format(&Settings::default())), Settings::default());
    }

    #[test]
    fn recents_move_to_the_front_and_cap() {
        let mut s = Settings::default();
        let last = RECENT_CAPACITY + 2;
        for i in 0..=last {
            assert!(s.push_recent(&PathBuf::from(format!(r"C:\p\{i}.jpg"))));
        }
        assert_eq!(s.recents.len(), RECENT_CAPACITY);
        assert!(!s.push_recent(&PathBuf::from(format!(r"c:\P\{last}.JPG")))); // already first
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
        assert!(s.strip_visible && s.theme == crate::theme::ThemeId::Dark && s.wheel_navigates && !s.zoom_center);
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
