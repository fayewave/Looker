//! The file actions behind the toolbar, context menu and keys (MainViewModel's M7 commands): the rotation
//! preview and its save, copy image, set as wallpaper, delete with a confirm dialog, plus the OSD toast
//! that reports them. Disk work runs in `fileops` and comes back as `WM_FILE_OP`.

use super::*;
use crate::fileops::{self, Done};
use crate::ui::{Dialog, DialogButton, Toast};
use crate::viewer::Removed;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Choice {
    Confirm,
    Cancel,
}

pub(super) enum DialogKind {
    Delete(PathBuf),
}

impl App {
    // --- Toast --------------------------------------------------------------------------------------

    pub(super) fn show_toast(&mut self, text: impl Into<String>, quick: bool) {
        self.toast = Some(Toast::new(text.into(), quick, self.toast.as_ref()));
        self.invalidate();
    }

    // --- Rotation preview ---------------------------------------------------------------------------

    pub(super) fn can_rotate(&self) -> bool {
        !self.saving_rotation && self.viewer.current_entry().is_some_and(|e| e.pages == 0)
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
        fileops::spawn(self.hwnd, false, move || {
            let image = fileops::full_image(&path);
            Done::Copied { path, image }
        });
    }

    pub(super) fn copy_path(&mut self) {
        let Some(path) = self.current_path() else { return };
        if clipboard::set_text(self.hwnd, &path.to_string_lossy()) {
            self.show_toast("Copied path", false);
        }
    }

    pub(super) fn set_wallpaper(&mut self) {
        let Some(path) = self.current_path().map(Path::to_path_buf) else { return };
        fileops::spawn(self.hwnd, false, move || Done::Wallpaper { ok: fileops::set_wallpaper(&path) });
    }

    // --- Delete -------------------------------------------------------------------------------------

    /// Asks before recycling. Cancel is the default button, so a stray Enter never deletes.
    pub(super) fn confirm_delete(&mut self, from_keyboard: bool) {
        let Some(path) = self.current_path().map(Path::to_path_buf) else { return };
        self.close_menu();
        self.hide_tooltip();
        let body = format!("Move \u{201C}{}\u{201D} to the Recycle Bin?", file_name(&path));
        let buttons = vec![
            DialogButton { label: "Delete".into(), action: Choice::Confirm },
            DialogButton { label: "Cancel".into(), action: Choice::Cancel },
        ];
        self.dialog = Some((DialogKind::Delete(path), Dialog::new("Delete photo".into(), body, buttons, 1, from_keyboard)));
        self.pressed = None;
        self.invalidate();
    }

    /// A dialog button was chosen (click, Enter, Escape = Cancel).
    pub(super) fn dialog_choose(&mut self, choice: Choice) {
        let Some((kind, _)) = self.dialog.take() else { return };
        self.invalidate();
        if choice != Choice::Confirm {
            return;
        }
        match kind {
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
    pub(super) fn dialog_key(&mut self, vk: VIRTUAL_KEY, shift: bool) {
        let Some((_, d)) = &mut self.dialog else { return };
        match vk {
            VK_ESCAPE => self.dialog_choose(Choice::Cancel),
            VK_RETURN | VK_SPACE => {
                let c = d.buttons[d.focus].action;
                self.dialog_choose(c);
            }
            VK_TAB => d.move_focus(!shift),
            VK_LEFT | VK_UP => d.move_focus(false),
            VK_RIGHT | VK_DOWN => d.move_focus(true),
            _ => {}
        }
        self.invalidate();
    }

    pub(super) fn dialog_click(&mut self, index: usize) {
        let Some(c) = self.dialog.as_ref().and_then(|(_, d)| d.buttons.get(index)).map(|b| b.action) else { return };
        self.dialog_choose(c);
    }

    fn on_recycled(&mut self, path: PathBuf, ok: bool) {
        if !ok {
            self.show_toast("Couldn't delete", false);
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
            Done::Wallpaper { ok } => self.show_toast(if ok { "Wallpaper set" } else { "Couldn't set the wallpaper" }, false),
            Done::Copied { path, image } => {
                if clipboard::set_image(self.hwnd, &path, image.as_ref()) {
                    self.show_toast("Copied", false);
                }
            }
        }
    }
}
