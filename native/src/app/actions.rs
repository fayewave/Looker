//! The file actions behind the toolbar, context menu and keys (MainViewModel's M7 commands): the rotation
//! preview and its save, copy image, set as wallpaper, delete with a confirm dialog, plus the OSD toast
//! that reports them. Disk work runs in `fileops` and comes back as `WM_FILE_OP`.

use super::*;
use crate::fileops::{self, Done};
use crate::textedit::{Move, TextEdit};
use crate::ui::{Dialog, DialogButton, Focus, TextField, Toast};
use windows::Win32::UI::Input::Ime::{
    CFS_POINT, COMPOSITIONFORM, HIMC, IACE_DEFAULT, ImmAssociateContextEx, ImmGetContext,
    ImmReleaseContext, ImmSetCompositionWindow,
};
use crate::viewer::Removed;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Choice {
    Confirm,
    Cancel,
}

pub(super) enum DialogKind {
    Delete(PathBuf),
    Rename(PathBuf),
    Reset,
}

impl App {
    // --- Toast --------------------------------------------------------------------------------------

    pub(super) fn show_toast(&mut self, text: impl Into<String>, quick: bool) {
        self.toast = Some(Toast::new(text.into(), quick, self.toast.as_ref()));
        self.invalidate();
    }

    // --- Rotation preview ---------------------------------------------------------------------------

    pub(super) fn can_rotate(&self) -> bool {
        !self.saving_rotation && self.placeholder.is_none() && self.viewer.current_entry().is_some_and(|e| e.pages == 0)
    }

    /// Turns the image on screen 90° as a preview only: the file is untouched until it is saved, so the
    /// folder doesn't re-sort under the user. Re-fits to the turned bounds.
    pub(super) fn rotate_preview(&mut self, clockwise: bool) {
        if !self.can_rotate() {
            return;
        }
        let Some((nw, nh)) = self.viewer.current_entry().map(|e| (e.native_w, e.native_h)) else { return };
        self.turns = (self.turns + if clockwise { 1 } else { 3 }) & 3;
        self.reset_view(nw, nh);
        self.schedule_upgrade();
        self.invalidate();
    }

    /// Bakes the preview into the file. The turned preview stays up until the re-decode of the saved file
    /// lands, so nothing flips back in between.
    pub(super) fn save_rotation(&mut self) {
        if self.turns == 0 || self.saving_rotation {
            return;
        }
        let Some(path) = self.current_path().map(Path::to_path_buf) else { return };
        let degrees = self.turns as i32 * 90;
        self.saving_rotation = true;
        self.invalidate();
        fileops::spawn(self.hwnd, false, move || {
            let r = fileops::rotate(&path, degrees);
            if let Err(e) = &r {
                crate::trace::mark(format!("rotate failed: {e}"));
            }
            Done::Rotated { path, ok: r.is_ok() }
        });
    }

    fn on_rotated(&mut self, path: PathBuf, ok: bool) {
        self.saving_rotation = false;
        if !ok {
            self.show_toast("Couldn't save rotation", false);
            return;
        }
        let stamp = self.viewer.listing.as_mut().and_then(|l| l.refresh(&path)).unwrap_or_else(|| crate::engine::stamp(&path));
        self.viewer.forget(&path);
        if self.current_path() == Some(path.as_path()) {
            self.rotation_saved = true;
            let out = self.viewer.show(path, stamp, self.gfx.as_ref());
            self.apply(out);
            self.current_changed();
        }
        self.show_toast("Rotation saved", false);
    }

    // --- Clipboard and wallpaper --------------------------------------------------------------------

    /// The file and its full-size pixels (decoded off the UI thread), so it pastes into Explorer and into
    /// image editors alike.
    pub(super) fn copy_image(&mut self) {
        let Some(path) = self.current_path().map(Path::to_path_buf) else { return };
        let page = self.pdf_page_of_current();
        fileops::spawn(self.hwnd, false, move || {
            let image = fileops::full_image(&path, page);
            Done::Copied { path, image }
        });
    }

    pub(super) fn copy_path(&mut self) {
        if let Some(path) = self.target_path() {
            self.copy_path_of(&path);
        }
    }

    pub(super) fn copy_path_of(&mut self, path: &Path) {
        if clipboard::set_text(self.hwnd, &path.to_string_lossy()) {
            self.show_toast("Copied path", false);
        }
    }

    pub(super) fn set_wallpaper(&mut self) {
        let Some(path) = self.current_path().map(Path::to_path_buf) else { return };
        let page = self.pdf_page_of_current();
        fileops::spawn(self.hwnd, false, move || Done::Wallpaper { ok: fileops::set_wallpaper(&path, page) });
    }

    // --- Delete -------------------------------------------------------------------------------------

    /// Asks before recycling. Cancel is the default button, so a stray Enter never deletes.
    pub(super) fn confirm_delete(&mut self, from_keyboard: bool) {
        if let Some(path) = self.target_path() {
            self.confirm_delete_path(path, from_keyboard);
        }
    }

    /// Any file (the explorer card's menu reaches files outside the open folder).
    pub(super) fn confirm_delete_path(&mut self, path: PathBuf, from_keyboard: bool) {
        self.close_menu();
        self.hide_tooltip();
        let body = format!("Move \u{201C}{}\u{201D} to the Recycle Bin?", file_name(&path));
        let buttons = vec![DialogButton::new("Delete", Choice::Confirm), DialogButton::new("Cancel", Choice::Cancel)];
        let title = if crate::format::is_supported(&path) { "Delete photo" } else { "Delete file" };
        self.open_dialog_ui(DialogKind::Delete(path), Dialog::new(title.into(), body, buttons, 1, from_keyboard));
    }

    // --- Rename -------------------------------------------------------------------------------------

    /// The rename box opens with just the base name selected, so typing replaces it and keeps the extension.
    pub(super) fn begin_rename(&mut self, from_keyboard: bool) {
        if let Some(path) = self.target_path() {
            self.begin_rename_path(path, from_keyboard);
        }
    }

    pub(super) fn begin_rename_path(&mut self, path: PathBuf, from_keyboard: bool) {
        let name = file_name(&path);
        let mut edit = TextEdit::new(&name);
        let end = match name.rfind('.') {
            Some(dot) if dot > 0 => name[..dot].encode_utf16().count(),
            _ => edit.units().len(),
        };
        edit.select(0, end);
        let buttons = vec![DialogButton::new("Rename", Choice::Confirm), DialogButton::new("Cancel", Choice::Cancel)];
        let d = Dialog::new("Rename file".into(), String::new(), buttons, 0, from_keyboard).with_field(TextField::new(edit));
        self.open_dialog_ui(DialogKind::Rename(path), d);
    }

    fn on_renamed(&mut self, from: PathBuf, to: Result<PathBuf, String>) {
        match to {
            Ok(to) if to != from => {
                self.viewer.renamed(&from, &to);
                // The covering file keeps covering under its new name.
                if let Some(p) = self.placeholder.as_mut().filter(|p| p.path == from) {
                    p.path = to.clone();
                    unsafe {
                        let _ = SetWindowTextW(self.hwnd, &HSTRING::from(file_name(&to)));
                    }
                }
                if self.current_path() == Some(to.as_path()) {
                    self.current_changed();
                }
                self.invalidate();
            }
            Ok(_) => {}
            Err(e) => self.show_toast(e, false),
        }
    }

    // --- Dialog plumbing ----------------------------------------------------------------------------

    pub(super) fn open_dialog_ui(&mut self, kind: DialogKind, d: Dialog<Choice>) {
        self.close_menu();
        self.hide_tooltip();
        let field = d.field.is_some();
        self.dialog = Some((kind, d));
        self.pressed = None;
        self.set_ime(field);
        if field {
            self.restart_caret();
        }
        self.invalidate();
    }

    /// The IME is only on while a text box has the keyboard, so typing shortcuts in the viewer with a
    /// Japanese or Chinese keyboard never opens a composition window.
    pub(super) fn set_ime(&self, on: bool) {
        unsafe {
            let _ = ImmAssociateContextEx(self.hwnd, HIMC::default(), if on { IACE_DEFAULT } else { 0 });
        }
    }

    /// Puts the IME's composition window at the caret.
    pub(super) fn place_ime(&self) {
        let Some(c) = self.caret_rect else { return };
        let s = self.scale();
        unsafe {
            let himc = ImmGetContext(self.hwnd);
            if himc.is_invalid() {
                return;
            }
            let form = COMPOSITIONFORM {
                dwStyle: CFS_POINT,
                ptCurrentPos: POINT { x: (c.left * s) as i32, y: (c.top * s) as i32 },
                ..Default::default()
            };
            let _ = ImmSetCompositionWindow(himc, &form);
            let _ = ImmReleaseContext(self.hwnd, himc);
        }
    }

    /// Shows the caret and restarts its blink (after any edit or move).
    pub(super) fn restart_caret(&mut self) {
        if let Some(f) = self.dialog.as_mut().and_then(|(_, d)| d.field.as_mut()) {
            f.caret_on = true;
        }
        let blink = unsafe { GetCaretBlinkTime() };
        unsafe {
            if blink != 0 && blink != u32::MAX {
                SetTimer(Some(self.hwnd), TIMER_CARET, blink, None);
            } else {
                let _ = KillTimer(Some(self.hwnd), TIMER_CARET);
            }
        }
        self.invalidate();
    }

    pub(super) fn blink_caret(&mut self) {
        match self.dialog.as_mut().filter(|(_, d)| d.field_focused()).and_then(|(_, d)| d.field.as_mut()) {
            Some(f) => {
                f.caret_on = !f.caret_on;
                self.invalidate();
            }
            None => unsafe {
                let _ = KillTimer(Some(self.hwnd), TIMER_CARET);
            },
        }
    }

    /// After the text changed: Rename can't commit an empty name.
    fn field_changed(&mut self) {
        if let Some((DialogKind::Rename(_), d)) = &mut self.dialog {
            let blank = d.field.as_ref().is_none_or(|f| f.edit.text().trim().is_empty());
            d.buttons[0].enabled = !blank;
        }
        self.restart_caret();
    }

    /// A character from WM_CHAR, for the focused text box.
    pub(super) fn dialog_char(&mut self, unit: u16) {
        let Some(f) = self.dialog.as_mut().filter(|(_, d)| d.field_focused()).and_then(|(_, d)| d.field.as_mut()) else { return };
        f.edit.type_unit(unit);
        self.field_changed();
    }

    /// Text box keys: caret moves, deletes, clipboard, undo. Returns false for keys it doesn't use.
    fn field_key(&mut self, vk: VIRTUAL_KEY, shift: bool, ctrl: bool) -> bool {
        let hwnd = self.hwnd;
        let Some(f) = self.dialog.as_mut().and_then(|(_, d)| d.field.as_mut()) else { return false };
        let e = &mut f.edit;
        let mut changed = false;
        match vk {
            VK_LEFT => e.move_caret(if ctrl { Move::WordLeft } else { Move::Left }, shift),
            VK_RIGHT => e.move_caret(if ctrl { Move::WordRight } else { Move::Right }, shift),
            VK_HOME | VK_UP => e.move_caret(Move::Home, shift),
            VK_END | VK_DOWN => e.move_caret(Move::End, shift),
            VK_BACK => {
                e.backspace(ctrl);
                changed = true;
            }
            VK_DELETE => {
                e.delete(ctrl);
                changed = true;
            }
            VK_A if ctrl => e.select_all(),
            VK_C if ctrl => {
                if e.has_selection() {
                    clipboard::set_text(hwnd, &e.selected());
                }
            }
            VK_X if ctrl => {
                if let Some(s) = e.cut() {
                    clipboard::set_text(hwnd, &s);
                    changed = true;
                }
            }
            VK_V if ctrl => {
                if let Some(s) = clipboard::get_text(hwnd) {
                    e.insert(&s);
                    changed = true;
                }
            }
            VK_Z if ctrl => changed = e.undo(),
            VK_Y if ctrl => changed = e.redo(),
            _ => return false,
        }
        if changed {
            self.field_changed();
        } else {
            self.restart_caret();
        }
        true
    }

    /// A press in the text box: focus it and put the caret there (Shift extends the selection); a
    /// double-click selects the word.
    pub(super) fn field_press(&mut self, x: f32, double: bool) {
        let shift = unsafe { GetKeyState(VK_SHIFT.0 as i32) < 0 };
        let Some(r) = self.hits.rect_of(Hit::DialogField) else { return };
        let Some(g) = self.gfx.as_ref() else { return };
        let Some((_, d)) = &mut self.dialog else { return };
        let with_clear = d.field_focused() && d.field.as_ref().is_some_and(|f| !f.edit.units().is_empty());
        d.focus = Focus::Field;
        let Some(f) = &mut d.field else { return };
        let pos = f.position_at(g, r, with_clear, x);
        if double {
            let (a, b) = f.edit.word_at(pos);
            f.edit.select(a, b);
        } else if shift {
            let (a, b) = f.edit.selection();
            let anchor = if f.edit.caret() == a { b } else { a };
            f.edit.select(anchor, pos);
        } else {
            f.edit.select(pos, pos);
            self.text_drag = Some(pos);
        }
        self.restart_caret();
    }

    /// Dragging across the text box selects from where the press was.
    pub(super) fn field_drag(&mut self, x: f32) {
        let Some(anchor) = self.text_drag else { return };
        let Some(r) = self.hits.rect_of(Hit::DialogField) else { return };
        let Some(g) = self.gfx.as_ref() else { return };
        let Some(f) = self.dialog.as_mut().and_then(|(_, d)| d.field.as_mut()) else { return };
        let with_clear = !f.edit.units().is_empty();
        let pos = f.position_at(g, r, with_clear, x);
        f.edit.select(anchor, pos);
        self.restart_caret();
    }

    pub(super) fn field_clear(&mut self) {
        if let Some(f) = self.dialog.as_mut().and_then(|(_, d)| d.field.as_mut()) {
            f.edit.clear();
            self.field_changed();
        }
    }

    /// A dialog button was chosen (click, Enter, Escape = Cancel).
    pub(super) fn dialog_choose(&mut self, choice: Choice) {
        let Some((kind, d)) = self.dialog.take() else { return };
        self.text_drag = None;
        self.caret_rect = None;
        self.set_ime(false);
        unsafe {
            let _ = KillTimer(Some(self.hwnd), TIMER_CARET);
        }
        self.invalidate();
        if choice != Choice::Confirm {
            return;
        }
        match kind {
            DialogKind::Reset => self.reset_app(),
            DialogKind::Rename(path) => {
                let name = d.field.map(|f| f.edit.text()).unwrap_or_default();
                if name.trim().is_empty() {
                    return;
                }
                fileops::spawn(self.hwnd, false, move || {
                    let to = fileops::rename(&path, &name);
                    Done::Renamed { from: path, to }
                });
            }
            DialogKind::Delete(path) => {
                let owner = self.hwnd.0 as isize;
                fileops::spawn(self.hwnd, true, move || {
                    let ok = fileops::recycle(&path, owner);
                    Done::Recycled { path, ok }
                });
            }
        }
    }

    /// Keys while a dialog is up: it has them all.
    pub(super) fn dialog_key(&mut self, vk: VIRTUAL_KEY, shift: bool, ctrl: bool) {
        let Some((_, d)) = &mut self.dialog else { return };
        let in_field = d.field_focused();
        match vk {
            VK_ESCAPE => self.dialog_choose(Choice::Cancel),
            VK_RETURN => {
                if let Some(c) = d.enter_action() {
                    self.dialog_choose(c);
                }
            }
            VK_TAB => {
                d.move_focus(!shift);
                if d.field_focused() {
                    self.restart_caret();
                }
            }
            _ if in_field => {
                self.field_key(vk, shift, ctrl);
            }
            VK_SPACE => {
                if let Some(c) = d.enter_action() {
                    self.dialog_choose(c);
                }
            }
            VK_LEFT | VK_UP => d.move_focus(false),
            VK_RIGHT | VK_DOWN => d.move_focus(true),
            _ => {}
        }
        self.invalidate();
    }

    pub(super) fn dialog_click(&mut self, index: usize) {
        let Some(c) = self.dialog.as_ref().and_then(|(_, d)| d.buttons.get(index)).filter(|b| b.enabled).map(|b| b.action) else {
            return;
        };
        self.dialog_choose(c);
    }

    fn on_recycled(&mut self, path: PathBuf, ok: bool) {
        if !ok {
            self.show_toast("Couldn't delete", false);
            return;
        }
        // The covering file: the strip's next file takes its place when the strip lists them all, else the
        // photo underneath comes back.
        if self.placeholder.as_ref().is_some_and(|p| p.path == path) {
            let rank = self.viewer.listing.as_ref().and_then(|l| l.rank_of(&path)).filter(|_| self.strip_lists_all());
            self.viewer.remove(&path, self.gfx.as_ref());
            self.placeholder = None;
            let n = self.strip_len();
            match rank {
                Some(r) if n > 0 => self.open_strip_item(r.min(n - 1)),
                _ => self.current_changed(),
            }
            self.invalidate();
            return;
        }
        if self.current_path() == Some(path.as_path()) {
            self.begin_navigation();
        }
        match self.viewer.remove(&path, self.gfx.as_ref()) {
            Removed::Other => self.invalidate(),
            Removed::Current(out) => {
                self.apply(out);
                self.current_changed();
            }
            Removed::Emptied => self.close(),
        }
    }

    // --- Results ------------------------------------------------------------------------------------

    pub(super) fn on_file_op(&mut self, done: Done) {
        match done {
            Done::Rotated { path, ok } => self.on_rotated(path, ok),
            Done::Recycled { path, ok } => self.on_recycled(path, ok),
            Done::Renamed { from, to } => self.on_renamed(from, to),
            Done::Wallpaper { ok } => self.show_toast(if ok { "Wallpaper set" } else { "Couldn't set the wallpaper" }, false),
            Done::Copied { path, image } => {
                if clipboard::set_image(self.hwnd, &path, image.as_ref()) {
                    self.show_toast("Copied", false);
                }
            }
        }
    }
}
