//! Editing a photo's words in the info card (Transient's Metadata panel): a pencil on a hovered field of the
//! Description group, or a double-click, edits it in place (Enter keeps it, empty removes it, Esc or a click
//! away leaves it); the rating is picked from five stars; "+ Add a field" offers the ones the file lacks.
//!
//! With several files selected the summary edits them all at once (Transient's): a value typed goes to every
//! selected file Looker can write (JPEG, TIFF, PNG, HEIC), as one undo step. A field whose values differ starts empty;
//! left empty it changes nothing, and keywords typed there are added to each file's own instead of replacing
//! them.
//!
//! Nothing is written until Ctrl+S (Faye, 2026-11-11, as Transient's): edits wait in an unsaved layer per file
//! (each field's value in the file and its new one), shown at once and undoable (Ctrl+Z / Ctrl+Y while the
//! photo or selection they're on is up). Ctrl+S writes every file's edits on a thread (`metawrite`); a failure
//! keeps them and says why. Closing with unsaved edits asks Save / Don't save / Cancel.

use std::collections::{BTreeMap, HashMap};

use super::actions::{Choice, DialogKind};
use super::info::{Block, grouped};
use crate::fileops::{self, Done};
use super::*;
use crate::metadata::Exif;
use crate::metawrite::{self, Field};
use crate::textedit::TextEdit;
use crate::ui::TextField;

/// A field's unsaved state in one file: its value in the file, and its new one (None: removed).
type Change = (Option<String>, Option<String>);

/// One file's unsaved edits.
pub(super) type FileEdits = BTreeMap<Field, Change>;

/// An undo step: one field, in each file the edit changed, before and after.
#[derive(Clone)]
struct Step {
    field: Field,
    changes: Vec<(PathBuf, Option<Change>, Option<Change>)>,
}

/// The field being typed into (or its rating picked), and the files it goes to.
pub(super) struct Editing {
    pub paths: Vec<PathBuf>,
    pub field: Field,
    pub text: TextField,
    /// The files differed (or only some had it) when it began: left empty it changes nothing, and keywords
    /// typed are added to each file's own.
    pub mixed: bool,
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

/// A file's keywords with `add`'s appended (those it lacks, case aside).
fn add_keywords(own: Option<&str>, add: &str) -> Option<String> {
    let mut k = own.map(metawrite::split_keywords).unwrap_or_default();
    for w in metawrite::split_keywords(add) {
        if !k.iter().any(|x| x.eq_ignore_ascii_case(&w)) {
            k.push(w);
        }
    }
    (!k.is_empty()).then(|| k.join(", "))
}

/// "Undid the keywords edit" (", in 6 files").
fn step_text(verb: &str, s: &Step) -> String {
    let what = format!("{verb} the {} edit", s.field.label().to_lowercase());
    if s.changes.len() > 1 { format!("{what} in {} files", grouped(s.changes.len() as u64)) } else { what }
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

    /// The files an edit in the card goes to: the photo shown, or the selection's files Looker can write (once
    /// they're all read). Empty: nothing to edit.
    pub(super) fn info_targets(&self) -> Vec<PathBuf> {
        if self.info_many() {
            return self.many_read().map(|v| v.into_iter().filter(|(p, e)| e.is_some() && metawrite::can_write(p)).map(|(p, _)| p.to_path_buf()).collect()).unwrap_or_default();
        }
        self.info_edit_path().into_iter().collect()
    }

    /// Where Ctrl+Z reaches: the photo shown, or every file of the selection.
    fn info_scope(&self) -> Vec<PathBuf> {
        if self.info_many() { self.many_paths() } else { self.current_path().map(Path::to_path_buf).into_iter().collect() }
    }

    /// The field's value in the file, as last read.
    fn file_value(&self, path: &Path, f: Field) -> Option<String> {
        let exif: Option<&Exif> = if self.info_many() {
            self.many_exif(path)
        } else if self.current_path() == Some(path) {
            self.current_exif()
        } else {
            None
        };
        exif?.words.iter().find(|(l, _)| *l == f.label()).map(|(_, v)| v.clone())
    }

    /// The field's value as the card shows it: the unsaved one, else the file's.
    fn shown_value(&self, path: &Path, f: Field) -> Option<String> {
        match self.info_card.edits.pending.get(path).and_then(|m| m.get(&f)) {
            Some((_, new)) => new.clone(),
            None => self.file_value(path, f),
        }
    }

    /// The value `paths` share for `f` (None: none of them has it), or (None, true) when they differ.
    fn info_common(&self, paths: &[PathBuf], f: Field) -> (Option<String>, bool) {
        let values: Vec<Option<String>> = paths.iter().map(|p| self.shown_value(p, f)).collect();
        if values.windows(2).all(|w| w[0] == w[1]) { (values.into_iter().next().flatten(), false) } else { (None, true) }
    }

    /// The rating the star picker starts from: the one the files being edited share.
    pub(super) fn info_rating_now(&self) -> u32 {
        let Some(e) = &self.info_card.edits.editing else { return 0 };
        self.info_common(&e.paths, Field::Rating).0.and_then(|s| metawrite::parse_rating(&s)).unwrap_or(0)
    }

    pub(super) fn info_unsaved(&self, path: &Path) -> bool {
        self.info_card.edits.pending.get(path).is_some_and(|m| !m.is_empty())
    }

    pub(super) fn info_unsaved_files(&self) -> usize {
        self.info_card.edits.pending.values().filter(|m| !m.is_empty()).count()
    }

    /// The fields with unsaved edits in the files the card shows: their rows are tinted.
    pub(super) fn info_changed_fields(&self) -> Vec<Field> {
        let mut out = Vec::new();
        for p in self.info_targets() {
            for f in self.info_card.edits.pending.get(&p).into_iter().flat_map(|m| m.keys()) {
                if !out.contains(f) {
                    out.push(*f);
                }
            }
        }
        out
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
        let editable = !self.info_targets().is_empty();
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

    /// The card's blocks (one photo's, or a selection's summary) with the unsaved layer over them: changed values
    /// (a selection's as shared or mixed), added and removed fields, the field being typed (even an empty one),
    /// "+ Add a field", and which of a selection's files edits reach.
    pub(super) fn apply_info_edits(&self, blocks: &mut Vec<Block>) {
        let targets = self.info_targets();
        if targets.is_empty() {
            return;
        }
        let many = self.info_many();
        // Every file the card sums up (a selection's unwritable ones too: their values count in "Mixed").
        let covered: Vec<PathBuf> = if many { self.many_read().unwrap_or_default().into_iter().map(|(p, _)| p.to_path_buf()).collect() } else { targets.clone() };
        let pending = &self.info_card.edits.pending;
        let editing = self.info_card.edits.editing.as_ref().filter(|e| e.paths == targets).map(|e| e.field);
        // The Description group: where it is, or a new one after Image (else after File or Selection).
        let group_end = |blocks: &Vec<Block>, start: usize| (start + 1..blocks.len()).find(|&i| matches!(blocks[i], Block::Title(_))).unwrap_or(blocks.len());
        let start = match blocks.iter().position(|b| matches!(b, Block::Title("Description"))) {
            Some(s) => s,
            None => {
                let after = ["Image", "File", "Selection"].iter().find_map(|t| blocks.iter().position(|b| matches!(b, Block::Title(x) if x == t)));
                let at = after.map_or(blocks.len(), |s| group_end(blocks, s));
                blocks.insert(at, Block::Title("Description"));
                at
            }
        };
        let rank = |l: &str| ORDER.iter().position(|o| *o == l).unwrap_or(ORDER.len());
        for f in Field::ALL {
            let touched = targets.iter().any(|p| pending.get(p).is_some_and(|m| m.contains_key(&f)));
            let being = editing == Some(f);
            // Untouched: as the files have it.
            if !touched && !being {
                continue;
            }
            let end = group_end(blocks, start);
            let row = (start + 1..end).find(|&i| matches!(&blocks[i], Block::Row { label, .. } if *label == f.label()));
            let values: Vec<Option<String>> = covered.iter().map(|p| self.shown_value(p, f)).collect();
            let plain = |v: String| Block::Row { label: f.label(), value: v, path: false, copy: None, dim: false };
            let new_row = if values.iter().all(Option::is_none) {
                if being {
                    Some(plain(String::new()))
                } else {
                    // Removed: the row stays (tinted, dimmed) until the save takes it out.
                    Some(Block::Row { label: f.label(), value: "Removed when you save".to_string(), path: false, copy: Some(String::new()), dim: true })
                }
            } else if many {
                let refs: Vec<Option<&String>> = values.iter().map(Option::as_ref).collect();
                Some(super::info_many::shared(f.label(), &refs, values.len()))
            } else {
                values.into_iter().next().flatten().map(plain)
            };
            match (row, new_row) {
                (Some(i), Some(b)) => blocks[i] = b,
                (Some(i), None) => {
                    blocks.remove(i);
                }
                (None, Some(b)) => {
                    let at = (start + 1..end).find(|&i| matches!(&blocks[i], Block::Row { label, .. } if rank(label) > rank(f.label()))).unwrap_or(end);
                    blocks.insert(at, b);
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
        // A selection with files Looker can't write: which ones an edit reaches, first in the group.
        if many && targets.len() < covered.len() {
            let value = format!("{} of {} files (Looker writes JPEG, TIFF, PNG and HEIC)", grouped(targets.len() as u64), grouped(covered.len() as u64));
            blocks.insert(start + 1, Block::Row { label: "Edits go to", value, path: false, copy: None, dim: true });
        }
        // (The count of unsaved edits, Save and Discard all are in the card's footer.)
    }

    /// Starts typing into field `f` (the rating: its stars), for the photo or every writable selected file.
    pub(super) fn info_start_edit(&mut self, f: Field) {
        let paths = self.info_targets();
        if paths.is_empty() {
            return;
        }
        if self.explorer.field.is_some() {
            self.explorer_blur_field();
        }
        self.explorer.focused = false;
        let (common, mixed) = self.info_common(&paths, f);
        let value = if f == Field::Rating { String::new() } else { common.unwrap_or_default() };
        let mut text = TextField::new(TextEdit::new(&value));
        text.edit.select_all();
        self.info_card.edits.editing = Some(Editing { paths, field: f, text, mixed });
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

    /// Enter: what was typed goes into the unsaved layer (of every file it's for).
    pub(super) fn info_commit(&mut self) {
        let Some(e) = self.info_end_edit() else { return };
        let new = normalize(e.field, &e.text.edit.text());
        match new {
            // Left empty over differing values: they stay as they are.
            None if e.mixed => {}
            Some(add) if e.mixed && e.field == Field::Keywords => {
                self.set_info_values(&e.paths, e.field, |own| add_keywords(own.as_deref(), &add));
                if e.paths.len() > 1 {
                    self.show_toast(format!("Added to each file's keywords ({} files)", grouped(e.paths.len() as u64)), false);
                }
            }
            new => self.set_info_values(&e.paths, e.field, |_| new.clone()),
        }
    }

    /// Esc or a click away: the field is left as it was.
    pub(super) fn info_cancel(&mut self) {
        self.info_end_edit();
    }

    /// A star of the rating picker: that many stars; the shared count again clears it.
    pub(super) fn info_pick_stars(&mut self, k: u32) {
        let now = self.info_rating_now();
        let Some(e) = self.info_end_edit() else { return };
        let new = (k != now && k > 0).then(|| stars(k));
        self.set_info_values(&e.paths, Field::Rating, |_| new.clone());
    }

    /// Sets field `f` of each of `paths` to `make(its shown value)` in the unsaved layer (a file's own value
    /// again drops its edit), as one undo step.
    fn set_info_values(&mut self, paths: &[PathBuf], f: Field, make: impl Fn(Option<String>) -> Option<String>) {
        let mut changes = Vec::new();
        for p in paths {
            let file = self.file_value(p, f);
            let new = make(self.shown_value(p, f));
            let pending = &mut self.info_card.edits.pending;
            let m = pending.entry(p.clone()).or_default();
            let before = m.get(&f).cloned();
            let old = before.as_ref().map_or(file, |b| b.0.clone());
            let after = if new == old {
                m.remove(&f);
                None
            } else {
                m.insert(f, (old.clone(), new));
                m.get(&f).cloned()
            };
            if m.is_empty() {
                pending.remove(p);
            }
            if before != after {
                changes.push((p.clone(), before, after));
            }
        }
        if !changes.is_empty() {
            let edits = &mut self.info_card.edits;
            edits.undo.push(Step { field: f, changes });
            edits.redo.clear();
        }
        self.invalidate();
    }

    fn put_step_state(&mut self, path: &Path, f: Field, state: Option<Change>) {
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

    /// Whether `s` touches the photo or selection shown (Ctrl+Z reaches only those).
    fn step_in_scope(&self, s: Option<&Step>) -> bool {
        let Some(s) = s else { return false };
        let scope = self.info_scope();
        s.changes.iter().any(|c| scope.contains(&c.0))
    }

    /// Ctrl+Z: the last edit, when it's on the photo or selection shown. Returns whether it undid one.
    pub(super) fn info_undo(&mut self) -> bool {
        if !self.step_in_scope(self.info_card.edits.undo.last()) {
            return false;
        }
        let s = self.info_card.edits.undo.pop().unwrap();
        for (p, before, _) in &s.changes {
            self.put_step_state(p, s.field, before.clone());
        }
        self.show_toast(step_text("Undid", &s), false);
        self.info_card.edits.redo.push(s);
        true
    }

    /// Ctrl+Y: the edit undone last, when it's on the photo or selection shown.
    pub(super) fn info_redo(&mut self) -> bool {
        if !self.step_in_scope(self.info_card.edits.redo.last()) {
            return false;
        }
        let s = self.info_card.edits.redo.pop().unwrap();
        for (p, _, after) in &s.changes {
            self.put_step_state(p, s.field, after.clone());
        }
        self.show_toast(step_text("Redid", &s), false);
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

    /// Whether any of the files being edited has field `f` (as shown): the row menu's Remove.
    pub(super) fn info_has_field(&self, f: Field) -> bool {
        self.info_targets().iter().any(|p| self.shown_value(p, f).is_some())
    }

    /// The row menu's Remove (from all): the field goes from every file the card edits, as one undo step. The
    /// way to clear a field over a selection whose values differ (an empty box there leaves them be).
    pub(super) fn info_remove_field(&mut self, f: Field) {
        self.info_end_edit();
        let paths = self.info_targets();
        let n = paths.iter().filter(|p| self.shown_value(p, f).is_some()).count();
        if n == 0 {
            return;
        }
        self.set_info_values(&paths, f, |_| None);
        if paths.len() > 1 {
            self.show_toast(format!("{} removed from {} files (unsaved)", f.label(), grouped(n as u64)), false);
        }
    }

    /// The "+ Add a field" menu: the fields none of the files has.
    pub(super) fn open_info_add_menu(&mut self, r: D2D_RECT_F) {
        let paths = self.info_targets();
        if paths.is_empty() {
            return;
        }
        let missing: Vec<Field> = Field::ALL.into_iter().filter(|&f| paths.iter().all(|p| self.shown_value(p, f).is_none())).collect();
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
        let mut written_paths: Vec<PathBuf> = Vec::new();
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
                        for list in [&mut edits.undo, &mut edits.redo] {
                            for s in list.iter_mut() {
                                s.changes.retain(|c| c.0 != path);
                            }
                            list.retain(|s| !s.changes.is_empty());
                        }
                    }
                    written_paths.push(path.clone());
                    if let Some(l) = self.viewer.listing.as_mut() {
                        l.refresh(&path);
                    }
                    if self.current_path() == Some(path.as_path()) {
                        self.forget_exif();
                        // Its size and Modified time too (saved from a selection, it's still the photo up).
                        self.read_file_info();
                    }
                }
                Err(e) => failed.push((path, e)),
            }
        }
        // A selection's summary reads what was written.
        self.reread_many(&written_paths);
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

    /// The footer's Discard all: asks first.
    pub(super) fn ask_info_discard(&mut self) {
        let n: usize = self.info_card.edits.pending.values().map(|m| m.len()).sum();
        if n == 0 || self.dialog.is_some() {
            return;
        }
        let files = self.info_unsaved_files();
        let what = match (n, files) {
            (1, _) => "the unsaved change".to_string(),
            (_, 1) => format!("all {n} unsaved changes"),
            _ => format!("all {n} unsaved changes in {files} files"),
        };
        let buttons = vec![ui::DialogButton::new("Discard", Choice::Confirm), ui::DialogButton::new("Cancel", Choice::Cancel)];
        let body = format!("Discard {what}? The files stay as they are.");
        self.open_dialog_ui(DialogKind::InfoDiscard, ui::Dialog::new("Looker".into(), body, buttons, 0, false));
    }

    /// Every unsaved edit gone, and their undo steps.
    pub(super) fn info_discard_all(&mut self) {
        self.info_end_edit();
        let e = &mut self.info_card.edits;
        let n: usize = e.pending.values().map(|m| m.len()).sum();
        e.pending.clear();
        e.undo.clear();
        e.redo.clear();
        self.show_toast(if n == 1 { "Discarded the change".to_string() } else { format!("Discarded {n} changes") }, false);
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

    #[test]
    fn keywords_are_added_once() {
        assert_eq!(add_keywords(Some("sea, Sand"), "sand, sun"), Some("sea, Sand, sun".into()));
        assert_eq!(add_keywords(None, "sun"), Some("sun".into()));
    }
}
