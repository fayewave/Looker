//! Menus: the viewport's right-click menu and the toolbar's sort drop-down (as in MainWindow.xaml), what
//! their items do, and the clipboard. Items for features that haven't landed yet are shown disabled, in the
//! C# app's order, so the menu doesn't change shape as they arrive.

use super::*;
use crate::folder::{Sort, SortField};
use crate::ui::{Entry, Menu, MenuItem};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Action {
    RotateRight,
    RotateLeft,
    CopyImage,
    CopyPath,
    Rename,
    Delete,
    Wallpaper,
    Reveal,
    ToggleExplorer,
    ToggleStrip,
    ToggleInfo,
    SortField(SortField),
    SortDescending(bool),
    /// The explorer card: open the item the menu is for (a file in the viewer, a folder as the viewer's folder).
    OpenItem,
    /// A crumb folded into "…".
    Crumb(usize),
    /// The landing page: forget one recent file.
    RemoveRecent,
    /// A ComboBox's item, by index.
    Choose(usize),
    /// The video bar's Settings menu: loop on/off.
    ToggleVideoLoop,
    /// The context menu: play this audio stream of the video.
    AudioTrack(i32),
    /// The context menu's Video track: show this stream.
    VideoTrack(i32),
    /// The video bar's times: picture numbers (true) or timecode.
    VideoFrames(bool),
    /// The context menu's Subtitles: none, one of the video's streams, a sidecar (by its place in the list), a
    /// file picked by hand, the delay (-1 earlier, +1 later, 0 none), the text's size (percent).
    SubOff,
    SubStream(i32),
    SubFile(usize),
    SubAdd,
    SubDelay(i32),
    SubSize(u16),
    // The explorer's (explorer_ops.rs).
    OpenInViewer,
    Cut,
    CopyFiles,
    Paste,
    PasteInto,
    Favourite,
    Colour(Option<u8>),
    Bookmark,
    NewFolder,
    Refresh,
    Properties,
    SelectAll,
    HistoryJump(usize),
    Group(crate::explorer::Group),
    ToggleSubfolders,
    /// The date columns' options: 0 the date, 1 the time, 2 a two-digit year.
    DateOption(u8),
    NewTab,
    CloseTab,
    CloseOtherTabs,
    OpenInNewTab,
    RowHeight(u8),
    SavedView(Option<crate::explorer::Scope>),
    SetDefaultView,
    Undo,
    Redo,
    // The column header menu's.
    ColShift(bool),
    ColFit,
    ColClose,
    ColFitAll,
    ColRestore,
    ColToggle(crate::explorer::ColKind),
}

fn item(action: Action, glyph: u16, label: &str, accel: Option<&'static str>, enabled: bool) -> Entry<Action> {
    Entry::Item(MenuItem { tint: None, action, glyph: Some(glyph), label: label.into(), accel, check: None, enabled, current: false })
}

/// An item with a tickbox at the right: ticked when on (a setting, or the one picked of several).
pub(super) fn check(action: Action, label: &str, on: bool) -> Entry<Action> {
    Entry::Item(MenuItem { tint: None, action, glyph: None, label: label.into(), accel: None, check: Some(on), enabled: true, current: false })
}

impl App {
    fn context_entries(&self) -> Vec<Entry<Action>> {
        let has = self.viewer.current.is_some();
        // A file Looker can't open covers the photo: the file actions are for it, the image ones are off.
        let image = has && self.placeholder.is_none();
        let file = has || self.placeholder.is_some();
        let rotate = image && self.can_rotate();
        // Set as wallpaper for photos only (not videos, documents or 3D models: Faye, 2026-11-10).
        let photo = image
            && self.viewer.current_entry().is_some_and(|e| !matches!(e.format, format::Format::Video | format::Format::Pdf | format::Format::Model | format::Format::Mtl));
        let mut e = vec![
            item(Action::RotateRight, 0xE7AD, "Rotate right", Some("Ctrl+R"), rotate),
            item(Action::RotateLeft, 0xE7AD, "Rotate left", Some("Ctrl+Shift+R"), rotate),
            Entry::Separator,
            item(Action::CopyImage, 0xE8C8, "Copy", Some("Ctrl+C"), image),
            item(Action::CopyPath, 0xE71B, "Copy path", Some("Ctrl+Shift+C"), file),
            item(Action::Rename, 0xE8AC, "Rename", Some("F2"), file),
            item(Action::Delete, 0xE74D, "Delete", Some("Del"), file),
            Entry::Separator,
        ];
        if photo {
            e.push(item(Action::Wallpaper, 0xE91B, "Set as wallpaper", None, true));
        }
        e.push(item(Action::Reveal, 0xEC50, "Reveal in Explorer\u{2026}", Some("Ctrl+Shift+E"), file));
        // A video: which audio stream plays, in a submenu, there even with one (Faye, 2026-11-10).
        if self.video_shown() && self.player_ref().is_some() {
            let tracks = self.video_audio_tracks();
            let rows: Vec<Entry<Action>> = if tracks.is_empty() {
                vec![Entry::Item(MenuItem { tint: None, action: Action::AudioTrack(-1), glyph: None, label: "No audio".into(), accel: None, check: None, enabled: false, current: false })]
            } else {
                tracks.into_iter().map(|(stream, label, playing)| check(Action::AudioTrack(stream), &label, playing)).collect()
            };
            e.push(Entry::Separator);
            // And which picture stream shows, as the audio (Faye, 2026-11-10), there even with one.
            let pictures: Vec<Entry<Action>> =
                self.video_picture_tracks().into_iter().map(|(stream, label, showing)| check(Action::VideoTrack(stream), &label, showing)).collect();
            if !pictures.is_empty() {
                e.push(Entry::Sub(Some(0xE714), "Video track".into(), pictures));
            }
            e.push(Entry::Sub(Some(0xE8D6), "Audio track".into(), rows));
            // Subtitles beside them: off, the video's own, the files beside it, one picked by hand; delay, size.
            e.push(Entry::Sub(Some(0xE7F0), "Subtitles".into(), self.subtitle_entries()));
        }
        e.extend([
            Entry::Separator,
            item(Action::ToggleExplorer, 0xE8B7, "File explorer", Some("Ctrl+E"), has),
            item(Action::ToggleStrip, 0xE8FD, "Thumbnail strip", Some("Ctrl+Shift+T"), has),
            item(Action::ToggleInfo, 0xE946, "Info panel", Some("Ctrl+I"), has),
        ]);
        e
    }

    fn sort_entries(&self) -> Vec<Entry<Action>> {
        let s = self.settings.sort;
        vec![
            check(Action::SortField(SortField::Name), "Name", s.field == SortField::Name),
            check(Action::SortField(SortField::Date), "Date modified", s.field == SortField::Date),
            check(Action::SortField(SortField::Size), "Size", s.field == SortField::Size),
            Entry::Separator,
            check(Action::SortDescending(false), "Ascending", !s.descending),
            check(Action::SortDescending(true), "Descending", s.descending),
        ]
    }

    pub(super) fn open_context_menu(&mut self, x: f32, y: f32) {
        let Some(g) = &self.gfx else { return };
        let (w, h) = self.size_dip();
        self.menu = Some((MenuKind::Context, Menu::open(g, self.context_entries(), x, y, rect(0.0, 0.0, w, h))));
        self.hide_tooltip();
        self.invalidate();
    }

    /// The sort drop-down, under the sort button; a second click on the button closes it.
    pub(super) fn toggle_sort_menu(&mut self) {
        if matches!(self.menu, Some((MenuKind::Sort, _))) {
            self.close_menu();
            return;
        }
        let Some(r) = self.hits.rect_of(Hit::Tool(Tool::Sort)) else { return };
        let Some(g) = &self.gfx else { return };
        let (w, h) = self.size_dip();
        self.menu = Some((MenuKind::Sort, Menu::below(g, self.sort_entries(), r, rect(0.0, 0.0, w, h))));
        self.hide_tooltip();
        self.invalidate();
    }

    pub(super) fn close_menu(&mut self) {
        if self.menu.take().is_some() {
            self.invalidate();
        }
    }

    /// The thumbnail strip's menu.
    pub(super) fn open_strip_menu(&mut self, x: f32, y: f32) {
        let entries = vec![
            item(Action::ToggleStrip, 0xE8FD, "Thumbnail strip", Some("Ctrl+Shift+T"), true),
        ];
        let Some(g) = &self.gfx else { return };
        let (w, h) = self.size_dip();
        self.menu = Some((MenuKind::Context, Menu::open(g, entries, x, y, rect(0.0, 0.0, w, h))));
        self.hide_tooltip();
        self.invalidate();
    }

    /// A recent file's menu on the landing page.
    pub(super) fn open_recent_menu(&mut self, i: usize, x: f32, y: f32) {
        let Some(path) = self.recent_path(i) else { return };
        let entries = vec![item(Action::RemoveRecent, 0xE711, "Remove from recent", None, true)];
        let Some(g) = &self.gfx else { return };
        let (w, h) = self.size_dip();
        self.menu = Some((MenuKind::Recent(path), Menu::open(g, entries, x, y, rect(0.0, 0.0, w, h))));
        self.hide_tooltip();
        self.invalidate();
    }

    /// The breadcrumb "…": the folded-away ancestors, top-down.
    pub(super) fn open_crumb_menu(&mut self) {
        let hidden = self.hidden_crumbs();
        let Some(r) = self.hits.rect_of(Hit::CrumbMore) else { return };
        let entries = hidden
            .iter()
            .enumerate()
            .map(|(i, p)| {
                let glyph = if p == &crate::explorer::Place::Computer { 0xE7F4 } else { 0xE8B7 };
                item(Action::Crumb(i), glyph, &p.name(), None, true)
            })
            .collect();
        let Some(g) = &self.gfx else { return };
        let (w, h) = self.size_dip();
        self.menu = Some((MenuKind::Crumbs(hidden), Menu::below(g, entries, r, rect(0.0, 0.0, w, h))));
        self.hide_tooltip();
        self.invalidate();
    }

    /// Runs the item at `index` of the open menu (if enabled) and closes the menu.
    pub(super) fn activate_menu(&mut self, index: usize) {
        let Some(action) = self.menu.as_ref().and_then(|(_, m)| m.action_at(index)) else { return };
        // The video bar's Settings stays open, its ticks moved (Faye, 2026-11-10).
        if matches!(self.menu, Some((MenuKind::VideoSettings, _))) {
            match action {
                Action::Choose(i) => self.set_video_speed(video::SPEEDS[i.min(video::SPEEDS.len() - 1)]),
                Action::ToggleVideoLoop => self.toggle_video_loop(),
                Action::VideoFrames(on) => self.set_video_frames(on),
                _ => {}
            }
            let entries = self.video_settings_entries();
            if let Some((_, m)) = self.menu.as_mut() {
                m.entries = entries;
            }
            self.invalidate();
            return;
        }
        // So does the explorer's Folders and files, as its Height stepper always did (Faye, 2026-11-10).
        if matches!(self.menu, Some((MenuKind::Group, _))) {
            self.run_explorer_action(action, MenuKind::Group);
            if matches!(self.menu, Some((MenuKind::Group, _))) {
                let entries = self.group_entries();
                if let Some((_, m)) = self.menu.as_mut() {
                    m.entries = entries;
                }
            }
            self.invalidate();
            return;
        }
        let kind = self.menu.take().map(|(k, _)| k);
        self.invalidate();
        match kind {
            Some(k @ (MenuKind::Rows(_) | MenuKind::ExplorerBg | MenuKind::Side(_) | MenuKind::History | MenuKind::Group | MenuKind::Columns(_) | MenuKind::Tab(_))) => {
                self.run_explorer_action(action, k)
            }
            Some(MenuKind::Recent(path)) => self.remove_recent(&path),
            Some(MenuKind::VideoTime) => {
                if let Action::VideoFrames(on) = action {
                    self.set_video_frames(on);
                }
            }
            Some(MenuKind::Combo(which)) => {
                if let Action::Choose(i) = action {
                    self.choose(which, i);
                }
            }
            Some(MenuKind::Crumbs(places) | MenuKind::Jump(_, places)) => {
                if let Action::Crumb(i) = action {
                    if let Some(p) = places.get(i) {
                        self.explorer_crumb(p.clone());
                    }
                }
            }
            _ => self.run_action(action),
        }
    }

    /// A colour swatch in the open menu was picked.
    pub(super) fn activate_swatch(&mut self, i: usize, k: usize) {
        let Some(action) = self.menu.as_ref().and_then(|(_, m)| m.swatch_action(i, k)) else { return };
        // A stepper's minus or plus: the menu stays, showing the new value.
        if self.menu.as_ref().is_some_and(|(_, m)| m.is_stepper(i)) {
            let Some(kind) = self.menu.as_ref().map(|(k, _)| k.clone()) else { return };
            self.run_explorer_action(action, kind.clone());
            if kind == MenuKind::Group {
                let entries = self.group_entries();
                if let Some((_, m)) = self.menu.as_mut() {
                    m.entries = entries;
                }
            }
            self.invalidate();
            return;
        }
        let kind = self.menu.take().map(|(k, _)| k);
        self.invalidate();
        if let Some(kind) = kind {
            self.run_explorer_action(action, kind);
        }
    }

    pub(super) fn run_action(&mut self, a: Action) {
        match a {
            Action::CopyPath => self.copy_path(),
            Action::CopyImage => self.copy_image(),
            Action::RotateRight => self.rotate_preview(true),
            Action::RotateLeft => self.rotate_preview(false),
            Action::Delete => self.confirm_delete(false),
            Action::Wallpaper => self.set_wallpaper(),
            Action::Reveal => self.reveal(),
            Action::SortField(f) => self.set_sort(Sort { field: f, ..self.settings.sort }),
            Action::SortDescending(d) => self.set_sort(Sort { descending: d, ..self.settings.sort }),
            Action::Rename => self.begin_rename(false),
            Action::ToggleInfo => self.toggle_info(),
            Action::ToggleStrip => self.toggle_strip(),
            Action::ToggleExplorer => self.toggle_explorer(),
            Action::AudioTrack(s) => self.set_audio_track(s),
            Action::VideoTrack(s) => self.set_video_track(s),
            Action::SubOff => self.set_subtitles(crate::subs::Choice::Off),
            Action::SubStream(s) => self.set_subtitles(crate::subs::Choice::Stream(s)),
            Action::SubFile(i) => {
                if let Some(p) = self.video.as_ref().and_then(|v| v.subs.sidecars.get(i)).map(|s| s.path.clone()) {
                    self.set_subtitles(crate::subs::Choice::File(p));
                }
            }
            Action::SubAdd => self.add_subtitle_file(),
            Action::SubDelay(d) => self.subtitle_delay(d),
            Action::SubSize(s) => self.subtitle_size(s as u32),
            _ => {}
        }
    }

    pub(super) fn set_sort(&mut self, sort: Sort) {
        if sort == self.settings.sort {
            return;
        }
        self.settings.sort = sort;
        settings::save(&self.settings);
        self.viewer.set_sort(sort);
        self.explorer.resort(sort, self.explorer.look.group);
        let field = match sort.field {
            SortField::Name => "Name",
            SortField::Date => "Date modified",
            SortField::Size => "Size",
        };
        self.show_toast(format!("Sort: {field} {}", if sort.descending { "\u{2193}" } else { "\u{2191}" }), false);
    }
}

#[derive(Clone, PartialEq, Debug)]
pub(super) enum MenuKind {
    Context,
    Sort,
    /// The explorer's: a row's menu (on these paths), the list's, a sidebar row's, a crumb's folders, History,
    /// and the folders/files grouping.
    Rows(Vec<PathBuf>),
    ExplorerBg,
    Side(crate::explorer::Place),
    History,
    Group,
    /// A column header's menu, for the column at this index.
    Columns(usize),
    /// An explorer tab's menu, for the tab at this index.
    Tab(usize),
    /// The breadcrumb overflow, holding these places.
    Crumbs(Vec<crate::explorer::Place>),
    /// The card's Bookmarks (false) or Collections (true) drop-down, holding these places.
    Jump(bool, Vec<crate::explorer::Place>),
    /// A recent file on the landing page.
    Recent(PathBuf),
    /// A Settings drop-down.
    Combo(page::Setting),
    /// The video bar's Settings menu: speed, loop and Timecode/Frames.
    VideoSettings,
    /// The menu off the video bar's times: Timecode or Frames.
    VideoTime,
}
