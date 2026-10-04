//! Layout and drawing of the window's own chrome and content: title bar, toolbar, viewport, status row,
//! landing page. Everything is laid out from the client size each frame (immediate mode); hit-testing uses
//! the same rects.

use super::*;

impl App {
    pub(super) fn chrome(&self) -> bool {
        self.fullscreen.is_none()
    }

    pub(super) fn viewport(&self) -> D2D_RECT_F {
        let (w, h) = self.size_dip();
        if self.chrome() {
            D2D_RECT_F { left: 0.0, top: TITLE_H + TOOLBAR_H, right: w, bottom: (h - STATUS_H).max(TITLE_H + TOOLBAR_H) }
        } else {
            D2D_RECT_F { left: 0.0, top: 0.0, right: w, bottom: h }
        }
    }

    pub(super) fn caption_rect(&self, c: Caption) -> D2D_RECT_F {
        let (w, _) = self.size_dip();
        let i = match c {
            Caption::Close => 1.0,
            Caption::Max => 2.0,
            Caption::Min => 3.0,
        };
        rect(w - CAPTION_W * i, 0.0, CAPTION_W, TITLE_H)
    }

    pub(super) fn tool_rects(&self) -> Vec<(Tool, u16, D2D_RECT_F)> {
        let (w, _) = self.size_dip();
        let y = TITLE_H + (TOOLBAR_H - 8.0 - BUTTON_H) / 2.0;
        let mut out = Vec::new();
        let mut x = 12.0;
        for &(t, g) in LEFT_TOOLS {
            let bw = if t == Tool::Sort { SORT_W } else { BUTTON_W };
            out.push((t, g, rect(x, y, bw, BUTTON_H)));
            x += bw + 4.0;
        }
        let mut x = w - 12.0;
        for &(t, g) in RIGHT_TOOLS.iter().rev() {
            x -= BUTTON_W;
            out.push((t, g, rect(x, y, BUTTON_W, BUTTON_H)));
            x -= 4.0;
        }
        out
    }

    pub(super) fn tool_enabled(&self, t: Tool) -> bool {
        let has = self.viewer.current.is_some();
        match t {
            Tool::Settings => true,
            Tool::Previous | Tool::Next => has && self.viewer.image_count() > 1,
            _ => has,
        }
    }

    pub(super) fn status_parts(&self) -> String {
        let Some(path) = self.current_path() else { return String::new() };
        let mut parts: Vec<String> = Vec::new();
        let cached = self.viewer.current_entry();
        if let Some(c) = cached {
            if let Some(name) = format::display_name(c.format, Some(path)) {
                parts.push(name);
            }
            if c.pages > 0 {
                parts.push(if c.pages == 1 { "1 page".into() } else { format!("{} pages", c.pages) });
            }
            parts.push(format!("{} × {}", c.native_w, c.native_h));
            if c.pages == 0 {
                // a page's 96-dpi size is not a pixel count
                parts.push(format!("{:.1} MP", c.native_w as f64 * c.native_h as f64 / 1_000_000.0));
            }
        }
        if let Some(i) = &self.info {
            if i.size > 0 {
                parts.push(format_bytes(i.size));
            }
        }
        if let Some(t) = cached.and_then(|c| c.taken) {
            parts.push(format!("Taken {}", format_time(t)));
        } else if let Some(i) = &self.info {
            parts.push(format!("Modified {}", format_time(i.modified)));
        }
        if cached.is_some() {
            parts.push(format!("{:.0}%", self.view.zoom_percent()));
        }
        parts.join("   ·   ")
    }

    pub(super) fn reveal_rect(&self) -> Option<D2D_RECT_F> {
        let g = self.gfx.as_ref()?;
        self.viewer.current.as_ref()?;
        let (_, h) = self.size_dip();
        let tw = g.measure(&wide(&self.status_parts()), &g.fonts.caption);
        let lw = g.measure(&wide("Open in Explorer"), &g.fonts.caption);
        Some(rect(12.0 + tw + 16.0, h - STATUS_H + 4.0, lw, STATUS_H - 10.0))
    }

    pub(super) fn open_rect(&self) -> D2D_RECT_F {
        let v = self.viewport();
        let cx = (v.left + v.right) / 2.0;
        let cy = (v.top + v.bottom) / 2.0;
        rect(cx - 70.0, cy + 8.0, 140.0, 32.0)
    }

    pub(super) fn hit(&self, x: f32, y: f32) -> Option<Hit> {
        if self.chrome() {
            for (t, _, r) in self.tool_rects() {
                if contains(&r, x, y) {
                    return Some(Hit::Tool(t));
                }
            }
            if let Some(r) = self.reveal_rect() {
                if contains(&r, x, y) {
                    return Some(Hit::Reveal);
                }
            }
        }
        if self.viewer.current.is_none() && contains(&self.open_rect(), x, y) {
            return Some(Hit::Open);
        }
        if contains(&self.viewport(), x, y) {
            return Some(Hit::Viewport);
        }
        None
    }

    // --- Drawing ------------------------------------------------------------------------------------

    pub(super) fn invalidate(&self) {
        unsafe {
            let _ = InvalidateRect(Some(self.hwnd), None, false);
        }
    }

    pub(super) fn render(&mut self) {
        let Some(mut g) = self.gfx.take() else { return };
        g.begin(0x000000);
        if self.chrome() {
            self.draw_title(&g);
            self.draw_toolbar(&g);
            self.draw_status(&g);
        }
        self.draw_viewport(&mut g);
        if let Err(e) = g.end() {
            crate::trace::mark(format!("present failed: {e}"));
        }
        self.gfx = Some(g);
        if self.view.animating() {
            self.invalidate();
        }
    }

    pub(super) fn draw_title(&self, g: &Gfx) {
        let (w, _) = self.size_dip();
        if let Some(icon) = &self.icon {
            let x = g.snap(16.0);
            let y = g.snap((TITLE_H - 16.0) / 2.0);
            g.draw_bitmap(icon, rect(x, y, 16.0, 16.0), 1.0);
        }
        let title = self.current_path().map(file_name).unwrap_or_else(|| "Looker".into());
        let fg = if self.active { white(0xFF) } else { white(TEXT_TERTIARY) };
        g.text(&wide(&title), &g.fonts.caption, rect(48.0, 0.0, (w - 48.0 - CAPTION_W * 3.0 - 16.0).max(0.0), TITLE_H), fg, Align::Left);

        for (c, glyph) in [
            (Caption::Min, 0xE921u16),
            (Caption::Max, if self.maximized { 0xE923 } else { 0xE922 }),
            (Caption::Close, 0xE8BB),
        ] {
            let r = self.caption_rect(c);
            let hover = self.cap_hover == Some(c);
            let pressed = self.cap_pressed == Some(c);
            let mut fg = if self.active { white(0xFF) } else { white(TEXT_DISABLED) };
            if c == Caption::Close && (hover || pressed) {
                g.fill(r, if pressed { gfx::rgba(CLOSE_RED, 0.9) } else { rgb(CLOSE_RED) });
                fg = white(0xFF);
            } else if pressed {
                g.fill(r, white(0x0A));
            } else if hover {
                g.fill(r, white(0x0F));
            }
            g.text(&[glyph], &g.fonts.caption_icons, r, fg, Align::Center);
        }
    }

    pub(super) fn draw_toolbar(&self, g: &Gfx) {
        for (t, glyph, r) in self.tool_rects() {
            let enabled = self.tool_enabled(t);
            let id = Some(Hit::Tool(t));
            let fill = if !enabled {
                white(0x0B)
            } else if self.pressed == id && self.hover == id {
                white(0x1C)
            } else if self.hover == id && self.pressed.is_none() {
                white(0x33)
            } else {
                white(0x24)
            };
            g.fill_round(r, 4.0, fill);
            g.outline_round(r, 4.0, white(0x12), g.px());
            let fg = if enabled { white(0xFF) } else { white(TEXT_DISABLED) };
            if t == Tool::Sort {
                g.text(&[glyph], &g.fonts.icons, rect(r.left + 11.0, r.top, 16.0, BUTTON_H), fg, Align::Center);
                g.text(&[0xE70D], &g.fonts.caption_icons, rect(r.right - 11.0 - 12.0, r.top, 12.0, BUTTON_H), fg, Align::Center);
            } else {
                g.text(&[glyph], &g.fonts.icons, r, fg, Align::Center);
            }
        }
    }

    pub(super) fn draw_status(&self, g: &Gfx) {
        let (w, h) = self.size_dip();
        let y = h - STATUS_H + 4.0;
        let row_h = STATUS_H - 10.0;
        let text = self.status_parts();
        if text.is_empty() {
            return;
        }
        g.text(&wide(&text), &g.fonts.caption, rect(12.0, y, (w - 24.0).max(0.0), row_h), white(TEXT_SECONDARY), Align::Left);
        if let Some(r) = self.reveal_rect() {
            let c = if self.hover == Some(Hit::Reveal) { white(0xFF) } else { white(TEXT_SECONDARY) };
            g.text(&wide("Open in Explorer"), &g.fonts.caption, r, c, Align::Left);
        }
        if let (Some(l), Some(i)) = (&self.viewer.listing, self.viewer.index) {
            let label = format!("{} / {}", l.rank[i] + 1, l.total_files);
            g.text(&wide(&label), &g.fonts.caption, rect(12.0, y, (w - 24.0).max(0.0), row_h), white(TEXT_SECONDARY), Align::Right);
        }
    }

    pub(super) fn draw_viewport(&mut self, g: &mut Gfx) {
        let v = self.viewport();
        if self.viewer.current.is_none() {
            self.draw_landing(g);
            return;
        }
        g.checkerboard(v);
        if let Some(c) = self.viewer.shown.clone() {
            {
                let r = self.view.frame();
                let dest = D2D_RECT_F {
                    left: v.left + r.x as f32,
                    top: v.top + r.y as f32,
                    right: v.left + (r.x + r.w) as f32,
                    bottom: v.top + (r.y + r.h) as f32,
                };
                unsafe {
                    g.dev.dc.PushAxisAlignedClip(&v, windows::Win32::Graphics::Direct2D::D2D1_ANTIALIAS_MODE_ALIASED);
                }
                let frames = c.frames.borrow();
                let frame = &frames[self.viewer.anim_frame.min(frames.len() - 1)].0;
                g.draw_bitmap(frame, dest, 1.0);
                unsafe {
                    g.dev.dc.PopAxisAlignedClip();
                }
                if !self.first_image_traced {
                    self.first_image_traced = true;
                    crate::trace::mark("first frame with the image drawn");
                }
            }
        } else if self.viewer.error.is_some() {
            g.text(&wide("Can't display this file"), &g.fonts.body, v, white(TEXT_SECONDARY), Align::Center);
        }
    }

    pub(super) fn draw_landing(&self, g: &Gfx) {
        let v = self.viewport();
        let cy = (v.top + v.bottom) / 2.0;
        g.text(&wide("Looker"), &g.fonts.body_strong, rect(v.left, cy - 40.0, v.right - v.left, 24.0), white(0xFF), Align::Center);
        g.text(&wide("Open a photo to start  ·  Ctrl+O"), &g.fonts.caption, rect(v.left, cy - 16.0, v.right - v.left, 18.0), white(TEXT_SECONDARY), Align::Center);
        let r = self.open_rect();
        let hover = self.hover == Some(Hit::Open);
        let fill = if self.pressed == Some(Hit::Open) && hover {
            gfx::rgba(ACCENT, 0.8)
        } else if hover {
            gfx::rgba(ACCENT, 0.9)
        } else {
            rgb(ACCENT)
        };
        g.fill_round(r, 4.0, fill);
        g.text(&wide("Open photo"), &g.fonts.caption, r, white(0xFF), Align::Center);
    }
}
