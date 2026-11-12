//! What the explorer does to files, as Transient does it: drag rows onto a folder to move them (Ctrl copies),
//! Ctrl+C / Ctrl+X / Ctrl+V with Explorer's clipboard (files copied here paste in Explorer and the other way
//! round), Delete (to the Recycle Bin), Ctrl+Shift+N for a new folder with its name ready to type, F2 to
//! rename in the row, Ctrl+D and Alt+1..7 for favourites and colour tags. Moves, copies, renames and new
//! folders go on an undo stack: Ctrl+Z puts a move back, sends a copy's copies to the Recycle Bin, renames
//! back, or removes a new folder that's still empty; Ctrl+Y does it again. Disk work runs in `fileops` and
//! comes back as `WM_FILE_OP`. Also the explorer's menus: a row's, the list's, the sidebar's, a crumb's
//! folders, History and the folders/files grouping.

use std::collections::HashSet;

use super::explorer::{EField, SideTarget};
use super::menus::{Action, MenuKind};
use super::*;
use crate::explorer::{self, ColKind, Group, Kind, Layout, Place, Scope};
use crate::fileops::{self, Done, Origin, Transfer};
use crate::textedit::TextEdit;
use crate::ui::{Entry, Menu, MenuItem, TextField};

/// One undoable file operation, as it was done: `(from, to)` for each item moved, copied or renamed.
pub(super) enum Step {
    Move(Vec<(PathBuf, PathBuf)>),
    Copy(Vec<(PathBuf, PathBuf)>),
    NewFolder(PathBuf),
    Rename(PathBuf, PathBuf),
}

impl Step {
    /// What undoing it does, for the menu: "move", "copy", "rename", "new folder".
    fn what(&self) -> &'static str {
        match self {
            Step::Move(_) => "move",
            Step::Copy(_) => "copy",
            Step::NewFolder(_) => "new folder",
            Step::Rename(..) => "rename",
        }
    }
}

fn items(n: usize) -> String {
    if n == 1 { "1 item".into() } else { format!("{n} items") }
}

/// The Folders and files button's glyph for a grouping (the menu's items show it too).
pub(super) fn group_glyph(g: Group) -> u16 {
    match g {
        Group::FoldersFirst => 0xE8B7,
        Group::FilesFirst => 0xE7C3,
        Group::Mixed => 0xE8FD,
    }
}

pub(super) fn item(action: Action, glyph: u16, label: &str, accel: Option<&'static str>, enabled: bool) -> Entry<Action> {
    Entry::Item(MenuItem { icon: None, tint: None, action, glyph: Some(glyph), label: label.into(), accel, check: None, enabled, current: false })
}

fn clipboard_has_files() -> bool {
    unsafe { windows::Win32::System::DataExchange::IsClipboardFormatAvailable(windows::Win32::System::Ole::CF_HDROP.0 as u32).is_ok() }
}

impl App {
    fn record(&mut self, step: Step, origin: Origin) {
        let e = &mut self.explorer;
        match origin {
            Origin::User => {
                e.undo.push(step);
                e.redo.clear();
            }
            Origin::Undo => e.redo.push(step),
            Origin::Redo => e.undo.push(step),
        }
    }

    // --- Moves and copies ---------------------------------------------------------------------------

    pub(super) fn explorer_transfer(&mut self, paths: Vec<PathBuf>, dest: PathBuf, copy: bool) {
        if paths.is_empty() {
            return;
        }
        fileops::spawn(self.hwnd, false, move || Done::Transferred(fileops::transfer(&paths, &dest, copy, Origin::User)));
    }

    fn on_transferred(&mut self, t: Transfer) {
        if !t.copy {
            for (from, to) in &t.pairs {
                self.explorer.tags.moved(from, to);
                self.explorer.history.moved(from, to);
                self.tabs_moved(from, to);
                if self.settings.folder_moved(from, to) {
                    settings::save(&self.settings);
                }
            }
        }
        let n = t.pairs.len();
        let dest = t.pairs.first().and_then(|(_, to)| to.parent()).map(explorer::folder_name).unwrap_or_default();
        let verb = if t.copy { "copy" } else { "move" };
        let msg = match t.origin {
            Origin::User if t.copy => format!("Copied {} to {dest}", items(n)),
            Origin::User => format!("Moved {} to {dest}", items(n)),
            Origin::Undo => format!("Undid the {verb}"),
            Origin::Redo => format!("Redid the {verb}"),
        };
        // Cut files that have been pasted: the clipboard empties, as after Explorer's paste.
        if !t.copy && t.origin == Origin::User {
            let moved: HashSet<String> = t.pairs.iter().map(|(f, _)| explorer::norm(f)).collect();
            if self.explorer.cut.as_ref().is_some_and(|(set, _)| set.iter().any(|k| moved.contains(k))) {
                self.explorer.cut = None;
                clipboard::clear(self.hwnd);
            }
        }
        if n > 0 {
            let step = if t.copy { Step::Copy(t.pairs) } else { Step::Move(t.pairs) };
            self.record(step, t.origin);
        }
        match t.failed.first() {
            Some((p, why)) if n == 0 => self.show_toast(format!("Couldn't {verb} {}: {why}", explorer::folder_name(p)), false),
            Some(_) => self.show_toast(format!("{msg} ({} couldn't)", t.failed.len()), false),
            None if n > 0 => self.show_toast(msg, false),
            None => {}
        }
        self.explorer_refresh();
        self.explorer_tags_changed();
    }

    // --- Clipboard ----------------------------------------------------------------------------------

    /// Ctrl+C / Ctrl+X: the selection onto the clipboard as files. Cut rows show faded until they're pasted
    /// or something else goes on the clipboard.
    pub(super) fn explorer_clip(&mut self, cut: bool) {
        let paths = self.explorer.targets();
        if paths.is_empty() {
            return;
        }
        match clipboard::set_files(self.hwnd, &paths, cut) {
            Some(seq) => {
                self.explorer.cut = cut.then(|| (paths.iter().map(|p| explorer::norm(p)).collect(), seq));
                self.show_toast(format!("{} {}", if cut { "Cut" } else { "Copied" }, items(paths.len())), false);
            }
            None => self.show_toast("Couldn't use the clipboard", false),
        }
    }

    /// Ctrl+V: the clipboard's files into the folder showing (or `into`): a copy copies, a cut moves.
    pub(super) fn explorer_paste(&mut self, into: Option<PathBuf>) {
        let Some(dest) = into.or_else(|| self.explorer.folder()) else {
            self.show_toast("Open a folder to paste into", false);
            return;
        };
        let Some((paths, cut)) = clipboard::get_files(self.hwnd) else {
            self.show_toast("No files on the clipboard", false);
            return;
        };
        self.explorer_transfer(paths, dest, !cut);
    }

    // --- Delete -------------------------------------------------------------------------------------

    /// Del: the selection to the Recycle Bin, after asking (one item asks as the viewer's Delete does).
    pub(super) fn explorer_delete(&mut self) {
        let paths = self.explorer.targets();
        match paths.len() {
            0 => {}
            1 => self.confirm_delete_path(paths[0].clone(), true),
            n => {
                let body = format!("Move these {n} items to the Recycle Bin?");
                let buttons = vec![ui::DialogButton::new("Delete", actions::Choice::Confirm), ui::DialogButton::new("Cancel", actions::Choice::Cancel)];
                self.open_dialog_ui(actions::DialogKind::DeleteMany(paths), ui::Dialog::new(format!("Delete {n} items"), body, buttons, 1, true));
            }
        }
    }

    pub(super) fn recycle_many(&mut self, paths: Vec<PathBuf>, undo_copy: Option<(Vec<(PathBuf, PathBuf)>, Origin)>) {
        let owner = self.hwnd.0 as isize;
        fileops::spawn(self.hwnd, true, move || {
            let paths = paths.into_iter().map(|p| { let ok = fileops::recycle(&p, owner); (p, ok) }).collect();
            Done::RecycledMany { paths, undo_copy }
        });
    }

    fn on_recycled_many(&mut self, paths: Vec<(PathBuf, bool)>, undo_copy: Option<(Vec<(PathBuf, PathBuf)>, Origin)>) {
        let gone = paths.iter().filter(|(_, ok)| *ok).count();
        for (p, ok) in paths {
            if ok {
                self.on_recycled(p, true);
            }
        }
        match undo_copy {
            Some((pairs, origin)) => {
                self.record(Step::Copy(pairs), origin);
                self.show_toast("Undid the copy", false);
            }
            None if gone > 0 => self.show_toast(format!("Deleted {}", items(gone)), false),
            None => self.show_toast("Couldn't delete", false),
        }
        self.explorer_refresh();
    }

    // --- New folder and rename ----------------------------------------------------------------------

    /// Ctrl+Shift+N: "New folder" in the folder showing, its name ready to type once it's listed.
    pub(super) fn explorer_new_folder(&mut self) {
        let Some(dir) = self.explorer.folder() else {
            self.show_toast("Open a folder to make one in", false);
            return;
        };
        fileops::spawn(self.hwnd, false, move || Done::NewFolder { path: fileops::new_folder(&dir), origin: Origin::User });
    }

    /// F2: the row's name becomes a text box, the name without its extension selected (a folder: all of it).
    pub(super) fn explorer_begin_rename(&mut self, path: PathBuf) {
        let Some(vi) = self.explorer.index_of(&path) else { return };
        let is_dir = self.explorer.row(vi).is_some_and(|r| r.is_folder());
        let name = explorer::folder_name(&path);
        let mut edit = TextEdit::new(&name);
        let end = match name.rfind('.') {
            Some(dot) if dot > 0 && !is_dir => name[..dot].encode_utf16().count(),
            _ => edit.units().len(),
        };
        edit.select(0, end);
        self.explorer.rename = Some((path, TextField::new(edit)));
        self.explorer.reveal = Some(vi);
        self.explorer_focus_field(EField::Rename);
    }

    pub(super) fn explorer_rename_cursor(&mut self) {
        if let Some(p) = self.explorer.targets().first().cloned() {
            self.explorer_begin_rename(p);
        }
    }

    /// Enter (or a press elsewhere) commits the name; Escape drops it.
    pub(super) fn explorer_finish_rename(&mut self, commit: bool) {
        let Some((path, f)) = self.explorer.rename.take() else { return };
        if self.explorer.field == Some(EField::Rename) {
            self.explorer_blur_field();
        }
        let name = f.edit.text();
        if !commit || name.trim().is_empty() || name == explorer::folder_name(&path) {
            self.invalidate();
            return;
        }
        fileops::spawn(self.hwnd, false, move || {
            let to = fileops::rename_item(&path, &name);
            Done::RenamedItem { from: path, to, origin: Origin::User }
        });
    }

    fn on_renamed_item(&mut self, from: PathBuf, to: Result<PathBuf, String>, origin: Origin) {
        match to {
            Ok(to) => {
                self.explorer.tags.moved(&from, &to);
                self.explorer.history.moved(&from, &to);
                self.tabs_moved(&from, &to);
                if self.settings.folder_moved(&from, &to) {
                    settings::save(&self.settings);
                }
                if self.explorer.cursor.as_deref().is_some_and(|c| explorer::same_path(c, &from)) {
                    self.explorer.cursor = Some(to.clone());
                    self.explorer.selected = [explorer::norm(&to)].into();
                }
                self.on_renamed(from.clone(), Ok(to.clone()));
                self.record(Step::Rename(from, to), origin);
                if origin != Origin::User {
                    self.show_toast(if origin == Origin::Undo { "Undid the rename" } else { "Redid the rename" }, false);
                }
                self.explorer_refresh();
                self.explorer_tags_changed();
            }
            Err(e) => self.show_toast(e, false),
        }
    }

    // --- Favourites and colours ---------------------------------------------------------------------

    /// Ctrl+D: stars the selection (or takes the star off, if they all have it).
    pub(super) fn explorer_favourite(&mut self) {
        let paths = self.explorer.targets();
        if paths.is_empty() {
            return;
        }
        let on = self.explorer.tags.toggle_favourite(&paths);
        self.show_toast(if on { "Added to favourites" } else { "Removed from favourites" }, false);
        self.explorer_tags_changed();
    }

    /// Alt+1..7 (Alt+0 clears): a colour tag on the selection; the colour they all have already comes off.
    pub(super) fn explorer_colour(&mut self, c: Option<u8>) {
        let paths = self.explorer.targets();
        if paths.is_empty() {
            return;
        }
        self.explorer.tags.set_colour(&paths, c);
        self.explorer_tags_changed();
    }

    // --- Undo and redo ------------------------------------------------------------------------------

    pub(super) fn explorer_undo(&mut self) {
        match self.explorer.undo.pop() {
            Some(s) => self.replay(s, Origin::Undo),
            None => self.show_toast("Nothing to undo", false),
        }
    }

    pub(super) fn explorer_redo(&mut self) {
        match self.explorer.redo.pop() {
            Some(s) => self.replay(s, Origin::Redo),
            None => self.show_toast("Nothing to redo", false),
        }
    }

    /// Does a step the other way: a move goes back, a copy's copies are recycled (or made again, redoing it),
    /// a rename goes back, a new folder is removed while it's empty (or made again).
    fn replay(&mut self, step: Step, origin: Origin) {
        match step {
            Step::Move(pairs) => fileops::spawn(self.hwnd, false, move || Done::Transferred(fileops::transfer_back(&pairs, origin))),
            Step::Copy(pairs) if origin == Origin::Undo => {
                let copies = pairs.iter().map(|(_, c)| c.clone()).collect();
                self.recycle_many(copies, Some((pairs, origin)));
            }
            Step::Copy(pairs) => {
                let Some(dest) = pairs.first().and_then(|(_, c)| c.parent()).map(Path::to_path_buf) else { return };
                let srcs: Vec<PathBuf> = pairs.into_iter().map(|(s, _)| s).collect();
                fileops::spawn(self.hwnd, false, move || Done::Transferred(fileops::transfer(&srcs, &dest, true, origin)));
            }
            Step::NewFolder(p) if origin == Origin::Undo => {
                fileops::spawn(self.hwnd, false, move || {
                    let ok = fileops::remove_empty_folder(&p);
                    Done::FolderRemoved { path: p, ok, origin }
                });
            }
            Step::NewFolder(p) => {
                fileops::spawn(self.hwnd, false, move || {
                    let path = std::fs::create_dir(&p).map(|_| p).map_err(|e| format!("Couldn't make the folder again: {e}"));
                    Done::NewFolder { path, origin }
                });
            }
            Step::Rename(from, to) => {
                let name = explorer::folder_name(&from);
                fileops::spawn(self.hwnd, false, move || {
                    let back = fileops::rename_item(&to, &name);
                    Done::RenamedItem { from: to, to: back, origin }
                });
            }
        }
    }

    /// The explorer's file operations, back from their threads.
    pub(super) fn on_explorer_file_op(&mut self, done: Done) {
        match done {
            Done::Transferred(t) => self.on_transferred(t),
            Done::RenamedItem { from, to, origin } => self.on_renamed_item(from, to, origin),
            Done::RecycledMany { paths, undo_copy } => self.on_recycled_many(paths, undo_copy),
            Done::NewFolder { path: Ok(p), origin } => {
                self.record(Step::NewFolder(p.clone()), origin);
                match origin {
                    Origin::User => self.explorer.pending_rename = Some(p),
                    Origin::Redo => self.show_toast("Made the folder again", false),
                    Origin::Undo => {}
                }
                self.explorer_refresh();
            }
            Done::NewFolder { path: Err(e), .. } => self.show_toast(e, false),
            Done::FolderRemoved { path, ok, origin } => {
                if ok {
                    self.record(Step::NewFolder(path), origin);
                    self.show_toast("Removed the new folder", false);
                } else {
                    self.show_toast("The folder isn't empty any more, so it stays", false);
                }
                self.explorer_refresh();
            }
            _ => {}
        }
    }

    // --- Menus --------------------------------------------------------------------------------------

    pub(super) fn show_menu(&mut self, kind: MenuKind, entries: Vec<Entry<Action>>, x: f32, y: f32) {
        let Some(g) = &self.gfx else { return };
        let (w, h) = self.size_dip();
        self.menu = Some((kind, Menu::open(g, entries, x, y, rect(0.0, 0.0, w, h))));
        self.hide_tooltip();
        self.invalidate();
    }

    fn show_menu_below(&mut self, kind: MenuKind, entries: Vec<Entry<Action>>, anchor: D2D_RECT_F) {
        let Some(g) = &self.gfx else { return };
        let (w, h) = self.size_dip();
        self.menu = Some((kind, Menu::below(g, entries, anchor, rect(0.0, 0.0, w, h))));
        self.hide_tooltip();
        self.invalidate();
    }

    /// The favourites row and the colour swatches, for a row's menu.
    fn tag_entries(&self, paths: &[PathBuf]) -> Vec<Entry<Action>> {
        let fav = !paths.is_empty() && paths.iter().all(|p| self.explorer.tags.get(p).favourite);
        let colours: Vec<Option<u8>> = paths.iter().map(|p| self.explorer.tags.get(p).colour).collect();
        let all = |c: u8| !colours.is_empty() && colours.iter().all(|&x| x == Some(c));
        vec![
            item(Action::Favourite, if fav { 0xE735 } else { 0xE734 }, if fav { "Remove from favourites" } else { "Add to favourites" }, Some("Ctrl+D"), true),
            Entry::Swatches(explorer::COLOURS.iter().enumerate().map(|(i, &(_, rgb_))| (Action::Colour(Some(i as u8)), rgb_, all(i as u8))).collect()),
        ]
    }

    /// A row's menu (right-click): what Explorer offers for it, on the selection when it's part of one.
    pub(super) fn explorer_menu(&mut self, vi: usize, x: f32, y: f32) {
        let Some(r) = self.explorer.row(vi).cloned() else { return };
        self.explorer.focused = true;
        self.explorer_commit_fields();
        if !self.explorer.is_selected(&r.path) {
            self.explorer.selected = [explorer::norm(&r.path)].into();
            self.explorer.anchor = Some(r.path.clone());
        }
        self.explorer.cursor = Some(r.path.clone());
        let paths = self.explorer.targets();
        let n = paths.len();
        let single = n == 1;
        let folder = single && r.is_folder();
        let can_paste = clipboard_has_files();
        let mut e = Vec::new();
        if single {
            match r.kind {
                Kind::Folder | Kind::Drive => {
                    e.push(item(Action::OpenItem, 0xE8B7, "Open", Some("Enter"), true));
                    e.push(item(Action::OpenInViewer, 0xE8A7, "Open in the viewer", None, r.kind == Kind::Folder));
                    e.push(item(Action::OpenInNewTab, 0xE8A0, "Open in new tab", Some("Middle-click"), r.kind == Kind::Folder));
                    if crate::dvd::is_dvd(&r.path) {
                        e.push(item(Action::RipDvd, 0xE958, "Rip DVD\u{2026}", None, self.ripping.is_none()));
                    }
                }
                Kind::Image => e.push(item(Action::OpenItem, 0xE8A7, "Open", Some("Enter"), true)),
                Kind::Other => e.push(item(Action::OpenItem, 0xE8A7, "Preview", None, true)),
            }
            e.push(Entry::Separator);
        }
        let drive = r.kind == Kind::Drive;
        e.push(item(Action::Cut, 0xE8C6, "Cut", Some("Ctrl+X"), !drive));
        e.push(item(Action::CopyFiles, 0xE8C8, "Copy", Some("Ctrl+C"), !drive));
        if folder {
            e.push(item(Action::PasteInto, 0xE77F, "Paste into this folder", None, can_paste));
        } else {
            e.push(item(Action::Paste, 0xE77F, "Paste", Some("Ctrl+V"), can_paste && self.explorer.folder().is_some()));
        }
        e.push(Entry::Separator);
        e.push(item(Action::Rename, 0xE8AC, "Rename", Some("F2"), single && !drive));
        e.push(item(Action::Delete, 0xE74D, &if single { "Delete".to_string() } else { format!("Delete {n} items") }, Some("Del"), !drive));
        e.push(Entry::Separator);
        e.extend(self.tag_entries(&paths));
        e.push(Entry::Separator);
        if folder {
            let marked = self.settings.is_bookmarked(&r.path);
            e.push(item(Action::Bookmark, if marked { 0xE840 } else { 0xE718 }, if marked { "Remove bookmark" } else { "Bookmark" }, None, true));
        }
        e.push(item(Action::CopyPath, 0xE71B, "Copy path", None, single));
        e.push(item(Action::Reveal, 0xEC50, "Reveal in Explorer\u{2026}", None, true));
        e.push(item(Action::Properties, 0xE946, "Properties", None, single));
        self.show_menu(MenuKind::Rows(paths), e, x, y);
    }

    /// The list's own menu, away from any row.
    pub(super) fn explorer_bg_menu(&mut self, x: f32, y: f32) {
        self.explorer.focused = true;
        let folder = self.explorer.folder().is_some();
        let undo = self.explorer.undo.last().map(|s| format!("Undo {}", s.what()));
        let redo = self.explorer.redo.last().map(|s| format!("Redo {}", s.what()));
        let mut e = vec![
            item(Action::Undo, 0xE7A7, undo.as_deref().unwrap_or("Undo"), Some("Ctrl+Z"), undo.is_some()),
            item(Action::Redo, 0xE7A6, redo.as_deref().unwrap_or("Redo"), Some("Ctrl+Y"), redo.is_some()),
            Entry::Separator,
            item(Action::NewFolder, 0xE8F4, "New folder", Some("Ctrl+Shift+N"), folder),
            item(Action::Paste, 0xE77F, "Paste", Some("Ctrl+V"), folder && clipboard_has_files()),
            Entry::Separator,
            item(Action::SelectAll, 0xE8B3, "Select all", Some("Ctrl+A"), !self.explorer.view.is_empty()),
            item(Action::Refresh, 0xE72C, "Refresh", Some("F5"), true),
        ];
        if self.explorer.folder().is_some_and(|f| crate::dvd::is_dvd(&f)) {
            e.push(Entry::Separator);
            e.push(item(Action::RipDvd, 0xE958, "Rip DVD\u{2026}", None, self.ripping.is_none()));
        }
        e.shrink_to_fit();
        self.show_menu(MenuKind::ExplorerBg, e, x, y);
    }

    /// A sidebar row's menu.
    pub(super) fn sidebar_menu(&mut self, i: usize, x: f32, y: f32) {
        let items_ = self.sidebar_items();
        let Some(SideTarget::Place(p)) = items_.get(i).map(|it| it.target.clone()) else { return };
        let mut e = vec![item(Action::OpenItem, 0xE8B7, "Open", None, true)];
        if let Place::Folder(f) = &p {
            e.push(item(Action::OpenInViewer, 0xE8A7, "Open in the viewer", None, true));
            e.push(item(Action::OpenInNewTab, 0xE8A0, "Open in new tab", None, true));
            e.push(Entry::Separator);
            let marked = self.settings.is_bookmarked(f);
            e.push(item(Action::Bookmark, if marked { 0xE840 } else { 0xE718 }, if marked { "Remove bookmark" } else { "Bookmark" }, None, true));
            e.push(item(Action::PasteInto, 0xE77F, "Paste into this folder", None, clipboard_has_files()));
            e.push(Entry::Separator);
            e.push(item(Action::CopyPath, 0xE71B, "Copy path", None, true));
            e.push(item(Action::Reveal, 0xEC50, "Reveal in Explorer\u{2026}", None, true));
            e.push(item(Action::Properties, 0xE946, "Properties", None, true));
        }
        self.show_menu(MenuKind::Side(p), e, x, y);
    }

    /// The History drop-down, as Transient's: Back and Forward's places, oldest first, each with its icon,
    /// the current one outlined in the accent.
    pub(super) fn explorer_history_menu(&mut self, anchor: D2D_RECT_F) {
        let (places, at) = self.explorer.history.entries();
        if places.is_empty() {
            return;
        }
        let e = places
            .iter()
            .enumerate()
            .map(|(i, p)| {
                let glyph = match p {
                    Place::Computer => 0xE7F8u16,
                    Place::Folder(_) => 0xE8B7,
                    Place::Collection(crate::explorer::Collection::Favourites) => 0xE734,
                    Place::Collection(_) => 0xE8EC,
                };
                Entry::Item(MenuItem { icon: super::explorer_view::place_icon(p), tint: None, action: Action::HistoryJump(i), glyph: Some(glyph), label: p.name(), accel: None, check: None, enabled: true, current: Some(i) == at })
            })
            .collect();
        self.show_menu_below(MenuKind::History, e, anchor);
    }

    /// The card's Bookmarks or Collections drop-down: browse mode's sidebar section as a menu, its rows in the order
    /// they're in there (Faye, 2026-11-10), where you are outlined in the accent.
    pub(super) fn explorer_jump_menu(&mut self, collections: bool, anchor: D2D_RECT_F) {
        let section = if collections { explorer::SideSection::Collections } else { explorer::SideSection::Bookmarks };
        let mut places = Vec::new();
        let mut e = Vec::new();
        for it in self.sidebar_items() {
            match &it.target {
                SideTarget::Header(s) => {
                    if *s == section {
                        places.clear();
                        e.clear();
                        e.push(Entry::Heading(it.label.clone()));
                    } else if !e.is_empty() {
                        break;
                    }
                }
                SideTarget::Place(p) if !e.is_empty() && it.depth == 1 => {
                    let current = self.explorer.place.as_ref().is_some_and(|q| q.same(p));
                    let (glyph, tint) = if it.glyph == 0xE8B7 { (0xE8D5, Some(super::explorer::FOLDER_TINT)) } else { (it.glyph, it.tint) };
                    // Windows' icons, as the sidebar's (Faye, 2026-11-11: the bookmarks had the plain glyph).
                    e.push(Entry::Item(MenuItem { icon: super::explorer_view::place_icon(p), tint, action: Action::Crumb(places.len()), glyph: Some(glyph), label: it.label.clone(), accel: None, check: None, enabled: true, current }));
                    places.push(p.clone());
                }
                SideTarget::Hint if !e.is_empty() => {
                    e.push(Entry::Item(MenuItem { icon: None, tint: None, action: Action::Crumb(usize::MAX), glyph: None, label: it.label.clone(), accel: None, check: None, enabled: false, current: false }));
                }
                _ => {}
            }
        }
        if e.is_empty() {
            return;
        }
        // Bookmarking the folder listed heads the Bookmarks (Faye, 2026-11-11: it was a button of its own).
        if let Some(f) = self.explorer.folder().filter(|_| !collections) {
            let marked = self.settings.is_bookmarked(&f);
            e.insert(0, item(Action::Bookmark, if marked { 0xE738 } else { 0xE710 }, if marked { "Remove bookmark" } else { "Add bookmark" }, None, true));
            e.insert(1, Entry::Separator);
        }
        self.show_menu_below(MenuKind::Jump(collections, places), e, anchor);
    }

    /// The Folders and files button's menu, after Transient's: where folders go, what's listed, the rows'
    /// height, and the folder's saved view.
    pub(super) fn group_entries(&self) -> Vec<Entry<Action>> {
        let v = &self.explorer.look;
        let folder = self.explorer.folder().is_some();
        let ticked = |action, glyph, label: &str, on: bool, enabled: bool| {
            Entry::Item(MenuItem { icon: None, tint: None, action, glyph: Some(glyph), label: label.into(), accel: None, check: Some(on), enabled, current: false })
        };
        let mut e = vec![Entry::Heading("Layout".into())];
        e.push(ticked(Action::Layout(Layout::List), 0xE8FD, "List", v.layout == Layout::List, true));
        e.push(ticked(Action::Layout(Layout::Columns), 0xE89A, "Columns", v.layout == Layout::Columns, true));
        e.push(Entry::Separator);
        e.push(Entry::Heading("Folders and files".into()));
        for g in Group::ALL {
            e.push(ticked(Action::Group(g), group_glyph(g), g.label(), v.group == g, true));
        }
        e.push(Entry::Separator);
        // Column view lists each folder alone.
        e.push(ticked(Action::ToggleSubfolders, 0xE8B7, "Include subfolders", v.subfolders, folder && v.layout == Layout::List));
        let h = v.height;
        e.push(Entry::Stepper(crate::ui::Stepper {
            label: "Height".into(),
            value: if h == 1 { "1 row".into() } else { format!("{h} rows") },
            dec: (h > 1).then_some(Action::RowHeight(h - 1)),
            inc: (h < crate::explorer::HEIGHT_MAX).then_some(Action::RowHeight(h + 1)),
        }));
        e.push(Entry::Separator);
        // Any change above is kept for this place alone ("This folder"); Save as default is the only way to change
        // what every folder shows (Faye, 2026-11-11).
        e.push(Entry::Heading("Saved view".into()));
        let view_place = self.view_place();
        let place = view_place.as_ref();
        let key = place.map(crate::explorer::view_key);
        let entry = self.view_entry().map(|i| &self.settings.views[i]);
        // A parent's "and subfolders" covering this folder: said so, nothing of this folder's own ticked.
        let inherited = entry.filter(|s| key.as_ref().is_some_and(|k| crate::explorer::norm(&s.path) != crate::explorer::norm(k)));
        let scope = entry.filter(|_| inherited.is_none()).map(|s| s.scope);
        let any = place.is_some();
        e.push(ticked(Action::SavedView(None), 0xE785, "Use default", scope.is_none() && inherited.is_none(), any));
        e.push(ticked(Action::SavedView(Some(Scope::Folder)), 0xE72E, "This folder", scope == Some(Scope::Folder), any));
        e.push(ticked(Action::SavedView(Some(Scope::Tree)), 0xE72E, "This folder and subfolders", scope == Some(Scope::Tree), folder));
        if let Some(s) = inherited {
            let name = crate::explorer::Place::Folder(s.path.clone()).name();
            e.push(Entry::Item(MenuItem { icon: None, tint: None, action: Action::Crumb(usize::MAX), glyph: None, label: format!("Following \u{201C}{name}\u{201D}"), accel: None, check: None, enabled: false, current: false }));
        }
        e.push(Entry::Separator);
        // Save as default also clears every saved view, so it's worth it while any exist even if this one matches.
        let can = *v != self.settings.default_view() || self.settings.views.iter().any(|s| s.scope != Scope::Off);
        e.push(item(Action::SetDefaultView, 0xE7F4, if can { "Save as default\u{2026}" } else { "Already the default" }, None, can));
        e
    }

    pub(super) fn explorer_group_menu(&mut self, anchor: D2D_RECT_F) {
        let e = self.group_entries();
        self.show_menu_below(MenuKind::Group, e, anchor);
    }

    /// A tab's menu (right-click), as Transient's: a new tab, close it, close the others.
    pub(super) fn tab_menu(&mut self, i: usize, x: f32, y: f32) {
        let n = self.explorer.tabs.len();
        let e = vec![
            item(Action::NewTab, 0xE710, "New tab", Some("Ctrl+T"), true),
            item(Action::CloseTab, 0xE711, "Close tab", Some("Ctrl+W"), n > 1),
            item(Action::CloseOtherTabs, 0xE8BB, "Close other tabs", None, n > 1),
        ];
        self.show_menu(MenuKind::Tab(i), e, x, y);
    }

    /// A text tab's menu (right-click): the explorer tab's entries, for documents.
    pub(super) fn text_tab_menu(&mut self, i: usize, x: f32, y: f32) {
        let n = self.text.docs.len();
        let e = vec![
            item(Action::NewTab, 0xE710, "New tab", Some("Ctrl+T"), true),
            item(Action::CloseTab, 0xE711, "Close tab", Some("Ctrl+W"), true),
            item(Action::CloseOtherTabs, 0xE8BB, "Close other tabs", None, n > 1),
        ];
        self.show_menu(MenuKind::TextTab(i), e, x, y);
    }

    /// A column header's menu (right-click), as Transient's: move it, size it, close it, then every column
    /// by group, ticked while it shows.
    pub(super) fn column_menu(&mut self, idx: usize, x: f32, y: f32) {
        let Some(e) = self.column_entries(idx) else { return };
        self.show_menu(MenuKind::Columns(idx), e, x, y);
    }

    /// The header menu's rows: the column's own actions, then a submenu per category (Transient's), ticked
    /// while any of its columns shows.
    pub(super) fn column_entries(&self, idx: usize) -> Option<Vec<Entry<Action>>> {
        let cols = &self.explorer.look.columns;
        let &(c, _) = cols.get(idx)?;
        let mut e = vec![
            item(Action::ColShift(false), 0xE76B, "Shift column left", None, idx > 0),
            item(Action::ColShift(true), 0xE76C, "Shift column right", None, idx + 1 < cols.len()),
            item(Action::ColFit, 0xE9A6, "Size column to fit", Some("Double-click edge"), c != ColKind::Name),
            item(Action::ColClose, 0xE711, "Close column", Some("Middle-click"), c != ColKind::Name),
            Entry::Separator,
            item(Action::ColFitAll, 0xE9A6, "Size all columns to fit", None, true),
            item(Action::ColRestore, 0xE7A7, "Restore default columns", None, true),
        ];
        e.push(Entry::Separator);
        for (name, kinds) in ColKind::GROUPS {
            let mut sub = Vec::new();
            let mut any = false;
            for &k in kinds {
                let shown = cols.iter().any(|(s, _)| *s == k);
                any |= shown;
                sub.push(Entry::Item(MenuItem { icon: None, tint: None, action: Action::ColToggle(k), glyph: None, label: k.label().into(), accel: None, check: Some(shown), enabled: k != ColKind::Name, current: false }));
            }
            let e_sub = &mut sub;
            // Transient's Date and Time options, under its columns: how the dates read.
            if kinds.contains(&ColKind::Modified) {
                e_sub.push(Entry::Separator);
                let s = &self.settings;
                for (i, label, on) in [(0u8, "Show the date", s.date_show_date), (1, "Show the time", s.date_show_time), (2, "Two-digit year", s.date_two_digit_year), (3, "Leading zeros in the date", s.date_pad), (4, "Align times", s.date_align)] {
                    // The date can't go while the time is off (something has to show), nor the year without the date.
                    let enabled = match i {
                        0 => s.date_show_time,
                        2 => s.date_show_date,
                        4 => s.date_show_time,
                        _ => s.date_show_date,
                    };
                    e_sub.push(Entry::Item(MenuItem { icon: None, tint: None, action: Action::DateOption(i), glyph: None, label: label.into(), accel: None, check: Some(on), enabled, current: false }));
                }
            }
            e.push(Entry::Sub(any.then_some(0xE73E), name.into(), sub));
        }
        Some(e)
    }

    /// Bookmarks the folder showing (or takes it off).
    pub(super) fn explorer_toggle_bookmark(&mut self, folder: Option<PathBuf>) {
        let Some(f) = folder.or_else(|| self.explorer.folder()) else { return };
        let on = self.settings.toggle_bookmark(&f);
        settings::save(&self.settings);
        self.show_toast(if on { "Bookmarked" } else { "Bookmark removed" }, false);
    }

    /// Windows' own Properties window for a file or folder.
    fn properties(&self, path: &Path) {
        use windows::Win32::UI::Shell::{SHOP_FILEPATH, SHObjectProperties};
        unsafe {
            let _ = SHObjectProperties(Some(self.hwnd), SHOP_FILEPATH, &HSTRING::from(path.as_os_str()), PCWSTR::null());
        }
    }

    /// A pick in one of the explorer's menus.
    pub(super) fn run_explorer_action(&mut self, a: Action, kind: MenuKind) {
        let paths: Vec<PathBuf> = match &kind {
            MenuKind::Rows(p) => p.clone(),
            MenuKind::Side(Place::Folder(f)) => vec![f.clone()],
            _ => Vec::new(),
        };
        let first = paths.first().cloned();
        match (a, kind) {
            (Action::HistoryJump(i), _) => self.explorer_history_jump(i),
            (Action::Group(g), _) => self.set_explorer_group(g),
            (Action::OpenItem, MenuKind::Side(p)) => self.explorer_crumb(p),
            (Action::OpenItem, MenuKind::Rows(_)) => {
                if let Some(vi) = first.as_deref().and_then(|p| self.explorer.index_of(p)) {
                    self.explorer_double_click(vi);
                }
            }
            (Action::OpenInViewer, _) => {
                if let Some(f) = first {
                    self.open_folder(f);
                    self.leave_browse();
                }
            }
            (Action::RipDvd, _) => {
                if let Some(p) = first.or_else(|| self.explorer.folder()) {
                    self.open_dvd(p);
                }
            }
            (Action::Cut, _) => self.explorer_clip(true),
            (Action::CopyFiles, _) => self.explorer_clip(false),
            (Action::Paste, _) => self.explorer_paste(None),
            (Action::PasteInto, _) => self.explorer_paste(first),
            (Action::Rename, _) => {
                if let Some(p) = first {
                    self.explorer_begin_rename(p);
                }
            }
            (Action::Delete, _) => self.explorer_delete(),
            (Action::Favourite, _) => self.explorer_favourite(),
            (Action::Colour(c), _) => self.explorer_colour(c),
            (Action::Bookmark, _) => self.explorer_toggle_bookmark(first),
            (Action::CopyPath, _) => {
                if let Some(p) = first {
                    self.copy_path_of(&p);
                }
            }
            (Action::Reveal, _) => {
                if let Some(p) = first {
                    self.reveal_path(&p);
                }
            }
            (Action::Properties, _) => {
                if let Some(p) = first {
                    self.properties(&p);
                }
            }
            (Action::NewFolder, _) => self.explorer_new_folder(),
            (Action::SelectAll, _) => self.explorer_select_all(),
            (Action::Refresh, _) => self.explorer_refresh(),
            (Action::ColShift(right), MenuKind::Columns(i)) => self.col_shift(i, right),
            (Action::ColFit, MenuKind::Columns(i)) => self.col_fit(i),
            (Action::ColClose, MenuKind::Columns(i)) => self.col_close(i),
            (Action::ColFitAll, _) => self.col_fit_all(),
            (Action::Layout(l), _) => self.set_layout(l),
            (Action::ColumnFit, MenuKind::ColumnEdge(i)) => self.column_fit(i),
            (Action::ColumnFitAll, _) => self.column_fit_all(),
            (Action::ColumnAllThis, MenuKind::ColumnEdge(i)) => self.column_all_this(i),
            (Action::ColumnReset, MenuKind::ColumnEdge(i)) => self.column_reset(i),
            (Action::ColumnResetAll, _) => self.column_reset_all(),
            (Action::ColRestore, _) => self.col_restore(),
            (Action::ColToggle(k), _) => self.col_toggle(k),
            (Action::Undo, _) => self.explorer_undo(),
            (Action::Redo, _) => self.explorer_redo(),
            (Action::ToggleSubfolders, _) => self.toggle_subfolders(),
            (Action::DateOption(i), _) => {
                let s = &mut self.settings;
                match i {
                    0 => s.date_show_date = !s.date_show_date,
                    1 => s.date_show_time = !s.date_show_time,
                    2 => s.date_two_digit_year = !s.date_two_digit_year,
                    3 => s.date_pad = !s.date_pad,
                    _ => s.date_align = !s.date_align,
                }
                super::set_date_columns(s.date_show_date, s.date_show_time, s.date_two_digit_year, s.date_pad, s.date_align);
                settings::save(&self.settings);
                self.invalidate();
            }
            (Action::NewTab, MenuKind::Tab(i)) => {
                let p = self.tab_place(i);
                self.new_tab(p);
            }
            (Action::NewTab, _) => {
                let p = self.explorer.place.clone();
                self.new_tab(p);
            }
            (Action::CloseTab, MenuKind::Tab(i)) => self.close_tab(i),
            (Action::CloseOtherTabs, MenuKind::Tab(i)) => self.close_other_tabs(i),
            (Action::OpenInNewTab, _) => {
                if let Some(f) = first.filter(|f| f.is_dir()) {
                    self.new_tab(Some(Place::Folder(f)));
                }
            }
            (Action::RowHeight(h), _) => self.set_row_height(h),
            (Action::SavedView(s), _) => self.set_saved_view(s),
            (Action::SetDefaultView, _) => self.confirm_default_view(),
            _ => {}
        }
    }
}
