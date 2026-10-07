//! Multi-page documents in the window: which page the bar points at, turning pages (the bar's buttons and
//! Page Up / Page Down; the arrow keys keep stepping through the folder, so a PDF in a photo folder never
//! interrupts browsing), drawing the page strip with the pages rendered so far, and the page bar.

use std::rc::Rc;

use super::*;
use crate::pages::Request;
use crate::view::Rect;
use crate::viewer::Entry;

const BAR_BUTTON: f32 = 32.0;
const BAR_PAD_X: f32 = 6.0;
const BAR_PAD_Y: f32 = 4.0;
const BAR_LABEL_MARGIN: f32 = 10.0;
/// A page not rendered yet: a blank sheet.
const PAPER: u32 = 0xFFFFFF;

impl App {
    /// The current image when it is a document (any page count).
    pub(super) fn current_doc(&self) -> Option<Rc<Entry>> {
        self.viewer.current_entry().filter(|e| e.layout.is_some()).cloned()
    }

    /// Pages of the current document; 0 for an image (or nothing up yet).
    pub(super) fn page_count(&self) -> usize {
        self.viewer.current_entry().map_or(0, |e| e.page_count())
    }

    /// The fit reference for a document that just came up: its first page.
    pub(super) fn first_page_focus(e: &Entry) -> Option<Rect> {
        e.layout.as_ref().and_then(|l| l.pages.first().copied())
    }

    /// What "the current image" measures: the page on the bar for a document, else the image.
    pub(super) fn current_dims(&self) -> Option<(u32, u32)> {
        let e = self.viewer.current_entry()?;
        match e.layout.as_ref().and_then(|l| l.pages.get(self.pdf_page.min(l.pages.len().saturating_sub(1)))) {
            Some(r) => Some((r.w.round() as u32, r.h.round() as u32)),
            None => Some((e.native_w, e.native_h)),
        }
    }

    /// The page copy and wallpaper take: the one on the bar (0 for an image).
    pub(super) fn pdf_page_of_current(&self) -> usize {
        if self.current_doc().is_some() { self.pdf_page } else { 0 }
    }

    /// Points the view at page `i` (clamped): fits it in Fit mode, else keeps the zoom and centres on it.
    pub(super) fn go_to_page(&mut self, i: usize) {
        let Some(e) = self.current_doc() else { return };
        let l = e.layout.as_ref().unwrap();
        let i = i.min(l.pages.len() - 1);
        if i == self.pdf_page {
            return;
        }
        self.pdf_page = i;
        self.view.set_focus(Some(l.pages[i]), true);
        self.schedule_upgrade(); // a differently sized page fits at another scale
        self.invalidate();
    }

    pub(super) fn turn_page(&mut self, delta: isize) {
        if self.page_count() > 1 {
            self.go_to_page(self.pdf_page.saturating_add_signed(delta));
        }
    }

    /// After a pan or zoom: the page nearest the middle of the view becomes the current one (bar text, fit
    /// reference) without moving the view, so a drag across the strip shows in the bar and Next goes on
    /// from there.
    pub(super) fn sync_page_to_view(&mut self) {
        let Some(e) = self.current_doc() else { return };
        let l = e.layout.as_ref().unwrap();
        let i = l.page_at(self.view.center_content_x());
        if i != self.pdf_page {
            self.pdf_page = i;
            self.view.set_focus(Some(l.pages[i]), false);
        }
    }

    pub(super) fn on_pages_rendered(&mut self) {
        let Some(g) = self.gfx.as_ref() else { return };
        let mut redraw = false;
        for r in self.renderer.take() {
            let Some(e) = self.viewer.entry_for(&r.key) else { continue };
            match r.result {
                Ok(px) => {
                    if let Ok(bmp) = g.photo_bitmap(r.w, r.h, &px, &crate::imaging::Colour::Srgb) {
                        e.store_page(r.page, bmp, self.pdf_page);
                        redraw = true;
                    }
                }
                // Not asked for again by every frame; a later visit to the file (new decode) may retry.
                Err(_) => {
                    self.page_failed.insert((r.key, r.page));
                }
            }
        }
        if redraw {
            self.invalidate();
        }
    }

    /// Draws every page of `e` that meets the viewport into the strip at `dest`; pages not rendered yet are
    /// blank sheets (or another tier's render, stretched) and get asked for, the current page first.
    pub(super) fn draw_pages(&mut self, g: &Gfx, e: &Rc<Entry>, dest: D2D_RECT_F, clip: D2D_RECT_F) {
        let l = e.layout.as_ref().unwrap();
        let s = (dest.right - dest.left) as f64 / l.width;
        let mut wanted = Vec::new();
        for (i, p) in l.pages.iter().enumerate() {
            let r = D2D_RECT_F {
                left: dest.left + (p.x * s) as f32,
                top: dest.top + (p.y * s) as f32,
                right: dest.left + ((p.x + p.w) * s) as f32,
                bottom: dest.top + ((p.y + p.h) * s) as f32,
            };
            if r.right < clip.left || r.left > clip.right || r.bottom < clip.top || r.top > clip.bottom {
                continue;
            }
            match e.page_bitmap(i) {
                Some(b) => {
                    g.draw_bitmap(&b, r, 1.0);
                    if i == self.pdf_page {
                        self.last_drawn = Some((b, r));
                    }
                }
                None => {
                    match self.viewer.page_stand_in(&e.key.path, e.key.stamp, i) {
                        Some(b) => g.draw_bitmap(&b, r, 1.0),
                        None => g.fill(r, g.photo_rgb(PAPER)),
                    }
                    if !self.page_failed.contains(&(e.key.clone(), i)) {
                        let (w, h) = e.page_pixels(i);
                        wanted.push(Request { key: e.key.clone(), page: i, w, h });
                    }
                }
            }
        }
        wanted.sort_by_key(|r| r.page.abs_diff(self.pdf_page));
        self.want_pages(wanted);
    }

    /// Hands the render thread its new wish list (only when there is one, or one to cancel).
    pub(super) fn want_pages(&mut self, wanted: Vec<Request>) {
        if wanted.is_empty() && !self.pages_wanted {
            return;
        }
        self.pages_wanted = !wanted.is_empty();
        self.renderer.want(wanted);
    }

    fn page_bar_rect(&self, g: &Gfx, label: &str) -> D2D_RECT_F {
        let a = self.image_area();
        let lw = g.measure(&wide(label), &g.fonts.body).ceil();
        let w = BAR_PAD_X * 2.0 + BAR_BUTTON * 2.0 + 4.0 + BAR_LABEL_MARGIN * 2.0 + lw;
        let h = BAR_PAD_Y * 2.0 + BAR_BUTTON;
        let x = g.snap((a.left + a.right - w) / 2.0);
        rect(x, g.snap(a.bottom - 16.0 - h), w, h)
    }

    /// The floating "‹ Page 3 of 12 ›" pill, bottom-centre of the space between the cards, for a document
    /// of more than one page (a single page has nothing to turn).
    pub(super) fn draw_page_bar(&mut self, g: &Gfx) {
        let n = self.page_count();
        if n < 2 || self.page.is_some() {
            return;
        }
        let i = self.pdf_page.min(n - 1);
        let label = format!("Page {} of {}", i + 1, n);
        let r = self.page_bar_rect(g, &label);
        ui::shadow(g, r, 8.0);
        g.fill_round(r, 8.0, rgb(self.theme().window));
        g.outline_round(r, 8.0, white(0x18), g.px());
        let y = r.top + BAR_PAD_Y;
        let prev = rect(r.left + BAR_PAD_X, y, BAR_BUTTON, BAR_BUTTON);
        let next = rect(r.right - BAR_PAD_X - BAR_BUTTON, y, BAR_BUTTON, BAR_BUTTON);
        // The pill itself swallows clicks so a near miss doesn't start a pan.
        self.hits.add(Hit::PageBar, r);
        for (id, b, glyph, enabled) in [(Hit::PagePrevious, prev, 0xE76Bu16, i > 0), (Hit::PageNext, next, 0xE76C, i + 1 < n)] {
            self.hits.add(id, b);
            let st = self.state(id, enabled);
            let fg = ui::button_frame(g, b, ui::Kind::Standard, &st);
            g.text(&[glyph], &g.fonts.body_icons, b, fg, Align::Center);
        }
        let text = rect(prev.right + 2.0 + BAR_LABEL_MARGIN, y, next.left - 2.0 - BAR_LABEL_MARGIN - (prev.right + 2.0 + BAR_LABEL_MARGIN), BAR_BUTTON);
        g.text(&wide(&label), &g.fonts.body, text, white(0xFF), Align::Center);
    }
}
