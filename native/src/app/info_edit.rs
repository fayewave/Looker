//! Editing a photo's words in the info card (Transient's Metadata panel): a pencil on a hovered field of the
//! Description group, or a double-click, edits it in place (Enter keeps it, empty removes it, Esc or a click
//! away leaves it); the rating is picked from five stars; "+ Add a field" offers the ones the file lacks.
//!
//! Nothing is written until Ctrl+S (Faye, 2026-11-11, as Transient's): edits wait in an unsaved layer per file
//! (each field's value in the file and its new one), shown at once and undoable (Ctrl+Z / Ctrl+Y while the
//! photo they're on is up). Ctrl+S writes every file's edits on a thread (`metawrite`); a failure keeps them
//! and says why. Closing with unsaved edits asks Save / Don't save / Cancel.

use std::collections::{BTreeMap, HashMap};

use super::actions::{Choice, DialogKind};
use super::info::Block;
use crate::fileops::{self, Done};
use super::*;
use crate::metawrite::{self, Field};
use crate::textedit::TextEdit;
use crate::ui::TextField;

/// One file's unsaved edits: each field's value in the file, and its new one (None: removed).
pub(super) type FileEdits = BTreeMap<Field, (Option<String>, Option<String>)>;

/// An undo step: one field of one file's unsaved layer, before and after.
#[derive(Clone)]
struct Step {
    path: PathBuf,
    field: Field,
    before: Option<(Option<String>, Option<String>)>,
    after: Option<(Option<String>, Option<String>)>,
}

/// The field being typed into (or its rating picked).
pub(super) struct Editing {
    pub path: PathBuf,
    pub field: Field,
    pub text: TextField,
}

#[derive(Default)]
pub(super) struct Edits {
    pub pending: HashMap<PathBuf, FileEdits>,
    undo: Vec<Step>,
    redo: Vec<Step>,
    pub editing: Option<Editing>,
    pub saving: bool,
    /// Closing waits for the save it asked for.
    pub close_after: bool,
}

/// The Description group's rows in the card's order (`describe::Description::rows`): where a field goes in.
const ORDER: [&str; 8] = ["Title", "Description", "Keywords", "Rating", "Label", "Author", "Copyright", "Software"];

/// A rating as the card shows it: five stars, the first `k` filled.
pub(super) fn stars(k: u32) -> String {
    format!("{}{}", "\u{2605}".repeat(k as usize), "\u{2606}".repeat(5 - k.min(5) as usize))
}

/// What was typed, as it would be kept (None: removed).
fn normalize(f: Field, s: &str) -> Option<String> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    match f {
        Field::Keywords | Field::Author => Some(metawrite::split_keywords(s).join(", ")).filter(|k| !k.is_empty()),
        Field::Rating => metawrite::parse_rating(s).filter(|&k| k > 0).map(stars),
        _ => Some(s.to_string()),
    }
}

impl App {
    /// The photo the card can edit now: one file shown (not a selection's summary or a file Looker can't open),
    /// in a format Looker writes, its metadata read.
    pub(super) fn info_edit_path(&self) -> Option<PathBuf> {
        if self.info_many() || self.placeholder.is_some() {
            return None;
        }
        let path = self.current_path()?;
        (metawrite::can_write(path) && self.current_exif().is_some()).then(|| path.to_path_buf())
    }

    /// The field's value in the file.
    fn file_value(&self, f: Field) -> Option<String> {
        self.current_exif()?.words.iter().find(|(l, _)| *l == f.label()).map(|(_, v)| v.clone())
    }

    /// The field's value as the card shows it: the unsaved one, else the file's.
    fn shown_value(&self, path: &Path, f: Field) -> Option<String> {
        match self.info_card.edits.pending.get(path).and_then(|m| m.get(&f)) {
            Some((_, new)) => new.clone(),
            None => self.file_value(f),
        }
    }

    pub(super) fn info_unsaved(&self, path: &Path) -> bool {
        self.info_card.edits.pending.get(path).is_some_and(|m| !m.is_empty())
    }

    pub(super) fn info_unsaved_files(&self) -> usize {
        self.info_card.edits.pending.values().filter(|m| !m.is_empty()).count()
    }

    pub(super) fn info_editing(&self) -> bool {
        self.info_card.edits.editing.is_some()
    }

    /// The text box taking the keys, while a field (not the rating) is being typed.
    pub(super) fn info_field_mut(&mut self) -> Option<&mut TextField> {
        self.info_card.edits.editing.as_mut().filter(|e| e.field != Field::Rating).map(|e| &mut e.text)
    }

    /// Which field each block is, when the card can edit it (a row of the Description group).
    pub(super) fn info_fields(&self, blocks: &[Block]) -> Vec<Option<Field>> {
        let editable = self.info_edit_path().is_some();
        let mut group = "";
        blocks
            .iter()
            .map(|b| match b {
                Block::Title(t) => {
                    group = t;
                    None
                }
                Block::Row { label, path: false, .. } if editable && group == "Description" => Field::from_label(label),
                _ => None,
            })
            .collect()
    }

    /// The field of the block at `i`, if it's editable.
    pub(super) fn info_row_field(&self, i: usize) -> Option<Field> {
        let blocks = self.info_blocks();
        self.info_fields(&blocks).get(i).copied().flatten()
    }

    /// The single card's blocks with the unsaved layer over them: changed values, added and removed fields, the
    /// field being typed (even an empty one), "+ Add a field", and the unsaved note under the name.
    pub(super) fn apply_info_edits(&self, blocks: &mut Vec<Block>) {
        let Some(path) = self.info_edit_path() else { return };
        let edits = self.info_card.edits.pending.get(&path);
        let editing = self.info_card.edits.editing.as_ref().filter(|e| e.path == path).map(|e| e.field);
        // The Description group: where it is, or a new one after Image (else after File).
        let group_end = |blocks: &Vec<Block>, start: usize| (start + 1..blocks.len()).find(|&i| matches!(blocks[i], Block::Title(_))).unwrap_or(blocks.len());
        let start = match blocks.iter().position(|b| matches!(b, Block::Title("Description"))) {
            Some(s) => s,
            None => {
                let after = blocks.iter().position(|b| matches!(b, Block::Title("Image"))).or_else(|| blocks.iter().position(|b| matches!(b, Block::Title("File"))));
                let at = after.map_or(blocks.len(), |s| group_end(blocks, s));
                blocks.insert(at, Block::Title("Description"));
                at
            }
        };
        let rank = |l: &str| ORDER.iter().position(|o| *o == l).unwrap_or(ORDER.len());
        for f in Field::ALL {
            let end = group_end(blocks, start);
            let row = (start + 1..end).find(|&i| matches!(&blocks[i], Block::Row { label, .. } if *label == f.label()));
            let change = edits.and_then(|m| m.get(&f));
            let value = match change {
                Some((_, new)) => new.clone(),
                None => row.and_then(|i| match &blocks[i] {
                    Block::Row { value, .. } => Some(value.clone()),
                    _ => None,
                }),
            };
            let value = match (value, editing == Some(f)) {
                (Some(v), _) => Some(v),
                (None, true) => Some(String::new()),
                (None, false) => None,
            };
            match (row, value) {
                (Some(i), Some(v)) => {
                    if let Block::Row { value, copy, .. } = &mut blocks[i] {
                        *value = v;
                        *copy = None;
                    }
                }
                (Some(i), None) => {
                    blocks.remove(i);
                }
                (None, Some(v)) => {
                    let at = (start + 1..end).find(|&i| matches!(&blocks[i], Block::Row { label, .. } if rank(label) > rank(f.label()))).unwrap_or(end);
                    blocks.insert(at, Block::Row { label: f.label(), value: v, path: false, copy: None, dim: false });
                }
                (None, None) => {}
            }
        }
        // "+ Add a field" while some are missing.
        let end = group_end(blocks, start);
        let have = (start + 1..end).filter(|&i| matches!(&blocks[i], Block::Row { label, .. } if Field::from_label(label).is_some())).count();
        if have < Field::ALL.len() {
            blocks.insert(end, Block::Add);
        }
        // The note under the name.
        let note = if self.info_card.edits.saving {
            Some("Saving\u{2026}".to_string())
        } else if self.info_unsaved(&path) {
            Some("Unsaved changes: Ctrl+S saves them into the file".to_string())
        } else {
            None
        };
        if let Some(n) = note {
            let at = blocks.iter().position(|b| matches!(b, Block::Heading(..))).map_or(0, |i| i + 1);
            blocks.insert(at, Block::Note(n));
        }
    }

    /// Starts typing into field `f` (the rating: its stars).
    pub(super) fn info_start_edit(&mut self, f: Field) {
        let Some(path) = self.info_edit_path() else { return };
        if self.explorer.field.is_some() {
            self.explorer_blur_field();
        }
        self.explorer.focused = false;
        let value = if f == Field::Rating { String::new() } else { self.shown_value(&path, f).unwrap_or_default() };
        let mut text = TextField::new(TextEdit::new(&value));
        text.edit.select_all();
        self.info_card.edits.editing = Some(Editing { path, field: f, text });
        self.hide_tooltip();
        self.set_ime(f != Field::Rating);
        self.restart_caret();
        self.invalidate();
    }

    fn info_end_edit(&mut self) -> Option<Editing> {
        let e = self.info_card.edits.editing.take();
        if e.is_some() {
            self.caret_rect = None;
            self.text_drag = None;
            self.set_ime(false);
            self.invalidate();
        }
        e
    }

    /// Enter: what was typed goes into the unsaved layer.
    pub(super) fn info_commit(&mut self) {
        let Some(e) = self.info_end_edit() else { return };
        let new = normalize(e.field, &e.text.edit.text());
        self.set_info_value(&e.path, e.field, new);
    }

    /// Esc or a click away: the field is left as it was.
    pub(super) fn info_cancel(&mut self) {
        self.info_end_edit();
    }

    /// A star of the rating picker: that many stars; the current count again clears it.
    pub(super) fn info_pick_stars(&mut self, k: u32) {
        let Some(e) = self.info_end_edit() else { return };
        let now = self.shown_value(&e.path, Field::Rating).and_then(|s| metawrite::parse_rating(&s)).unwrap_or(0);
        let new = (k != now && k > 0).then(|| stars(k));
        self.set_info_value(&e.path, Field::Rating, new);
    }

    /// Sets field `f` of the shown photo to `new` in the unsaved layer (the file's own value again drops the
    /// edit), as one undo step.
    fn set_info_value(&mut self, path: &Path, f: Field, new: Option<String>) {
        let file = self.file_value(f);
        let edits = &mut self.info_card.edits;
        let m = edits.pending.entry(path.to_path_buf()).or_default();
        let before = m.get(&f).cloned();
        let old = before.as_ref().map_or(file, |b| b.0.clone());
        let after = if new == old {
            m.remove(&f);
            None
        } else {
            m.insert(f, (old.clone(), new.clone()));
            Some((old, new))
        };
        if m.is_empty() {
            edits.pending.remove(path);
        }
        if before != after {
            edits.undo.push(Step { path: path.to_path_buf(), field: f, before, after });
            edits.redo.clear();
        }
        self.invalidate();
    }

    fn put_step_state(&mut self, path: &Path, f: Field, state: Option<(Option<String>, Option<String>)>) {
        let pending = &mut self.info_card.edits.pending;
        let m = pending.entry(path.to_path_buf()).or_default();
        match state {
            Some(s) => {
                m.insert(f, s);
            }
            None => {
                m.remove(&f);
            }
        }
        if m.is_empty() {
            pending.remove(path);
        }
        self.invalidate();
    }

    /// Ctrl+Z: the last edit, when it's on the photo shown. Returns whether it undid one.
    pub(super) fn info_undo(&mut self) -> bool {
        let Some(path) = self.current_path().map(Path::to_path_buf) else { return false };
        if !self.info_card.edits.undo.last().is_some_and(|s| s.path == path) {
            return false;
        }
        let s = self.info_card.edits.undo.pop().unwrap();
        self.put_step_state(&s.path, s.field, s.before.clone());
        self.show_toast(format!("Undid the {} edit", s.field.label().to_lowercase()), false);
        self.info_card.edits.redo.push(s);
        true
    }

    /// Ctrl+Y: the edit undone last, when it's on the photo shown.
    pub(super) fn info_redo(&mut self) -> bool {
        let Some(path) = self.current_path().map(Path::to_path_buf) else { return false };
        if !self.info_card.edits.redo.last().is_some_and(|s| s.path == path) {
            return false;
        }
        let s = self.info_card.edits.redo.pop().unwrap();
        self.put_step_state(&s.path, s.field, s.after.clone());
        self.show_toast(format!("Redid the {} edit", s.field.label().to_lowercase()), false);
        self.info_card.edits.undo.push(s);
        true
    }

    /// Keys while a field is being edited: Enter keeps it, Esc leaves it, a digit picks the rating; the rest go
    /// to its text box.
    pub(super) fn info_edit_key(&mut self, vk: VIRTUAL_KEY, shift: bool, ctrl: bool) {
        let rating = self.info_card.edits.editing.as_ref().is_some_and(|e| e.field == Field::Rating);
        match vk {
            VK_RETURN if !rating => self.info_commit(),
            VK_ESCAPE => self.info_cancel(),
            _ if rating && (VK_0.0..=VK_5.0).contains(&vk.0) => self.info_pick_stars((vk.0 - VK_0.0) as u32),
            _ if rating => {}
            _ => {
                self.field_key(vk, shift, ctrl);
            }
        }
    }

    pub(super) fn info_edit_char(&mut self, unit: u16) {
        if unit < 0x20 || unit == 0x7F {
            return;
        }
        if let Some(f) = self.info_field_mut() {
            f.edit.type_unit(unit);
            self.restart_caret();
        }
    }

    /// The "+ Add a field" menu: the fields the photo doesn't have.
    pub(super) fn open_info_add_menu(&mut self, r: D2D_RECT_F) {
        let Some(path) = self.info_edit_path() else { return };
        let missing: Vec<Field> = Field::ALL.into_iter().filter(|&f| self.shown_value(&path, f).is_none()).collect();
        self.open_info_add(missing, r);
    }

    /// Ctrl+S: every photo's unsaved edits into their files, on a thread. Returns whether there were any.
    pub(super) fn save_info_edits(&mut self) -> bool {
        if self.info_card.edits.saving {
            return true;
        }
        if let Some(e) = &self.info_card.edits.editing {
            // A field still being typed is kept first, as Transient's Enter would.
            if e.field != Field::Rating {
                self.info_commit();
            }
        }
        let jobs: Vec<(PathBuf, Vec<(Field, Option<String>)>)> =
            self.info_card.edits.pending.iter().filter(|(_, m)| !m.is_empty()).map(|(p, m)| (p.clone(), m.iter().map(|(f, (_, new))| (*f, new.clone())).collect())).collect();
        if jobs.is_empty() {
            return false;
        }
        self.info_card.edits.saving = true;
        self.invalidate();
        fileops::spawn(self.hwnd, false, move || Done::MetaWritten {
            results: jobs
                .into_iter()
                .map(|(p, edits)| {
                    let r = metawrite::write(&p, &edits);
                    (p, edits, r)
                })
                .collect(),
        });
        true
    }

    pub(super) fn on_meta_written(&mut self, results: Vec<(PathBuf, Vec<(Field, Option<String>)>, Result<(), String>)>) {
        self.info_card.edits.saving = false;
        let mut saved = 0;
        let mut failed: Vec<(PathBuf, String)> = Vec::new();
        for (path, written, r) in results {
            match r {
                Ok(()) => {
                    saved += 1;
                    // What was written leaves the layer; an edit made while it saved stays.
                    let edits = &mut self.info_card.edits;
                    if let Some(m) = edits.pending.get_mut(&path) {
                        for (f, new) in &written {
                            if m.get(f).is_some_and(|(_, n)| n == new) {
                                m.remove(f);
                            }
                        }
                        if m.is_empty() {
                            edits.pending.remove(&path);
                        }
                    }
                    if !self.info_unsaved(&path) {
                        let edits = &mut self.info_card.edits;
                        edits.undo.retain(|s| s.path != path);
                        edits.redo.retain(|s| s.path != path);
                    }
                    if let Some(l) = self.viewer.listing.as_mut() {
                        l.refresh(&path);
                    }
                    if self.current_path() == Some(path.as_path()) {
                        self.forget_exif();
                    }
                }
                Err(e) => failed.push((path, e)),
            }
        }
        match failed.first() {
            None => {
                self.show_toast(if saved == 1 { "Saved".to_string() } else { format!("Saved {saved} files") }, false);
                if std::mem::take(&mut self.info_card.edits.close_after) {
                    unsafe {
                        let _ = PostMessageW(Some(self.hwnd), WM_CLOSE, WPARAM(0), LPARAM(0));
                    }
                }
            }
            Some((p, e)) => {
                self.info_card.edits.close_after = false;
                self.show_toast(format!("Couldn't save {}: {e}", file_name(p)), false);
            }
        }
        self.invalidate();
    }

    /// Closing with unsaved edits: Save / Don't save / Cancel. Returns whether it asked (the close waits).
    pub(super) fn ask_info_close(&mut self) -> bool {
        let n = self.info_unsaved_files();
        if n == 0 || self.dialog.is_some() {
            return n > 0;
        }
        let what = match self.info_card.edits.pending.keys().next() {
            Some(p) if n == 1 => file_name(p),
            _ => format!("{n} files"),
        };
        let buttons = vec![
            ui::DialogButton::new("Save", Choice::Confirm),
            ui::DialogButton::new("Don't save", Choice::Discard),
            ui::DialogButton::new("Cancel", Choice::Cancel),
        ];
        let body = format!("Do you want to save the details you changed in {what}?");
        self.open_dialog_ui(DialogKind::InfoClose, ui::Dialog::new("Looker".into(), body, buttons, 0, false));
        true
    }

    pub(super) fn info_close_answer(&mut self, choice: Choice) {
        match choice {
            Choice::Confirm => {
                self.info_card.edits.close_after = true;
                if !self.save_info_edits() {
                    self.info_card.edits.close_after = false;
                }
            }
            Choice::Discard => {
                self.info_card.edits.pending.clear();
                self.info_card.edits.undo.clear();
                self.info_card.edits.redo.clear();
                unsafe {
                    let _ = PostMessageW(Some(self.hwnd), WM_CLOSE, WPARAM(0), LPARAM(0));
                }
            }
            Choice::Cancel => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typed_values_are_tidied() {
        assert_eq!(normalize(Field::Keywords, " a ;b,, c "), Some("a, b, c".into()));
        assert_eq!(normalize(Field::Title, "   "), None);
        assert_eq!(normalize(Field::Rating, "3"), Some(stars(3)));
        assert_eq!(normalize(Field::Rating, "0"), None);
    }
}
