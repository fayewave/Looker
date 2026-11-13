//! The info card (Controls/InfoPanel + MetadataService): a floating card on the right of the viewport with
//! the histogram and the File / Camera / Exposure / Date / Location groups. The image fits in the space to
//! its left (`right_inset`), the checkerboard runs underneath it, and a grip on its left border resizes
//! it (260–640 DIPs, never leaving less than 480 for the image; double-click resets to 320).
//!
//! The histogram comes with every decode (the pool computes it), so it is there with the first frame. EXIF is
//! read on a worker thread once per file while the card is open.

use std::time::Instant;

use super::*;
use crate::metadata::{self, Exif, ExifTime};
use crate::settings::{INFO_MAX, INFO_MIN, INFO_WIDTH};

pub(super) const WM_EXIF: u32 = WM_APP + 5;
const MARGIN: f32 = 12.0;
const PAD_X: f32 = 12.0;
const PAD_TOP: f32 = 12.0;
const PAD_BOTTOM: f32 = 16.0;
const GROUP_GAP: f32 = 14.0;
/// Between one property (its name over its value) and the next.
const ROW_GAP: f32 = 8.0;
const TITLE_H: f32 = 16.0;
/// The footer that holds still under the rows while there are unsaved edits.
const FOOTER_H: f32 = 46.0;
const SCROLL_MS: f32 = 150.0;
/// Pixels scrolled per wheel notch.
const SCROLL_STEP: f32 = 48.0;
/// The image keeps at least this much width when the card is dragged wider.
const MIN_IMAGE_W: f32 = 480.0;

pub(super) struct ExifResult {
    pub path: PathBuf,
    pub stamp: u64,
    pub exif: Option<Exif>,
}

pub(super) struct Card {
    /// The EXIF of this file (the inner None: it has none).
    exif: Option<(PathBuf, u64, Option<Exif>)>,
    loading: Option<(PathBuf, u64)>,
    scroll: f32,
    anim: Option<(f32, f32, Instant)>,
    max_scroll: f32,
    /// A grip drag: the pointer x and the width when it started.
    pub drag: Option<(f32, f32)>,
    /// Its width easing back to the default (its edge double-clicked).
    pub settle: Option<crate::motion::Settle>,
    /// Pulled past its narrowest or widest: how far it gives (the explorer edge's rubber band, + wider), and the
    /// bounce back once let go (from how far, since when).
    stretch: f32,
    spring: Option<(f32, Instant)>,
    /// The file it last showed: another one starts back at the top (Transient's).
    shown: Option<PathBuf>,
    /// Several files selected in the explorer: their summary (info_many.rs).
    pub(super) many: super::info_many::ManyState,
    /// Edits waiting for Ctrl+S, and the field being typed (info_edit.rs).
    pub(super) edits: super::info_edit::Edits,
}

impl Card {
    pub fn new() -> Card {
        Card { exif: None, loading: None, scroll: 0.0, anim: None, max_scroll: 0.0, drag: None, settle: None, stretch: 0.0, spring: None, shown: None, many: Default::default(), edits: Default::default() }
    }

    /// The scroll offset to draw this frame, and whether it is still moving.
    fn scroll_now(&mut self) -> (f32, bool) {
        let Some((from, to, t0)) = self.anim else { return (self.scroll, false) };
        let t = (t0.elapsed().as_secs_f32() * 1000.0 / SCROLL_MS).min(1.0);
        let k = 1.0 - (1.0 - t).powi(3);
        if t >= 1.0 {
            self.anim = None;
            self.scroll = to;
            return (to, false);
        }
        (from + (to - from) * k, true)
    }

    fn target(&self) -> f32 {
        self.anim.map_or(self.scroll, |(_, to, _)| to)
    }
}

#[derive(Clone)]
pub(super) enum Block {
    /// The file's name over everything, or "12 files selected" (a click copies it, or their names: the second).
    Heading(String, Option<String>),
    Histogram,
    Title(&'static str),
    /// A property. `copy`: what a click copies when it isn't the value shown (a selection's mixed values);
    /// `dim`: the value in the label colour ("Mixed values").
    Row { label: &'static str, value: String, path: bool, copy: Option<String>, dim: bool },
    /// "+ Add a field", at the end of the Description group.
    Add,
}

/// The Location group's rows: right-click offers the map on any of them.
pub(super) const LOCATION_ROWS: [&str; 7] = ["Place", "City", "State", "Country", "Latitude", "Longitude", "Altitude"];

/// 1,234,567.
pub(super) fn grouped(v: u64) -> String {
    let s = v.to_string();
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// A size as Explorer's Properties gives it: "2.4 MB (2,456,789 bytes)".
pub(super) fn size_text(b: u64) -> String {
    if b < 1024 { format_bytes(b) } else { format!("{} ({} bytes)", format_bytes(b), grouped(b)) }
}

fn ft_set(t: FILETIME) -> bool {
    t.dwLowDateTime != 0 || t.dwHighDateTime != 0
}

/// An EXIF wall-clock time in the user's short date and time format.
pub(super) fn format_exif_time(t: ExifTime) -> String {
    let st = SYSTEMTIME { wYear: t.year, wMonth: t.month, wDay: t.day, wHour: t.hour, wMinute: t.minute, wSecond: t.second, ..Default::default() };
    format_local(&st)
}

impl App {
    pub(super) fn info_shown(&self) -> bool {
        self.settings.info_visible && self.panels_allowed()
    }

    /// What the card takes from the right of the viewport (its width, margins included).
    pub(super) fn right_inset(&self) -> f32 {
        self.info_w() * self.slides.info.value()
    }

    /// The card's width, easing back to its default after a double-click on its edge, stretched or bouncing
    /// past its narrowest or widest.
    pub(super) fn info_w(&self) -> f32 {
        let w = self.settings.info_width + self.info_stretch();
        match self.info_card.settle {
            Some(s) if s.moving() => s.value(w),
            _ => w,
        }
    }

    fn info_stretch(&self) -> f32 {
        let c = &self.info_card;
        if c.stretch != 0.0 {
            return c.stretch;
        }
        c.spring.map_or(0.0, |(from, at)| super::explorer::bounce(from, at))
    }

    /// Still easing back to its default or bouncing (the layout follows each frame); once more as it lands.
    pub(super) fn info_settling(&mut self) -> bool {
        let springing = self.info_card.spring.is_some_and(|(_, at)| at.elapsed().as_secs_f32() < super::explorer::SPRING_SECS);
        let sprung = !springing && self.info_card.spring.take().is_some();
        let moving = self.info_card.settle.is_some_and(|s| s.moving());
        if !moving && self.info_card.settle.take().is_some() {
            return true;
        }
        moving || springing || sprung
    }

    /// Slid out to the right by however much of it is hidden.
    fn info_card_rect(&self) -> D2D_RECT_F {
        let v = self.viewport();
        let w = self.info_w();
        let dx = w * (1.0 - self.slides.info.value());
        // Stretched or bouncing at a limit, its far side goes with it a third as much, as the explorer's does.
        let pull = self.info_stretch() * 0.33;
        D2D_RECT_F { left: v.right - w + MARGIN + dx, top: v.top + MARGIN, right: v.right - MARGIN + dx - pull, bottom: v.bottom - MARGIN }
    }

    pub(super) fn toggle_info(&mut self) {
        if self.viewer.current.is_none() {
            return;
        }
        self.settings.info_visible = !self.settings.info_visible;
        settings::save(&self.settings);
        self.slide_panels();
    }

    /// The space for the image changed (window size, a card shown, hidden or resized): refit and decode for it.
    pub(super) fn layout_changed(&mut self) {
        let (vw, vh) = self.view_size();
        self.view.set_viewport(vw, vh, self.scale() as f64);
        let (bw, bh) = self.fit_box();
        self.viewer.set_fit_box(bw, bh);
        self.schedule_upgrade();
        self.invalidate();
    }

    /// The shown file's metadata is read again (its words were just saved into it).
    pub(super) fn forget_exif(&mut self) {
        self.info_card.exif = None;
        self.info_card.loading = None;
        self.invalidate();
    }

    pub(super) fn on_exif(&mut self, r: ExifResult) {
        if self.info_card.loading.as_ref().is_some_and(|(p, s)| *p == r.path && *s == r.stamp) {
            self.info_card.loading = None;
        }
        self.info_card.exif = Some((r.path, r.stamp, r.exif));
        self.invalidate();
    }

    /// The current file's EXIF, once read (None: not yet, or it has none).
    pub(super) fn current_exif(&self) -> Option<&Exif> {
        let (path, stamp) = self.viewer.current.as_ref()?;
        match &self.info_card.exif {
            Some((p, s, Some(e))) if p == path && s == stamp => Some(e),
            _ => None,
        }
    }

    /// Reads the current file's EXIF in the background, once per file.
    pub(super) fn request_exif(&mut self) {
        let Some((path, stamp)) = self.viewer.current.clone() else { return };
        let have = |x: &Option<(PathBuf, u64)>| x.as_ref().is_some_and(|(p, s)| *p == path && *s == stamp);
        if have(&self.info_card.exif.as_ref().map(|(p, s, _)| (p.clone(), *s))) || have(&self.info_card.loading) {
            return;
        }
        self.info_card.loading = Some((path.clone(), stamp));
        let hwnd = self.hwnd.0 as isize;
        std::thread::Builder::new()
            .name("exif".into())
            .spawn(move || {
                let exif = metadata::read(&path);
                let ptr = Box::into_raw(Box::new(ExifResult { path, stamp, exif }));
                unsafe {
                    if PostMessageW(Some(HWND(hwnd as _)), WM_EXIF, WPARAM(0), LPARAM(ptr as isize)).is_err() {
                        drop(Box::from_raw(ptr));
                    }
                }
            })
            .ok();
    }

    /// The wheel over the card scrolls it (eased), never the image under it.
    pub(super) fn scroll_info(&mut self, delta: f32) {
        let c = &mut self.info_card;
        let (now, _) = c.scroll_now();
        let to = (c.target() - delta / 120.0 * SCROLL_STEP).clamp(0.0, c.max_scroll.max(0.0));
        c.scroll = now;
        c.anim = Some((now, to, Instant::now()));
        self.invalidate();
    }

    // --- Resize grip --------------------------------------------------------------------------------

    pub(super) fn info_grip_press(&mut self, x: f32) {
        self.info_card.spring = None;
        self.info_card.stretch = 0.0;
        self.info_card.drag = Some((x, self.settings.info_width));
    }

    pub(super) fn info_grip_drag(&mut self, x: f32) {
        let Some((x0, w0)) = self.info_card.drag else { return };
        let (ww, _) = self.size_dip();
        let cap = INFO_MAX.min((ww - MIN_IMAGE_W).max(INFO_MIN));
        // Dragging left makes the card wider.
        let raw = (w0 + (x0 - x)).round();
        let w = raw.clamp(INFO_MIN, cap);
        // Pulled on past either end, it gives a little on the explorer edge's rubber band (Faye, 2026-11-12).
        let stretch = if raw > cap {
            super::explorer::rubber(raw - cap)
        } else if raw < INFO_MIN {
            -super::explorer::rubber(INFO_MIN - raw)
        } else {
            0.0
        };
        if w != self.settings.info_width || stretch != self.info_card.stretch {
            self.settings.info_width = w;
            self.info_card.stretch = stretch;
            self.layout_changed();
        }
    }

    pub(super) fn info_grip_release(&mut self) {
        // Let go stretched: it bounces back.
        if self.info_card.stretch != 0.0 {
            self.info_card.spring = Some((self.info_card.stretch, Instant::now()));
            self.info_card.stretch = 0.0;
            self.layout_changed();
        }
        if self.info_card.drag.take().is_some() {
            settings::save(&self.settings);
            self.invalidate();
        }
    }

    pub(super) fn info_grip_reset(&mut self) {
        self.info_card.drag = None;
        // It eases from where it is to its default (Faye, 2026-11-10).
        self.info_card.settle = Some(crate::motion::Settle::from(self.info_w()));
        self.settings.info_width = INFO_WIDTH;
        settings::save(&self.settings);
        self.layout_changed();
    }

    // --- Content ------------------------------------------------------------------------------------

    /// The property at `i` in `info_blocks`: its name, value and whether it's the file's path (the heading's
    /// name is "Name").
    pub(super) fn info_property(&self, i: usize) -> Option<(&'static str, String, bool)> {
        match self.info_blocks().into_iter().nth(i)? {
            Block::Heading(name, None) => Some(("Name", name, false)),
            Block::Heading(_, Some(names)) => Some(("File names", names, false)),
            Block::Row { label, value, path, copy, .. } => Some((label, copy.unwrap_or(value), path)),
            _ => None,
        }
    }

    /// Where the path is in `info_blocks`.
    pub(super) fn info_path_row(&self) -> Option<usize> {
        self.info_blocks().iter().position(|b| matches!(b, Block::Row { path: true, .. }))
    }

    /// A click on a property: its value to the clipboard.
    pub(super) fn info_copy(&mut self, i: usize) {
        let Some((label, value, _)) = self.info_property(i) else { return };
        if clipboard::set_text(self.hwnd, &value) {
            // The property's own name ("Copied Modified", "Copied ISO"): "the modified" read badly.
            self.show_toast(format!("Copied {label}"), false);
        }
    }

    /// Every property, a group to a paragraph ("Camera", then "Model: X100V" lines), the name first.
    fn info_text(&self) -> String {
        let mut out = String::new();
        for b in self.info_blocks() {
            match b {
                Block::Heading(name, _) => out.push_str(&name),
                Block::Histogram | Block::Add => {}
                Block::Title(t) => {
                    out.push_str("\r\n\r\n");
                    out.push_str(t);
                }
                Block::Row { label, value, .. } => out.push_str(&format!("\r\n{label}: {value}")),
            }
        }
        out
    }

    pub(super) fn info_action(&mut self, action: super::menus::Action, i: Option<usize>) {
        use super::menus::Action;
        match action {
            Action::InfoCopy => {
                if let Some(i) = i {
                    self.info_copy(i);
                }
            }
            Action::InfoCopyAll => {
                let text = self.info_text();
                if clipboard::set_text(self.hwnd, &text) {
                    self.show_toast("Copied all details", false);
                }
            }
            Action::Reveal => self.reveal(),
            Action::InfoEditField(f) => self.info_start_edit(f),
            Action::InfoRemoveField(f) => self.info_remove_field(f),
            Action::InfoCopyCoordinates => {
                if let Some((lat, lon)) = self.photo_coordinates() {
                    if clipboard::set_text(self.hwnd, &format!("{lat:.6}, {lon:.6}")) {
                        self.show_toast("Copied coordinates", false);
                    }
                }
            }
            Action::InfoMap => {
                // Only when asked: the coordinates go to the map site in the address.
                if let Some((lat, lon)) = self.photo_coordinates() {
                    super::page::shell_open(&format!("https://www.google.com/maps/search/?api=1&query={lat:.6},{lon:.6}"));
                }
            }
            _ => {}
        }
    }

    /// The current photo's GPS fix, once its EXIF is read.
    pub(super) fn photo_coordinates(&self) -> Option<(f64, f64)> {
        let e = self.current_exif()?;
        Some((e.latitude?, e.longitude?))
    }

    pub(super) fn info_blocks(&self) -> Vec<Block> {
        let mut out = Vec::new();
        if let Some(mut blocks) = self.many_blocks() {
            self.apply_info_edits(&mut blocks);
            return blocks;
        }
        let row = |label, value: String| Block::Row { label, value, path: false, copy: None, dim: false };
        // A file Looker can't open covers the photo: what Explorer would say about it.
        if let Some(p) = &self.placeholder {
            let ext = p.path.extension().map(|e| e.to_string_lossy().to_uppercase()).unwrap_or_default();
            out.push(Block::Heading(file_name(&p.path), None));
            out.push(Block::Title(if p.folder { "Folder" } else { "File" }));
            out.push(row("Type", if p.folder { "Folder".to_string() } else if ext.is_empty() { "File".to_string() } else { format!("{ext} file") }));
            if let Some(s) = p.size {
                out.push(row("Size", size_text(s)));
            }
            if let Some(t) = p.modified {
                out.push(row("Modified", format_time(t)));
            }
            out.push(Block::Row { label: "File Path", value: p.path.to_string_lossy().into_owned(), path: true, copy: None, dim: false });
            return out;
        }
        let Some(path) = self.current_path() else { return out };
        let entry = self.viewer.current_entry();
        let model = entry.is_some_and(|e| e.format == format::Format::Model);
        let video = entry.is_some_and(|e| matches!(e.format, format::Format::Video | format::Format::Audio));
        let audio = entry.is_some_and(|e| e.format == format::Format::Audio);
        let text = entry.is_some_and(|e| e.format == format::Format::Text);
        // A model's histogram would be of its render, not of anything in the file; a video's of its first picture
        // only (hidden: Faye, 2026-11-10).
        out.push(Block::Heading(file_name(path), None));
        if entry.is_some_and(|e| e.histogram.is_some()) && !model && !video && !text {
            out.push(Block::Histogram);
        }
        out.push(Block::Title("File"));
        if let Some(e) = entry {
            if let Some(t) = format::display_name(e.format, Some(path)) {
                out.push(row("Type", t));
            }
            if model {
                for (label, value) in self.model_facts() {
                    out.push(row(label, value));
                }
            } else if text {
                if let Some((lines, chars)) = self.text_facts() {
                    out.push(row("Lines", lines.to_string()));
                    out.push(row("Characters", chars.to_string()));
                }
            } else if e.format == format::Format::Archive {
                // An archive: what's in it (its thumbnail's size is a picture inside, not the archive's).
                if let Some((files, bytes)) = self.archive_facts() {
                    out.push(row("Files", files.to_string()));
                    out.push(row("Unpacked", format_bytes(bytes)));
                }
            } else if !audio {
                let (w, h) = self.current_dims().unwrap_or((e.native_w, e.native_h));
                out.push(row("Dimensions", format!("{w} × {h}")));
            }
            if e.pages == 0 && !model && !video && !text && e.format != format::Format::Archive {
                out.push(row("Megapixels", format!("{:.1} MP", e.native_w as f64 * e.native_h as f64 / 1_000_000.0)));
            }
        }
        if let Some(i) = &self.info {
            if i.size > 0 {
                out.push(row("Size", size_text(i.size)));
            }
            out.push(row("Modified", format_time(i.modified)));
            if ft_set(i.created) {
                out.push(row("Created", format_time(i.created)));
            }
            if ft_set(i.accessed) {
                out.push(row("Accessed", format_time(i.accessed)));
            }
        }
        out.push(Block::Row { label: "File Path", value: path.to_string_lossy().into_owned(), path: true, copy: None, dim: false });
        let stamp = self.viewer.current.as_ref().map(|(_, s)| *s);
        if let Some((p, s, Some(exif))) = &self.info_card.exif {
            if p == path && Some(*s) == stamp {
                for g in metadata::groups(exif, format_exif_time) {
                    out.push(Block::Title(g.title));
                    for (label, value) in g.rows {
                        out.push(row(label, value));
                    }
                }
            }
        }
        self.apply_info_edits(&mut out);
        out
    }

    /// Draws the card and records its hits. Returns whether it is still animating (a scroll).
    pub(super) fn draw_info(&mut self, g: &Gfx) -> bool {
        if self.slides.info.value() <= 0.0 {
            return false;
        }
        self.request_exif();
        self.sync_many();
        // An edit belongs to its photo (or selection): moving on leaves it.
        if self.info_card.edits.editing.as_ref().is_some_and(|e| self.info_targets() != e.paths) {
            self.info_cancel();
        }
        // Another file (or selection): back to the top at once, no animation (Transient's).
        let showing = match self.info_card.many.key() {
            Some(k) => Some(PathBuf::from(format!("::selection:{k:x}"))),
            None => self.placeholder.as_ref().map(|p| p.path.clone()).or_else(|| self.current_path().map(Path::to_path_buf)),
        };
        if showing != self.info_card.shown {
            self.info_card.shown = showing;
            self.info_card.scroll = 0.0;
            self.info_card.anim = None;
        }
        let card = self.info_card_rect();
        if card.bottom - card.top < 40.0 {
            return false;
        }
        self.hits.add(Hit::InfoCard, card);
        ui::card(g, card);
        let th = theme::t();

        let inner = D2D_RECT_F { left: card.left + 1.0, top: card.top + 1.0, right: card.right - 1.0, bottom: card.bottom - 1.0 };
        // Unsaved edits (on any photo): a footer under the rows that doesn't scroll, with Save and Discard all.
        let unsaved: usize = self.info_card.edits.pending.values().map(|m| m.len()).sum();
        let footer = (unsaved > 0 || self.info_card.edits.saving).then(|| D2D_RECT_F { top: inner.bottom - FOOTER_H, ..inner });
        let inner = match footer {
            Some(f) => D2D_RECT_F { bottom: f.top, ..inner },
            None => inner,
        };
        // The fields with unsaved edits in the photo (or selection) shown: their rows are tinted.
        let changed = self.info_changed_fields();
        let x = inner.left + PAD_X;
        // 2 more on the right: clear of the scrollbar now it's always 8 wide.
        let w = (inner.right - PAD_X - 2.0 - x).max(1.0);
        let view_h = (inner.bottom - inner.top - PAD_TOP - PAD_BOTTOM).max(1.0);

        // Measure first: the content height decides how far the card scrolls.
        let blocks = self.info_blocks();
        let fields = self.info_fields(&blocks);
        let editing = self.info_card.edits.editing.as_ref().map(|e| e.field);
        let hist_h = (w - 14.0) * 120.0 / 256.0 + 14.0;
        // A property is its name on one line and its value (tabular figures) under it, both full width; the
        // name's height is kept to place the value.
        let mut heights = Vec::with_capacity(blocks.len());
        let mut label_hs = Vec::with_capacity(blocks.len());
        for (bi, b) in blocks.iter().enumerate() {
            let being_edited = editing.is_some() && fields[bi] == editing;
            let (h, lh) = match b {
                Block::Heading(name, _) => (g.measure_height(&wide(name), &g.fonts.heading_wrap, w).max(19.0).ceil(), 0.0),
                Block::Histogram => (hist_h, 0.0),
                Block::Title(_) => (TITLE_H + 2.0, 0.0),
                Block::Add => (26.0, 0.0),
                Block::Row { label, value, .. } => {
                    let lh = g.measure_height(&wide(label), &g.fonts.caption_wrap, w).max(16.0).ceil();
                    // Being typed into: a text box under its name (the rating: a row of stars).
                    if being_edited {
                        let bh = if editing == Some(crate::metawrite::Field::Rating) { 24.0 } else { ui::FIELD_H };
                        (lh + bh + 4.0, lh)
                    } else {
                        let vh = g.measure_height_tabular(&wide(value), &g.fonts.caption_wrap, w).max(16.0).ceil();
                        (lh + vh + 2.0, lh)
                    }
                }
            };
            heights.push(h);
            label_hs.push(lh);
        }
        let mut content_h = 0.0;
        for (i, (b, h)) in blocks.iter().zip(&heights).enumerate() {
            if i > 0 {
                content_h += if matches!(b, Block::Title(_)) { GROUP_GAP } else { ROW_GAP };
            }
            content_h += h;
        }
        self.info_card.max_scroll = (content_h - view_h).max(0.0);
        if self.info_card.scroll > self.info_card.max_scroll {
            self.info_card.scroll = self.info_card.max_scroll;
            self.info_card.anim = None;
        }
        let (scroll, moving) = self.info_card.scroll_now();
        let scroll = scroll.clamp(0.0, self.info_card.max_scroll);

        g.push_clip(inner);
        let mut y = inner.top + PAD_TOP - scroll;
        for (i, (b, h)) in blocks.iter().zip(&heights).enumerate() {
            if i > 0 {
                y += if matches!(b, Block::Title(_)) { GROUP_GAP } else { ROW_GAP };
            }
            let lh = label_hs[i];
            if y + h >= inner.top && y <= inner.bottom {
                // A property (and the name) is a whole-row target with Transient's rounded wash under the pointer,
                // fading in and out, a touch less while pressed. A click copies it; the path's shows the file.
                let being_edited = editing.is_some() && fields[i] == editing;
                let hit = match b {
                    _ if being_edited => None,
                    Block::Row { path: true, .. } => Some(Hit::InfoPath),
                    Block::Row { .. } | Block::Heading(..) => Some(Hit::InfoRow(i)),
                    _ => None,
                };
                // An unsaved edit: the accent's tint under the row, so what Ctrl+S will write is plain to see (Faye,
                // 2026-11-11; no bar down its left).
                if fields[i].is_some_and(|f| changed.contains(&f)) {
                    let rr = D2D_RECT_F { left: x - 6.0, top: y - 3.0, right: x + w + 4.0, bottom: y + h + 3.0 };
                    g.fill_round(rr, 5.0, gfx::rgba(th.accent, 0.12));
                }
                if let Some(hit) = hit {
                    let rr = D2D_RECT_F { left: x - 6.0, top: y - 3.0, right: x + w + 4.0, bottom: y + h + 3.0 };
                    let st = self.state(hit, true);
                    ui::link_fill(g, rr, st.hover, st.pressed);
                    let visible = D2D_RECT_F { top: rr.top.max(inner.top), bottom: rr.bottom.min(inner.bottom), ..rr };
                    if visible.bottom > visible.top {
                        self.hits.add(hit, visible);
                    }
                }
                match b {
                    Block::Heading(name, _) => g.text(&wide(name), &g.fonts.heading_wrap, rect(x, y, w, *h), rgb(th.text), Align::Left),
                    Block::Histogram => self.draw_histogram(g, rect(x, y, w, *h)),
                    Block::Title(t) => {
                        ui::section(g, t, rect(x, y, w, TITLE_H));
                    }
                    Block::Add => {
                        let r = rect(x - 6.0, y, ui::icon_label_width(g, "Add a field") - 8.0, *h);
                        let st = self.state(Hit::InfoAdd, true);
                        ui::icon_label_button(g, r, 0xE710, "Add a field", ui::Kind::Subtle, &st);
                        if r.bottom > inner.top && r.top < inner.bottom {
                            self.hits.add(Hit::InfoAdd, r);
                        }
                    }
                    Block::Row { label, .. } if being_edited => {
                        g.text(&wide(label), &g.fonts.caption_wrap, rect(x, y + 1.0, w, lh), rgb(th.label), Align::Left);
                        if editing == Some(crate::metawrite::Field::Rating) {
                            self.draw_star_picker(g, x, y + lh + 3.0);
                        } else {
                            let fr = rect(x - 6.0, y + lh + 3.0, w + 10.0, ui::FIELD_H);
                            if let Some(e) = self.info_card.edits.editing.as_mut() {
                                self.caret_rect = e.text.draw(g, fr, th, true, 0.0, None);
                            }
                            self.hits.add(Hit::InfoField, fr);
                        }
                    }
                    Block::Row { label, value, dim, .. } => {
                        g.text(&wide(label), &g.fonts.caption_wrap, rect(x, y + 1.0, w, lh), rgb(th.label), Align::Left);
                        let vr = rect(x, y + 1.0 + lh, w, h - 2.0 - lh);
                        g.text_tabular(&wide(value), &g.fonts.caption_wrap, vr, rgb(if *dim { th.label } else { th.text }), Align::Left);
                        // An editable field: a pencil while the row's hovered (or double-click it).
                        if fields[i].is_some() && matches!(self.hover, Some(Hit::InfoRow(k) | Hit::InfoEdit(k)) if k == i) {
                            let pr = rect(x + w - 22.0, y - 2.0, 24.0, 22.0);
                            let st = self.state(Hit::InfoEdit(i), true);
                            let fg = ui::button_frame(g, pr, ui::Kind::Subtle, &st);
                            g.text(&[0xE70F], &g.fonts.small_icons, pr, fg, Align::Center);
                            if pr.top > inner.top && pr.bottom < inner.bottom {
                                self.hits.add(Hit::InfoEdit(i), pr);
                            }
                        }
                    }
                }
                // A faint hairline between the properties of a group and under the name, halfway down the gap
                // (Transient's; Faye, 2026-11-11).
                let next = blocks.get(i + 1);
                let line = match (b, next) {
                    (Block::Row { .. }, Some(Block::Row { .. })) => true,
                    (Block::Heading(..), Some(_)) => true,
                    _ => false,
                };
                if line {
                    let gap = if matches!(next, Some(Block::Title(_))) { GROUP_GAP } else { ROW_GAP };
                    g.hline(x, g.snap(y + h + gap / 2.0), w, gfx::rgba(th.divider, 0.6));
                }
            }
            y += h;
        }
        g.pop_clip();
        if let Some(f) = footer {
            self.draw_info_footer(g, f, unsaved);
        }

        // The scroll thumb while the pointer is over an overflowing card, or while it scrolls.
        let over = matches!(self.hover, Some(Hit::InfoCard | Hit::InfoPath | Hit::InfoRow(_) | Hit::InfoEdit(_) | Hit::InfoField | Hit::InfoStar(_) | Hit::InfoAdd));
        if self.info_card.max_scroll > 0.0 && (over || moving) {
            let thumb_h = (view_h * view_h / content_h).max(28.0);
            let track = inner.bottom - inner.top - 8.0;
            let ty = inner.top + 4.0 + (track - thumb_h) * scroll / self.info_card.max_scroll;
            ui::scroll_thumb(g, rect(inner.right - 2.0 - ui::SCROLL_W, ty, ui::SCROLL_W, thumb_h), 0.0);
        }

        // The resize grip straddles the left border; its line shows on hover and turns accent while dragging.
        self.hits.add(Hit::InfoGrip, ui::card_grip(card.left, card.top, card.bottom, -1.0));
        let dragging = self.info_card.drag.is_some();
        let t = self.fades.hover(Hit::InfoGrip, self.hover == Some(Hit::InfoGrip) || dragging);
        ui::grip_line(g, rect(card.left - 1.0, card.top + 10.0, 2.0, card.bottom - card.top - 20.0), t, dragging);
        moving
    }

    /// The footer: how many edits wait (and in how many files), Discard all and Save.
    fn draw_info_footer(&mut self, g: &Gfx, f: D2D_RECT_F, unsaved: usize) {
        let th = theme::t();
        g.hline(f.left, g.snap(f.top), f.right - f.left, rgb(th.divider));
        let saving = self.info_card.edits.saving;
        let files = self.info_unsaved_files();
        let text = if saving {
            "Saving\u{2026}".to_string()
        } else {
            // Short, beside two buttons in a card that can be 260 wide ("3 files unsaved" didn't fit): files
            // once there are several (a selection's edit is one change in each), else changes.
            format!("{} unsaved", if files > 1 { files } else { unsaved })
        };
        let bh = 28.0;
        let by = f.top + (f.bottom - f.top - bh) / 2.0;
        let save_w = ui::icon_label_width(g, "Save");
        let save = rect(f.right - PAD_X - save_w, by, save_w, bh);
        let discard_w = ui::icon_label_width(g, "Discard");
        let discard = rect(save.left - 8.0 - discard_w, by, discard_w, bh);
        g.text(&wide(&text), &g.fonts.body, rect(f.left + PAD_X, f.top, (discard.left - 8.0 - f.left - PAD_X).max(0.0), f.bottom - f.top), rgb(th.warn), Align::Left);
        let st = self.state(Hit::InfoDiscard, !saving);
        ui::icon_label_button(g, discard, 0xE894, "Discard", ui::Kind::Standard, &st);
        let st = self.state(Hit::InfoSave, !saving);
        ui::icon_label_button(g, save, 0xE74E, "Save", ui::Kind::Accent, &st);
        if !saving {
            self.hits.add(Hit::InfoDiscard, discard);
            self.hits.add(Hit::InfoSave, save);
        }
    }

    /// The rating picker: five stars from `x`, filled up to the hovered one (else the rating as it stands).
    fn draw_star_picker(&mut self, g: &Gfx, x: f32, y: f32) {
        let th = theme::t();
        let now = self.info_rating_now();
        let lit = match self.hover {
            Some(Hit::InfoStar(k)) => k as u32 + 1,
            _ => now,
        };
        for k in 0..5u8 {
            let r = rect(x - 3.0 + k as f32 * 24.0, y, 24.0, 24.0);
            let on = (k as u32) < lit;
            g.text(&wide(if on { "\u{2605}" } else { "\u{2606}" }), &g.fonts.body_large, r, rgb(if on { th.text } else { th.label }), Align::Center);
            self.hits.add(Hit::InfoStar(k), r);
        }
    }

    /// Luma as a filled area, red, green and blue as curves over it, in a small inset card; drawn to the
    /// card's width at the C# panel's 256 × 120 proportions.
    fn draw_histogram(&self, g: &Gfx, r: D2D_RECT_F) {
        let Some(h) = self.viewer.current_entry().and_then(|e| e.histogram.clone()) else { return };
        let t = theme::t();
        g.fill_round(r, 5.0, rgb(t.field));
        ui::stroke(g, r, 5.0, rgb(t.divider));
        let p = D2D_RECT_F { left: r.left + 7.0, top: r.top + 7.0, right: r.right - 7.0, bottom: r.bottom - 7.0 };
        let (pw, ph) = (p.right - p.left, p.bottom - p.top);
        let max = h.max.max(1) as f32;
        let curve = |bins: &[u32; 256]| -> Vec<(f32, f32)> {
            (0..256).map(|i| (p.left + i as f32 / 255.0 * pw, p.bottom - (bins[i] as f32 / max).min(1.0) * ph)).collect()
        };
        let mut area = vec![(p.left, p.bottom)];
        area.extend(curve(&h.luma));
        area.push((p.right, p.bottom));
        g.fill_polygon(&area, gfx::rgba(t.label, 0.3));
        let a = 0xC0 as f32 / 255.0;
        g.polyline(&curve(&h.red), gfx::rgba(0xE0574B, a), 1.0);
        g.polyline(&curve(&h.green), gfx::rgba(0x4CAF50, a), 1.0);
        g.polyline(&curve(&h.blue), gfx::rgba(0x5B9BD5, a), 1.0);
    }
}
