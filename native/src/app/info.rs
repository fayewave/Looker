//! The info card (Controls/InfoPanel + MetadataService): a floating card on the right of the viewport with
//! the histogram and the File / Camera / Exposure / Date / Location groups. The image fits in the space to
//! its left (`right_inset`), the checkerboard runs underneath it, and an 8 px grip on its left border resizes
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
const LABEL_W: f32 = 96.0;
const GROUP_GAP: f32 = 14.0;
const ROW_GAP: f32 = 2.0;
const TITLE_H: f32 = 16.0;
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
}

impl Card {
    pub fn new() -> Card {
        Card { exif: None, loading: None, scroll: 0.0, anim: None, max_scroll: 0.0, drag: None }
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

enum Block {
    Histogram,
    Title(&'static str),
    Row { label: &'static str, value: String, path: bool },
}

/// An EXIF wall-clock time in the user's short date and time format.
fn format_exif_time(t: ExifTime) -> String {
    let st = SYSTEMTIME { wYear: t.year, wMonth: t.month, wDay: t.day, wHour: t.hour, wMinute: t.minute, wSecond: t.second, ..Default::default() };
    format_local(&st)
}

impl App {
    pub(super) fn info_shown(&self) -> bool {
        self.settings.info_visible && self.viewer.current.is_some() && self.chrome() && self.page.is_none()
    }

    /// What the card takes from the right of the viewport (its width, margins included).
    pub(super) fn right_inset(&self) -> f32 {
        if self.info_shown() { self.settings.info_width } else { 0.0 }
    }

    fn info_card_rect(&self) -> D2D_RECT_F {
        let v = self.viewport();
        let w = self.settings.info_width;
        D2D_RECT_F { left: v.right - w + MARGIN, top: v.top + MARGIN, right: v.right - MARGIN, bottom: v.bottom - MARGIN }
    }

    pub(super) fn toggle_info(&mut self) {
        if self.viewer.current.is_none() {
            return;
        }
        self.settings.info_visible = !self.settings.info_visible;
        settings::save(&self.settings);
        self.layout_changed();
    }

    /// The space for the image changed (window size, a card shown, hidden or resized): refit and decode for it.
    pub(super) fn layout_changed(&mut self) {
        let a = self.image_area();
        self.view.set_viewport((a.right - a.left) as f64, (a.bottom - a.top) as f64, self.scale() as f64);
        let (bw, bh) = self.fit_box();
        self.viewer.set_fit_box(bw, bh);
        self.schedule_upgrade();
        self.invalidate();
    }

    pub(super) fn on_exif(&mut self, r: ExifResult) {
        if self.info_card.loading.as_ref().is_some_and(|(p, s)| *p == r.path && *s == r.stamp) {
            self.info_card.loading = None;
        }
        self.info_card.exif = Some((r.path, r.stamp, r.exif));
        self.invalidate();
    }

    /// Reads the current file's EXIF in the background, once per file.
    fn request_exif(&mut self) {
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
        self.info_card.drag = Some((x, self.settings.info_width));
    }

    pub(super) fn info_grip_drag(&mut self, x: f32) {
        let Some((x0, w0)) = self.info_card.drag else { return };
        let (ww, _) = self.size_dip();
        let cap = INFO_MAX.min((ww - MIN_IMAGE_W).max(INFO_MIN));
        // Dragging left makes the card wider.
        let w = (w0 + (x0 - x)).clamp(INFO_MIN, cap).round();
        if w != self.settings.info_width {
            self.settings.info_width = w;
            self.layout_changed();
        }
    }

    pub(super) fn info_grip_release(&mut self) {
        if self.info_card.drag.take().is_some() {
            settings::save(&self.settings);
            self.invalidate();
        }
    }

    pub(super) fn info_grip_reset(&mut self) {
        self.info_card.drag = None;
        self.settings.info_width = INFO_WIDTH;
        settings::save(&self.settings);
        self.layout_changed();
    }

    // --- Content ------------------------------------------------------------------------------------

    fn info_blocks(&self) -> Vec<Block> {
        let mut out = Vec::new();
        let Some(path) = self.current_path() else { return out };
        let entry = self.viewer.current_entry();
        if entry.is_some_and(|e| e.histogram.is_some()) {
            out.push(Block::Histogram);
        }
        let row = |label, value: String| Block::Row { label, value, path: false };
        out.push(Block::Title("File"));
        out.push(row("Name", file_name(path)));
        if let Some(e) = entry {
            if let Some(t) = format::display_name(e.format, Some(path)) {
                out.push(row("Type", t));
            }
            let (w, h) = self.current_dims().unwrap_or((e.native_w, e.native_h));
            out.push(row("Dimensions", format!("{w} × {h}")));
            if e.pages == 0 {
                out.push(row("Megapixels", format!("{:.1} MP", e.native_w as f64 * e.native_h as f64 / 1_000_000.0)));
            }
        }
        if let Some(i) = &self.info {
            if i.size > 0 {
                out.push(row("Size", format_bytes(i.size)));
            }
            out.push(row("Modified", format_time(i.modified)));
        }
        out.push(Block::Row { label: "File Path", value: path.to_string_lossy().into_owned(), path: true });
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
        out
    }

    /// Draws the card and records its hits. Returns whether it is still animating (a scroll).
    pub(super) fn draw_info(&mut self, g: &Gfx) -> bool {
        if !self.info_shown() {
            return false;
        }
        self.request_exif();
        let card = self.info_card_rect();
        if card.bottom - card.top < 40.0 {
            return false;
        }
        self.hits.add(Hit::InfoCard, card);
        g.fill_round(card, 8.0, rgb(self.theme().window));
        g.outline_round(card, 8.0, white(0x18), g.px()); // ControlStrokeColorSecondary

        let inner = D2D_RECT_F { left: card.left + 1.0, top: card.top + 1.0, right: card.right - 1.0, bottom: card.bottom - 1.0 };
        let x = inner.left + PAD_X;
        let w = (inner.right - PAD_X - x).max(1.0);
        let value_w = (w - LABEL_W).max(1.0);
        let view_h = (inner.bottom - inner.top - PAD_TOP - PAD_BOTTOM).max(1.0);

        // Measure first: the content height decides how far the card scrolls.
        let blocks = self.info_blocks();
        let hist_h = (w - 14.0) * 120.0 / 256.0 + 14.0;
        let mut heights = Vec::with_capacity(blocks.len());
        for b in &blocks {
            heights.push(match b {
                Block::Histogram => hist_h,
                Block::Title(_) => TITLE_H + 2.0,
                Block::Row { label, value, .. } => {
                    let lh = g.measure_height(&wide(label), &g.fonts.caption_wrap, LABEL_W - 4.0);
                    let vh = g.measure_height(&wide(value), &g.fonts.caption_wrap, value_w);
                    lh.max(vh).max(16.0).ceil() + 2.0
                }
            });
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
            if y + h >= inner.top && y <= inner.bottom {
                match b {
                    Block::Histogram => self.draw_histogram(g, rect(x, y, w, *h)),
                    Block::Title(t) => {
                        g.text(&wide(&t.to_uppercase()), &g.fonts.overline, rect(x, y, w, TITLE_H), white(TEXT_TERTIARY), Align::Left);
                    }
                    Block::Row { label, value, path } => {
                        g.text(&wide(label), &g.fonts.caption_wrap, rect(x, y + 1.0, LABEL_W - 4.0, h - 2.0), white(TEXT_SECONDARY), Align::Left);
                        let vr = rect(x + LABEL_W, y + 1.0, value_w, h - 2.0);
                        g.text(&wide(value), &g.fonts.caption_wrap, vr, white(0xFF), Align::Left);
                        if *path {
                            let visible = D2D_RECT_F { top: vr.top.max(inner.top), bottom: vr.bottom.min(inner.bottom), ..vr };
                            self.hits.add(Hit::InfoPath, visible);
                        }
                    }
                }
            }
            y += h;
        }
        g.pop_clip();

        // A thin scroll indicator while the pointer is over an overflowing card, or while it scrolls.
        let over = matches!(self.hover, Some(Hit::InfoCard | Hit::InfoPath));
        if self.info_card.max_scroll > 0.0 && (over || moving) {
            let thumb_h = (view_h * view_h / content_h).max(24.0);
            let track = inner.bottom - inner.top - 8.0;
            let ty = inner.top + 4.0 + (track - thumb_h) * scroll / self.info_card.max_scroll;
            g.fill_round(rect(inner.right - 5.0, ty, 2.0, thumb_h), 1.0, white(0x8B));
        }

        // The resize grip straddles the left border; its line shows on hover and turns accent while dragging.
        let grip = rect(card.left - 4.0, card.top, 8.0, card.bottom - card.top);
        self.hits.add(Hit::InfoGrip, grip);
        let dragging = self.info_card.drag.is_some();
        let t = self.fades.get(Hit::InfoGrip, self.hover == Some(Hit::InfoGrip) || dragging);
        if t > 0.0 {
            let c = if dragging { gfx::rgba(ui::ACCENT, 1.0) } else { gfx::rgba(0xFFFFFF, 0.4 * t) };
            g.fill_round(rect(card.left - 1.0, card.top, 2.0, card.bottom - card.top), 1.0, c);
        }
        moving
    }

    /// Luma as a filled area, red, green and blue as curves over it, in a small inset card; drawn to the
    /// card's width at the C# panel's 256 × 120 proportions.
    fn draw_histogram(&self, g: &Gfx, r: D2D_RECT_F) {
        let Some(h) = self.viewer.current_entry().and_then(|e| e.histogram.clone()) else { return };
        g.fill_round(r, 6.0, white(0x08)); // CardBackgroundFillColorSecondary
        g.outline_round(r, 6.0, gfx::rgba(0x000000, 0x19 as f32 / 255.0), g.px());
        let p = D2D_RECT_F { left: r.left + 7.0, top: r.top + 7.0, right: r.right - 7.0, bottom: r.bottom - 7.0 };
        let (pw, ph) = (p.right - p.left, p.bottom - p.top);
        let max = h.max.max(1) as f32;
        let curve = |bins: &[u32; 256]| -> Vec<(f32, f32)> {
            (0..256).map(|i| (p.left + i as f32 / 255.0 * pw, p.bottom - (bins[i] as f32 / max).min(1.0) * ph)).collect()
        };
        let mut area = vec![(p.left, p.bottom)];
        area.extend(curve(&h.luma));
        area.push((p.right, p.bottom));
        g.fill_polygon(&area, gfx::rgba(0x888888, 0x55 as f32 / 255.0));
        let a = 0xC0 as f32 / 255.0;
        g.polyline(&curve(&h.red), gfx::rgba(0xE0574B, a), 1.0);
        g.polyline(&curve(&h.green), gfx::rgba(0x4CAF50, a), 1.0);
        g.polyline(&curve(&h.blue), gfx::rgba(0x5B9BD5, a), 1.0);
    }
}
