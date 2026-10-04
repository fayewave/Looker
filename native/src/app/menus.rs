//! Menus: the viewport's right-click menu and the toolbar's sort drop-down (as in MainWindow.xaml), what
//! their items do, and the clipboard. Items for features that haven't landed yet are shown disabled, in the
//! C# app's order, so the menu doesn't change shape as they arrive.

use windows::Win32::Foundation::{HANDLE, HGLOBAL};
use windows::Win32::System::DataExchange::{CloseClipboard, EmptyClipboard, OpenClipboard, SetClipboardData};
use windows::Win32::System::Memory::{GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalUnlock};
use windows::Win32::System::Ole::CF_UNICODETEXT;

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
        vec![
            item(Action::RotateRight, 0xE7AD, "Rotate right", Some("Ctrl+R"), false),
            item(Action::RotateLeft, 0xE7AD, "Rotate left", Some("Ctrl+Shift+R"), false),
            Entry::Separator,
            item(Action::CopyImage, 0xE8C8, "Copy image", Some("Ctrl+C"), false),
            item(Action::CopyPath, 0xE71B, "Copy path", Some("Ctrl+Shift+C"), has),
            item(Action::Rename, 0xE8AC, "Rename", Some("F2"), false),
            item(Action::Delete, 0xE74D, "Delete", Some("Del"), false),
            Entry::Separator,
            item(Action::Wallpaper, 0xE91B, "Set as wallpaper", None, false),
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
            Action::Reveal => self.reveal(),
            Action::SortField(f) => self.set_sort(Sort { field: f, ..self.settings.sort }),
            Action::SortDescending(d) => self.set_sort(Sort { descending: d, ..self.settings.sort }),
            Action::RotateRight
            | Action::RotateLeft
            | Action::CopyImage
            | Action::Rename
            | Action::Delete
            | Action::Wallpaper
            | Action::ToggleExplorer
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
        self.invalidate();
    }

    pub(super) fn copy_path(&self) {
        let Some(path) = self.current_path() else { return };
        set_clipboard_text(self.hwnd, &path.to_string_lossy());
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum MenuKind {
    Context,
    Sort,
}

/// Puts UTF-16 text on the clipboard (CF_UNICODETEXT). The clipboard owns the memory once set.
pub(super) fn set_clipboard_text(hwnd: HWND, text: &str) -> bool {
    let mut w: Vec<u16> = text.encode_utf16().collect();
    w.push(0);
    unsafe {
        if OpenClipboard(Some(hwnd)).is_err() {
            return false;
        }
        let _ = EmptyClipboard();
        let ok = (|| {
            let mem: HGLOBAL = GlobalAlloc(GMEM_MOVEABLE, w.len() * 2).ok()?;
            let p = GlobalLock(mem) as *mut u16;
            if p.is_null() {
                return None;
            }
            std::ptr::copy_nonoverlapping(w.as_ptr(), p, w.len());
            let _ = GlobalUnlock(mem);
            SetClipboardData(CF_UNICODETEXT.0 as u32, Some(HANDLE(mem.0))).ok()
        })()
        .is_some();
        let _ = CloseClipboard();
        ok
    }
}
