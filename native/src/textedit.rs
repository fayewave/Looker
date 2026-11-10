//! The editing model behind the text box: a single line of UTF-16 (the units DirectWrite hit-tests in) with a
//! caret, a selection anchor and undo. Pure, so every key's behaviour is unit tested; the widget in `ui` only
//! draws it and turns mouse positions into caret positions.
//!
//! Keys follow the Windows edit control / WinUI TextBox: Ctrl+arrows jump to word starts, Ctrl+Backspace and
//! Ctrl+Delete remove a word, typing over a selection replaces it, a run of typing undoes as one step.

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Move {
    Left,
    Right,
    WordLeft,
    WordRight,
    Home,
    End,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Kind {
    Typing,
    Deleting,
    Other,
}

/// One edit: at `at`, `removed` went and `inserted` came in its place.
#[derive(Clone)]
struct Change {
    at: usize,
    removed: Vec<u16>,
    inserted: Vec<u16>,
}

/// An undo step: its edits in order, and the caret and anchor before and after (a run of typing is one step).
/// Only what changed is kept, so a big text's undo costs what was typed, not a copy of the text a step.
#[derive(Clone)]
struct Step {
    changes: Vec<Change>,
    before: (usize, usize),
    after: (usize, usize),
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Class {
    Space,
    Word,
    Punct,
}

fn class(u: u16) -> Class {
    match char::from_u32(u as u32) {
        Some(c) if c.is_whitespace() => Class::Space,
        Some(c) if c.is_alphanumeric() || c == '_' => Class::Word,
        // Surrogate halves (emoji, rare scripts) count as word characters.
        None => Class::Word,
        _ => Class::Punct,
    }
}

fn is_high(u: u16) -> bool {
    (0xD800..0xDC00).contains(&u)
}
fn is_low(u: u16) -> bool {
    (0xDC00..0xE000).contains(&u)
}

pub struct TextEdit {
    text: Vec<u16>,
    caret: usize,
    anchor: usize,
    undo: Vec<Step>,
    redo: Vec<Step>,
    last: Kind,
}

impl TextEdit {
    pub fn new(s: &str) -> TextEdit {
        let text: Vec<u16> = s.encode_utf16().collect();
        let end = text.len();
        TextEdit { text, caret: end, anchor: end, undo: Vec::new(), redo: Vec::new(), last: Kind::Other }
    }

    pub fn text(&self) -> String {
        String::from_utf16_lossy(&self.text)
    }
    pub fn units(&self) -> &[u16] {
        &self.text
    }
    pub fn caret(&self) -> usize {
        self.caret
    }
    /// Where the selection started (the caret's other end).
    pub fn anchor(&self) -> usize {
        self.anchor
    }
    /// The selection as (start, end); empty when they are equal.
    pub fn selection(&self) -> (usize, usize) {
        (self.caret.min(self.anchor), self.caret.max(self.anchor))
    }
    pub fn has_selection(&self) -> bool {
        self.caret != self.anchor
    }
    pub fn selected(&self) -> String {
        let (a, b) = self.selection();
        String::from_utf16_lossy(&self.text[a..b])
    }

    /// Never inside a surrogate pair.
    fn snap(&self, p: usize) -> usize {
        let p = p.min(self.text.len());
        if p > 0 && p < self.text.len() && is_low(self.text[p]) && is_high(self.text[p - 1]) { p - 1 } else { p }
    }
    fn prev(&self, p: usize) -> usize {
        if p == 0 {
            0
        } else if p >= 2 && is_low(self.text[p - 1]) && is_high(self.text[p - 2]) {
            p - 2
        } else {
            p - 1
        }
    }
    fn next(&self, p: usize) -> usize {
        let n = self.text.len();
        if p >= n {
            n
        } else if p + 1 < n && is_high(self.text[p]) && is_low(self.text[p + 1]) {
            p + 2
        } else {
            p + 1
        }
    }
    /// Start of the next word (Ctrl+Right): past this run of word or punctuation, then past the spaces.
    fn word_right(&self, p: usize) -> usize {
        let n = self.text.len();
        let mut i = p;
        if i < n && class(self.text[i]) != Class::Space {
            let c = class(self.text[i]);
            while i < n && class(self.text[i]) == c {
                i += 1;
            }
        }
        while i < n && class(self.text[i]) == Class::Space {
            i += 1;
        }
        self.snap(i)
    }
    /// Start of this or the previous word (Ctrl+Left).
    fn word_left(&self, p: usize) -> usize {
        let mut i = p;
        while i > 0 && class(self.text[i - 1]) == Class::Space {
            i -= 1;
        }
        if i > 0 {
            let c = class(self.text[i - 1]);
            while i > 0 && class(self.text[i - 1]) == c {
                i -= 1;
            }
        }
        self.snap(i)
    }

    /// The word (or run of spaces or punctuation) around a position: what a double-click selects.
    pub fn word_at(&self, p: usize) -> (usize, usize) {
        let n = self.text.len();
        if n == 0 {
            return (0, 0);
        }
        let p = p.min(n - 1);
        let c = class(self.text[p]);
        let (mut a, mut b) = (p, p);
        while a > 0 && class(self.text[a - 1]) == c {
            a -= 1;
        }
        while b < n && class(self.text[b]) == c {
            b += 1;
        }
        (self.snap(a), self.snap(b))
    }

    pub fn select(&mut self, anchor: usize, caret: usize) {
        self.anchor = self.snap(anchor);
        self.caret = self.snap(caret);
        self.last = Kind::Other;
    }
    pub fn select_all(&mut self) {
        self.select(0, self.text.len());
    }

    pub fn move_caret(&mut self, m: Move, extend: bool) {
        let (a, b) = self.selection();
        let to = match m {
            // Plain Left/Right with a selection collapse it to that side.
            Move::Left if !extend && a != b => a,
            Move::Right if !extend && a != b => b,
            Move::Left => self.prev(self.caret),
            Move::Right => self.next(self.caret),
            Move::WordLeft => self.word_left(self.caret),
            Move::WordRight => self.word_right(self.caret),
            Move::Home => 0,
            Move::End => self.text.len(),
        };
        self.caret = to;
        if !extend {
            self.anchor = to;
        }
        self.last = Kind::Other;
    }

    fn remember(&mut self, kind: Kind) {
        if kind != self.last || kind == Kind::Other || self.undo.is_empty() {
            self.undo.push(Step { changes: Vec::new(), before: (self.caret, self.anchor), after: (self.caret, self.anchor) });
            if self.undo.len() > 1000 {
                self.undo.remove(0);
            }
        }
        self.redo.clear();
        self.last = kind;
    }

    /// `a..b` becomes `with`, the caret after it; recorded in the open undo step.
    fn replace(&mut self, a: usize, b: usize, with: &[u16]) {
        let removed: Vec<u16> = self.text.splice(a..b, with.iter().copied()).collect();
        self.caret = a + with.len();
        self.anchor = self.caret;
        if let Some(step) = self.undo.last_mut() {
            step.changes.push(Change { at: a, removed, inserted: with.to_vec() });
            step.after = (self.caret, self.anchor);
        }
    }

    /// `a..b` replaced by `with` as an undo step of its own, then selected (Replace).
    pub fn replace_range(&mut self, a: usize, b: usize, with: &str) {
        let units: Vec<u16> = with.encode_utf16().collect();
        self.remember(Kind::Other);
        self.replace(a, b, &units);
        self.anchor = a;
        if let Some(step) = self.undo.last_mut() {
            step.after = (self.caret, self.anchor);
        }
    }

    /// Every range (sorted, apart) replaced by `with`, as one undo step (Replace all). The caret goes after the
    /// last one.
    pub fn replace_all(&mut self, ranges: &[(usize, usize)], with: &str) {
        if ranges.is_empty() {
            return;
        }
        let units: Vec<u16> = with.encode_utf16().collect();
        self.remember(Kind::Other);
        let mut shift: isize = 0;
        for &(a, b) in ranges {
            let (a, b) = ((a as isize + shift) as usize, (b as isize + shift) as usize);
            self.replace(a, b, &units);
            shift += units.len() as isize - (b - a) as isize;
        }
        self.last = Kind::Other;
    }

    /// The selection moved (or copied) to `to`, as one undo step, and selected there (a drag of the text).
    pub fn move_selection(&mut self, to: usize, copy: bool) {
        let (a, b) = self.selection();
        if a == b || (!copy && to >= a && to <= b) {
            return;
        }
        let moved = self.text[a..b].to_vec();
        self.remember(Kind::Other);
        let at = if copy || to < a {
            to
        } else {
            to - (b - a)
        };
        if !copy {
            self.replace(a, b, &[]);
        }
        self.replace(at, at, &moved);
        self.anchor = at;
        self.caret = at + moved.len();
        if let Some(step) = self.undo.last_mut() {
            step.after = (self.caret, self.anchor);
        }
        self.last = Kind::Other;
    }

    /// Typed or pasted text, replacing the selection. A single-line box keeps only the first line, and
    /// control characters never get in.
    pub fn insert(&mut self, s: &str) {
        let line = s.split(['\r', '\n']).next().unwrap_or("");
        let units: Vec<u16> = line.chars().filter(|c| !c.is_control()).collect::<String>().encode_utf16().collect();
        self.insert_units(&units, if units.len() == 1 { Kind::Typing } else { Kind::Other });
    }

    /// Text for the multi-line editor (app/textview.rs): line breaks as \n, tabs kept, other control characters
    /// dropped. One typed character joins the run of typing before it (one undo step); a line break doesn't.
    pub fn insert_multiline(&mut self, s: &str) {
        let s = s.replace("\r\n", "\n").replace('\r', "\n");
        let units: Vec<u16> = s.chars().filter(|c| *c == '\n' || *c == '\t' || !c.is_control()).collect::<String>().encode_utf16().collect();
        let kind = if units.len() == 1 && units[0] != b'\n' as u16 { Kind::Typing } else { Kind::Other };
        self.insert_units(&units, kind);
    }

    /// One UTF-16 unit from WM_CHAR (a surrogate pair arrives as two).
    pub fn type_unit(&mut self, u: u16) {
        if u < 0x20 || u == 0x7F {
            return;
        }
        self.insert_units(&[u], Kind::Typing);
    }

    fn insert_units(&mut self, units: &[u16], kind: Kind) {
        if units.is_empty() && !self.has_selection() {
            return;
        }
        let kind = if self.has_selection() { Kind::Other } else { kind };
        self.remember(kind);
        let (a, b) = self.selection();
        self.replace(a, b, units);
    }

    pub fn backspace(&mut self, word: bool) {
        let (a, b) = self.selection();
        let (a, b) = if a != b { (a, b) } else if word { (self.word_left(b), b) } else { (self.prev(b), b) };
        if a < b {
            self.remember(Kind::Deleting);
            self.replace(a, b, &[]);
        }
    }

    pub fn delete(&mut self, word: bool) {
        let (a, b) = self.selection();
        let (a, b) = if a != b { (a, b) } else if word { (a, self.word_right(a)) } else { (a, self.next(a)) };
        if a < b {
            self.remember(Kind::Deleting);
            self.replace(a, b, &[]);
        }
    }

    /// Ctrl+X: the selected text, now removed.
    pub fn cut(&mut self) -> Option<String> {
        if !self.has_selection() {
            return None;
        }
        let s = self.selected();
        self.remember(Kind::Other);
        let (a, b) = self.selection();
        self.replace(a, b, &[]);
        Some(s)
    }

    pub fn clear(&mut self) {
        if self.text.is_empty() {
            return;
        }
        self.remember(Kind::Other);
        let n = self.text.len();
        self.replace(0, n, &[]);
    }

    pub fn undo(&mut self) -> bool {
        let Some(step) = self.undo.pop() else { return false };
        for c in step.changes.iter().rev() {
            self.text.splice(c.at..c.at + c.inserted.len(), c.removed.iter().copied());
        }
        (self.caret, self.anchor) = step.before;
        self.redo.push(step);
        self.last = Kind::Other;
        true
    }

    pub fn redo(&mut self) -> bool {
        let Some(step) = self.redo.pop() else { return false };
        for c in &step.changes {
            self.text.splice(c.at..c.at + c.removed.len(), c.inserted.iter().copied());
        }
        (self.caret, self.anchor) = step.after;
        self.undo.push(step);
        self.last = Kind::Other;
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn typed(t: &mut TextEdit, s: &str) {
        for u in s.encode_utf16() {
            t.type_unit(u);
        }
    }

    #[test]
    fn replace_all_and_moves_undo_in_one_step() {
        let mut t = TextEdit::new("a cat, a cat");
        t.replace_all(&[(2, 5), (9, 12)], "dog");
        assert_eq!(t.text(), "a dog, a dog");
        t.select(2, 5);
        t.move_selection(12, false);
        assert_eq!(t.text(), "a , a dogdog");
        assert!(t.undo());
        assert_eq!(t.text(), "a dog, a dog");
        assert!(t.undo());
        assert_eq!(t.text(), "a cat, a cat");
        assert!(t.redo());
        assert_eq!(t.text(), "a dog, a dog");
    }

    #[test]
    fn typing_replaces_the_selection() {
        let mut t = TextEdit::new("holiday.jpg");
        t.select(0, 7); // the base name, as the rename dialog opens
        typed(&mut t, "beach");
        assert_eq!(t.text(), "beach.jpg");
        assert_eq!(t.caret(), 5);
    }

    #[test]
    fn word_jumps_and_word_deletes() {
        let mut t = TextEdit::new("my holiday photo.jpg");
        t.move_caret(Move::Home, false);
        t.move_caret(Move::WordRight, false);
        assert_eq!(t.caret(), 3);
        t.move_caret(Move::WordRight, false);
        assert_eq!(t.caret(), 11);
        t.move_caret(Move::WordRight, false);
        assert_eq!(t.caret(), 16); // "photo" ends at the dot
        t.move_caret(Move::WordLeft, false);
        assert_eq!(t.caret(), 11);
        t.move_caret(Move::End, false);
        t.backspace(true);
        assert_eq!(t.text(), "my holiday photo.");
        t.move_caret(Move::Home, false);
        t.delete(true);
        assert_eq!(t.text(), "holiday photo.");
    }

    #[test]
    fn shift_extends_and_plain_arrows_collapse() {
        let mut t = TextEdit::new("abcdef");
        t.move_caret(Move::Home, false);
        t.move_caret(Move::Right, true);
        t.move_caret(Move::Right, true);
        assert_eq!(t.selected(), "ab");
        t.move_caret(Move::Right, false);
        assert_eq!((t.caret(), t.has_selection()), (2, false));
    }

    #[test]
    fn surrogate_pairs_move_and_delete_whole() {
        let mut t = TextEdit::new("a😀b");
        t.move_caret(Move::End, false);
        t.move_caret(Move::Left, false);
        assert_eq!(t.caret(), 3);
        t.backspace(false);
        assert_eq!(t.text(), "ab");
        t.select(0, 2);
        t.select(2, 2);
        assert_eq!(t.caret(), 2);
    }

    #[test]
    fn a_run_of_typing_undoes_as_one_step() {
        let mut t = TextEdit::new("x");
        typed(&mut t, "abc");
        t.backspace(false);
        assert_eq!(t.text(), "xab");
        assert!(t.undo());
        assert_eq!(t.text(), "xabc");
        assert!(t.undo());
        assert_eq!(t.text(), "x");
        assert!(t.redo());
        assert_eq!(t.text(), "xabc");
    }

    #[test]
    fn paste_keeps_the_first_line_and_drops_controls() {
        let mut t = TextEdit::new("");
        t.insert("one\ttwo\r\nthree");
        assert_eq!(t.text(), "onetwo");
        t.type_unit(0x08);
        assert_eq!(t.text(), "onetwo");
    }

    #[test]
    fn double_click_selects_a_word() {
        let t = TextEdit::new("my holiday.jpg");
        assert_eq!(t.word_at(5), (3, 10));
        assert_eq!(t.word_at(10), (10, 11));
    }
}
