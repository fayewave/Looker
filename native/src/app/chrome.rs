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
            D2D_RECT_F { left: 0.0, top: TITLE_H + TOOLBAR_H, right: w, bottom: (h - STATUS_H - self.bottom_inset()).max(TITLE_H + TOOLBAR_H) }
        } else {
            D2D_RECT_F { left: 0.0, top: 0.0, right: w, bottom: h }
        }
    }

    /// Where the image fits and zooms: the viewport minus the floating cards.
    pub(super) fn image_area(&self) -> D2D_RECT_F {
        let v = self.viewport();
        let left = v.left + self.left_inset();
        D2D_RECT_F { left, right: (v.right - self.right_inset()).max(left + 1.0), ..v }
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
        if self.turns != 0 {
            out.push((Tool::SaveRotation, 0, rect(x, y, SAVE_W, BUTTON_H)));
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
        let has = self.viewer.current.is_some() && self.page.is_none();
        match t {
            Tool::Settings => true,
            Tool::Previous | Tool::Next => has && self.viewer.image_count() > 1,
            Tool::Sort | Tool::Delete => has,
            Tool::Rotate => has && self.can_rotate(),
            Tool::SaveRotation => !self.saving_rotation,
            Tool::Info | Tool::Strip | Tool::Explorer => has,
            _ => has,
        }
    }

    /// A tooltip for anything: the static ones below, or a thumbnail's file name.
    pub(super) fn tooltip_for(&self, h: Hit) -> Option<String> {
        match h {
            Hit::StripCell(i) => self.strip_tooltip(i),
            Hit::RecentRow(i) => self.recent_path(i).map(|p| p.to_string_lossy().into_owned()),
            Hit::ExplorerRow(_) | Hit::Crumb(_) | Hit::CrumbMore | Hit::ExplorerBack | Hit::ExplorerForward => self.explorer_tooltip(h),
            _ => Self::tooltip_text(h).map(String::from),
        }
    }

    /// Tooltip text, as in MainWindow.xaml.
    pub(super) fn tooltip_text(h: Hit) -> Option<&'static str> {
        Some(match h {
            Hit::Tool(Tool::Previous) => "Previous (←)",
            Hit::Tool(Tool::Next) => "Next (→)",
            Hit::Tool(Tool::ZoomOut) => "Zoom out (Ctrl+-)",
            Hit::Tool(Tool::Fit) => "Fit to window (F)",
            Hit::Tool(Tool::ZoomIn) => "Zoom in (Ctrl++)",
            Hit::Tool(Tool::Fullscreen) => "Fullscreen (F11)",
            Hit::Tool(Tool::Rotate) => "Rotate right (Ctrl+R)",
            Hit::Tool(Tool::Delete) => "Delete (Del)",
            Hit::Tool(Tool::Sort) => "Sort",
            Hit::Tool(Tool::SaveRotation) => "Save the rotation to the file (Ctrl+S)",
            Hit::Tool(Tool::Home) => "Home",
            Hit::Tool(Tool::Explorer) => "File explorer (E)",
            Hit::Tool(Tool::Strip) => "Thumbnails (T)",
            Hit::Tool(Tool::Info) => "Info panel (I)",
            Hit::Tool(Tool::Settings) => "Settings (Ctrl+,)",
            Hit::Reveal => "Show this file in File Explorer",
            Hit::InfoPath => "Show in File Explorer",
            Hit::PagePrevious => "Previous page (Page Up)",
            Hit::PageNext => "Next page (Page Down)",
            _ => return None,
        })
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
            let (w, h) = self.current_dims().unwrap_or((c.native_w, c.native_h));
            parts.push(format!("{w} × {h}"));
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

    /// What is under a point, from the regions the last frame drew.
    pub(super) fn hit(&self, x: f32, y: f32) -> Option<Hit> {
        self.hits.at(x, y)
    }

    // --- Drawing ------------------------------------------------------------------------------------

    pub(super) fn invalidate(&self) {
        unsafe {
            let _ = InvalidateRect(Some(self.hwnd), None, false);
        }
    }

    pub(super) fn render(&mut self) {
        let Some(mut g) = self.gfx.take() else { return };
        self.hits.clear();
        self.fades.begin();
        g.begin(self.theme().window);
        // Bottom to top: hits added later win.
        self.draw_viewport(&mut g);
        let card_moving = self.draw_info(&g) | self.draw_explorer(&g);
        let strip_moving = self.draw_strip(&g);
        if self.viewer.current.is_some() {
            self.draw_page_bar(&g);
        }
        if self.chrome() {
            self.draw_title(&g);
            self.draw_toolbar(&g);
            if self.page.is_some() {
                self.draw_page(&g);
            } else {
                self.draw_status(&g);
            }
        }
        let toast_moving = self.draw_overlays(&g);
        if let Err(e) = g.end() {
            crate::trace::mark(format!("present failed: {e}"));
        }
        self.gfx = Some(g);
        self.pump_thumbs();
        if self.view.animating() || self.fades.moving || toast_moving || card_moving || strip_moving {
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

    /// Hover fade and pressed state of a hit, for drawing it.
    pub(super) fn state(&mut self, id: Hit, enabled: bool) -> ui::State {
        let over = self.hover == Some(id) && (self.pressed.is_none() || self.pressed == Some(id));
        let hover = self.fades.get(id, over && enabled);
        ui::State { hover, pressed: self.pressed == Some(id) && self.hover == Some(id), enabled }
    }

    pub(super) fn draw_toolbar(&mut self, g: &Gfx) {
        for (t, glyph, r) in self.tool_rects() {
            let enabled = self.tool_enabled(t);
            let id = Hit::Tool(t);
            self.hits.add(id, r);
            let open = t == Tool::Sort && matches!(self.menu, Some((menus::MenuKind::Sort, _)));
            let mut st = self.state(id, enabled);
            st.pressed |= open;
            if t == Tool::SaveRotation {
                let fg = ui::button_frame(g, r, ui::Kind::Accent, &st);
                g.text(&wide("Save"), &g.fonts.body, r, fg, Align::Center);
                continue;
            }
            let kind = match t {
                Tool::Info => ui::Kind::Toggle(self.info_shown()),
                Tool::Strip => ui::Kind::Toggle(self.strip_shown()),
                Tool::Explorer => ui::Kind::Toggle(self.explorer_shown()),
                Tool::Settings => ui::Kind::Toggle(self.page.is_some()),
                _ => ui::Kind::Standard,
            };
            let fg = ui::button_frame(g, r, kind, &st);
            if t == Tool::Sort {
                g.text(&[glyph], &g.fonts.icons, rect(r.left + 11.0, r.top, 16.0, BUTTON_H), fg, Align::Center);
                g.text(&[0xE70D], &g.fonts.caption_icons, rect(r.right - 11.0 - 12.0, r.top, 12.0, BUTTON_H), fg, Align::Center);
            } else {
                g.text(&[glyph], &g.fonts.icons, r, fg, Align::Center);
            }
        }
    }

    pub(super) fn draw_status(&mut self, g: &Gfx) {
        let (w, h) = self.size_dip();
        let y = h - STATUS_H + 4.0;
        let row_h = STATUS_H - 10.0;
        let text = self.status_parts();
        if text.is_empty() {
            return;
        }
        g.text(&wide(&text), &g.fonts.caption, rect(12.0, y, (w - 24.0).max(0.0), row_h), white(TEXT_SECONDARY), Align::Left);
        if let Some(r) = self.reveal_rect() {
            self.hits.add(Hit::Reveal, r);
            let t = self.fades.get(Hit::Reveal, self.hover == Some(Hit::Reveal));
            let c = if self.pressed == Some(Hit::Reveal) {
                white(TEXT_TERTIARY)
            } else {
                white((TEXT_SECONDARY as f32 + (255.0 - TEXT_SECONDARY as f32) * t) as u8)
            };
            g.text(&wide("Open in Explorer"), &g.fonts.caption, r, c, Align::Left);
        }
        if let (Some(l), Some(i)) = (&self.viewer.listing, self.viewer.index) {
            let label = format!("{} / {}", l.rank[i] + 1, l.total_files);
            g.text(&wide(&label), &g.fonts.caption, rect(12.0, y, (w - 24.0).max(0.0), row_h), white(TEXT_SECONDARY), Align::Right);
        }
    }

    pub(super) fn draw_viewport(&mut self, g: &mut Gfx) {
        let v = self.viewport();
        self.hits.add(Hit::Viewport, v);
        if self.viewer.current.is_none() {
            self.draw_landing(g);
            return;
        }
        g.checkerboard(v);
        let doc = self.viewer.shown.clone().filter(|e| e.layout.is_some() && self.turns == 0);
        if doc.is_none() {
            self.want_pages(Vec::new());
        }
        if let Some(c) = self.viewer.shown.clone() {
            {
                let r = self.view.frame();
                let a = self.image_area();
                let dest = D2D_RECT_F {
                    left: a.left + r.x as f32,
                    top: a.top + r.y as f32,
                    right: a.left + (r.x + r.w) as f32,
                    bottom: a.top + (r.y + r.h) as f32,
                };
                unsafe {
                    g.dev.dc.PushAxisAlignedClip(&v, windows::Win32::Graphics::Direct2D::D2D1_ANTIALIAS_MODE_ALIASED);
                }
                let frames = c.frames.borrow();
                let frame = &frames[self.viewer.anim_frame.min(frames.len() - 1)].0;
                if let Some(d) = &doc {
                    self.draw_pages(g, d, dest, v);
                } else if self.turns == 0 {
                    g.draw_bitmap(frame, dest, 1.0);
                } else {
                    // `dest` is the turned image's bounds: draw the unturned frame into the box that lands on
                    // it once turned about the centre (width and height swap for odd turns).
                    let (cx, cy) = ((dest.left + dest.right) / 2.0, (dest.top + dest.bottom) / 2.0);
                    let (w, h) = (dest.right - dest.left, dest.bottom - dest.top);
                    let (dw, dh) = if self.turns & 1 == 1 { (h, w) } else { (w, h) };
                    let unturned = rect(cx - dw / 2.0, cy - dh / 2.0, dw, dh);
                    g.with_transform(&quarter_turns(self.turns, cx, cy), || g.draw_bitmap(frame, unturned, 1.0));
                }
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

    /// The toast, menus, dialogs and tooltips, above everything. Returns whether the toast is mid-fade.
    fn draw_overlays(&mut self, g: &Gfx) -> bool {
        let mut toast_moving = false;
        if let Some(t) = &self.toast {
            let area = self.image_area();
            match t.phase() {
                ui::ToastPhase::Fading(a) => {
                    t.draw(g, area, a);
                    toast_moving = true;
                }
                ui::ToastPhase::Holding(ms) => {
                    t.draw(g, area, 1.0);
                    unsafe {
                        SetTimer(Some(self.hwnd), TIMER_TOAST, ms, None);
                    }
                }
                ui::ToastPhase::Done => self.toast = None,
            }
        }
        if let Some((_, m)) = &self.menu {
            self.hits.add(Hit::MenuSurface, m.bounds());
            for (i, r) in m.item_rects() {
                self.hits.add(Hit::MenuItem(i), r);
            }
            let hover = match self.hover {
                Some(Hit::MenuItem(i)) => Some(i),
                _ => None,
            };
            let pressed = match self.pressed {
                Some(Hit::MenuItem(i)) if hover == Some(i) => Some(i),
                _ => None,
            };
            m.draw(g, hover, pressed);
        }
        if self.dialog.is_some() {
            let (w, h) = self.size_dip();
            let window = rect(0.0, 0.0, w, h);
            let layout = self.dialog.as_ref().map(|(_, d)| d.layout(g, window)).unwrap();
            self.hits.add(Hit::DialogSurface, window);
            let mut states = Vec::new();
            for (i, r) in layout.buttons.iter().enumerate() {
                self.hits.add(Hit::DialogButton(i), *r);
                states.push(self.state(Hit::DialogButton(i), true));
            }
            let mut field_hover = 0.0;
            if let Some(r) = layout.field {
                self.hits.add(Hit::DialogField, r);
                field_hover = self.state(Hit::DialogField, true).hover;
            }
            if let Some(r) = layout.field_clear {
                self.hits.add(Hit::DialogFieldClear, r);
            }
            let clear = self.state(Hit::DialogFieldClear, true);
            if let Some((_, d)) = &mut self.dialog {
                let theme = ui::Theme::new(self.settings.dark_grey);
                self.caret_rect = d.draw(g, &layout, window, theme, &states, field_hover, clear);
            }
        }
        if let Some(t) = self.tooltip {
            if let (Some(text), Some(anchor)) = (self.tooltip_for(t), self.hits.rect_of(t)) {
                let (w, h) = self.size_dip();
                ui::tooltip(g, &text, anchor, rect(0.0, 0.0, w, h));
            }
        }
        toast_moving
    }
}

/// Turns the drawing `turns` quarter turns clockwise about (cx, cy). Exact, unlike a sin/cos of 90°.
fn quarter_turns(turns: u8, cx: f32, cy: f32) -> windows_numerics::Matrix3x2 {
    let (cos, sin) = match turns & 3 {
        1 => (0.0, 1.0),
        2 => (-1.0, 0.0),
        3 => (0.0, -1.0),
        _ => (1.0, 0.0),
    };
    windows_numerics::Matrix3x2 {
        M11: cos,
        M12: sin,
        M21: -sin,
        M22: cos,
        M31: cx - (cx * cos - cy * sin),
        M32: cy - (cx * sin + cy * cos),
    }
}
