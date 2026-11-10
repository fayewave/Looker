//! A text file on screen (.txt, and Source materials, .vmt, which are text): one scrolling view to read and edit
//! it in the photo's place, as Windows 11's Notepad does (Faye, 2026-11-11). The theme's card with line numbers
//! down the left, the text in Consolas (wrapped at the view's width, or not: Word wrap), a find bar at the top
//! (Find, Replace, Go to line), a status line at the foot (where the caret is, the selection's length, the
//! encoding and line endings, which pick others, Word wrap, the size).
//!
//! Only the lines on screen are laid out (each line of the file its own DirectWrite layout, made as it comes into
//! view): a file of any length scrolls and edits as quickly as a short one. Lines not laid out yet count as many
//! rows as their length fills at Consolas's fixed width, so the scrollbar is right before they're seen.
//!
//! A click puts the caret in and gives it the keyboard (Escape or a click elsewhere hands it back to the viewer's
//! keys). Ctrl+S saves in the file's own encoding and line endings; Save As (Ctrl+Shift+S) picks others. Moving to
//! another file or closing Looker saves unsaved edits. A file changed on disk reloads when it has no edits here;
//! with edits, the foot says so and Ctrl+S asks twice before overwriting it.
//!
//! The decode pool still draws the first page of it (`imaging/text.rs`) for thumbnails; that still isn't drawn here.

use std::collections::HashMap;
use std::time::Instant;

use windows::Win32::Graphics::DirectWrite::*;
use windows::core::Interface;

use super::*;
use crate::textedit::{Move, TextEdit};
use crate::ui::{Entry, TextField};

const PAD: f32 = 16.0;
const FOOT_H: f32 = 28.0;
const BAR_ROW: f32 = 40.0;
const SIZE_MIN: f32 = 8.0;
const SIZE_MAX: f32 = 48.0;
const SIZE_DEFAULT: f32 = 14.0;
/// Past this the file opens read-only, its start only.
const EDIT_MAX_BYTES: usize = 64 << 20;
/// Most find results counted and highlighted.
const MATCH_MAX: usize = 100_000;
/// Checking the file for changes made elsewhere.
pub(super) const TIMER_TEXT: usize = 14;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Encoding {
    Utf8,
    Utf8Bom,
    Utf16Le,
    Utf16Be,
    Ansi,
}

impl Encoding {
    const ALL: [Encoding; 5] = [Encoding::Utf8, Encoding::Utf8Bom, Encoding::Utf16Le, Encoding::Utf16Be, Encoding::Ansi];

    fn label(self) -> &'static str {
        match self {
            Encoding::Utf8 => "UTF-8",
            Encoding::Utf8Bom => "UTF-8 with BOM",
            Encoding::Utf16Le => "UTF-16 LE",
            Encoding::Utf16Be => "UTF-16 BE",
            Encoding::Ansi => "ANSI",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Eol {
    CrLf,
    Lf,
    Cr,
}

impl Eol {
    const ALL: [Eol; 3] = [Eol::CrLf, Eol::Lf, Eol::Cr];

    fn label(self) -> &'static str {
        match self {
            Eol::CrLf => "Windows (CRLF)",
            Eol::Lf => "Unix (LF)",
            Eol::Cr => "Macintosh (CR)",
        }
    }

    fn short(self) -> &'static str {
        match self {
            Eol::CrLf => "CRLF",
            Eol::Lf => "LF",
            Eol::Cr => "CR",
        }
    }

    fn text(self) -> &'static str {
        match self {
            Eol::CrLf => "\r\n",
            Eol::Lf => "\n",
            Eol::Cr => "\r",
        }
    }
}

/// The right-click menu's and the foot's picks.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum TextAct {
    Undo,
    Redo,
    Cut,
    Copy,
    Paste,
    Delete,
    SelectAll,
    Find,
    Replace,
    GoTo,
    TimeDate,
    Wrap,
    SaveAs,
    Encoding(u8),
    Eol(u8),
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Focus {
    None,
    Text,
    Find,
    Replace,
    GoTo,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum BarMode {
    Find,
    Replace,
    GoTo,
}

struct Bar {
    mode: BarMode,
    find: TextField,
    replace: TextField,
    goto: TextField,
    case: bool,
    wrap_around: bool,
}

/// A mouse drag in the text: selecting, or a press on the selection that may become a drag of it.
#[derive(Clone, Copy, PartialEq)]
enum Drag {
    Select,
    /// Pressed on the selection at `pos`; `to` once it has moved far enough to be a drag of the text.
    Move { pos: usize, x: f32, y: f32, to: Option<usize> },
    Thumb,
    HThumb,
}

struct Line {
    layout: IDWriteTextLayout,
    height: f32,
    width: f32,
}

pub(super) struct TextView {
    pub(super) path: PathBuf,
    edit: TextEdit,
    encoding: Encoding,
    eol: Eol,
    read_only: bool,
    /// The text's hash as saved (or loaded), and now; the encoding or line endings picked since.
    saved_hash: u64,
    hash: u64,
    meta_dirty: bool,
    scroll: f32,
    scroll_x: f32,
    size: f32,
    focus: Focus,
    pub(super) caret_on: bool,
    /// Up/Down keep to this x across short lines.
    goal_x: Option<f32>,
    version: u64,
    /// Where each line starts, for `lines_version`.
    starts: Vec<usize>,
    lines_version: u64,
    /// Laid-out lines by index, for `cache_key` (version, width, size, wrap).
    cache: HashMap<usize, Line>,
    cache_key: (u64, u32, u32, bool),
    format: Option<(IDWriteTextFormat, f32, bool)>,
    /// A row's height and a character's width at this size.
    metrics: (f32, f32, f32),
    /// The widest line laid out (no wrap: how far it scrolls sideways).
    max_w: f32,
    reveal: bool,
    drag: Option<Drag>,
    /// When the last double-click was, and where: a third click soon after selects the line.
    last_double: Option<(Instant, f32, f32)>,
    bar: Option<Bar>,
    matches: Vec<(usize, usize)>,
    matches_key: Option<(u64, Vec<u16>, bool)>,
    /// The file on disk as last read or written (write time, length); whether it has changed since under edits,
    /// and whether Ctrl+S has warned about it once.
    disk: (u64, u64),
    disk_changed: bool,
    overwrite_armed: bool,
    /// Enter was used by the find bar: the WM_CHAR that follows it isn't typed (Go to line closing the bar
    /// put a line break into the text).
    swallow_enter: bool,
    error: Option<String>,
}

fn decode_text(b: &[u8]) -> (String, Encoding) {
    if let Some(rest) = b.strip_prefix(&[0xFF, 0xFE]) {
        return (String::from_utf16_lossy(&rest.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect::<Vec<_>>()), Encoding::Utf16Le);
    }
    if let Some(rest) = b.strip_prefix(&[0xFE, 0xFF]) {
        return (String::from_utf16_lossy(&rest.chunks_exact(2).map(|c| u16::from_be_bytes([c[0], c[1]])).collect::<Vec<_>>()), Encoding::Utf16Be);
    }
    if let Some(rest) = b.strip_prefix(&[0xEF, 0xBB, 0xBF]) {
        return (String::from_utf8_lossy(rest).into_owned(), Encoding::Utf8Bom);
    }
    match std::str::from_utf8(b) {
        Ok(s) => (s.to_string(), Encoding::Utf8),
        Err(e) if e.error_len().is_none() && b.len() > 4 => (String::from_utf8_lossy(b).into_owned(), Encoding::Utf8),
        Err(_) => (crate::imaging::text_ansi(b), Encoding::Ansi),
    }
}

fn encode_text(s: &str, e: Encoding) -> Vec<u8> {
    match e {
        Encoding::Utf8 => s.as_bytes().to_vec(),
        Encoding::Utf8Bom => [&[0xEF, 0xBB, 0xBF][..], s.as_bytes()].concat(),
        Encoding::Utf16Le => [0xFF, 0xFE].into_iter().chain(s.encode_utf16().flat_map(u16::to_le_bytes)).collect(),
        Encoding::Utf16Be => [0xFE, 0xFF].into_iter().chain(s.encode_utf16().flat_map(u16::to_be_bytes)).collect(),
        // Back to Windows-1252 where it can; anything else would be lost, so that saves as UTF-8 instead.
        Encoding::Ansi => match crate::imaging::to_ansi(s) {
            Some(b) => b,
            None => s.as_bytes().to_vec(),
        },
    }
}

fn hash(u: &[u16]) -> u64 {
    let mut h = 0xcbf29ce484222325u64;
    for chunk in u.chunks(4) {
        let mut v = 0u64;
        for (i, &x) in chunk.iter().enumerate() {
            v |= (x as u64) << (16 * i);
        }
        h = (h ^ v).wrapping_mul(0x100000001b3);
    }
    h ^ u.len() as u64
}

fn disk_stamp(p: &Path) -> (u64, u64) {
    std::fs::metadata(p).map_or((0, 0), |m| (m.last_write_time(), m.len()))
}

/// A UTF-16 unit, case-folded for a search that ignores case.
fn fold(u: u16) -> u16 {
    match char::from_u32(u as u32) {
        Some(c) => {
            let mut l = c.to_lowercase();
            match (l.next(), l.next()) {
                (Some(x), None) if (x as u32) < 0x10000 => x as u32 as u16,
                _ => u,
            }
        }
        None => u,
    }
}

const NL: u16 = b'\n' as u16;

impl TextView {
    fn open(path: PathBuf) -> TextView {
        let disk = disk_stamp(&path);
        let (text, encoding, eol, read_only, error) = match std::fs::read(&path) {
            Ok(b) => {
                let big = b.len() > EDIT_MAX_BYTES;
                let (s, e) = decode_text(&b[..b.len().min(EDIT_MAX_BYTES)]);
                let eol = if s.contains("\r\n") {
                    Eol::CrLf
                } else if s.contains('\n') {
                    Eol::Lf
                } else if s.contains('\r') {
                    Eol::Cr
                } else {
                    Eol::CrLf
                };
                (s.replace("\r\n", "\n").replace('\r', "\n"), e, eol, big, None)
            }
            Err(e) => (String::new(), Encoding::Utf8, Eol::CrLf, true, Some(e.to_string())),
        };
        let mut edit = TextEdit::new(&text);
        edit.select(0, 0);
        let h = hash(edit.units());
        TextView {
            path,
            edit,
            encoding,
            eol,
            read_only,
            saved_hash: h,
            hash: h,
            meta_dirty: false,
            scroll: 0.0,
            scroll_x: 0.0,
            size: SIZE_DEFAULT,
            focus: Focus::None,
            caret_on: true,
            goal_x: None,
            version: 0,
            starts: Vec::new(),
            lines_version: u64::MAX,
            cache: HashMap::new(),
            cache_key: (u64::MAX, 0, 0, false),
            format: None,
            metrics: (0.0, 0.0, 0.0),
            max_w: 0.0,
            reveal: false,
            drag: None,
            last_double: None,
            bar: None,
            matches: Vec::new(),
            matches_key: None,
            disk,
            disk_changed: false,
            overwrite_armed: false,
            swallow_enter: false,
            error,
        }
    }

    fn dirty(&self) -> bool {
        self.hash != self.saved_hash || self.meta_dirty
    }

    /// After an edit: the line index, the hash and the laid-out lines are made again when next needed.
    fn changed(&mut self) {
        self.version += 1;
        self.hash = hash(self.edit.units());
        self.goal_x = None;
        self.reveal = true;
        self.caret_on = true;
    }

    /// Writes the file in its encoding and line endings (to `to`, or its own path).
    fn save_to(&mut self, to: &Path) -> Result<(), String> {
        let mut s = self.edit.text();
        if self.eol != Eol::Lf {
            s = s.replace('\n', self.eol.text());
        }
        std::fs::write(to, encode_text(&s, self.encoding)).map_err(|e| e.to_string())?;
        if to == self.path {
            self.saved_hash = self.hash;
            self.meta_dirty = false;
            self.disk = disk_stamp(to);
            self.disk_changed = false;
            self.overwrite_armed = false;
        }
        Ok(())
    }

    fn lines(&mut self) -> &[usize] {
        if self.lines_version != self.version {
            let u = self.edit.units();
            let mut s = Vec::with_capacity(u.len() / 40 + 1);
            s.push(0);
            s.extend(u.iter().enumerate().filter(|(_, c)| **c == NL).map(|(i, _)| i + 1));
            self.starts = s;
            self.lines_version = self.version;
        }
        &self.starts
    }

    fn line_count(&mut self) -> usize {
        self.lines().len()
    }

    /// Line `i`'s range, without its line break.
    fn line_range(&mut self, i: usize) -> (usize, usize) {
        let n = self.edit.units().len();
        let starts = self.lines();
        let a = starts[i];
        let b = starts.get(i + 1).map_or(n, |s| s - 1);
        (a, b)
    }

    fn line_of(&mut self, pos: usize) -> usize {
        let starts = self.lines();
        starts.partition_point(|&s| s <= pos).saturating_sub(1)
    }

    fn format(&mut self, g: &Gfx, wrap: bool) -> Option<IDWriteTextFormat> {
        if let Some((f, s, w)) = &self.format {
            if *s == self.size && *w == wrap {
                return Some(f.clone());
            }
        }
        let f = unsafe {
            let f = g.text.dwrite.CreateTextFormat(w!("Consolas"), None, DWRITE_FONT_WEIGHT_NORMAL, DWRITE_FONT_STYLE_NORMAL, DWRITE_FONT_STRETCH_NORMAL, self.size, w!("en-us")).ok()?;
            f.SetWordWrapping(if wrap { DWRITE_WORD_WRAPPING_WRAP } else { DWRITE_WORD_WRAPPING_NO_WRAP }).ok()?;
            f.SetIncrementalTabStop(self.size * 0.5498 * 4.0).ok()?;
            f
        };
        // A row's height and a character's advance: "X" laid out once.
        let probe: Vec<u16> = "X".encode_utf16().collect();
        let (row, ch) = unsafe {
            match g.text.dwrite.CreateTextLayout(&probe, &f, 1000.0, 1000.0) {
                Ok(l) => {
                    let mut m = DWRITE_TEXT_METRICS::default();
                    let _ = l.GetMetrics(&mut m);
                    (m.height.max(1.0), m.widthIncludingTrailingWhitespace.max(1.0))
                }
                Err(_) => (self.size * 1.2, self.size * 0.55),
            }
        };
        self.metrics = (row, ch, self.size);
        self.format = Some((f.clone(), self.size, wrap));
        Some(f)
    }

    fn row_h(&self) -> f32 {
        self.metrics.0.max(1.0)
    }

    /// The laid-out lines belong to this text, width, size and wrap; else they're dropped.
    fn check_cache(&mut self, width: f32, wrap: bool) {
        let key = (self.version, width.round() as u32, (self.size * 10.0) as u32, wrap);
        if self.cache_key != key {
            if self.cache_key.0 != key.0 || self.cache_key.3 != key.3 || self.cache_key.2 != key.2 {
                self.max_w = 0.0;
            }
            self.cache.clear();
            self.cache_key = key;
        }
    }

    /// Line `i` laid out (made now if it isn't yet).
    fn line(&mut self, g: &Gfx, i: usize, width: f32, wrap: bool) -> Option<&Line> {
        if !self.cache.contains_key(&i) {
            let fmt = self.format(g, wrap)?;
            let (a, b) = self.line_range(i);
            let l = unsafe { g.text.dwrite.CreateTextLayout(&self.edit.units()[a..b], &fmt, if wrap { width.max(1.0) } else { 1.0e6 }, 1.0e6).ok()? };
            let mut m = DWRITE_TEXT_METRICS::default();
            unsafe {
                let _ = l.GetMetrics(&mut m);
            }
            let line = Line { layout: l, height: m.height.max(self.row_h()), width: m.widthIncludingTrailingWhitespace };
            self.max_w = self.max_w.max(line.width);
            self.cache.insert(i, line);
        }
        self.cache.get(&i)
    }

    /// Line `i`'s height: laid out, or as many rows as its characters fill at the fixed width.
    fn height_of(&self, i: usize, width: f32, wrap: bool) -> f32 {
        if let Some(l) = self.cache.get(&i) {
            return l.height;
        }
        let row = self.row_h();
        if !wrap {
            return row;
        }
        let a = self.starts[i];
        let b = self.starts.get(i + 1).map_or(self.edit.units().len(), |s| s - 1);
        let cols: usize = self.edit.units()[a..b].iter().map(|&c| if c == b'\t' as u16 { 4 } else { 1 }).sum();
        let per_row = ((width / self.metrics.1.max(1.0)).floor() as usize).max(1);
        cols.div_ceil(per_row).max(1) as f32 * row
    }

    /// Every line's top (and the total height last), laying out the lines `view` shows on the way.
    fn tops(&mut self, g: &Gfx, width: f32, wrap: bool, view: Option<(f32, f32)>) -> Vec<f32> {
        self.format(g, wrap);
        self.check_cache(width, wrap);
        let n = self.line_count();
        let mut pass = 0;
        loop {
            let mut tops = Vec::with_capacity(n + 1);
            let mut y = 0.0f32;
            for i in 0..n {
                tops.push(y);
                y += self.height_of(i, width, wrap);
            }
            tops.push(y);
            let Some((top, bottom)) = view else { return tops };
            let first = tops.partition_point(|&t| t <= top).saturating_sub(1).min(n - 1);
            let mut fresh = false;
            let mut i = first;
            while i < n && tops[i] <= bottom {
                if !self.cache.contains_key(&i) {
                    self.line(g, i, width, wrap);
                    fresh = true;
                }
                i += 1;
            }
            pass += 1;
            if !fresh || pass >= 3 {
                return tops;
            }
        }
    }

    /// Where the caret sits before `pos`: (x, top, height) in the text's own space (0, 0 at its start).
    fn caret_xy(&mut self, g: &Gfx, tops: &[f32], pos: usize, width: f32, wrap: bool) -> (f32, f32, f32) {
        let i = self.line_of(pos);
        let start = self.starts[i];
        let row = self.row_h();
        let Some(l) = self.line(g, i, width, wrap) else { return (0.0, tops[i], row) };
        unsafe {
            let (mut x, mut y) = (0.0f32, 0.0f32);
            let mut m = DWRITE_HIT_TEST_METRICS::default();
            if l.layout.HitTestTextPosition((pos - start) as u32, false, &mut x, &mut y, &mut m).is_ok() {
                (x, tops[i] + m.top, m.height.max(1.0))
            } else {
                (0.0, tops[i], row)
            }
        }
    }

    /// The position nearest a point in the text's own space.
    fn pos_at(&mut self, g: &Gfx, tops: &[f32], x: f32, y: f32, width: f32, wrap: bool) -> usize {
        let n = tops.len() - 1;
        let i = tops.partition_point(|&t| t <= y.max(0.0)).saturating_sub(1).min(n - 1);
        let start = self.starts[i];
        let ty = (y - tops[i]).max(0.0);
        let Some(l) = self.line(g, i, width, wrap) else { return start };
        unsafe {
            let mut trailing = windows::core::BOOL(0);
            let mut inside = windows::core::BOOL(0);
            let mut m = DWRITE_HIT_TEST_METRICS::default();
            if l.layout.HitTestPoint(x, ty.min(l.height - 1.0), &mut trailing, &mut inside, &mut m).is_err() {
                return start;
            }
            start + (m.textPosition + if trailing.as_bool() { m.length } else { 0 }) as usize
        }
    }

    /// The visual row holding `pos` (a wrapped line has several): its start and end, before any line break.
    fn row_range(&mut self, g: &Gfx, pos: usize, width: f32, wrap: bool) -> (usize, usize) {
        let i = self.line_of(pos);
        let (a, b) = self.line_range(i);
        let Some(l) = self.line(g, i, width, wrap) else { return (a, b) };
        let metrics = unsafe {
            let mut n = 0u32;
            let _ = l.layout.GetLineMetrics(None, &mut n);
            let mut v = vec![DWRITE_LINE_METRICS::default(); n as usize];
            if l.layout.GetLineMetrics(Some(&mut v), &mut n).is_err() {
                return (a, b);
            }
            v
        };
        let mut s = a;
        for (k, m) in metrics.iter().enumerate() {
            let e = s + m.length as usize;
            if pos < e || k + 1 == metrics.len() {
                return (s, (e - m.newlineLength as usize).min(b));
            }
            s = e;
        }
        (a, b)
    }

    /// The find bar's results for its query (worked out again when the text or the query changes).
    fn update_matches(&mut self) {
        let Some(bar) = &self.bar else {
            self.matches.clear();
            self.matches_key = None;
            return;
        };
        let q: Vec<u16> = bar.find.edit.units().to_vec();
        let key = (self.version, q.clone(), bar.case);
        if self.matches_key.as_ref() == Some(&key) {
            return;
        }
        self.matches.clear();
        if !q.is_empty() {
            let case = bar.case;
            let f = |u: u16| if case { u } else { fold(u) };
            let q: Vec<u16> = q.iter().map(|&u| f(u)).collect();
            let t = self.edit.units();
            let mut i = 0;
            while i + q.len() <= t.len() && self.matches.len() < MATCH_MAX {
                if f(t[i]) == q[0] && t[i..i + q.len()].iter().zip(&q).all(|(&a, &b)| f(a) == b) {
                    self.matches.push((i, i + q.len()));
                    i += q.len();
                } else {
                    i += 1;
                }
            }
        }
        self.matches_key = Some(key);
    }
}

/// Where the parts sit: the card, the find bar, the gutter, the text, the scrollbars, the foot.
#[derive(Clone, Copy)]
struct Rects {
    card: D2D_RECT_F,
    bar: Option<D2D_RECT_F>,
    gutter: D2D_RECT_F,
    text: D2D_RECT_F,
    track: D2D_RECT_F,
    htrack: D2D_RECT_F,
    foot: D2D_RECT_F,
}

impl App {
    /// The current file is a text file: its view, made when it comes up. Leaving one with edits saves them.
    pub(super) fn prepare_text(&mut self) {
        let want = self
            .current_path()
            .filter(|p| self.placeholder.is_none() && format::from_extension(p.extension().and_then(|e| e.to_str())) == format::Format::Text)
            .map(Path::to_path_buf);
        if self.text_view.as_ref().map(|t| &t.path) == want.as_ref() {
            return;
        }
        self.close_text();
        self.text_view = want.map(TextView::open);
        unsafe {
            if self.text_view.is_some() {
                SetTimer(Some(self.hwnd), TIMER_TEXT, 1000, None);
            } else {
                let _ = KillTimer(Some(self.hwnd), TIMER_TEXT);
            }
        }
    }

    /// Puts the text view away, saving its edits (another file came up, or the window is closing).
    pub(super) fn close_text(&mut self) {
        let Some(mut old) = self.text_view.take() else { return };
        if old.dirty() && !old.read_only {
            let name = old.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            let path = old.path.clone();
            match old.save_to(&path) {
                Ok(()) => self.show_toast(format!("Saved {name}"), false),
                Err(e) => self.show_toast(format!("Couldn't save {name}: {e}"), false),
            }
            self.viewer.forget(&old.path);
        }
        if old.focus != Focus::None {
            self.set_ime(false);
        }
    }

    /// The text's lines and characters, for the status row and the info card (a text file has no pixels).
    pub(super) fn text_facts(&self) -> Option<(usize, usize)> {
        let tv = self.text_view.as_ref().filter(|_| self.text_shown())?;
        let u = tv.edit.units();
        Some((u.iter().filter(|&&c| c == NL).count() + 1, String::from_utf16_lossy(u).chars().count()))
    }

    pub(super) fn text_shown(&self) -> bool {
        self.text_view.is_some() && self.placeholder.is_none()
    }

    /// The view, or one of its find bar's boxes, has the keyboard.
    pub(super) fn text_focused(&self) -> bool {
        self.text_view.as_ref().is_some_and(|t| t.focus != Focus::None) && self.text_shown()
    }

    fn text_rects(&self) -> Option<Rects> {
        let tv = self.text_view.as_ref()?;
        let a = self.image_area();
        let card = D2D_RECT_F { left: a.left, top: a.top, right: a.right, bottom: a.bottom + self.preview_pad() - 12.0 };
        if card.right - card.left < 160.0 || card.bottom - card.top < 120.0 {
            return None;
        }
        let bar_h = match tv.bar.as_ref().map(|b| b.mode) {
            Some(BarMode::Replace) => 2.0 * BAR_ROW + 4.0,
            Some(_) => BAR_ROW + 4.0,
            None => 0.0,
        };
        let bar = (bar_h > 0.0).then(|| rect(card.left, card.top, card.right - card.left, bar_h));
        let top = card.top + bar_h + PAD / 2.0;
        let body_bottom = card.bottom - FOOT_H;
        let lines = tv.starts.len().max(1);
        let digits = lines.to_string().len().max(3) as f32;
        let gutter_w = digits * tv.size * 0.55 + 22.0;
        let gutter = D2D_RECT_F { left: card.left, top, right: card.left + gutter_w, bottom: body_bottom - 6.0 };
        let text = D2D_RECT_F { left: gutter.right + 8.0, top, right: card.right - 8.0 - ui::SCROLL_W - 4.0, bottom: body_bottom - 6.0 };
        let track = rect(card.right - 4.0 - ui::SCROLL_W, top, ui::SCROLL_W, text.bottom - top);
        let htrack = rect(text.left, body_bottom - 4.0 - ui::SCROLL_W, text.right - text.left, ui::SCROLL_W);
        let foot = D2D_RECT_F { left: card.left, top: body_bottom, right: card.right, bottom: card.bottom };
        Some(Rects { card, bar, gutter, text, track, htrack, foot })
    }

    /// Runs `f` with the view, the graphics, the rects, the line tops (laying out what `view_lines` shows) and the
    /// text's width and wrap.
    fn with_text<R>(&mut self, view_lines: bool, f: impl FnOnce(&mut TextView, &Gfx, &Rects, &[f32], f32, bool) -> R) -> Option<R> {
        let r = self.text_rects()?;
        let wrap = self.settings.text_wrap;
        let g = self.gfx.take()?;
        let out = self.text_view.as_mut().map(|tv| {
            let width = r.text.right - r.text.left - 4.0;
            let view = view_lines.then(|| (tv.scroll, tv.scroll + (r.text.bottom - r.text.top)));
            let tops = tv.tops(&g, width, wrap, view);
            f(tv, &g, &r, &tops, width, wrap)
        });
        self.gfx = Some(g);
        out
    }

    pub(super) fn draw_text_view(&mut self, g: &Gfx) {
        if !self.text_shown() {
            return;
        }
        let Some(r) = self.text_rects() else { return };
        let t = theme::t();
        let wrap = self.settings.text_wrap;
        let width = r.text.right - r.text.left - 4.0;
        let view_h = r.text.bottom - r.text.top;
        let focused_text = self.text_focused() && self.text_view.as_ref().is_some_and(|tv| tv.focus == Focus::Text);
        let focused_any = self.text_focused();
        let (mouse, pressed) = (self.mouse, self.pressed);

        // A drag held past the top or bottom keeps scrolling, as it does in Notepad.
        if let Some(tv) = self.text_view.as_mut() {
            if matches!(tv.drag, Some(Drag::Select | Drag::Move { to: Some(_), .. })) && pressed == Some(Hit::TextView) {
                let over = if mouse.1 < r.text.top { mouse.1 - r.text.top } else if mouse.1 > r.text.bottom { mouse.1 - r.text.bottom } else { 0.0 };
                let side = if !wrap && mouse.0 < r.text.left { mouse.0 - r.text.left } else if !wrap && mouse.0 > r.text.right { mouse.0 - r.text.right } else { 0.0 };
                if over != 0.0 || side != 0.0 {
                    tv.scroll += over.clamp(-60.0, 60.0) * 0.5;
                    tv.scroll_x = (tv.scroll_x + side.clamp(-60.0, 60.0) * 0.5).max(0.0);
                    self.invalidate();
                }
            }
        }
        let Some(tv) = self.text_view.as_mut() else { return };
        tv.update_matches();
        let mut tops = tv.tops(g, width, wrap, Some((tv.scroll, tv.scroll + view_h)));
        // The caret into view after a key or an edit.
        if tv.reveal {
            tv.reveal = false;
            let (x, top, h) = tv.caret_xy(g, &tops, tv.edit.caret(), width, wrap);
            if top < tv.scroll {
                tv.scroll = top;
            } else if top + h > tv.scroll + view_h {
                tv.scroll = top + h - view_h;
            }
            if !wrap {
                let vw = r.text.right - r.text.left;
                if x < tv.scroll_x {
                    tv.scroll_x = (x - 40.0).max(0.0);
                } else if x > tv.scroll_x + vw - 8.0 {
                    tv.scroll_x = x - vw + 40.0;
                }
            }
            tops = tv.tops(g, width, wrap, Some((tv.scroll, tv.scroll + view_h)));
        }
        let total_h = *tops.last().unwrap_or(&0.0) + view_h * 0.5;
        let max_scroll = (total_h - view_h).max(0.0);
        tv.scroll = tv.scroll.clamp(0.0, max_scroll);
        let vw = r.text.right - r.text.left;
        let max_x = if wrap { 0.0 } else { (tv.max_w + 40.0 - vw).max(0.0) };
        tv.scroll_x = tv.scroll_x.clamp(0.0, max_x);
        let (scroll, scroll_x) = (tv.scroll, tv.scroll_x);
        let tops = tv.tops(g, width, wrap, Some((scroll, scroll + view_h)));
        let n = tops.len() - 1;
        let first = tops.partition_point(|&y| y <= scroll).saturating_sub(1).min(n.saturating_sub(1));
        let row = tv.row_h();
        let fmt = tv.format(g, wrap);

        // The card, as the explorer's and the info card's.
        g.fill_round(r.card, 8.0, rgb(t.sidebar));
        ui::stroke(g, r.card, 8.0, rgb(t.divider));
        self.hits.add(Hit::TextView, D2D_RECT_F { left: r.card.left, right: r.track.left - 2.0, top: r.gutter.top - PAD / 2.0, bottom: r.foot.top });

        let Some(tv) = self.text_view.as_mut() else { return };
        let ox = r.text.left - scroll_x;
        let oy = r.text.top - scroll;
        let (sa, sb) = tv.edit.selection();
        let caret = tv.edit.caret();
        let caret_on = tv.caret_on;
        let ch_w = tv.metrics.1;
        let mut caret_rect = None;
        g.push_clip(D2D_RECT_F { left: r.text.left - 2.0, right: r.text.right + 2.0, top: r.text.top, bottom: r.text.bottom });
        let mut i = first;
        let mut visible = Vec::new();
        while i < n && tops[i] < scroll + view_h {
            visible.push(i);
            i += 1;
        }
        let match_first = tv.matches.partition_point(|m| m.1 <= tv.starts.get(first).copied().unwrap_or(0));
        for &i in &visible {
            let (a, b) = tv.line_range(i);
            let has_nl = i + 1 < n;
            let y = oy + tops[i];
            let Some(line) = tv.line(g, i, width, wrap) else { continue };
            let l = line.layout.clone();
            let ranges = |from: usize, to: usize| -> Vec<DWRITE_HIT_TEST_METRICS> {
                unsafe {
                    let mut cnt = 0u32;
                    let _ = l.HitTestTextRange(from as u32, (to - from) as u32, 0.0, 0.0, None, &mut cnt);
                    let mut v = vec![DWRITE_HIT_TEST_METRICS::default(); cnt as usize];
                    if l.HitTestTextRange(from as u32, (to - from) as u32, 0.0, 0.0, Some(&mut v), &mut cnt).is_err() {
                        return Vec::new();
                    }
                    v.truncate(cnt as usize);
                    v
                }
            };
            // Find results on this line, then the selection over them.
            let mut k = match_first;
            while k < tv.matches.len() && tv.matches[k].0 < b + 1 {
                let (ma, mb) = tv.matches[k];
                if mb > a && ma < b {
                    for m in ranges(ma.max(a) - a, mb.min(b) - a) {
                        g.fill(rect(ox + m.left, y + m.top, m.width.max(2.0), m.height), gfx::rgba(t.accent, 0.22));
                    }
                }
                k += 1;
            }
            if sa < sb && sb > a && sa <= b {
                let (la, lb) = (sa.max(a) - a, sb.min(b) - a);
                let sel = gfx::rgba(t.accent, if focused_any { 0.45 } else { 0.24 });
                if la < lb {
                    for m in ranges(la, lb) {
                        g.fill(rect(ox + m.left, y + m.top, m.width, m.height), sel);
                    }
                }
                // The line break itself, when it's selected: a sliver past the line's end.
                if has_nl && sb > b {
                    let (ex, et, eh) = unsafe {
                        let (mut x, mut yy) = (0.0f32, 0.0f32);
                        let mut m = DWRITE_HIT_TEST_METRICS::default();
                        let _ = l.HitTestTextPosition((b - a) as u32, false, &mut x, &mut yy, &mut m);
                        (x, m.top, m.height)
                    };
                    g.fill(rect(ox + ex, y + et, ch_w * 0.6, eh.max(row)), sel);
                }
            }
            g.draw_layout(&l, ox, y, rgb(t.text));
        }
        // The caret, and where a dragged selection would drop.
        if focused_text || matches!(tv.drag, Some(Drag::Move { to: Some(_), .. })) {
            let at = match tv.drag {
                Some(Drag::Move { to: Some(to), .. }) => to,
                _ => caret,
            };
            let (x, top, h) = tv.caret_xy(g, &tops, at, width, wrap);
            let cr = rect(g.snap(ox + x), oy + top, 1.5, h);
            if focused_text {
                caret_rect = Some(cr);
            }
            if caret_on || at != caret {
                g.fill(cr, rgb(if at != caret { t.accent } else { t.text }));
            }
        }
        g.pop_clip();

        // The line numbers, the caret's brighter.
        if let Some(fmt) = &fmt {
            let caret_line = tv.line_of(caret);
            g.push_clip(r.gutter);
            for &i in &visible {
                let nr = rect(r.gutter.left, oy + tops[i], r.gutter.right - r.gutter.left - 12.0, row);
                g.text_tabular(&wide(&(i + 1).to_string()), fmt, nr, rgb(if i == caret_line { t.label } else { t.placeholder }), Align::Right);
            }
            g.pop_clip();
        }

        // The scrollbars, when it overflows.
        let held = matches!(tv.drag, Some(Drag::Thumb));
        let hheld = matches!(tv.drag, Some(Drag::HThumb));
        if max_scroll > 0.0 {
            let track_h = r.track.bottom - r.track.top;
            let thumb_h = (track_h * view_h / total_h).max(28.0);
            let ty = r.track.top + (track_h - thumb_h) * scroll / max_scroll;
            self.hits.add(Hit::TextScroll, D2D_RECT_F { left: r.track.left - 2.0, right: r.track.right + 2.0, ..r.track });
            ui::scroll_thumb(g, rect(r.track.left, ty, ui::SCROLL_W, thumb_h), held);
        }
        if max_x > 0.0 {
            let track_w = r.htrack.right - r.htrack.left;
            let thumb_w = (track_w * vw / (vw + max_x)).max(28.0);
            let tx = r.htrack.left + (track_w - thumb_w) * scroll_x / max_x;
            self.hits.add(Hit::TextHScroll, D2D_RECT_F { top: r.htrack.top - 2.0, bottom: r.htrack.bottom + 2.0, ..r.htrack });
            ui::scroll_thumb(g, rect(tx, r.htrack.top, thumb_w, ui::SCROLL_W), hheld);
        }

        self.draw_text_bar(g, &r);
        self.draw_text_foot(g, &r);
        self.caret_rect = caret_rect.or(self.caret_rect.filter(|_| !focused_text));
        if focused_text {
            self.place_ime();
        }
    }

    /// The find bar: Find (and Replace) or Go to line, with their buttons.
    fn draw_text_bar(&mut self, g: &Gfx, r: &Rects) {
        let Some(br) = r.bar else { return };
        let t = theme::t();
        let focus = self.text_view.as_ref().map_or(Focus::None, |tv| tv.focus);
        let Some(tv) = self.text_view.as_ref() else { return };
        let Some(bar) = tv.bar.as_ref() else { return };
        let mode = bar.mode;
        let (case, wrap_around) = (bar.case, bar.wrap_around);
        let count = tv.matches.len();
        let (sa, sb) = tv.edit.selection();
        let current = tv.matches.iter().position(|&m| m == (sa, sb));
        let has_query = !bar.find.edit.units().is_empty();
        g.hline(br.left + 1.0, br.bottom, br.right - br.left - 2.0, rgb(t.divider));
        self.hits.add(Hit::TextBar, br);
        let btn = 28.0;
        let row1 = rect(br.left + 10.0, br.top + 6.0, br.right - br.left - 20.0, btn);
        let button = |app: &mut App, id: u8, r: D2D_RECT_F, glyph: u16, on: Option<bool>, enabled: bool| {
            let hit = Hit::TextButton(id);
            if enabled {
                app.hits.add(hit, r);
            }
            let st = app.state(hit, enabled);
            let kind = match on {
                Some(on) => ui::Kind::Toggle(on),
                None => ui::Kind::Standard,
            };
            let fg = ui::quiet_frame(g, r, kind, &st);
            g.text(&[glyph], &g.fonts.icons, r, fg, Align::Center);
        };
        let mut right = row1.right;
        let mut take = |w: f32| {
            right -= w;
            let x = right;
            right -= 4.0;
            rect(x, row1.top, w, btn)
        };
        let close = take(btn);
        button(self, 2, close, 0xE711, None, true);
        if mode == BarMode::GoTo {
            let go = take(64.0);
            let hit = Hit::TextButton(8);
            self.hits.add(hit, go);
            let st = self.state(hit, true);
            let fg = ui::quiet_frame(g, go, ui::Kind::Standard, &st);
            g.text(&wide("Go"), &g.fonts.body, go, fg, Align::Center);
            let label = rect(row1.left, row1.top, 92.0, btn);
            g.text(&wide("Go to line"), &g.fonts.body, label, rgb(t.label), Align::Left);
            let field = D2D_RECT_F { left: label.right, right: right - 4.0, ..row1 };
            self.hits.add(Hit::TextField(2), field);
            if let Some(f) = self.text_view.as_mut().and_then(|tv| tv.bar.as_mut()) {
                let c = f.goto.draw(g, field, t, focus == Focus::GoTo, 0.0, None);
                if focus == Focus::GoTo {
                    self.caret_rect = c;
                }
            }
            return;
        }
        let wrap_btn = take(btn);
        button(self, 4, wrap_btn, 0xE8EE, Some(wrap_around), true);
        let case_btn = take(btn);
        button(self, 3, case_btn, 0xE8D2, Some(case), true);
        let next = take(btn);
        button(self, 1, next, 0xE70D, None, has_query);
        let prev = take(btn);
        button(self, 0, prev, 0xE70E, None, has_query);
        let status = if !has_query {
            String::new()
        } else if count == 0 {
            "No results".into()
        } else {
            let more = if count >= MATCH_MAX { "+" } else { "" };
            match current {
                Some(k) => format!("{} of {count}{more}", k + 1),
                None => format!("{count}{more} results"),
            }
        };
        let sw = 96.0;
        let status_r = take(sw);
        g.text_tabular(&wide(&status), &g.fonts.caption, status_r, rgb(t.label), Align::Right);
        let expand = rect(row1.left, row1.top, btn, btn);
        button(self, 7, expand, if mode == BarMode::Replace { 0xE70D } else { 0xE76C }, None, true);
        let field = D2D_RECT_F { left: expand.right + 6.0, right: right - 4.0, ..row1 };
        self.hits.add(Hit::TextField(0), field);
        let mut caret = None;
        if let Some(b) = self.text_view.as_mut().and_then(|tv| tv.bar.as_mut()) {
            let c = b.find.draw(g, field, t, focus == Focus::Find, 0.0, None);
            if focus == Focus::Find {
                caret = c;
            }
        }
        if mode == BarMode::Replace {
            let row2 = D2D_RECT_F { top: row1.bottom + 8.0, bottom: row1.bottom + 8.0 + btn, ..row1 };
            let all = rect(row2.right - 92.0, row2.top, 92.0, btn);
            let one = rect(all.left - 4.0 - 76.0, row2.top, 76.0, btn);
            for (id, rr, label) in [(5u8, one, "Replace"), (6, all, "Replace all")] {
                let hit = Hit::TextButton(id);
                let enabled = has_query && !self.text_view.as_ref().is_some_and(|tv| tv.read_only);
                if enabled {
                    self.hits.add(hit, rr);
                }
                let st = self.state(hit, enabled);
                let fg = ui::quiet_frame(g, rr, ui::Kind::Standard, &st);
                g.text(&wide(label), &g.fonts.body, rr, fg, Align::Center);
            }
            let rfield = D2D_RECT_F { left: field.left, right: one.left - 8.0, ..row2 };
            self.hits.add(Hit::TextField(1), rfield);
            if let Some(b) = self.text_view.as_mut().and_then(|tv| tv.bar.as_mut()) {
                let c = b.replace.draw(g, rfield, t, focus == Focus::Replace, 0.0, None);
                if focus == Focus::Replace {
                    caret = c;
                }
            }
        }
        if caret.is_some() {
            self.caret_rect = caret;
        }
    }

    /// The foot: where the caret is and what's selected on the left; on the right whether it's edited, then the
    /// encoding, the line endings and Word wrap (each a button) and the size.
    fn draw_text_foot(&mut self, g: &Gfx, r: &Rects) {
        let t = theme::t();
        let wrap = self.settings.text_wrap;
        let Some(tv) = self.text_view.as_mut() else { return };
        g.hline(r.foot.left + 1.0, r.foot.top, r.foot.right - r.foot.left - 2.0, rgb(t.divider));
        let caret = tv.edit.caret();
        let line = tv.line_of(caret);
        let col = caret - tv.starts[line];
        let mut left = format!("Ln {}, Col {}", line + 1, col + 1);
        let (a, b) = tv.edit.selection();
        if a < b {
            let chars = String::from_utf16_lossy(&tv.edit.units()[a..b]).chars().count();
            left.push_str(&format!("   {chars} selected"));
        }
        let fr = D2D_RECT_F { left: r.foot.left + 12.0, right: r.foot.right - 8.0, ..r.foot };
        g.text_tabular(&wide(&left), &g.fonts.caption, fr, rgb(t.label), Align::Left);
        let note = if let Some(e) = &tv.error {
            Some(e.clone())
        } else if tv.read_only {
            Some("Read only: too large to edit".into())
        } else if tv.disk_changed {
            Some("Changed on disk".into())
        } else if tv.dirty() {
            Some("Edited".into())
        } else {
            None
        };
        let chips = [(2u8, if wrap { "Word wrap: on".to_string() } else { "Word wrap: off".to_string() }), (1, tv.eol.short().to_string()), (0, tv.encoding.label().to_string())];
        let size = format!("{}%", (tv.size / SIZE_DEFAULT * 100.0).round());
        let mut x = fr.right;
        let sw = g.measure_tabular(&wide(&size), &g.fonts.caption).ceil() + 12.0;
        x -= sw;
        g.text_tabular(&wide(&size), &g.fonts.caption, rect(x, r.foot.top, sw, FOOT_H), rgb(t.label), Align::Center);
        for (id, label) in chips {
            let w = g.measure(&wide(&label), &g.fonts.caption).ceil() + 16.0;
            x -= w + 2.0;
            let cr = rect(x, r.foot.top + 3.0, w, FOOT_H - 6.0);
            let hit = Hit::TextFoot(id);
            self.hits.add(hit, cr);
            let st = self.state(hit, true);
            let fg = ui::quiet_frame(g, cr, ui::Kind::Standard, &st);
            g.text(&wide(&label), &g.fonts.caption, cr, fg, Align::Center);
        }
        if let Some(note) = note {
            let w = g.measure(&wide(&note), &g.fonts.caption).ceil() + 12.0;
            g.text(&wide(&note), &g.fonts.caption, rect(x - w - 4.0, r.foot.top, w, FOOT_H), rgb(if self.text_view.as_ref().is_some_and(|tv| tv.disk_changed) { t.warn } else { t.label }), Align::Center);
        }
    }

    // --- The mouse ----------------------------------------------------------------------------------

    /// A press in the text: the caret there and the keyboard to the view. Shift extends; a press on the selection
    /// may become a drag of it; a double-click picks the word and a third click soon after the line.
    pub(super) fn text_press(&mut self, x: f32, y: f32, double: bool) {
        let shift = unsafe { GetKeyState(VK_SHIFT.0 as i32) < 0 };
        self.explorer.focused = false;
        let triple = self.text_view.as_ref().and_then(|tv| tv.last_double).is_some_and(|(at, px, py)| {
            at.elapsed().as_millis() < unsafe { GetDoubleClickTime() } as u128 && (px - x).abs() < 4.0 && (py - y).abs() < 4.0
        });
        let Some(pos) = self.with_text(false, |tv, g, r, tops, width, wrap| tv.pos_at(g, tops, x - r.text.left + tv.scroll_x, y - r.text.top + tv.scroll, width, wrap)) else { return };
        let was = self.text_view.as_ref().map_or(Focus::None, |tv| tv.focus);
        let Some(tv) = self.text_view.as_mut() else { return };
        tv.focus = Focus::Text;
        tv.goal_x = None;
        tv.caret_on = true;
        if double {
            tv.last_double = Some((Instant::now(), x, y));
            let (a, b) = tv.edit.word_at(pos);
            tv.edit.select(a, b);
            tv.drag = Some(Drag::Select);
        } else if triple {
            tv.last_double = None;
            let i = tv.line_of(pos);
            let (a, b) = tv.line_range(i);
            let n = tv.edit.units().len();
            tv.edit.select(a, (b + 1).min(n));
            tv.drag = None;
        } else {
            let (a, b) = tv.edit.selection();
            if !shift && a < b && pos > a && pos < b {
                tv.drag = Some(Drag::Move { pos, x, y, to: None });
            } else {
                let anchor = if shift { tv.edit.anchor() } else { pos };
                tv.edit.select(anchor, pos);
                tv.drag = Some(Drag::Select);
            }
        }
        if was == Focus::None {
            self.set_ime(true);
        }
        self.restart_caret();
        self.invalidate();
    }

    /// The pointer moved with the button down in the text.
    pub(super) fn text_drag(&mut self, x: f32, y: f32) {
        let Some(drag) = self.text_view.as_ref().and_then(|tv| tv.drag) else { return };
        if let Drag::Move { x: px, y: py, to: None, .. } = drag {
            if (x - px).abs() + (y - py).abs() < 5.0 {
                return;
            }
        }
        let Some(pos) = self.with_text(false, |tv, g, r, tops, width, wrap| tv.pos_at(g, tops, x - r.text.left + tv.scroll_x, y - r.text.top + tv.scroll, width, wrap)) else { return };
        let Some(tv) = self.text_view.as_mut() else { return };
        match drag {
            Drag::Select => {
                let anchor = tv.edit.anchor();
                tv.edit.select(anchor, pos);
            }
            Drag::Move { pos: p0, x: px, y: py, .. } => tv.drag = Some(Drag::Move { pos: p0, x: px, y: py, to: Some(pos) }),
            _ => {}
        }
        self.invalidate();
    }

    pub(super) fn text_release(&mut self) {
        let ctrl = unsafe { GetKeyState(VK_CONTROL.0 as i32) < 0 };
        let Some(tv) = self.text_view.as_mut() else { return };
        match tv.drag.take() {
            // A click on the selection that didn't move: the caret goes there.
            Some(Drag::Move { pos, to: None, .. }) => tv.edit.select(pos, pos),
            // Dropped: the text moves there (Ctrl copies), unless that's inside itself.
            Some(Drag::Move { to: Some(to), .. }) if !tv.read_only => {
                tv.edit.move_selection(to, ctrl);
                tv.changed();
            }
            _ => {}
        }
        self.invalidate();
    }

    pub(super) fn text_scroll_press(&mut self, x: f32, y: f32, horizontal: bool) {
        if let Some(tv) = self.text_view.as_mut() {
            tv.drag = Some(if horizontal { Drag::HThumb } else { Drag::Thumb });
        }
        self.text_scroll_drag(x, y);
    }

    pub(super) fn text_scroll_drag(&mut self, x: f32, y: f32) {
        let horizontal = self.text_view.as_ref().is_some_and(|tv| tv.drag == Some(Drag::HThumb));
        self.with_text(false, |tv, _, r, tops, _, _| {
            let view_h = r.text.bottom - r.text.top;
            if horizontal {
                let vw = r.text.right - r.text.left;
                let max_x = (tv.max_w + 40.0 - vw).max(0.0);
                let f = ((x - r.htrack.left) / (r.htrack.right - r.htrack.left).max(1.0)).clamp(0.0, 1.0);
                tv.scroll_x = f * max_x;
            } else {
                let total_h = tops.last().copied().unwrap_or(0.0) + view_h * 0.5;
                let track_h = r.track.bottom - r.track.top;
                let thumb_h = (track_h * view_h / total_h.max(1.0)).max(28.0);
                let f = ((y - r.track.top - thumb_h / 2.0) / (track_h - thumb_h).max(1.0)).clamp(0.0, 1.0);
                tv.scroll = f * (total_h - view_h).max(0.0);
            }
        });
        self.invalidate();
    }

    pub(super) fn text_scroll_release(&mut self) {
        if let Some(tv) = self.text_view.as_mut() {
            if matches!(tv.drag, Some(Drag::Thumb | Drag::HThumb)) {
                tv.drag = None;
            }
        }
    }

    /// The wheel over the text scrolls it (Shift: sideways); with Ctrl it sizes the text.
    pub(super) fn text_wheel(&mut self, delta: f32, ctrl: bool, shift: bool) {
        let Some(tv) = self.text_view.as_mut() else { return };
        if ctrl {
            tv.size = (tv.size * 1.1f32.powf(delta / 120.0)).clamp(SIZE_MIN, SIZE_MAX).round();
        } else if shift {
            tv.scroll_x = (tv.scroll_x - delta / 120.0 * tv.metrics.1 * 6.0).max(0.0);
        } else {
            tv.scroll -= delta / 120.0 * tv.row_h() * 3.0;
        }
        self.invalidate();
    }

    /// Sideways wheels (WM_MOUSEHWHEEL).
    pub(super) fn text_hwheel(&mut self, delta: f32) {
        if let Some(tv) = self.text_view.as_mut() {
            tv.scroll_x = (tv.scroll_x + delta / 120.0 * tv.metrics.1 * 6.0).max(0.0);
            self.invalidate();
        }
    }

    /// A press anywhere else hands the keyboard back.
    pub(super) fn text_blur(&mut self) {
        if let Some(tv) = self.text_view.as_mut().filter(|t| t.focus != Focus::None) {
            tv.focus = Focus::None;
            self.set_ime(false);
            self.invalidate();
        }
    }

    /// Ctrl+=, Ctrl+- and Ctrl+0 over a text file size its text (the still under it isn't shown).
    pub(super) fn text_zoom_key(&mut self, vk: VIRTUAL_KEY) -> bool {
        let Some(tv) = self.text_view.as_mut().filter(|_| self.placeholder.is_none()) else { return false };
        tv.size = match vk {
            VK_OEM_PLUS | VK_ADD => (tv.size + 2.0).min(SIZE_MAX),
            VK_OEM_MINUS | VK_SUBTRACT => (tv.size - 2.0).max(SIZE_MIN),
            VK_0 | VK_NUMPAD0 => SIZE_DEFAULT,
            _ => return false,
        };
        self.invalidate();
        true
    }

    /// The caret's blink: the view's and its find bar's boxes'.
    pub(super) fn text_blink(&mut self) -> bool {
        if !self.text_focused() {
            return false;
        }
        if let Some(tv) = self.text_view.as_mut() {
            tv.caret_on = !tv.caret_on;
            let on = tv.caret_on;
            if let Some(b) = tv.bar.as_mut() {
                b.find.caret_on = on;
                b.replace.caret_on = on;
                b.goto.caret_on = on;
            }
        }
        self.invalidate();
        true
    }

    // --- Clicks on the bar and the foot -------------------------------------------------------------

    pub(super) fn text_field_press(&mut self, i: u8, x: f32) {
        let r = self.text_rects();
        let Some(tv) = self.text_view.as_mut() else { return };
        tv.focus = match i {
            0 => Focus::Find,
            1 => Focus::Replace,
            _ => Focus::GoTo,
        };
        let id = Hit::TextField(i);
        let fr = self.hits.rect_of(id);
        if let (Some(g), Some(fr), Some(bar)) = (self.gfx.as_ref(), fr, tv.bar.as_mut()) {
            let f = match i {
                0 => &mut bar.find,
                1 => &mut bar.replace,
                _ => &mut bar.goto,
            };
            let p = f.position_at(g, fr, false, x);
            f.edit.select(p, p);
            f.caret_on = true;
        }
        let _ = r;
        self.set_ime(true);
        self.restart_caret();
        self.invalidate();
    }

    pub(super) fn text_bar_button(&mut self, i: u8) {
        match i {
            0 => self.text_find(false),
            1 => self.text_find(true),
            2 => self.text_close_bar(),
            3 | 4 => {
                if let Some(b) = self.text_view.as_mut().and_then(|tv| tv.bar.as_mut()) {
                    if i == 3 {
                        b.case = !b.case;
                    } else {
                        b.wrap_around = !b.wrap_around;
                    }
                }
            }
            5 => self.text_replace_one(),
            6 => self.text_replace_all(),
            7 => {
                let mode = self.text_view.as_ref().and_then(|tv| tv.bar.as_ref()).map(|b| b.mode);
                self.text_open_bar(if mode == Some(BarMode::Replace) { BarMode::Find } else { BarMode::Replace });
            }
            8 => self.text_go_to(),
            _ => {}
        }
        self.invalidate();
    }

    pub(super) fn text_foot_click(&mut self, i: u8) {
        let Some(r) = self.hits.rect_of(Hit::TextFoot(i)) else { return };
        match i {
            2 => self.text_action(TextAct::Wrap),
            0 | 1 => {
                let Some(tv) = self.text_view.as_ref() else { return };
                let e: Vec<Entry<menus::Action>> = if i == 0 {
                    Encoding::ALL.iter().enumerate().map(|(k, e)| menus::check(menus::Action::Text(TextAct::Encoding(k as u8)), e.label(), *e == tv.encoding)).collect()
                } else {
                    Eol::ALL.iter().enumerate().map(|(k, e)| menus::check(menus::Action::Text(TextAct::Eol(k as u8)), e.label(), *e == tv.eol)).collect()
                };
                let Some(g) = &self.gfx else { return };
                let (w, h) = self.size_dip();
                let menu = ui::Menu::open(g, e, r.left, r.top, rect(0.0, 0.0, w, h));
                // Upwards from the button: the foot is at the bottom.
                let mut menu = menu;
                let mh = menu.height();
                menu.y = (r.top - mh - 4.0).max(4.0);
                self.menu = Some((menus::MenuKind::Text, menu));
                self.hide_tooltip();
                self.invalidate();
            }
            _ => {}
        }
    }

    /// The right-click menu: Notepad's, with the find bar's and Word wrap.
    pub(super) fn text_menu(&mut self, x: f32, y: f32) {
        // A right-click off the selection moves the caret there first, as Notepad does.
        let pos = self.with_text(false, |tv, g, r, tops, width, wrap| tv.pos_at(g, tops, x - r.text.left + tv.scroll_x, y - r.text.top + tv.scroll, width, wrap));
        let Some(tv) = self.text_view.as_mut() else { return };
        if let Some(p) = pos {
            let (a, b) = tv.edit.selection();
            if p < a || p > b || a == b {
                tv.edit.select(p, p);
            }
        }
        tv.focus = Focus::Text;
        let ro = tv.read_only;
        let sel = tv.edit.has_selection();
        let can_paste = unsafe { windows::Win32::System::DataExchange::IsClipboardFormatAvailable(13).is_ok() };
        let item = |a: TextAct, glyph: u16, label: &str, accel: Option<&'static str>, enabled: bool| {
            Entry::Item(ui::MenuItem { icon: None, tint: None, action: menus::Action::Text(a), glyph: Some(glyph), label: label.into(), accel, check: None, enabled, current: false })
        };
        let wrap = self.settings.text_wrap;
        let e = vec![
            item(TextAct::Undo, 0xE7A7, "Undo", Some("Ctrl+Z"), !ro),
            item(TextAct::Redo, 0xE7A6, "Redo", Some("Ctrl+Y"), !ro),
            Entry::Separator,
            item(TextAct::Cut, 0xE8C6, "Cut", Some("Ctrl+X"), sel && !ro),
            item(TextAct::Copy, 0xE8C8, "Copy", Some("Ctrl+C"), sel),
            item(TextAct::Paste, 0xE77F, "Paste", Some("Ctrl+V"), can_paste && !ro),
            item(TextAct::Delete, 0xE74D, "Delete", Some("Del"), sel && !ro),
            Entry::Separator,
            item(TextAct::SelectAll, 0xE8B3, "Select all", Some("Ctrl+A"), true),
            item(TextAct::TimeDate, 0xEC92, "Time/Date", Some("F5"), !ro),
            Entry::Separator,
            item(TextAct::Find, 0xE721, "Find", Some("Ctrl+F"), true),
            item(TextAct::Replace, 0xE8AC, "Replace", Some("Ctrl+H"), !ro),
            item(TextAct::GoTo, 0xE8FD, "Go to line", Some("Ctrl+G"), true),
            Entry::Separator,
            Entry::Item(ui::MenuItem { icon: None, tint: None, action: menus::Action::Text(TextAct::Wrap), glyph: Some(0xE7FD), label: "Word wrap".into(), accel: None, check: Some(wrap), enabled: true, current: false }),
            item(TextAct::SaveAs, 0xE792, "Save as", Some("Ctrl+Shift+S"), true),
        ];
        let Some(g) = &self.gfx else { return };
        let (w, h) = self.size_dip();
        self.menu = Some((menus::MenuKind::Text, ui::Menu::open(g, e, x, y, rect(0.0, 0.0, w, h))));
        self.hide_tooltip();
        self.invalidate();
    }

    pub(super) fn text_action(&mut self, a: TextAct) {
        let hwnd = self.hwnd;
        let Some(tv) = self.text_view.as_mut() else { return };
        let ro = tv.read_only;
        let mut edited = false;
        match a {
            TextAct::Undo if !ro => edited = tv.edit.undo(),
            TextAct::Redo if !ro => edited = tv.edit.redo(),
            TextAct::Cut if !ro => {
                if let Some(s) = tv.edit.cut() {
                    clipboard::set_text(hwnd, &s.replace('\n', "\r\n"));
                    edited = true;
                }
            }
            TextAct::Copy => {
                if tv.edit.has_selection() {
                    clipboard::set_text(hwnd, &tv.edit.selected().replace('\n', "\r\n"));
                }
            }
            TextAct::Paste if !ro => {
                if let Some(s) = clipboard::get_text(hwnd) {
                    tv.edit.insert_multiline(&s);
                    edited = true;
                }
            }
            TextAct::Delete if !ro => {
                if tv.edit.has_selection() {
                    tv.edit.delete(false);
                    edited = true;
                }
            }
            TextAct::SelectAll => tv.edit.select_all(),
            TextAct::TimeDate if !ro => {
                let s = unsafe {
                    let st = windows::Win32::System::SystemInformation::GetLocalTime();
                    let mut d = [0u16; 64];
                    let mut t = [0u16; 64];
                    let dn = GetDateFormatEx(PCWSTR::null(), DATE_SHORTDATE, Some(&st), PCWSTR::null(), Some(&mut d), PCWSTR::null());
                    let tn = GetTimeFormatEx(PCWSTR::null(), TIME_NOSECONDS, Some(&st), PCWSTR::null(), Some(&mut t));
                    format!("{} {}", String::from_utf16_lossy(&t[..(tn.max(1) - 1) as usize]), String::from_utf16_lossy(&d[..(dn.max(1) - 1) as usize]))
                };
                tv.edit.insert_multiline(&s);
                edited = true;
            }
            TextAct::Find => return self.text_open_bar(BarMode::Find),
            TextAct::Replace if !ro => return self.text_open_bar(BarMode::Replace),
            TextAct::GoTo => return self.text_open_bar(BarMode::GoTo),
            TextAct::Wrap => {
                self.settings.text_wrap = !self.settings.text_wrap;
                settings::save(&self.settings);
                if let Some(tv) = self.text_view.as_mut() {
                    tv.reveal = true;
                    tv.scroll_x = 0.0;
                }
                self.invalidate();
                return;
            }
            TextAct::SaveAs => return self.text_save_as(),
            TextAct::Encoding(k) => {
                if let Some(&e) = Encoding::ALL.get(k as usize) {
                    if e != tv.encoding {
                        tv.encoding = e;
                        tv.meta_dirty = true;
                    }
                }
            }
            TextAct::Eol(k) => {
                if let Some(&e) = Eol::ALL.get(k as usize) {
                    if e != tv.eol {
                        tv.eol = e;
                        tv.meta_dirty = true;
                    }
                }
            }
            _ => {}
        }
        if edited {
            tv.changed();
        }
        self.invalidate();
    }

    // --- The find bar -------------------------------------------------------------------------------

    fn text_open_bar(&mut self, mode: BarMode) {
        let Some(tv) = self.text_view.as_mut() else { return };
        // The selection, if it's on one line, is what's looked for.
        let (a, b) = tv.edit.selection();
        let seed = (a < b && b - a < 200 && !tv.edit.units()[a..b].contains(&NL)).then(|| tv.edit.selected());
        let bar = tv.bar.get_or_insert_with(|| Bar {
            mode,
            find: TextField::new(TextEdit::new("")),
            replace: TextField::new(TextEdit::new("")),
            goto: TextField::new(TextEdit::new("")),
            case: false,
            wrap_around: true,
        });
        bar.mode = mode;
        if mode == BarMode::GoTo {
            let line = tv.edit.units()[..tv.edit.caret()].iter().filter(|&&c| c == NL).count() + 1;
            bar.goto.edit = TextEdit::new(&line.to_string());
            bar.goto.edit.select_all();
            tv.focus = Focus::GoTo;
        } else {
            if let Some(s) = seed {
                bar.find.edit = TextEdit::new(&s);
            }
            bar.find.edit.select_all();
            tv.focus = Focus::Find;
        }
        tv.matches_key = None;
        self.set_ime(true);
        self.restart_caret();
        self.invalidate();
    }

    fn text_close_bar(&mut self) {
        if let Some(tv) = self.text_view.as_mut() {
            tv.bar = None;
            tv.matches.clear();
            tv.matches_key = None;
            tv.focus = Focus::Text;
            tv.reveal = true;
        }
        self.invalidate();
    }

    /// F3 / Enter in the find box: the next result after the selection (or before it), wrapping if allowed.
    fn text_find(&mut self, forward: bool) {
        let Some(tv) = self.text_view.as_mut() else { return };
        if tv.bar.is_none() {
            return self.text_open_bar(BarMode::Find);
        }
        tv.update_matches();
        let wrap_around = tv.bar.as_ref().is_some_and(|b| b.wrap_around);
        let (a, b) = tv.edit.selection();
        let m = &tv.matches;
        let hit = if forward {
            let k = m.partition_point(|x| x.0 < b.max(a + 1).min(b.max(a)) || (x.0 == a && x.1 == b));
            let k = if k < m.len() && m[k] == (a, b) { k + 1 } else { k };
            m.get(k).copied().or_else(|| if wrap_around { m.first().copied() } else { None })
        } else {
            let k = m.partition_point(|x| x.1 <= a);
            if k > 0 { Some(m[k - 1]) } else if wrap_around { m.last().copied() } else { None }
        };
        match hit {
            Some((x, y)) => {
                tv.edit.select(x, y);
                tv.reveal = true;
            }
            None => {
                let none = tv.matches.is_empty();
                self.show_toast(if none { "No results" } else if forward { "No more results below" } else { "No more results above" }, true);
            }
        }
        self.invalidate();
    }

    fn text_replace_one(&mut self) {
        let Some(tv) = self.text_view.as_mut() else { return };
        if tv.read_only {
            return;
        }
        tv.update_matches();
        let sel = tv.edit.selection();
        let with = tv.bar.as_ref().map(|b| b.replace.edit.text()).unwrap_or_default();
        if tv.matches.binary_search(&sel).is_ok() {
            tv.edit.replace_range(sel.0, sel.1, &with);
            let end = tv.edit.selection().1;
            tv.edit.select(end, end);
            tv.changed();
            tv.update_matches();
        }
        self.text_find(true);
    }

    fn text_replace_all(&mut self) {
        let Some(tv) = self.text_view.as_mut() else { return };
        if tv.read_only {
            return;
        }
        tv.update_matches();
        let n = tv.matches.len();
        if n == 0 {
            return self.show_toast("No results", true);
        }
        let with = tv.bar.as_ref().map(|b| b.replace.edit.text()).unwrap_or_default();
        let ranges = tv.matches.clone();
        tv.edit.replace_all(&ranges, &with);
        tv.changed();
        self.show_toast(format!("Replaced {n}"), true);
    }

    fn text_go_to(&mut self) {
        let Some(tv) = self.text_view.as_mut() else { return };
        let Some(n) = tv.bar.as_ref().and_then(|b| b.goto.edit.text().trim().parse::<usize>().ok()) else { return self.show_toast("Type a line number", true) };
        let count = tv.line_count();
        if n == 0 || n > count {
            return self.show_toast(format!("The line number is beyond the {count} lines"), true);
        }
        let start = tv.starts[n - 1];
        tv.edit.select(start, start);
        tv.reveal = true;
        self.text_close_bar();
    }

    // --- Save As, and changes on disk ---------------------------------------------------------------

    /// Save As: a file name, an encoding and line endings (the dialog's own drop-downs); the copy saved is the
    /// file Looker shows from then on.
    fn text_save_as(&mut self) {
        let Some(tv) = self.text_view.as_ref() else { return };
        let (path, enc, eol) = (tv.path.clone(), tv.encoding, tv.eol);
        let picked = unsafe {
            (|| -> Option<(PathBuf, u32, u32)> {
                let dlg: IFileSaveDialog = CoCreateInstance(&FileSaveDialog, None, CLSCTX_INPROC_SERVER).ok()?;
                let ext = path.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_else(|| "txt".into());
                let filters = [
                    COMDLG_FILTERSPEC { pszName: w!("Text documents (*.txt)"), pszSpec: w!("*.txt") },
                    COMDLG_FILTERSPEC { pszName: w!("Source materials (*.vmt)"), pszSpec: w!("*.vmt") },
                    COMDLG_FILTERSPEC { pszName: w!("All files (*.*)"), pszSpec: w!("*.*") },
                ];
                let _ = dlg.SetFileTypes(&filters);
                let _ = dlg.SetFileTypeIndex(if ext == "vmt" { 2 } else { 1 });
                let _ = dlg.SetDefaultExtension(&HSTRING::from(ext.as_str()));
                if let Some(name) = path.file_name() {
                    let _ = dlg.SetFileName(&HSTRING::from(name));
                }
                if let Some(dir) = path.parent() {
                    if let Ok(item) = SHCreateItemFromParsingName::<_, _, IShellItem>(&HSTRING::from(dir.as_os_str()), None) {
                        let _ = dlg.SetFolder(&item);
                    }
                }
                let c: IFileDialogCustomize = dlg.cast().ok()?;
                let _ = c.StartVisualGroup(100, w!("Encoding:"));
                let _ = c.AddComboBox(101);
                for (k, e) in Encoding::ALL.iter().enumerate() {
                    let _ = c.AddControlItem(101, k as u32, &HSTRING::from(e.label()));
                }
                let _ = c.SetSelectedControlItem(101, Encoding::ALL.iter().position(|e| *e == enc).unwrap_or(0) as u32);
                let _ = c.EndVisualGroup();
                let _ = c.StartVisualGroup(102, w!("Line endings:"));
                let _ = c.AddComboBox(103);
                for (k, e) in Eol::ALL.iter().enumerate() {
                    let _ = c.AddControlItem(103, k as u32, &HSTRING::from(e.label()));
                }
                let _ = c.SetSelectedControlItem(103, Eol::ALL.iter().position(|e| *e == eol).unwrap_or(0) as u32);
                let _ = c.EndVisualGroup();
                dlg.Show(Some(self.hwnd)).ok()?;
                let item = dlg.GetResult().ok()?;
                let name = item.GetDisplayName(SIGDN_FILESYSPATH).ok()?;
                let out = PathBuf::from(name.to_string().unwrap_or_default());
                CoTaskMemFree(Some(name.0 as _));
                Some((out, c.GetSelectedControlItem(101).unwrap_or(0), c.GetSelectedControlItem(103).unwrap_or(0)))
            })()
        };
        let Some((to, e, l)) = picked else { return };
        let Some(tv) = self.text_view.as_mut() else { return };
        let (old_enc, old_eol) = (tv.encoding, tv.eol);
        tv.encoding = Encoding::ALL.get(e as usize).copied().unwrap_or(old_enc);
        tv.eol = Eol::ALL.get(l as usize).copied().unwrap_or(old_eol);
        let name = to.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        match tv.save_to(&to) {
            Ok(()) => {
                // The copy is the file now: the original keeps what it had on disk (its edits went to the copy).
                tv.saved_hash = tv.hash;
                tv.meta_dirty = false;
                self.show_toast(format!("Saved {name}"), true);
                if to != path {
                    self.open(to);
                }
            }
            Err(err) => {
                tv.encoding = old_enc;
                tv.eol = old_eol;
                self.show_toast(format!("Couldn't save {name}: {err}"), false);
            }
        }
        self.invalidate();
    }

    fn text_save(&mut self) {
        let Some(tv) = self.text_view.as_mut() else { return };
        let name = tv.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        if tv.read_only {
            return self.show_toast(format!("{name} is read only: too large to edit"), true);
        }
        // Changed on disk since it was read: the first Ctrl+S warns, the second overwrites.
        if tv.disk_changed && !tv.overwrite_armed {
            tv.overwrite_armed = true;
            return self.show_toast(format!("{name} changed on disk since it was opened. Ctrl+S again to overwrite it"), false);
        }
        let path = tv.path.clone();
        match tv.save_to(&path) {
            Ok(()) => {
                self.viewer.forget(&path);
                self.show_toast(format!("Saved {name}"), true);
            }
            Err(e) => self.show_toast(format!("Couldn't save {name}: {e}"), false),
        }
        self.invalidate();
    }

    /// Every second while a text file is up: changed on disk by something else? With no edits here it reloads
    /// (the caret and scroll kept as near as they can be); with edits, the foot says so.
    pub(super) fn text_poll(&mut self) {
        let Some(tv) = self.text_view.as_mut() else {
            unsafe {
                let _ = KillTimer(Some(self.hwnd), TIMER_TEXT);
            }
            return;
        };
        let now = disk_stamp(&tv.path);
        if now == tv.disk || now == (0, 0) {
            return;
        }
        tv.disk = now;
        if tv.dirty() {
            tv.disk_changed = true;
            self.invalidate();
            return;
        }
        let (caret, scroll, scroll_x, size, focus, bar) = (tv.edit.caret(), tv.scroll, tv.scroll_x, tv.size, tv.focus, tv.bar.take());
        let mut fresh = TextView::open(tv.path.clone());
        let n = fresh.edit.units().len();
        fresh.edit.select(caret.min(n), caret.min(n));
        (fresh.scroll, fresh.scroll_x, fresh.size, fresh.focus, fresh.bar) = (scroll, scroll_x, size, focus, bar);
        *tv = fresh;
        let path = tv.path.clone();
        self.viewer.forget(&path);
        self.show_toast("Reloaded: the file changed on disk", true);
        self.invalidate();
    }

    // --- The keyboard -------------------------------------------------------------------------------

    /// A character from WM_CHAR while the view (or a box of its find bar) has the keyboard.
    pub(super) fn text_char(&mut self, u: u16) {
        let Some(tv) = self.text_view.as_mut() else { return };
        tv.caret_on = true;
        if std::mem::take(&mut tv.swallow_enter) && u == 0x0D {
            return;
        }
        match tv.focus {
            Focus::Text => {
                if tv.read_only {
                    return;
                }
                match u {
                    0x0D => tv.edit.insert_multiline("\n"),
                    0x09 => tv.edit.insert_multiline("\t"),
                    u if u >= 0x20 && u != 0x7F => tv.edit.type_unit(u),
                    _ => return,
                }
                tv.changed();
            }
            Focus::Find | Focus::Replace | Focus::GoTo => {
                let focus = tv.focus;
                let Some(bar) = tv.bar.as_mut() else { return };
                let f = match focus {
                    Focus::Find => &mut bar.find,
                    Focus::Replace => &mut bar.replace,
                    _ => &mut bar.goto,
                };
                if u >= 0x20 && u != 0x7F {
                    f.edit.type_unit(u);
                    f.caret_on = true;
                }
            }
            Focus::None => return,
        }
        self.restart_caret();
        self.invalidate();
    }

    /// A key while the view (or its find bar) has the keyboard. It keeps them all (a letter's character follows as
    /// WM_CHAR) but the function keys it doesn't use.
    pub(super) fn text_key(&mut self, vk: VIRTUAL_KEY, shift: bool, ctrl: bool) -> bool {
        let focus = self.text_view.as_ref().map_or(Focus::None, |tv| tv.focus);
        // Everywhere in the view.
        match vk {
            VK_F if ctrl => return self.with_key(|a| a.text_open_bar(BarMode::Find)),
            VK_H if ctrl => return self.with_key(|a| a.text_action(TextAct::Replace)),
            VK_G if ctrl => return self.with_key(|a| a.text_open_bar(BarMode::GoTo)),
            VK_F3 => return self.with_key(|a| a.text_find(!shift)),
            VK_S if ctrl && shift => return self.with_key(|a| a.text_save_as()),
            VK_S if ctrl => return self.with_key(|a| a.text_save()),
            VK_OEM_PLUS | VK_ADD | VK_OEM_MINUS | VK_SUBTRACT | VK_0 | VK_NUMPAD0 if ctrl => return self.text_zoom_key(vk),
            VK_ESCAPE => {
                if self.text_view.as_ref().is_some_and(|tv| tv.bar.is_some()) {
                    self.text_close_bar();
                } else {
                    self.text_blur();
                }
                return true;
            }
            _ => {}
        }
        if matches!(focus, Focus::Find | Focus::Replace | Focus::GoTo) {
            return self.text_bar_key(vk, shift, ctrl, focus);
        }
        if vk == VK_F5 {
            self.text_action(TextAct::TimeDate);
            return true;
        }
        if (VK_F1.0..=VK_F24.0).contains(&vk.0) {
            return false;
        }
        let hwnd = self.hwnd;
        let page = self.text_rects().map_or(400.0, |r| r.text.bottom - r.text.top);
        let Some(tv) = self.text_view.as_mut() else { return false };
        let ro = tv.read_only;
        tv.caret_on = true;
        let mut edited = false;
        let mut moved = true;
        let mut vertical: Option<(f32, bool)> = None;
        let mut row_end: Option<bool> = None;
        match vk {
            VK_LEFT => tv.edit.move_caret(if ctrl { Move::WordLeft } else { Move::Left }, shift),
            VK_RIGHT => tv.edit.move_caret(if ctrl { Move::WordRight } else { Move::Right }, shift),
            VK_UP if !ctrl => vertical = Some((-1.0, false)),
            VK_DOWN if !ctrl => vertical = Some((1.0, false)),
            VK_UP | VK_DOWN => {
                tv.scroll += if vk == VK_UP { -tv.row_h() } else { tv.row_h() };
                moved = false;
            }
            VK_PRIOR => vertical = Some((-1.0, true)),
            VK_NEXT => vertical = Some((1.0, true)),
            VK_HOME | VK_END if ctrl => tv.edit.move_caret(if vk == VK_HOME { Move::Home } else { Move::End }, shift),
            VK_HOME => row_end = Some(false),
            VK_END => row_end = Some(true),
            VK_BACK if !ro => {
                tv.edit.backspace(ctrl);
                edited = true;
            }
            // Shift+Delete cuts, as in every Windows edit box.
            VK_DELETE if shift && !ctrl && !ro => {
                if let Some(s) = tv.edit.cut() {
                    clipboard::set_text(hwnd, &s.replace('\n', "\r\n"));
                    edited = true;
                }
            }
            VK_DELETE if !ro => {
                tv.edit.delete(ctrl);
                edited = true;
            }
            VK_A if ctrl => tv.edit.select_all(),
            VK_C if ctrl => {
                if tv.edit.has_selection() {
                    clipboard::set_text(hwnd, &tv.edit.selected().replace('\n', "\r\n"));
                }
                moved = false;
            }
            VK_INSERT if ctrl && !shift => {
                if tv.edit.has_selection() {
                    clipboard::set_text(hwnd, &tv.edit.selected().replace('\n', "\r\n"));
                }
                moved = false;
            }
            VK_X if ctrl && !ro => {
                if let Some(s) = tv.edit.cut() {
                    clipboard::set_text(hwnd, &s.replace('\n', "\r\n"));
                    edited = true;
                }
            }
            VK_V if ctrl && !ro => {
                if let Some(s) = clipboard::get_text(hwnd) {
                    tv.edit.insert_multiline(&s);
                    edited = true;
                }
            }
            VK_INSERT if shift && !ro => {
                if let Some(s) = clipboard::get_text(hwnd) {
                    tv.edit.insert_multiline(&s);
                    edited = true;
                }
            }
            VK_Z if ctrl && shift && !ro => edited = tv.edit.redo(),
            VK_Z if ctrl && !ro => edited = tv.edit.undo(),
            VK_Y if ctrl && !ro => edited = tv.edit.redo(),
            _ => moved = false,
        }
        if let Some((dir, by_page)) = vertical {
            let goal = tv.goal_x;
            let r = self.with_text(false, |tv, g, _, tops, width, wrap| {
                let (x, top, h) = tv.caret_xy(g, tops, tv.edit.caret(), width, wrap);
                let gx = goal.unwrap_or(x);
                let y = if by_page { top + h / 2.0 + dir * page } else { top + h / 2.0 + dir * h };
                let total = tops.last().copied().unwrap_or(0.0);
                let pos = if y < 0.0 { 0 } else if y >= total { tv.edit.units().len() } else { tv.pos_at(g, tops, gx, y, width, wrap) };
                (gx, pos)
            });
            if let (Some((gx, pos)), Some(tv)) = (r, self.text_view.as_mut()) {
                let anchor = if shift { tv.edit.anchor() } else { pos };
                tv.edit.select(anchor, pos);
                tv.goal_x = Some(gx);
                if by_page {
                    tv.scroll += dir * page;
                }
            }
        }
        if let Some(end) = row_end {
            let r = self.with_text(false, |tv, g, _, _, width, wrap| tv.row_range(g, tv.edit.caret(), width, wrap));
            if let (Some((a, b)), Some(tv)) = (r, self.text_view.as_mut()) {
                let to = if end {
                    b
                } else {
                    // Home goes to the first non-blank first, then to the row's start, as editors do.
                    let first = a + tv.edit.units()[a..b].iter().take_while(|&&u| u == b' ' as u16 || u == b'\t' as u16).count();
                    if tv.edit.caret() == first { a } else { first }
                };
                let anchor = if shift { tv.edit.anchor() } else { to };
                tv.edit.select(anchor, to);
            }
        }
        let Some(tv) = self.text_view.as_mut() else { return true };
        if edited {
            tv.changed();
        } else if moved || vertical.is_some() || row_end.is_some() {
            if vertical.is_none() {
                tv.goal_x = None;
            }
            tv.reveal = true;
        }
        self.restart_caret();
        self.invalidate();
        true
    }

    fn with_key(&mut self, f: impl FnOnce(&mut App)) -> bool {
        f(self);
        true
    }

    /// Keys in a box of the find bar: Enter finds (Shift+Enter backwards), replaces, or goes; Tab moves between
    /// Find and Replace; the usual editing keys edit the box.
    fn text_bar_key(&mut self, vk: VIRTUAL_KEY, shift: bool, ctrl: bool, focus: Focus) -> bool {
        match vk {
            VK_RETURN => {
                if let Some(tv) = self.text_view.as_mut() {
                    tv.swallow_enter = true;
                }
                match focus {
                    Focus::Find => self.text_find(!shift),
                    Focus::Replace => self.text_replace_one(),
                    _ => self.text_go_to(),
                }
                return true;
            }
            VK_TAB => {
                if let Some(tv) = self.text_view.as_mut() {
                    if tv.bar.as_ref().is_some_and(|b| b.mode == BarMode::Replace) {
                        tv.focus = if focus == Focus::Find { Focus::Replace } else { Focus::Find };
                    }
                }
                self.invalidate();
                return true;
            }
            _ => {}
        }
        let hwnd = self.hwnd;
        let Some(f) = self.text_view.as_mut().and_then(|tv| tv.bar.as_mut()).map(|b| match focus {
            Focus::Find => &mut b.find,
            Focus::Replace => &mut b.replace,
            _ => &mut b.goto,
        }) else {
            return true;
        };
        let e = &mut f.edit;
        match vk {
            VK_LEFT => e.move_caret(if ctrl { Move::WordLeft } else { Move::Left }, shift),
            VK_RIGHT => e.move_caret(if ctrl { Move::WordRight } else { Move::Right }, shift),
            VK_HOME | VK_UP => e.move_caret(Move::Home, shift),
            VK_END | VK_DOWN => e.move_caret(Move::End, shift),
            VK_BACK => e.backspace(ctrl),
            VK_DELETE => e.delete(ctrl),
            VK_A if ctrl => e.select_all(),
            VK_C if ctrl => {
                if e.has_selection() {
                    clipboard::set_text(hwnd, &e.selected());
                }
            }
            VK_X if ctrl => {
                if let Some(s) = e.cut() {
                    clipboard::set_text(hwnd, &s);
                }
            }
            VK_V if ctrl => {
                if let Some(s) = clipboard::get_text(hwnd) {
                    e.insert(&s);
                }
            }
            VK_Z if ctrl => {
                e.undo();
            }
            VK_Y if ctrl => {
                e.redo();
            }
            _ => {}
        }
        f.caret_on = true;
        self.restart_caret();
        self.invalidate();
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodings_round_trip() {
        for (bytes, e) in [
            (b"caf\xC3\xA9".to_vec(), Encoding::Utf8),
            (b"\xEF\xBB\xBFcaf\xC3\xA9".to_vec(), Encoding::Utf8Bom),
            (vec![0xFF, 0xFE, b'h', 0, 0xE9, 0], Encoding::Utf16Le),
            (vec![0xFE, 0xFF, 0, b'h', 0, 0xE9], Encoding::Utf16Be),
            (b"caf\xE9 \x93q\x94".to_vec(), Encoding::Ansi),
        ] {
            let (s, got) = decode_text(&bytes);
            assert!(got == e, "{}", e.label());
            assert_eq!(encode_text(&s, got), bytes, "{}", e.label());
        }
    }

    #[test]
    fn folding_ignores_case() {
        let a: Vec<u16> = "Café ÉTÉ".encode_utf16().map(fold).collect();
        let b: Vec<u16> = "café été".encode_utf16().collect();
        assert_eq!(a, b);
    }
}
