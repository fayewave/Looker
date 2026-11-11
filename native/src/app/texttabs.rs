//! The text editor's documents and the rows above them, as Windows 11's Notepad has them (Faye, 2026-11-11): a
//! tab per document (the viewer's text file, files opened, new untitled ones), and a menu bar (File, Edit, View)
//! holding every command. Tabs and their unsaved edits outlive Looker: they're kept in a session (the data dir's
//! `text-session`) and come back, edits and all, without anything being written to the files themselves.
//!
//! The viewer's text file is a preview tab, as VS Code's: stepping through a folder of text files replaces it
//! rather than piling up tabs; editing it (or opening it from File) keeps it.

use std::path::{Path, PathBuf};

use super::textview::{Eol, Encoding, TextAct, TextView};
use super::*;
use super::actions::{Choice, DialogKind};
use crate::shell_icons;
use crate::ui::{Entry, MenuItem};

const MENUBAR_H: f32 = 30.0;

/// The editor's typeface: family, weight, italic, and size in DIPs.
#[derive(Clone, PartialEq, Debug)]
pub(super) struct FontSpec {
    pub family: String,
    pub weight: i32,
    pub italic: bool,
    pub px: f32,
}

/// Families offered under Edit > Font, those installed.
const FAMILIES: [&str; 16] = [
    "Consolas",
    "Cascadia Mono",
    "Cascadia Code",
    "Courier New",
    "Lucida Console",
    "Segoe UI",
    "Segoe UI Variable Text",
    "Arial",
    "Calibri",
    "Cambria",
    "Candara",
    "Georgia",
    "Tahoma",
    "Times New Roman",
    "Trebuchet MS",
    "Verdana",
];
const STYLES: [(&str, i32, bool); 6] = [("Light", 300, false), ("Regular", 400, false), ("Italic", 400, true), ("Semibold", 600, false), ("Bold", 700, false), ("Bold italic", 700, true)];
/// Points, as Notepad lists them.
pub(super) const SIZES: [f32; 15] = [8.0, 9.0, 10.0, 10.5, 11.0, 12.0, 14.0, 16.0, 18.0, 20.0, 24.0, 28.0, 36.0, 48.0, 72.0];

pub(super) struct TextDocs {
    pub(super) docs: Vec<TextView>,
    pub(super) active: usize,
    /// The viewer's text file as last seen.
    viewer: Option<PathBuf>,
    /// The viewer is on a text file: the editor is up.
    pub(super) shown: bool,
    restored: bool,
    /// The session as last written: each document's id and version.
    session_mark: Vec<(u64, u64)>,
    next_id: u64,
    /// Windows' spell checker, made the first time it's wanted (None inside: no checker for the language).
    pub(super) spell: Option<Option<crate::spell::Spell>>,
    /// The spell check of the lines on screen, on its own thread (started the first time it's wanted).
    pub(super) spell_worker: Option<crate::spell::Worker>,
    /// The tab strip's motion, drag and tooltips: the explorer's own (tabstrip.rs).
    pub(super) strip: super::tabstrip::TabStrip,
    /// Tabs still to close after "Close other tabs" (ids), one Save / Don't save / Cancel at a time.
    close_queue: Vec<u64>,
}

impl TextDocs {
    pub(super) fn new() -> TextDocs {
        TextDocs { docs: Vec::new(), active: 0, viewer: None, shown: false, restored: false, session_mark: Vec::new(), next_id: 1, spell: None, spell_worker: None, strip: Default::default(), close_queue: Vec::new() }
    }

    pub(super) fn cur(&self) -> Option<&TextView> {
        self.docs.get(self.active)
    }

    pub(super) fn cur_mut(&mut self) -> Option<&mut TextView> {
        self.docs.get_mut(self.active)
    }

    fn id(&mut self) -> u64 {
        self.next_id += 1;
        self.next_id
    }

    fn next_untitled(&self) -> u32 {
        (1..).find(|n| self.docs.iter().all(|d| d.untitled != *n)).unwrap_or(1)
    }

    /// Bitmaps and brushes die with their device.
    pub(super) fn device_lost(&mut self) {
        for d in &mut self.docs {
            d.device_lost();
        }
    }
}

fn session_dir() -> Option<PathBuf> {
    settings::data_dir().map(|d| d.join("text-session"))
}

fn text_ext(p: &Path) -> bool {
    format::of_path(p) == format::Format::Text
}

impl App {
    pub(super) fn font_spec(&self) -> FontSpec {
        let s = &self.settings;
        FontSpec { family: s.text_font.clone(), weight: s.text_font_weight, italic: s.text_font_italic, px: s.text_font_pt * 96.0 / 72.0 }
    }

    /// After every navigation and before each frame: the viewer's text file has its tab (and is the one shown).
    pub(super) fn prepare_text(&mut self) {
        if !self.text.restored {
            self.text.restored = true;
            self.restore_session();
        }
        let want = self.current_path().filter(|p| self.placeholder.is_none() && text_ext(p)).map(Path::to_path_buf);
        let was_shown = self.text.shown;
        self.text.shown = want.is_some();
        if want != self.text.viewer {
            self.text.viewer = want.clone();
            if let Some(p) = want {
                let from = self.text.active;
                match self.text.docs.iter().position(|d| d.path == p) {
                    Some(i) => {
                        self.text.active = i;
                        self.text_carry_focus(from);
                    }
                    None => {
                        let id = self.text.id();
                        let doc = TextView::open(p, id, &self.font_spec());
                        // The preview tab (unedited) gives way to it; else it's a tab of its own.
                        match self.text.docs.iter().position(|d| d.preview && !d.dirty()) {
                            Some(i) => {
                                self.text.docs[i] = doc;
                                self.text.active = i;
                            }
                            None => {
                                self.text.docs.push(doc);
                                self.text.active = self.text.docs.len() - 1;
                            }
                        }
                    }
                }
                self.text_carry_focus(from);
                self.text_check_disk();
            }
        }
        if self.text.shown && self.text.docs.is_empty() {
            self.text_new_tab();
        }
        if self.text.shown != was_shown {
            unsafe {
                if self.text.shown {
                    SetTimer(Some(self.hwnd), super::textview::TIMER_TEXT, 1000, None);
                } else {
                    let _ = KillTimer(Some(self.hwnd), super::textview::TIMER_TEXT);
                }
            }
            if !self.text.shown {
                self.text_blur();
            }
        }
    }

    /// The window is closing: the tabs and their unsaved edits go into the session (nothing is written to the
    /// files themselves, as Notepad does).
    pub(super) fn close_text(&mut self) {
        self.save_session(true);
    }

    // --- The session --------------------------------------------------------------------------------

    /// Writes the tabs, and the text of those with unsaved edits or no file, when anything changed since last time
    /// (`force`: whatever).
    pub(super) fn save_session(&mut self, force: bool) {
        let mark: Vec<(u64, u64)> = self.text.docs.iter().map(|d| (d.id, d.session_version())).collect();
        if !force && mark == self.text.session_mark {
            return;
        }
        let Some(dir) = session_dir() else { return };
        let _ = std::fs::create_dir_all(&dir);
        let mut out = format!("active={}\n", self.text.active);
        let mut keep = Vec::new();
        for d in &self.text.docs {
            let buffer = if d.untitled > 0 || d.dirty() {
                let name = format!("buffer-{}.txt", d.id);
                if std::fs::write(dir.join(&name), d.text_utf8()).is_ok() {
                    keep.push(name.clone());
                    name
                } else {
                    String::new()
                }
            } else {
                String::new()
            };
            if d.untitled > 0 && d.is_empty() {
                continue;
            }
            out.push_str(&format!(
                "tab={}\t{}\t{}\t{}\t{}\t{}\n",
                d.path.display(),
                d.untitled,
                buffer,
                Encoding::ALL.iter().position(|e| *e == d.encoding).unwrap_or(0),
                Eol::ALL.iter().position(|e| *e == d.eol).unwrap_or(0),
                d.edit_caret()
            ));
        }
        let _ = std::fs::write(dir.join("session.txt"), out);
        // Buffers no tab names any more.
        if let Ok(rd) = std::fs::read_dir(&dir) {
            for e in rd.flatten() {
                let n = e.file_name().to_string_lossy().into_owned();
                if n.starts_with("buffer-") && !keep.contains(&n) {
                    let _ = std::fs::remove_file(e.path());
                }
            }
        }
        self.text.session_mark = mark;
    }

    fn restore_session(&mut self) {
        let Some(dir) = session_dir() else { return };
        let Ok(s) = std::fs::read_to_string(dir.join("session.txt")) else { return };
        let font = self.font_spec();
        let mut active = 0;
        for line in s.lines() {
            if let Some(a) = line.strip_prefix("active=") {
                active = a.trim().parse().unwrap_or(0);
                continue;
            }
            let Some(rest) = line.strip_prefix("tab=") else { continue };
            let f: Vec<&str> = rest.split('\t').collect();
            if f.len() < 6 {
                continue;
            }
            let path = PathBuf::from(f[0]);
            let untitled: u32 = f[1].parse().unwrap_or(0);
            let buffer = (!f[2].is_empty()).then(|| std::fs::read(dir.join(f[2])).ok()).flatten().map(|b| String::from_utf8_lossy(&b).into_owned());
            let id = self.text.id();
            let mut doc = if untitled > 0 {
                TextView::untitled(untitled, id, &font)
            } else if path.is_file() {
                TextView::open(path, id, &font)
            } else if buffer.is_some() {
                // Its file has gone: what was being written is kept, as a new document.
                TextView::untitled(self.text.next_untitled(), id, &font)
            } else {
                continue;
            };
            if let Some(b) = buffer {
                doc.set_text(&b);
            }
            if let Some(e) = f[3].parse::<usize>().ok().and_then(|k| Encoding::ALL.get(k)) {
                doc.encoding = *e;
            }
            if let Some(e) = f[4].parse::<usize>().ok().and_then(|k| Eol::ALL.get(k)) {
                doc.eol = *e;
            }
            doc.set_caret(f[5].parse().unwrap_or(0));
            doc.preview = false;
            self.text.docs.push(doc);
        }
        self.text.active = active.min(self.text.docs.len().saturating_sub(1));
        self.text.session_mark = self.text.docs.iter().map(|d| (d.id, d.session_version())).collect();
    }

    // --- Tabs ---------------------------------------------------------------------------------------

    pub(super) fn text_new_tab(&mut self) {
        let n = self.text.next_untitled();
        let id = self.text.id();
        let font = self.font_spec();
        let doc = TextView::untitled(n, id, &font);
        self.text.docs.push(doc);
        self.text.strip.animate();
        self.text.active = self.text.docs.len() - 1;
        if let Some(tv) = self.text.cur_mut() {
            tv.take_focus();
        }
        self.set_ime(true);
        self.restart_caret();
        self.invalidate();
    }

    /// A press on a tab: to the front, and moving on drags it.
    pub(super) fn text_tab_press(&mut self, i: usize, x: f32) {
        self.text_select_tab(i);
        // A press on a tab gives the editor the keyboard, as one on an explorer tab gives it the explorer's: the
        // tab in front is outlined in the accent (Faye, 2026-11-11).
        self.explorer.focused = false;
        if let Some(tv) = self.text.cur_mut() {
            tv.take_focus();
        }
        self.set_ime(true);
        self.restart_caret();
        self.strip_press(super::tabstrip::StripKind::Text, i, x);
    }

    /// The document in front changed: the keyboard goes with it (it belongs to the editor, not to a tab).
    fn text_carry_focus(&mut self, from: usize) {
        let had = self.text.docs.get(from).is_some_and(|d| d.has_focus());
        if from == self.text.active {
            return;
        }
        if let Some(d) = self.text.docs.get_mut(from) {
            d.drop_focus();
        }
        if had {
            if let Some(tv) = self.text.cur_mut() {
                tv.take_focus();
            }
        }
    }

    /// Let go of a dragged tab: it drops into its slot (the one in front stays in front). True when it was a drag.
    pub(super) fn text_tab_release(&mut self) -> bool {
        let Some((from, to)) = self.strip_release(super::tabstrip::StripKind::Text) else { return false };
        if from < self.text.docs.len() {
            let front = self.text.docs[self.text.active].id;
            let d = self.text.docs.remove(from);
            let to = to.min(self.text.docs.len());
            self.text.docs.insert(to, d);
            self.text.active = self.text.docs.iter().position(|d| d.id == front).unwrap_or(0);
            self.save_session(false);
        }
        true
    }

    /// A tab clicked: its document up (a file's in the viewer too, so the folder, the strip and the info follow).
    pub(super) fn text_select_tab(&mut self, i: usize) {
        if i >= self.text.docs.len() || i == self.text.active {
            return;
        }
        let from = self.text.active;
        self.text.active = i;
        self.text_carry_focus(from);
        let path = self.text.docs[i].path.clone();
        if self.text.docs[i].untitled == 0 && Some(&path) != self.text.viewer.as_ref() {
            self.text.viewer = Some(path.clone());
            self.open(path);
        }
        self.text_check_disk();
        self.invalidate();
    }

    /// A tab's close button (or Ctrl+W, or a middle-click): unsaved edits ask first.
    pub(super) fn text_close_tab(&mut self, i: usize) {
        let Some(d) = self.text.docs.get(i) else { return };
        if d.dirty() && !d.read_only {
            let name = d.title();
            let id = d.id;
            let buttons = vec![
                ui::DialogButton::new("Save", Choice::Confirm),
                ui::DialogButton::new("Don't save", Choice::Discard),
                ui::DialogButton::new("Cancel", Choice::Cancel),
            ];
            let body = format!("Do you want to save the changes to {name}?");
            self.open_dialog_ui(DialogKind::TextClose(id), ui::Dialog::new("Looker".into(), body, buttons, 0, false));
            return;
        }
        self.text_remove_tab(i);
    }

    /// "Close other tabs": every tab but this one, asking about each edited one in turn (Cancel stops).
    pub(super) fn text_close_others(&mut self, i: usize) {
        let Some(keep) = self.text.docs.get(i).map(|d| d.id) else { return };
        self.text.close_queue = self.text.docs.iter().map(|d| d.id).filter(|&id| id != keep).collect();
        self.text_close_next();
    }

    /// The next tab of the queue: closed when it has nothing to save, otherwise asked about (and the rest wait).
    fn text_close_next(&mut self) {
        while let Some(id) = self.text.close_queue.first().copied() {
            self.text.close_queue.remove(0);
            let Some(i) = self.text.docs.iter().position(|d| d.id == id) else { continue };
            let asks = self.text.docs[i].dirty() && !self.text.docs[i].read_only;
            self.text_close_tab(i);
            if asks {
                return;
            }
        }
    }

    /// The Save / Don't save / Cancel answer for the tab with this id.
    pub(super) fn text_close_answer(&mut self, id: u64, choice: Choice) {
        let Some(i) = self.text.docs.iter().position(|d| d.id == id) else { return };
        match choice {
            Choice::Cancel => self.text.close_queue.clear(),
            Choice::Discard => self.text_remove_tab(i),
            Choice::Confirm => {
                self.text.active = i;
                if self.text_save() {
                    if let Some(i) = self.text.docs.iter().position(|d| d.id == id) {
                        self.text_remove_tab(i);
                    }
                } else {
                    self.text.close_queue.clear();
                }
            }
        }
        self.text_close_next();
    }

    fn text_remove_tab(&mut self, i: usize) {
        let gone = self.text.docs.remove(i);
        self.text.strip.pos.remove(&gone.id);
        self.text.strip.animate();
        if self.text.active > i || self.text.active >= self.text.docs.len() {
            self.text.active = self.text.active.saturating_sub(1);
        }
        if self.text.docs.is_empty() {
            if self.text.shown {
                self.text_new_tab();
            }
            return;
        }
        // The tab now up: its file in the viewer.
        let a = self.text.active;
        let path = self.text.docs[a].path.clone();
        if self.text.docs[a].untitled == 0 && Some(&path) != self.text.viewer.as_ref() {
            self.text.viewer = Some(path.clone());
            self.open(path);
        }
        self.save_session(false);
        self.invalidate();
    }

    pub(super) fn text_save_all(&mut self) {
        let keep = self.text.active;
        let ids: Vec<u64> = self.text.docs.iter().filter(|d| d.dirty() && !d.read_only).map(|d| d.id).collect();
        for id in ids {
            if let Some(i) = self.text.docs.iter().position(|d| d.id == id) {
                self.text.active = i;
                self.text_save();
            }
        }
        self.text.active = keep.min(self.text.docs.len().saturating_sub(1));
        self.invalidate();
    }

    /// File > Open: a text file, opened in the viewer (its tab comes with it, kept).
    pub(super) fn text_open_dialog(&mut self) {
        let picked = unsafe {
            (|| -> Option<PathBuf> {
                let dlg: IFileOpenDialog = CoCreateInstance(&FileOpenDialog, None, CLSCTX_INPROC_SERVER).ok()?;
                let exts: Vec<String> = format::EXTENSIONS.iter().filter(|e| format::from_extension(Some(e)) == format::Format::Text).map(|e| format!("*.{e}")).collect();
                let pattern = HSTRING::from(exts.join(";"));
                let filters = [COMDLG_FILTERSPEC { pszName: w!("Text documents"), pszSpec: PCWSTR(pattern.as_ptr()) }];
                let _ = dlg.SetFileTypes(&filters);
                dlg.Show(Some(self.hwnd)).ok()?;
                let item = dlg.GetResult().ok()?;
                let name = item.GetDisplayName(SIGDN_FILESYSPATH).ok()?;
                let out = PathBuf::from(name.to_string().unwrap_or_default());
                CoTaskMemFree(Some(name.0 as _));
                Some(out)
            })()
        };
        let Some(path) = picked else { return };
        // Opened on purpose: a tab that stays.
        if let Some(d) = self.text.docs.iter_mut().find(|d| d.path == path) {
            d.preview = false;
        } else {
            let id = self.text.id();
            let mut doc = TextView::open(path.clone(), id, &self.font_spec());
            doc.preview = false;
            self.text.docs.push(doc);
        }
        self.text.active = self.text.docs.iter().position(|d| d.path == path).unwrap_or(0);
        self.text.viewer = Some(path.clone());
        self.open(path);
    }

    /// The font picked under Edit: every document takes it.
    pub(super) fn text_font_changed(&mut self) {
        settings::save(&self.settings);
        let f = self.font_spec();
        for d in &mut self.text.docs {
            d.set_font(&f);
        }
        self.invalidate();
    }

    // --- The tab strip and the menu bar -------------------------------------------------------------

    /// The rows above the text: the tabs, then the menu bar. Returns where they end.
    pub(super) fn text_rows_height() -> f32 {
        super::tabstrip::STRIP_H + MENUBAR_H
    }

    pub(super) fn draw_text_rows(&mut self, g: &Gfx, card: D2D_RECT_F) -> bool {
        use super::tabstrip::{STRIP_H, StripKind, TabIcon, TabSpec};
        let t = theme::t();
        let tabs = rect(card.left, card.top, card.right - card.left, STRIP_H);
        let bar = rect(card.left, tabs.bottom, card.right - card.left, MENUBAR_H);
        g.hline(card.left + 1.0, bar.bottom, card.right - card.left - 2.0, rgb(t.divider));

        // The tabs: the explorer's strip, exactly (Faye, 2026-11-11). Each document's icon is Windows' for its type.
        let specs: Vec<TabSpec> = self
            .text
            .docs
            .iter()
            .map(|d| {
                let ext = d.path.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_else(|| "txt".into());
                TabSpec { id: d.id, title: d.title(), icon: TabIcon::Shell(shell_icons::Kind::Extension(ext), 0xE8A5, t.icon), dirty: d.dirty() }
            })
            .collect();
        let focused = self.text_focused();
        let (_, moving) = self.draw_tab_strip(g, StripKind::Text, tabs, card, 9.0, &specs, self.text.active, focused, true);

        // The menu bar.
        let mut x = bar.left + 8.0;
        let open = match self.menu {
            Some((menus::MenuKind::TextBar(k), _)) => Some(k),
            _ => None,
        };
        for (k, label) in ["File", "Edit", "View"].iter().enumerate() {
            let w = g.measure(&wide(label), &g.fonts.body).ceil() + 22.0;
            let r = rect(x, bar.top + 3.0, w, MENUBAR_H - 6.0);
            let hit = Hit::TextMenuBar(k as u8);
            self.hits.add(hit, r);
            let st = self.state(hit, true);
            let fg = ui::quiet_frame(g, r, ui::Kind::Toggle(open == Some(k as u8)), &st);
            g.text(&wide(label), &g.fonts.body, r, fg, Align::Center);
            x += w + 2.0;
        }
        moving
    }

    pub(super) fn text_menubar_click(&mut self, k: u8) {
        if matches!(self.menu, Some((menus::MenuKind::TextBar(o), _)) if o == k) {
            self.close_menu();
            return;
        }
        let Some(r) = self.hits.rect_of(Hit::TextMenuBar(k)) else { return };
        self.text_open_bar_menu(k, r);
    }

    fn text_open_bar_menu(&mut self, k: u8, r: D2D_RECT_F) {
        let e = self.text_bar_entries(k);
        let Some(g) = &self.gfx else { return };
        let (w, h) = self.size_dip();
        self.menu = Some((menus::MenuKind::TextBar(k), ui::Menu::below(g, e, r, rect(0.0, 0.0, w, h))));
        self.hide_tooltip();
        self.invalidate();
    }

    fn text_bar_entries(&self, k: u8) -> Vec<Entry<menus::Action>> {
        let Some(tv) = self.text.cur() else { return Vec::new() };
        let ro = tv.read_only;
        let sel = tv.has_selection();
        let s = &self.settings;
        let item = |a: TextAct, glyph: u16, label: &str, accel: Option<&'static str>, enabled: bool| {
            Entry::Item(MenuItem { icon: None, tint: None, action: menus::Action::Text(a), glyph: Some(glyph), label: label.into(), accel, check: None, enabled, current: false })
        };
        let tick = |a: TextAct, label: &str, accel: Option<&'static str>, on: bool, enabled: bool| {
            Entry::Item(MenuItem { icon: None, tint: None, action: menus::Action::Text(a), glyph: None, label: label.into(), accel, check: Some(on), enabled, current: false })
        };
        match k {
            0 => vec![
                item(TextAct::NewTab, 0xE710, "New tab", Some("Ctrl+N"), true),
                item(TextAct::Open, 0xE8E5, "Open", Some("Ctrl+O"), true),
                Entry::Separator,
                item(TextAct::Save, 0xE74E, "Save", Some("Ctrl+S"), !ro),
                item(TextAct::SaveAs, 0xE792, "Save as", Some("Ctrl+Shift+S"), true),
                item(TextAct::SaveAll, 0xEA35, "Save all", Some("Ctrl+Alt+S"), self.text.docs.iter().any(|d| d.dirty() && !d.read_only)),
                Entry::Separator,
                item(TextAct::PageSetup, 0xE7C3, "Page setup", None, true),
                item(TextAct::Print, 0xE749, "Print", Some("Ctrl+P"), true),
                Entry::Separator,
                item(TextAct::CloseTab, 0xE711, "Close tab", Some("Ctrl+W"), true),
            ],
            1 => {
                let fonts = self.installed_families();
                let font_entries: Vec<Entry<menus::Action>> =
                    fonts.iter().enumerate().map(|(i, f)| tick(TextAct::Font(i as u8), f, None, *f == s.text_font, true)).collect();
                let style_entries: Vec<Entry<menus::Action>> = STYLES
                    .iter()
                    .enumerate()
                    .map(|(i, (name, w, it))| tick(TextAct::FontStyle(i as u8), name, None, *w == s.text_font_weight && *it == s.text_font_italic, true))
                    .collect();
                let size_entries: Vec<Entry<menus::Action>> =
                    SIZES.iter().enumerate().map(|(i, p)| tick(TextAct::FontSize(i as u8), &format!("{p} pt"), None, (*p - s.text_font_pt).abs() < 0.01, true)).collect();
                vec![
                    item(TextAct::Undo, 0xE7A7, "Undo", Some("Ctrl+Z"), !ro),
                    item(TextAct::Redo, 0xE7A6, "Redo", Some("Ctrl+Y"), !ro),
                    Entry::Separator,
                    item(TextAct::Cut, 0xE8C6, "Cut", Some("Ctrl+X"), sel && !ro),
                    item(TextAct::Copy, 0xE8C8, "Copy", Some("Ctrl+C"), sel),
                    item(TextAct::Paste, 0xE77F, "Paste", Some("Ctrl+V"), !ro),
                    item(TextAct::Delete, 0xE74D, "Delete", Some("Del"), sel && !ro),
                    Entry::Separator,
                    item(TextAct::Find, 0xE721, "Find", Some("Ctrl+F"), true),
                    item(TextAct::FindNext, 0xE70D, "Find next", Some("F3"), true),
                    item(TextAct::FindPrev, 0xE70E, "Find previous", Some("Shift+F3"), true),
                    item(TextAct::Replace, 0xE8AC, "Replace", Some("Ctrl+H"), !ro),
                    item(TextAct::GoTo, 0xE8FD, "Go to line", Some("Ctrl+G"), true),
                    Entry::Separator,
                    item(TextAct::SelectAll, 0xE8B3, "Select all", Some("Ctrl+A"), true),
                    item(TextAct::TimeDate, 0xEC92, "Time/Date", Some("F5"), !ro),
                    Entry::Separator,
                    Entry::Sub(Some(0xE8D2), format!("Font: {}", s.text_font), font_entries),
                    Entry::Sub(Some(0xE8DD), "Font style".into(), style_entries),
                    Entry::Sub(Some(0xE8E8), format!("Font size: {} pt", s.text_font_pt), size_entries),
                ]
            }
            _ => {
                let md = tv.is_markdown();
                vec![
                    item(TextAct::ZoomIn, 0xE8A3, "Zoom in", Some("Ctrl+Plus"), true),
                    item(TextAct::ZoomOut, 0xE71F, "Zoom out", Some("Ctrl+Minus"), true),
                    item(TextAct::ZoomReset, 0xE9A6, "Restore default zoom", Some("Ctrl+0"), true),
                    Entry::Separator,
                    tick(TextAct::StatusBar, "Status bar", None, s.text_status, true),
                    tick(TextAct::LineNumbers, "Line numbers", None, s.text_numbers, true),
                    tick(TextAct::Wrap, "Word wrap", None, s.text_wrap, true),
                    Entry::Separator,
                    tick(TextAct::Spell, "Spell check", None, s.text_spell, true),
                    tick(TextAct::Autocorrect, "Autocorrect", None, s.text_autocorrect, s.text_spell),
                    tick(TextAct::Markdown, "Formatted Markdown", None, s.text_markdown, md),
                ]
            }
        }
    }

    /// The families of `FAMILIES` that are installed.
    pub(super) fn installed_families(&self) -> Vec<String> {
        static FOUND: std::sync::OnceLock<Vec<String>> = std::sync::OnceLock::new();
        let Some(g) = &self.gfx else { return vec!["Consolas".into()] };
        FOUND
            .get_or_init(|| unsafe {
                let mut col: Option<windows::Win32::Graphics::DirectWrite::IDWriteFontCollection1> = None;
                if g.text.dwrite.GetSystemFontCollection(false, &mut col, false).is_err() {
                    return vec!["Consolas".into()];
                }
                let Some(col) = col else { return vec!["Consolas".into()] };
                FAMILIES
                    .iter()
                    .filter(|f| {
                        let (mut idx, mut exists) = (0u32, windows::core::BOOL(0));
                        col.FindFamilyName(&HSTRING::from(**f), &mut idx, &mut exists).is_ok() && exists.as_bool()
                    })
                    .map(|f| f.to_string())
                    .collect()
            })
            .clone()
    }

    pub(super) fn text_pick_font(&mut self, a: TextAct) {
        match a {
            TextAct::Font(i) => {
                if let Some(f) = self.installed_families().get(i as usize) {
                    self.settings.text_font = f.clone();
                }
            }
            TextAct::FontStyle(i) => {
                if let Some((_, w, it)) = STYLES.get(i as usize) {
                    self.settings.text_font_weight = *w;
                    self.settings.text_font_italic = *it;
                }
            }
            TextAct::FontSize(i) => {
                if let Some(p) = SIZES.get(i as usize) {
                    self.settings.text_font_pt = *p;
                }
            }
            _ => return,
        }
        self.text_font_changed();
    }
}
