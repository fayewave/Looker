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
}

fn item(action: Action, glyph: u16, label: &str, accel: Option<&'static str>, enabled: bool) -> Entry<Action> {
    Entry::Item(MenuItem { action, glyph: Some(glyph), label: label.into(), accel, checked: false, enabled })
}

fn radio(action: Action, label: &str, checked: bool) -> Entry<Action> {
    Entry::Item(MenuItem { action, glyph: None, label: label.into(), accel: None, checked, enabled: true })
}

impl App {
    fn context_entries(&self) -> Vec<Entry<Action>> {
        let has = self.viewer.current.is_some();
        let rotate = self.can_rotate();
        vec![
            item(Action::RotateRight, 0xE7AD, "Rotate right", Some("Ctrl+R"), rotate),
            item(Action::RotateLeft, 0xE7AD, "Rotate left", Some("Ctrl+Shift+R"), rotate),
            Entry::Separator,
            item(Action::CopyImage, 0xE8C8, "Copy image", Some("Ctrl+C"), has),
            item(Action::CopyPath, 0xE71B, "Copy path", Some("Ctrl+Shift+C"), has),
            item(Action::Rename, 0xE8AC, "Rename", Some("F2"), has),
            item(Action::Delete, 0xE74D, "Delete", Some("Del"), has),
            Entry::Separator,
            item(Action::Wallpaper, 0xE91B, "Set as wallpaper", None, has),
            item(Action::Reveal, 0xEC50, "Reveal in File Explorer", Some("Ctrl+E"), has),
            Entry::Separator,
            item(Action::ToggleExplorer, 0xE8B7, "File explorer", Some("E"), has),
            item(Action::ToggleStrip, 0xE8FD, "Thumbnail strip", Some("T"), has),
            item(Action::ToggleInfo, 0xE946, "Info panel", Some("I"), has),
        ]
    }

    fn sort_entries(&self) -> Vec<Entry<Action>> {
        let s = self.settings.sort;
        vec![
            radio(Action::SortField(SortField::Name), "Name", s.field == SortField::Name),
            radio(Action::SortField(SortField::Date), "Date modified", s.field == SortField::Date),
            radio(Action::SortField(SortField::Size), "Size", s.field == SortField::Size),
            Entry::Separator,
            radio(Action::SortDescending(false), "Ascending", !s.descending),
            radio(Action::SortDescending(true), "Descending", s.descending),
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
        self.menu = Some((MenuKind::Sort, Menu::open(g, self.sort_entries(), r.left, r.bottom + 4.0, rect(0.0, 0.0, w, h))));
        self.hide_tooltip();
        self.invalidate();
    }

    pub(super) fn close_menu(&mut self) {
        if self.menu.take().is_some() {
            self.invalidate();
        }
    }

    /// The menu for one explorer row: what Explorer offers for that kind of item.
    pub(super) fn open_item_menu(&mut self, path: PathBuf, kind: crate::explorer::Kind, x: f32, y: f32) {
        use crate::explorer::Kind;
        let mut entries = Vec::new();
        match kind {
            Kind::Folder | Kind::Drive => {
                entries.push(item(Action::OpenItem, 0xE8B7, "Open folder", None, true));
                entries.push(Entry::Separator);
            }
            Kind::Image => {
                entries.push(item(Action::OpenItem, 0xE8A7, "Open", None, true));
                entries.push(item(Action::Rename, 0xE8AC, "Rename", None, true));
                entries.push(item(Action::Delete, 0xE74D, "Delete", None, true));
                entries.push(Entry::Separator);
            }
            Kind::Other => {}
        }
        entries.push(item(Action::CopyPath, 0xE71B, "Copy path", None, true));
        entries.push(item(Action::Reveal, 0xEC50, "Reveal in File Explorer", None, true));
        let Some(g) = &self.gfx else { return };
        let (w, h) = self.size_dip();
        self.menu = Some((MenuKind::Item(path), Menu::open(g, entries, x, y, rect(0.0, 0.0, w, h))));
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
        self.menu = Some((MenuKind::Crumbs(hidden), Menu::open(g, entries, r.left, r.bottom + 4.0, rect(0.0, 0.0, w, h))));
        self.hide_tooltip();
        self.invalidate();
    }

    /// Runs the item at `index` of the open menu (if enabled) and closes the menu.
    pub(super) fn activate_menu(&mut self, index: usize) {
        let Some(action) = self.menu.as_ref().and_then(|(_, m)| m.action_at(index)) else { return };
        let kind = self.menu.take().map(|(k, _)| k);
        self.invalidate();
        match kind {
            Some(MenuKind::Item(path)) => self.run_item_action(action, path),
            Some(MenuKind::Crumbs(places)) => {
                if let Action::Crumb(i) = action {
                    if let Some(p) = places.get(i) {
                        self.explorer_crumb(p.clone());
                    }
                }
            }
            _ => self.run_action(action),
        }
    }

    /// An explorer row's menu: the same actions, on that file.
    fn run_item_action(&mut self, a: Action, path: PathBuf) {
        match a {
            Action::OpenItem if path.is_dir() => self.open_folder(path),
            Action::OpenItem => self.open_file(path),
            Action::Rename => self.begin_rename_path(path, false),
            Action::Delete => self.confirm_delete_path(path, false),
            Action::CopyPath => self.copy_path_of(&path),
            Action::Reveal => self.reveal_path(&path),
            _ => {}
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
            Action::OpenItem | Action::Crumb(_) => {}
        }
    }

    fn set_sort(&mut self, sort: Sort) {
        if sort == self.settings.sort {
            return;
        }
        self.settings.sort = sort;
        settings::save(&self.settings);
        self.viewer.set_sort(sort);
        self.explorer.resort(sort);
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
    /// An explorer row's menu, for this path.
    Item(PathBuf),
    /// The breadcrumb overflow, holding these places.
    Crumbs(Vec<crate::explorer::Place>),
}
