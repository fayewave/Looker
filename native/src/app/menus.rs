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
            item(Action::ToggleExplorer, 0xE8B7, "File explorer", Some("E"), false),
            item(Action::ToggleStrip, 0xE8FD, "Thumbnail strip", Some("T"), false),
            item(Action::ToggleInfo, 0xE946, "Info panel", Some("I"), false),
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

    /// Runs the item at `index` of the open menu (if enabled) and closes the menu.
    pub(super) fn activate_menu(&mut self, index: usize) {
        let Some(action) = self.menu.as_ref().and_then(|(_, m)| m.action_at(index)) else { return };
        self.close_menu();
        self.run_action(action);
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
            Action::ToggleExplorer
            | Action::ToggleStrip
            | Action::ToggleInfo => {}
        }
    }

    fn set_sort(&mut self, sort: Sort) {
        if sort == self.settings.sort {
            return;
        }
        self.settings.sort = sort;
        settings::save(&self.settings);
        self.viewer.set_sort(sort);
        let field = match sort.field {
            SortField::Name => "Name",
            SortField::Date => "Date modified",
            SortField::Size => "Size",
        };
        self.show_toast(format!("Sort: {field} {}", if sort.descending { "\u{2193}" } else { "\u{2191}" }), false);
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum MenuKind {
    Context,
    Sort,
}
