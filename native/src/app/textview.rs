//! A text file on screen (.txt, and Source materials, .vmt, which are text): one scrolling view to read and edit it, in the photo's place (Faye, 2026-11-11:
//! "a typical text viewer and editor in one view"). The theme's card with line numbers down the left, the text
//! wrapped at the view's width in Consolas, a status line at the foot (line and column, encoding, line endings,
//! whether it's edited). A click puts the caret in and gives it the keyboard (Escape or a click elsewhere hands it
//! back to the viewer's keys); typing, the usual caret keys, Ctrl+A/C/X/V/Z/Y, Ctrl+wheel for the text size.
//! Ctrl+S saves in the file's own encoding and line endings; moving to another file with edits unsaved saves them.
//!
//! The decode pool still draws the first page of it (`imaging/text.rs`) for thumbnails; that still isn't drawn here.

use windows::Win32::Graphics::DirectWrite::*;

use super::*;
use crate::textedit::{Move, TextEdit};

const PAD: f32 = 16.0;
const FOOT_H: f32 = 28.0;
const SIZE_MIN: f32 = 8.0;
const SIZE_MAX: f32 = 40.0;
const SIZE_DEFAULT: f32 = 14.0;
/// Past this the file opens read-only (laying out every edit would crawl).
const EDIT_MAX_BYTES: usize = 8 << 20;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Encoding {
    Utf8,
    Utf8Bom,
    Utf16Le,
    Utf16Be,
    Ansi,
}

impl Encoding {
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

pub(super) struct TextView {
    path: PathBuf,
    edit: TextEdit,
    /// The text as last saved (or loaded), to tell whether it's edited.
    saved: Vec<u16>,
    encoding: Encoding,
    crlf: bool,
    read_only: bool,
    scroll: f32,
    size: f32,
    pub(super) focused: bool,
    /// Up/Down keep to this x across short lines.
    goal_x: Option<f32>,
    /// The laid-out text, and what it was laid out for: (text version, width, size).
    layout: Option<(IDWriteTextLayout, u64, f32, f32)>,
    format: Option<(IDWriteTextFormat, f32)>,
    version: u64,
    /// Bring the caret into view at the next draw.
    reveal: bool,
    /// The scrollbar's grab offset while it's dragged.
    thumb_grab: Option<f32>,
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

impl TextView {
    fn open(path: PathBuf) -> TextView {
        let (text, encoding, crlf, read_only, error) = match std::fs::read(&path) {
            Ok(b) => {
                let big = b.len() > EDIT_MAX_BYTES;
                let (s, e) = decode_text(&b[..b.len().min(EDIT_MAX_BYTES)]);
                let crlf = s.contains("\r\n") || !s.contains('\n');
                (s.replace("\r\n", "\n").replace('\r', "\n"), e, crlf, big, None)
            }
            Err(e) => (String::new(), Encoding::Utf8, true, true, Some(e.to_string())),
        };
        let mut edit = TextEdit::new(&text);
        edit.select(0, 0);
        let saved = edit.units().to_vec();
        TextView { path, edit, saved, encoding, crlf, read_only, scroll: 0.0, size: SIZE_DEFAULT, focused: false, goal_x: None, layout: None, format: None, version: 0, reveal: false, thumb_grab: None, error }
    }

    fn dirty(&self) -> bool {
        self.edit.units() != self.saved.as_slice()
    }

    fn changed(&mut self) {
        self.version += 1;
        self.goal_x = None;
        self.reveal = true;
    }

    /// Writes the file in its own encoding and line endings.
    fn save(&mut self) -> Result<(), String> {
        let mut s = self.edit.text();
        if self.crlf {
            s = s.replace('\n', "\r\n");
        }
        std::fs::write(&self.path, encode_text(&s, self.encoding)).map_err(|e| e.to_string())?;
        self.saved = self.edit.units().to_vec();
        Ok(())
    }

    fn format(&mut self, g: &Gfx) -> Option<IDWriteTextFormat> {
        if let Some((f, s)) = &self.format {
            if *s == self.size {
                return Some(f.clone());
            }
        }
        let f = unsafe {
            let f = g.text.dwrite.CreateTextFormat(w!("Consolas"), None, DWRITE_FONT_WEIGHT_NORMAL, DWRITE_FONT_STYLE_NORMAL, DWRITE_FONT_STRETCH_NORMAL, self.size, w!("en-us")).ok()?;
            f.SetWordWrapping(DWRITE_WORD_WRAPPING_WRAP).ok()?;
            f.SetIncrementalTabStop(self.size * 0.5498 * 4.0).ok()?;
            f
        };
        self.format = Some((f.clone(), self.size));
        self.layout = None;
        Some(f)
    }

    /// The text laid out `width` wide (made again only when the text, the width or the size changed).
    fn layout(&mut self, g: &Gfx, width: f32) -> Option<IDWriteTextLayout> {
        if let Some((l, v, w, s)) = &self.layout {
            if *v == self.version && (*w - width).abs() < 0.5 && *s == self.size {
                return Some(l.clone());
            }
        }
        let fmt = self.format(g)?;
        let l = unsafe { g.text.dwrite.CreateTextLayout(self.edit.units(), &fmt, width.max(1.0), 1.0e7).ok()? };
        self.layout = Some((l.clone(), self.version, width, self.size));
        Some(l)
    }
}

fn line_metrics(l: &IDWriteTextLayout) -> Vec<DWRITE_LINE_METRICS> {
    unsafe {
        let mut n = 0u32;
        let _ = l.GetLineMetrics(None, &mut n);
        let mut v = vec![DWRITE_LINE_METRICS::default(); n as usize];
        if l.GetLineMetrics(Some(&mut v), &mut n).is_err() {
            return Vec::new();
        }
        v.truncate(n as usize);
        v
    }
}

fn content_height(l: &IDWriteTextLayout) -> f32 {
    unsafe {
        let mut m = DWRITE_TEXT_METRICS::default();
        if l.GetMetrics(&mut m).is_ok() { m.height } else { 0.0 }
    }
}

/// The caret's spot before UTF-16 position `pos`: (x, line top, line height), layout-relative.
fn caret_at(l: &IDWriteTextLayout, pos: usize) -> (f32, f32, f32) {
    unsafe {
        let (mut x, mut y) = (0.0f32, 0.0f32);
        let mut m = DWRITE_HIT_TEST_METRICS::default();
        if l.HitTestTextPosition(pos as u32, false, &mut x, &mut y, &mut m).is_ok() { (x, m.top, m.height) } else { (0.0, 0.0, 0.0) }
    }
}

/// The text position nearest a layout-relative point.
fn position_at(l: &IDWriteTextLayout, x: f32, y: f32) -> usize {
    unsafe {
        let mut trailing = windows::core::BOOL(0);
        let mut inside = windows::core::BOOL(0);
        let mut m = DWRITE_HIT_TEST_METRICS::default();
        if l.HitTestPoint(x, y, &mut trailing, &mut inside, &mut m).is_err() {
            return 0;
        }
        (m.textPosition + if trailing.as_bool() { m.length } else { 0 }) as usize
    }
}

fn range_rects(l: &IDWriteTextLayout, a: usize, b: usize) -> Vec<DWRITE_HIT_TEST_METRICS> {
    unsafe {
        let mut n = 0u32;
        let _ = l.HitTestTextRange(a as u32, (b - a) as u32, 0.0, 0.0, None, &mut n);
        let mut v = vec![DWRITE_HIT_TEST_METRICS::default(); n as usize];
        if l.HitTestTextRange(a as u32, (b - a) as u32, 0.0, 0.0, Some(&mut v), &mut n).is_err() {
            return Vec::new();
        }
        v.truncate(n as usize);
        v
    }
}

/// The visual line holding `pos`: its start and its end before the line break.
fn line_of(lines: &[DWRITE_LINE_METRICS], pos: usize) -> (usize, usize) {
    let mut start = 0usize;
    for (i, m) in lines.iter().enumerate() {
        let end = start + m.length as usize;
        if pos < end || i + 1 == lines.len() {
            return (start, end - m.newlineLength as usize);
        }
        start = end;
    }
    (0, 0)
}

/// Where the parts sit: the card, the gutter, the text, the scrollbar track, the foot.
struct Layout {
    card: D2D_RECT_F,
    gutter: D2D_RECT_F,
    text: D2D_RECT_F,
    track: D2D_RECT_F,
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
    }

    /// Puts the text view away, saving its edits (another file came up, or the window is closing).
    pub(super) fn close_text(&mut self) {
        let Some(mut old) = self.text_view.take() else { return };
        if old.dirty() && !old.read_only {
            let name = old.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            match old.save() {
                Ok(()) => self.show_toast(format!("Saved {name}"), false),
                Err(e) => self.show_toast(format!("Couldn't save {name}: {e}"), false),
            }
            self.viewer.forget(&old.path);
        }
    }

    /// The text's lines and characters, for the status row and the info card (a text file has no pixels).
    pub(super) fn text_facts(&self) -> Option<(usize, usize)> {
        let tv = self.text_view.as_ref().filter(|_| self.text_shown())?;
        let u = tv.edit.units();
        Some((u.iter().filter(|&&c| c == b'\n' as u16).count() + 1, String::from_utf16_lossy(u).chars().count()))
    }

    pub(super) fn text_shown(&self) -> bool {
        self.text_view.is_some() && self.placeholder.is_none()
    }

    pub(super) fn text_focused(&self) -> bool {
        self.text_view.as_ref().is_some_and(|t| t.focused) && self.text_shown()
    }

    fn text_layout_rects(&self) -> Option<Layout> {
        let a = self.image_area();
        let card = D2D_RECT_F { left: a.left, top: a.top, right: a.right, bottom: a.bottom + self.preview_pad() - 12.0 };
        if card.right - card.left < 120.0 || card.bottom - card.top < 80.0 {
            return None;
        }
        let tv = self.text_view.as_ref()?;
        let digits = (tv.edit.units().iter().filter(|&&u| u == b'\n' as u16).count() + 1).to_string().len().max(3) as f32;
        let gutter_w = digits * tv.size * 0.55 + 20.0;
        let body_bottom = card.bottom - FOOT_H;
        let gutter = rect(card.left, card.top + PAD / 2.0, gutter_w, body_bottom - card.top - PAD / 2.0);
        let text = D2D_RECT_F { left: gutter.right + 8.0, top: card.top + PAD / 2.0, right: card.right - PAD - ui::SCROLL_W, bottom: body_bottom };
        let track = rect(card.right - 4.0 - ui::SCROLL_W, card.top + 6.0, ui::SCROLL_W, body_bottom - card.top - 12.0);
        let foot = D2D_RECT_F { left: card.left, top: body_bottom, right: card.right, bottom: card.bottom };
        Some(Layout { card, gutter, text, track, foot })
    }

    pub(super) fn draw_text_view(&mut self, g: &Gfx) {
        if !self.text_shown() {
            return;
        }
        let Some(lr) = self.text_layout_rects() else { return };
        let t = theme::t();
        let view_h = lr.text.bottom - lr.text.top;
        let focused = self.text_focused();
        let Some(tv) = self.text_view.as_mut() else { return };
        let Some(layout) = tv.layout(g, lr.text.right - lr.text.left) else { return };
        let Some(fmt) = tv.format(g) else { return };
        let total_h = content_height(&layout) + PAD;
        let lines = line_metrics(&layout);
        // The caret into view after a key or an edit.
        if tv.reveal {
            tv.reveal = false;
            let (_, top, h) = caret_at(&layout, tv.edit.caret());
            if top < tv.scroll {
                tv.scroll = top;
            } else if top + h > tv.scroll + view_h {
                tv.scroll = top + h - view_h;
            }
        }
        let max_scroll = (total_h - view_h).max(0.0);
        tv.scroll = tv.scroll.clamp(0.0, max_scroll);
        let scroll = tv.scroll;

        // The card, as the explorer's and the info card's.
        g.fill_round(lr.card, 8.0, rgb(t.sidebar));
        ui::stroke(g, lr.card, 8.0, rgb(t.divider));
        self.hits.add(Hit::TextView, D2D_RECT_F { left: lr.card.left, right: lr.track.left, top: lr.card.top, bottom: lr.foot.top });

        let tv = self.text_view.as_mut().unwrap();
        let origin_y = lr.text.top - scroll;
        g.push_clip(D2D_RECT_F { left: lr.card.left, right: lr.card.right, top: lr.card.top + 1.0, bottom: lr.foot.top });
        // The selection, then the text over it.
        let (a, b) = tv.edit.selection();
        if a < b {
            for m in range_rects(&layout, a, b) {
                let r = rect(lr.text.left + m.left, origin_y + m.top, m.width.max(4.0), m.height);
                if r.bottom >= lr.text.top && r.top <= lr.text.bottom {
                    g.fill(r, gfx::rgba(t.accent, if focused { 0.35 } else { 0.2 }));
                }
            }
        }
        g.draw_layout(&layout, lr.text.left, origin_y, rgb(t.text));
        // The line numbers: one per line of the file, on the first row it wraps into; the caret's line brighter.
        let caret_line = tv.edit.units()[..tv.edit.caret()].iter().filter(|&&u| u == b'\n' as u16).count();
        let (mut y, mut number, mut fresh) = (origin_y, 0usize, true);
        for m in &lines {
            if fresh && y + m.height >= lr.text.top && y <= lr.text.bottom {
                let r = rect(lr.gutter.left, y, lr.gutter.right - lr.gutter.left - 10.0, m.height);
                let c = if number == caret_line { t.label } else { t.placeholder };
                g.text_tabular(&wide(&(number + 1).to_string()), &fmt, r, rgb(c), Align::Right);
            }
            fresh = m.newlineLength > 0;
            if fresh {
                number += 1;
            }
            y += m.height;
            if y > lr.text.bottom {
                break;
            }
        }
        if focused {
            let (x, top, h) = caret_at(&layout, tv.edit.caret());
            g.fill(rect(g.snap(lr.text.left + x), origin_y + top, 1.5, h), rgb(t.text));
        }
        g.pop_clip();

        // The scrollbar, when it overflows.
        if max_scroll > 0.0 {
            let track_h = lr.track.bottom - lr.track.top;
            let thumb_h = (track_h * view_h / total_h).max(28.0);
            let ty = lr.track.top + (track_h - thumb_h) * scroll / max_scroll;
            self.hits.add(Hit::TextScroll, D2D_RECT_F { left: lr.track.left - 2.0, right: lr.track.right + 2.0, ..lr.track });
            let held = self.text_view.as_ref().is_some_and(|t| t.thumb_grab.is_some());
            ui::scroll_thumb(g, rect(lr.track.left, ty, ui::SCROLL_W, thumb_h), held);
        }

        // The foot: where the caret is, the encoding and line endings, and whether it's edited.
        let tv = self.text_view.as_ref().unwrap();
        g.hline(lr.foot.left + 1.0, lr.foot.top, lr.foot.right - lr.foot.left - 2.0, rgb(t.divider));
        let col = tv.edit.caret() - tv.edit.units()[..tv.edit.caret()].iter().rposition(|&u| u == b'\n' as u16).map_or(0, |i| i + 1);
        let left = format!("Ln {}, Col {}", caret_line + 1, col + 1);
        let fr = D2D_RECT_F { left: lr.foot.left + 12.0, right: lr.foot.right - 12.0, ..lr.foot };
        g.text_tabular(&wide(&left), &g.fonts.caption, fr, rgb(t.label), Align::Left);
        let mut right = vec![tv.encoding.label().to_string(), if tv.crlf { "CRLF".into() } else { "LF".into() }, format!("{}%", (tv.size / SIZE_DEFAULT * 100.0).round())];
        if let Some(e) = &tv.error {
            right.insert(0, e.clone());
        } else if tv.read_only {
            right.insert(0, "Read only: too large to edit".into());
        } else if tv.dirty() {
            right.insert(0, "Edited \u{b7} Ctrl+S to save".into());
        }
        g.text(&wide(&right.join("  \u{b7}  ")), &g.fonts.caption, fr, rgb(t.label), Align::Right);
    }

    /// The text layout and its origin on screen, for hit tests.
    fn text_hit_layout(&mut self) -> Option<(IDWriteTextLayout, f32, f32)> {
        let lr = self.text_layout_rects()?;
        let g = self.gfx.take()?;
        let l = self.text_view.as_mut().and_then(|t| {
            let scroll = t.scroll;
            t.layout(&g, lr.text.right - lr.text.left).map(|l| (l, lr.text.left, lr.text.top - scroll))
        });
        self.gfx = Some(g);
        l
    }

    /// A press in the text: the caret there and the keyboard to the view (Shift extends; a double-click picks
    /// the word, a triple the line).
    pub(super) fn text_press(&mut self, x: f32, y: f32, clicks: u8) {
        let shift = unsafe { GetKeyState(VK_SHIFT.0 as i32) < 0 };
        let Some((l, ox, oy)) = self.text_hit_layout() else { return };
        let pos = position_at(&l, x - ox, y - oy);
        self.explorer.focused = false;
        let Some(tv) = self.text_view.as_mut() else { return };
        tv.focused = true;
        tv.goal_x = None;
        match clicks {
            2 => {
                let (a, b) = tv.edit.word_at(pos);
                tv.edit.select(a, b);
            }
            3.. => {
                let u = tv.edit.units();
                let a = u[..pos].iter().rposition(|&c| c == b'\n' as u16).map_or(0, |i| i + 1);
                let b = u[pos..].iter().position(|&c| c == b'\n' as u16).map_or(u.len(), |i| pos + i + 1);
                tv.edit.select(a, b);
            }
            _ => {
                let anchor = if shift { tv.edit.anchor() } else { pos };
                tv.edit.select(anchor, pos);
            }
        }
        self.invalidate();
    }

    /// Dragging from a press: the selection follows (the view scrolls past the edges).
    pub(super) fn text_drag(&mut self, x: f32, y: f32) {
        let Some(lr) = self.text_layout_rects() else { return };
        if let Some(tv) = self.text_view.as_mut() {
            if y < lr.text.top {
                tv.scroll -= 12.0;
            } else if y > lr.text.bottom {
                tv.scroll += 12.0;
            }
        }
        let Some((l, ox, oy)) = self.text_hit_layout() else { return };
        let pos = position_at(&l, x - ox, y - oy);
        if let Some(tv) = self.text_view.as_mut() {
            let anchor = tv.edit.anchor();
            tv.edit.select(anchor, pos);
        }
        self.invalidate();
    }

    pub(super) fn text_scroll_press(&mut self, y: f32) {
        let Some(lr) = self.text_layout_rects() else { return };
        let Some(tv) = self.text_view.as_mut() else { return };
        tv.thumb_grab = Some(y);
        let _ = lr;
        self.text_scroll_drag(y);
    }

    pub(super) fn text_scroll_drag(&mut self, y: f32) {
        let Some(lr) = self.text_layout_rects() else { return };
        let Some((l, _, _)) = self.text_hit_layout() else { return };
        let view_h = lr.text.bottom - lr.text.top;
        let total_h = content_height(&l) + PAD;
        let track_h = lr.track.bottom - lr.track.top;
        let thumb_h = (track_h * view_h / total_h).max(28.0);
        if let Some(tv) = self.text_view.as_mut() {
            let f = ((y - lr.track.top - thumb_h / 2.0) / (track_h - thumb_h).max(1.0)).clamp(0.0, 1.0);
            tv.scroll = f * (total_h - view_h).max(0.0);
        }
        self.invalidate();
    }

    pub(super) fn text_release(&mut self) {
        if let Some(tv) = self.text_view.as_mut() {
            tv.thumb_grab = None;
        }
    }

    /// The wheel over the text scrolls it; with Ctrl it sizes the text.
    pub(super) fn text_wheel(&mut self, delta: f32, ctrl: bool) {
        let Some(tv) = self.text_view.as_mut() else { return };
        if ctrl {
            tv.size = (tv.size * 1.1f32.powf(delta / 120.0)).clamp(SIZE_MIN, SIZE_MAX).round();
        } else {
            tv.scroll -= delta / 120.0 * tv.size * 1.35 * 3.0;
        }
        self.invalidate();
    }

    /// A press anywhere else hands the keyboard back.
    pub(super) fn text_blur(&mut self) {
        if let Some(tv) = self.text_view.as_mut().filter(|t| t.focused) {
            tv.focused = false;
            self.invalidate();
        }
    }

    /// A character from WM_CHAR while the view has the keyboard.
    pub(super) fn text_char(&mut self, u: u16) {
        let Some(tv) = self.text_view.as_mut() else { return };
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
        self.invalidate();
    }

    /// A key while the view has the keyboard. It keeps them all (a letter's character follows as WM_CHAR) but
    /// Escape, which hands the keyboard back, and the function keys.
    pub(super) fn text_key(&mut self, vk: VIRTUAL_KEY, shift: bool, ctrl: bool) -> bool {
        if (VK_F1.0..=VK_F24.0).contains(&vk.0) {
            return false;
        }
        if vk == VK_ESCAPE {
            self.text_blur();
            return true;
        }
        let hwnd = self.hwnd;
        let page = self.text_layout_rects().map_or(400.0, |l| l.text.bottom - l.text.top);
        let hit = self.text_hit_layout();
        let Some(tv) = self.text_view.as_mut() else { return false };
        let ro = tv.read_only;
        let mut edited = false;
        let mut moved = true;
        // Up, Down, Page Up and Page Down: by the layout, keeping the column.
        let vertical = |tv: &mut TextView, dy_lines: f32, by_page: bool| {
            let Some((l, _, _)) = &hit else { return };
            let (x, top, h) = caret_at(l, tv.edit.caret());
            let gx = *tv.goal_x.get_or_insert(x);
            let y = if by_page { top + h / 2.0 + dy_lines.signum() * page } else { top + h / 2.0 + dy_lines * h };
            let pos = position_at(l, gx, y.max(0.0));
            let anchor = if shift { tv.edit.anchor() } else { pos };
            tv.edit.select(anchor, pos);
            if by_page {
                tv.scroll += dy_lines.signum() * page;
            }
        };
        match vk {
            VK_LEFT => tv.edit.move_caret(if ctrl { Move::WordLeft } else { Move::Left }, shift),
            VK_RIGHT => tv.edit.move_caret(if ctrl { Move::WordRight } else { Move::Right }, shift),
            VK_UP => vertical(tv, -1.0, false),
            VK_DOWN => vertical(tv, 1.0, false),
            VK_PRIOR => vertical(tv, -1.0, true),
            VK_NEXT => vertical(tv, 1.0, true),
            VK_HOME | VK_END if ctrl => tv.edit.move_caret(if vk == VK_HOME { Move::Home } else { Move::End }, shift),
            VK_HOME | VK_END => {
                if let Some((l, _, _)) = &hit {
                    let (a, b) = line_of(&line_metrics(l), tv.edit.caret());
                    let to = if vk == VK_HOME {
                        // Home goes to the first non-blank first, then to the line's start (as editors do).
                        let first = a + tv.edit.units()[a..b].iter().take_while(|&&u| u == b' ' as u16 || u == b'\t' as u16).count();
                        if tv.edit.caret() == first { a } else { first }
                    } else {
                        b
                    };
                    let anchor = if shift { tv.edit.anchor() } else { to };
                    tv.edit.select(anchor, to);
                }
            }
            VK_BACK if !ro => {
                tv.edit.backspace(ctrl);
                edited = true;
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
            VK_Z if ctrl && !ro => edited = tv.edit.undo(),
            VK_Y if ctrl && !ro => edited = tv.edit.redo(),
            VK_S if ctrl => {
                let name = tv.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                let path = tv.path.clone();
                let r = if ro { Err("it's read only".to_string()) } else { tv.save() };
                match r {
                    Ok(()) => {
                        self.viewer.forget(&path);
                        self.show_toast(format!("Saved {name}"), true);
                    }
                    Err(e) => self.show_toast(format!("Couldn't save {name}: {e}"), false),
                }
                self.invalidate();
                return true;
            }
            _ => moved = false,
        }
        let Some(tv) = self.text_view.as_mut() else { return true };
        if edited {
            tv.changed();
        } else if moved {
            if !matches!(vk, VK_UP | VK_DOWN | VK_PRIOR | VK_NEXT) {
                tv.goal_x = None;
            }
            tv.reveal = true;
        }
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
}
