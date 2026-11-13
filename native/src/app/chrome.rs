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
            // Clean view gives the bars' room to the photo as they fade (`clean_k`).
            let (top, status) = ((TITLE_H + TOOLBAR_H) * self.clean_k, status_h() * self.clean_k);
            D2D_RECT_F { left: 0.0, top, right: w, bottom: (h - status - self.bottom_inset()).max(top) }
        } else {
            D2D_RECT_F { left: 0.0, top: 0.0, right: w, bottom: (h - self.bottom_inset()).max(1.0) }
        }
    }

    /// Where the content ends at the bottom: above the status row, or the window's edge in fullscreen.
    pub(super) fn content_bottom(&self) -> f32 {
        let (_, h) = self.size_dip();
        if self.chrome() { h - status_h() * self.clean_k } else { h }
    }

    /// The cards and the strip follow their toggles while a photo is up, in fullscreen too, but not behind
    /// a page, during a slideshow (that is the photos alone), or for a video in fullscreen (the video alone, like a
    /// cinema: Faye, 2026-11-10).
    pub(super) fn panels_allowed(&self) -> bool {
        self.viewer.current.is_some() && !self.slideshow.running && !(self.fullscreen.is_some() && self.video_shown()) && !self.clean()
    }

    /// The panels slide to where their toggles say (a toggle, a slideshow starting or ending). A panel toggled
    /// brings the UI back from clean view.
    pub(super) fn slide_panels(&mut self) {
        self.clean_view = false;
        self.animate_panels();
    }

    /// The panels slide to where `*_shown` says now.
    pub(super) fn animate_panels(&mut self) {
        let (i, e, s) = (self.info_shown(), self.explorer_shown(), self.strip_shown());
        self.slides.info.animate(i);
        self.slides.explorer.animate(e);
        self.slides.strip.animate(s);
        self.invalidate();
    }

    /// Before each frame: any other change of panel state (launch, landing page, a page) jumps and re-lays the
    /// image out; a slide in progress re-lays it out every frame, and once more, with a sharper decode, when it
    /// lands.
    /// True while one is moving.
    fn step_slides(&mut self) -> bool {
        let (i, e, s) = (self.info_shown(), self.explorer_shown(), self.strip_shown());
        let sl = &mut self.slides;
        let mut snapped = false;
        for (slide, shown) in [(&mut sl.info, i), (&mut sl.explorer, e), (&mut sl.strip, s)] {
            if slide.shown() != shown {
                slide.snap(shown);
                snapped = true;
            }
        }
        let moving = sl.info.moving() || sl.explorer.moving() || sl.strip.moving();
        let landed = sl.info.settle() | sl.explorer.settle() | sl.strip.settle();
        let browse = self.browse_moving() | self.info_settling();
        let (moving, landed) = (moving || browse, landed);
        // A snap moves the insets too: the layout done by whatever changed the state (a page closing) measured
        // the old ones.
        if moving || landed || snapped {
            self.layout_changed();
        }
        moving
    }

    /// Where the image fits and zooms: the viewport minus the floating cards.
    /// The background and the photo fill the whole window: the title bar, toolbar, strip, status row and cards
    /// float on them (not behind a page, which keeps its own surface below the toolbar).
    pub(super) fn photo_layer(&self) -> D2D_RECT_F {
        let v = self.viewport();
        let (_, wh) = self.size_dip();
        let (top, bottom) = if self.chrome() { (0.0, wh) } else { (v.top, self.content_bottom()) };
        D2D_RECT_F { top, bottom, ..v }
    }

    /// The photo's room off the toolbar and the status row: `PREVIEW_PAD`, none in fullscreen.
    pub(super) fn preview_pad(&self) -> f32 {
        if self.chrome() { PREVIEW_PAD * self.clean_k } else { 0.0 }
    }

    pub(super) fn image_area(&self) -> D2D_RECT_F {
        let v = self.viewport();
        let left = v.left + self.left_inset();
        // Off the toolbar, and off the status row or the strip, by the cards' margin: the same 12 all round as
        // between the photo and a card (Faye, 2026-11-10: it touched the toolbar's buttons). Fullscreen has neither.
        let pad = self.preview_pad();
        let top = (v.top + pad).min(v.bottom - 1.0);
        let bottom = (v.bottom - pad).max(top + 1.0);
        D2D_RECT_F { left, top, right: (v.right - self.right_inset()).max(left + 1.0), bottom }
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

    /// The toolbar's buttons' top edge: centred in the toolbar.
    pub(super) fn toolbar_y() -> f32 {
        TITLE_H + (TOOLBAR_H - BUTTON_H) / 2.0
    }

    /// The toolbar's groups; buttons of one group sit `BAR_GAP` apart, or touch when the group is joined into
    /// one box (`joined`).
    pub(super) fn tool_group(t: Tool) -> u8 {
        match t {
            Tool::Home => 0,
            Tool::Previous | Tool::Next => 1,
            Tool::ZoomOut | Tool::Zoom | Tool::Fit | Tool::ZoomIn => 2,
            Tool::Rotate | Tool::Edit | Tool::Delete | Tool::More => 3,
            Tool::SaveRotation => 4,
            Tool::Sort => 10,
            Tool::Explorer | Tool::Strip | Tool::Info => 11,
            Tool::Fullscreen | Tool::Settings => 12,
        }
    }

    /// The zoom control and the panel switches are each one box of segments.
    pub(super) fn tool_joined(t: Tool) -> bool {
        matches!(Self::tool_group(t), 2 | 11)
    }

    pub(super) fn tool_rects(&self) -> Vec<(Tool, u16, D2D_RECT_F)> {
        // A narrow window gives up room in this order (each has a key, the wheel, a double-click or the ⋯ menu):
        // zoom's − and +, Fullscreen, Rotate, Sort's label, the zoom level, Delete.
        #[derive(Clone, Copy)]
        enum Give {
            Drop(&'static [Tool]),
            SortLabel,
        }
        const GIVE: [Give; 6] = [
            Give::Drop(&[Tool::ZoomOut, Tool::ZoomIn]),
            Give::Drop(&[Tool::Fullscreen]),
            Give::Drop(&[Tool::Rotate]),
            Give::SortLabel,
            Give::Drop(&[Tool::Zoom]),
            Give::Drop(&[Tool::Delete]),
        ];
        let gap = |a: Tool, b: Tool| {
            if Self::tool_group(a) != Self::tool_group(b) {
                BAR_APART
            } else if Self::tool_joined(a) {
                0.0
            } else {
                BAR_GAP
            }
        };
        let width = |t: Tool, given: &[Give]| match t {
            Tool::Sort if given.iter().any(|g| matches!(g, Give::SortLabel)) => SORT_W,
            Tool::Sort => SORT_LABEL_W,
            Tool::SaveRotation => SAVE_W,
            Tool::Zoom => ZOOM_W,
            _ => BUTTON_W,
        };
        let run = |tools: &[Tool], given: &[Give]| {
            let mut w = 0.0;
            for (i, &t) in tools.iter().enumerate() {
                if i > 0 {
                    w += gap(tools[i - 1], t);
                }
                w += width(t, given);
            }
            w
        };
        let dropped = |t: Tool, given: &[Give]| given.iter().any(|g| matches!(g, Give::Drop(d) if d.contains(&t)));
        let left_of = |given: &[Give]| {
            let mut v: Vec<Tool> = LEFT_TOOLS.iter().map(|&(t, _)| t).filter(|&t| !dropped(t, given)).collect();
            if self.turns != 0 {
                v.push(Tool::SaveRotation);
            }
            v
        };
        // No Sort while the explorer is open: its columns sort (Faye, 2026-11-10). It's the right group's first, so
        // nothing else moves.
        let explorer = self.explorer_shown();
        let right_of = |given: &[Give]| -> Vec<Tool> {
            RIGHT_TOOLS.iter().map(|&(t, _)| t).filter(|&t| !dropped(t, given) && !(explorer && t == Tool::Sort)).collect()
        };
        let (w, _) = self.size_dip();
        let room = w - SIDE_PAD * 2.0 - 16.0;
        let mut n = 0;
        while n < GIVE.len() && run(&left_of(&GIVE[..n]), &GIVE[..n]) + run(&right_of(&GIVE[..n]), &GIVE[..n]) > room {
            n += 1;
        }
        let given = &GIVE[..n];
        let y = Self::toolbar_y();
        let glyph = |t: Tool| LEFT_TOOLS.iter().chain(RIGHT_TOOLS).find(|(x, _)| *x == t).map_or(0, |&(_, g)| g);
        let mut out = Vec::new();
        let mut x = SIDE_PAD;
        let mut prev: Option<Tool> = None;
        for t in left_of(given) {
            if let Some(p) = prev {
                x += gap(p, t);
            }
            out.push((t, glyph(t), rect(x, y, width(t, given), BUTTON_H)));
            x += width(t, given);
            prev = Some(t);
        }
        let mut x = w - SIDE_PAD;
        let mut prev: Option<Tool> = None;
        for t in right_of(given).into_iter().rev() {
            if let Some(p) = prev {
                x -= gap(t, p);
            }
            x -= width(t, given);
            out.push((t, glyph(t), rect(x, y, width(t, given), BUTTON_H)));
            prev = Some(t);
        }
        out
    }

    pub(super) fn tool_enabled(&self, t: Tool) -> bool {
        let has = self.viewer.current.is_some();
        match t {
            Tool::Settings => true,
            Tool::Previous | Tool::Next => has && self.viewer.image_count() > 1,
            Tool::Sort | Tool::Delete | Tool::More => has,
            Tool::Rotate => has && self.can_rotate(),
            Tool::Edit => has && self.can_edit(),
            // Nothing to zoom while a file Looker can't open covers the photo.
            Tool::ZoomIn | Tool::ZoomOut | Tool::Fit | Tool::Zoom => has && self.placeholder.is_none(),
            Tool::SaveRotation => !self.saving_rotation,
            Tool::Info | Tool::Strip => has,
            Tool::Explorer => has || self.home_explorer(),
            _ => has,
        }
    }

    /// A tooltip for anything: the static ones below, or a thumbnail's file name.
    pub(super) fn tooltip_for(&mut self, h: Hit) -> Option<String> {
        match h {
            Hit::StripCell(i) => self.strip_tooltip(i),
            // A text tab: its file's path (an untitled one, its name).
            Hit::TextTab(i) => self.text.docs.get(i as usize).map(|d| if d.untitled > 0 { d.title() } else { d.path.to_string_lossy().into_owned() }),
            Hit::RecentRow(i) => self.recent_path(i).map(|p| p.to_string_lossy().into_owned()),
            Hit::InfoRow(i) if self.info_row_field(i).is_some() => Some("Click to copy, double-click to edit".to_string()),
            Hit::Tool(Tool::Sort) => Some(self.sort_tooltip()),
            Hit::Tool(Tool::Zoom) if self.model_active() => Some("Drag left or right to zoom, click to reset the view".to_string()),
            Hit::ExplorerRow(_)
            | Hit::Crumb(_)
            | Hit::CrumbMore
            | Hit::ExplorerBack
            | Hit::ExplorerForward
            | Hit::ExplorerUp
            | Hit::ExplorerHistory
            | Hit::ExplorerBookmarks
            | Hit::ExplorerCollections
            | Hit::ExplorerGroup
            | Hit::ExplorerLayout
            | Hit::ExplorerBrowse
            | Hit::Tab(_)
            | Hit::RailIcon(_)
            | Hit::SideRow(_) => self.explorer_tooltip(h),
            _ => Self::tooltip_text(h).map(String::from),
        }
    }

    /// Tooltip text, as in MainWindow.xaml.
    pub(super) fn tooltip_text(h: Hit) -> Option<&'static str> {
        Some(match h {
            Hit::Tool(Tool::Previous) => "Previous (←)",
            Hit::Tool(Tool::Next) => "Next (→)",
            Hit::Tool(Tool::ZoomOut) => "Zoom out (Ctrl+-)",
            Hit::Tool(Tool::Fit) => "Fit to window (Ctrl+0)",
            Hit::Tool(Tool::Zoom) => "Drag left or right to zoom, click for fit or 100 %",
            Hit::Tool(Tool::ZoomIn) => "Zoom in (Ctrl++)",
            Hit::Tool(Tool::More) => "More",
            Hit::Tool(Tool::Fullscreen) => "Fullscreen (F11)",
            Hit::Tool(Tool::Rotate) => "Rotate right (Ctrl+R)",
            Hit::Tool(Tool::Edit) => "Edit: crop, straighten, exposure (E)",
            Hit::Tool(Tool::Delete) => "Delete (Del)",
            Hit::Tool(Tool::Sort) => "Sort",
            Hit::Tool(Tool::SaveRotation) => "Save the rotation to the file (Ctrl+S)",
            Hit::Tool(Tool::Home) => "Home",
            Hit::Tool(Tool::Explorer) => "File explorer (Ctrl+E)",
            Hit::Tool(Tool::Strip) => "Thumbnails (Ctrl+Shift+T)",
            Hit::Tool(Tool::Info) => "Info panel (Ctrl+I)",
            Hit::Tool(Tool::Settings) => "Settings (Ctrl+,)",
            Hit::Reveal => "Show this file in File Explorer",
            Hit::InfoPath => "Show in File Explorer",
            Hit::InfoRow(_) => "Click to copy",
            Hit::InfoEdit(_) => "Edit",
            Hit::InfoSave => "Save the changes into the files (Ctrl+S)",
            Hit::InfoDiscard => "Throw away every unsaved change",
            Hit::InfoAdd => "Add a field this photo doesn't have yet",
            Hit::InfoStar(_) => "Set the rating (the same star again clears it)",
            Hit::PagePrevious => "Previous page (Page Up)",
            Hit::DefaultHintClose => "Don't show this again",
            Hit::PageNext => "Next page (Page Down)",
            Hit::VideoPlay => "Play or pause (Space)",
            Hit::VideoStop => "Stop",
            Hit::TextButton(0) => "Previous result (Shift+F3)",
            Hit::TextButton(1) => "Next result (F3)",
            Hit::TextButton(2) => "Close (Esc)",
            Hit::TextButton(3) => "Match case",
            Hit::TextButton(4) => "Wrap around",
            Hit::TextButton(7) => "Replace (Ctrl+H)",
            Hit::TextFoot(0) => "Encoding",
            Hit::TextFoot(1) => "Line endings",
            Hit::TextTabNew => "New tab (Ctrl+N)",
            Hit::TextTabClose(_) => "Close tab (Ctrl+W)",
            Hit::AudioPrev => "Previous file (\u{2190})",
            Hit::AudioNext => "Next file (\u{2192})",
            Hit::AudioLoop => "Loop",
            Hit::VideoMute => "Mute (M)",
            Hit::VideoVolume => "Volume (\u{2191} \u{2193}, wheel)",
            Hit::VideoSettings => "Settings",
            Hit::VideoTime => "Timecode or frames",
            Hit::VideoFullscreen => "Fullscreen (F11, double-click)",
            Hit::MotionBadge => "Live Photo: play it as a video, or back to the photo",
            _ => return None,
        })
    }

    /// The status row's parts in order, each with how soon it goes when the row is narrow (higher goes first;
    /// 0 stays).
    pub(super) fn status_parts(&self) -> Vec<(u8, String)> {
        if let Some(s) = self.placeholder_status() {
            return vec![(0, s)];
        }
        let Some(path) = self.current_path() else { return Vec::new() };
        let mut parts = Vec::new();
        let cached = self.viewer.current_entry();
        if let Some(c) = cached {
            if let Some(name) = format::display_name(c.format, Some(path)) {
                parts.push((0, name));
            }
            // A text file: its lines and characters (the still is only its thumbnail).
            if c.format == format::Format::Text {
                if let Some((lines, chars)) = self.text_facts() {
                    parts.push((1, format!("{lines} lines")));
                    parts.push((4, format!("{chars} characters")));
                }
                if let Some(i) = &self.info {
                    parts.push((3, format_bytes(i.size)));
                    parts.push((6, format!("Modified {}", format_time(i.modified))));
                }
                return parts;
            }
            // A model has triangles, not pixels (and the still's zoom means nothing).
            if c.format == format::Format::Model {
                if let Some(s) = self.model_status() {
                    parts.push((1, s));
                }
                if let Some(i) = &self.info {
                    parts.push((3, format_bytes(i.size)));
                    parts.push((6, format!("Modified {}", format_time(i.modified))));
                }
                return parts;
            }
            if c.pages > 0 {
                parts.push((4, if c.pages == 1 { "1 page".into() } else { format!("{} pages", c.pages) }));
            }
            let (w, h) = self.current_dims().unwrap_or((c.native_w, c.native_h));
            // An audio file: its format and length (its cover's size means nothing here); an archive: how many files
            // (its thumbnail is a picture inside).
            if c.format == format::Format::Archive {
                if let Some((files, _)) = self.archive_facts() {
                    parts.push((1, format!("{files} file{}", if files == 1 { "" } else { "s" })));
                }
            } else if c.format != format::Format::Audio {
                parts.push((1, format!("{w} × {h}")));
            }
            // A video: its frame rate, codec, HDR, length and bit rate instead of megapixels (Faye, 2026-11-10).
            if matches!(c.format, format::Format::Video | format::Format::Audio) {
                parts.extend(self.video_status());
            } else if let Some(e) = self.current_exif() {
                // A photo: the camera and its exposure, when the file says.
                if let Some(s) = crate::metadata::exposure_label(e) {
                    parts.push((4, s));
                }
                if let Some(s) = crate::metadata::camera_label(e) {
                    parts.push((5, s));
                }
            }
            if c.pages == 0 && !matches!(c.format, format::Format::Video | format::Format::Audio | format::Format::Archive) {
                // a page's 96-dpi size is not a pixel count
                parts.push((5, format!("{:.1} MP", c.native_w as f64 * c.native_h as f64 / 1_000_000.0)));
            }
        }
        if let Some(i) = &self.info {
            if i.size > 0 {
                parts.push((3, format_bytes(i.size)));
            }
        }
        if let Some(t) = cached.and_then(|c| c.taken) {
            parts.push((6, format!("Taken {}", format_time(t))));
        } else if let Some(i) = &self.info {
            parts.push((6, format!("Modified {}", format_time(i.modified))));
        }
        parts
    }

    /// The "3 / 61" at the right of the status row.
    fn status_rank(&self) -> Option<String> {
        let rank = match (&self.viewer.listing, &self.placeholder, self.viewer.index) {
            (Some(l), Some(p), _) => l.rank_of(&p.path).map(|r| (r, l.total_files)),
            (Some(l), None, Some(i)) => Some((l.rank[i], l.total_files)),
            _ => None,
        };
        rank.map(|(r, n)| format!("{} / {}", r + 1, n))
    }

    /// The status row as it fits beside the counter: its text and, when there's room for it too, the "Reveal in
    /// Explorer…" link's rect. A narrow window drops parts (the date first, the format last), then the link.
    pub(super) fn status_line(&self, g: &Gfx) -> (String, Option<D2D_RECT_F>) {
        const LINK: &str = STATUS_LINK;
        let (w, h) = self.size_dip();
        let parts = self.status_parts();
        let counter = self.status_rank().map_or(0.0, |s| g.measure_tabular(&wide(&s), &g.fonts.caption) + 24.0);
        let room = (w - SIDE_PAD * 2.0 - counter).max(0.0);
        let lw = g.measure_tabular(&wide(LINK), &g.fonts.caption);
        let can_link = self.viewer.current.is_some();
        let join = |keep_below: u8| parts.iter().filter(|(p, _)| *p < keep_below).map(|(_, s)| s.as_str()).collect::<Vec<_>>().join("   ·   ");
        for with_link in [true, false] {
            if with_link && !can_link {
                continue;
            }
            for keep_below in (1..=8).rev() {
                let text = join(keep_below);
                let tw = g.measure_tabular(&wide(&text), &g.fonts.caption);
                let need = tw + if with_link { 16.0 + lw } else { 0.0 };
                if need <= room {
                    // A little wider than its measured text: exactly as wide, rounding cut its last letter off.
                    let link = with_link.then(|| rect(SIDE_PAD + tw + 16.0, h - status_h(), lw + 4.0, status_h()));
                    return (text, link);
                }
            }
        }
        (join(1), None) // trimmed with an ellipsis
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
        crate::motion::refresh();
        self.hits.clear();
        self.a11y_marks.clear();
        *g.texts.borrow_mut() = self.a11y_recording().then(Vec::new);
        self.fades.begin();
        let text_moving = self.step_text_scale(&mut g);
        // Clean view's opacity: what floats over the photo is drawn at it, and not at all (nor hit) once gone.
        let ui_k = self.step_clean();
        let ui_on = ui_k > 0.0;
        let sliding = self.step_slides();
        self.prepare_model(&g);
        self.prepare_video(&g);
        self.prepare_audio(&g);
        self.prepare_archive(&g);
        self.prepare_text();
        self.prepare_subtitles(&g);
        g.begin(self.theme().content);
        // Bottom to top: hits added later win.
        self.draw_viewport(&mut g, ui_k);
        g.push_fade(ui_k, 1.0, 0.0, 0.0);
        // Under the cards: where the explorer's edge will hold, while it's dragged near there.
        let editing = self.edit.is_some();
        let guide_moving = !editing && self.draw_sticky_guide(&g);
        let info_moving = !editing && self.draw_info(&g) | guide_moving;
        // The explorer's hover highlights end at once, no fade out (Faye, 2026-11-10).
        self.fades.instant_out = true;
        let card_moving = info_moving | (!editing && self.draw_explorer(&g));
        self.fades.instant_out = false;
        let strip_moving = !editing && self.draw_strip(&g);
        if editing {
            self.draw_edit_panel(&g);
        } else if self.viewer.current.is_some() && self.placeholder.is_none() {
            if ui_on {
                self.draw_page_bar(&g);
            }
            self.draw_view_cube(&g);
            self.draw_video_flash(&g);
            self.draw_video_bar(&g);
            if ui_on {
                self.draw_motion_bar(&g);
            }
        }
        self.update_media();
        if self.chrome() && ui_on && !editing {
            self.draw_toolbar(&g);
            self.draw_status(&g);
        }
        g.pop_fade();
        // The Settings card blurs and dims all of that; the title bar stays over it, as Snapper's does.
        let texts_at_page = g.texts.borrow().as_ref().map_or(0, Vec::len);
        let page_moving = self.draw_page(&mut g);
        if self.chrome() && ui_on {
            g.push_fade(ui_k, 1.0, 0.0, 0.0);
            self.draw_title(&g);
            g.pop_fade();
        }
        let texts_at_overlays = g.texts.borrow().as_ref().map_or(0, Vec::len);
        let overlays_moving = self.draw_overlays(&g);
        if let Err(e) = g.end() {
            crate::trace::mark(format!("present failed: {e}"));
        }
        let texts = g.texts.borrow_mut().take();
        self.gfx = Some(g);
        if let Some(t) = texts {
            self.a11y_publish(t, texts_at_page, texts_at_overlays);
        }
        self.pump_thumbs();
        if text_moving || self.view.animating() || self.fades.moving || overlays_moving || card_moving || strip_moving || sliding || self.clean_moving() || page_moving || self.fade.is_some() || self.video_animating() || self.motion_animating() || self.audio_animating() || self.archive_animating() || self.model_animating() {
            self.invalidate();
        }
    }

    pub(super) fn draw_title(&mut self, g: &Gfx) {
        let t = theme::t();
        let (w, _) = self.size_dip();
        if let Some(icon) = &self.icon {
            let x = g.snap(16.0);
            let y = g.snap((TITLE_H - 16.0) / 2.0);
            g.draw_bitmap(icon, rect(x, y, 16.0, 16.0), 1.0);
        }
        // "photo.jpg - Looker" with a file up (Faye, 2026-11-10), as the window's own title is.
        let title = self.placeholder.as_ref().map(|p| p.path.as_path()).or(self.current_path()).map(|p| format!("{} - Looker", file_name(p))).unwrap_or_else(|| "Looker".into());
        let fg = if self.active { rgb(t.text) } else { rgb(t.placeholder) };
        g.text(&wide(&title), &g.fonts.body, rect(44.0, 0.0, (w - 44.0 - CAPTION_W * 3.0 - 16.0).max(0.0), TITLE_H), fg, Align::Left);

        for (c, glyph) in [
            (Caption::Min, 0xE921u16),
            (Caption::Max, if self.maximized { 0xE923 } else { 0xE922 }),
            (Caption::Close, 0xE8BB),
        ] {
            let r = self.caption_rect(c);
            let k = self.fades.hover(Hit::Caption(c), self.cap_hover == Some(c));
            let pressed = self.cap_pressed == Some(c) && self.cap_hover == Some(c);
            let idle = if self.active { rgb(t.text) } else { rgb(t.placeholder) };
            if c == Caption::Close {
                g.fill(r, gfx::rgba(t.close_hover, if pressed { 0.9 } else { k }));
                let fg = if pressed { gfx::rgba(0xFFFFFF, 0.7) } else { theme::mix(if self.active { t.text } else { t.placeholder }, 0xFFFFFF, k) };
                g.text(&[glyph], &g.fonts.caption_icons, r, fg, Align::Center);
            } else {
                let a = if pressed { 0.04 } else { 0.06 * k };
                if a > 0.0 {
                    g.fill(r, gfx::rgba(t.ink, a));
                }
                g.text(&[glyph], &g.fonts.caption_icons, r, idle, Align::Center);
            }
        }
    }

    /// Hover fade and pressed state of a hit, for drawing it: the hover comes on at once and fades out.
    pub(super) fn state(&mut self, id: Hit, enabled: bool) -> ui::State {
        let over = self.hover == Some(id) && (self.pressed.is_none() || self.pressed == Some(id));
        let hover = self.fades.hover(id, over && enabled);
        ui::State { hover, pressed: self.pressed == Some(id) && self.hover == Some(id), enabled }
    }

    /// What the toolbar's Sort says it sorts by.
    fn sort_label(&self) -> &'static str {
        match self.viewer_sort().field {
            crate::folder::SortField::Name => "Name",
            crate::folder::SortField::Date => "Date",
            crate::folder::SortField::Size => "Size",
        }
    }

    pub(super) fn sort_tooltip(&self) -> String {
        use crate::folder::SortField;
        let s = self.viewer_sort();
        let (field, order) = match (s.field, s.descending) {
            (SortField::Name, false) => ("name", "A to Z"),
            (SortField::Name, true) => ("name", "Z to A"),
            (SortField::Date, false) => ("date modified", "oldest first"),
            (SortField::Date, true) => ("date modified", "newest first"),
            (SortField::Size, false) => ("size", "smallest first"),
            (SortField::Size, true) => ("size", "largest first"),
        };
        format!("Sorted by {field}, {order}")
    }

    pub(super) fn draw_toolbar(&mut self, g: &Gfx) {
        let t = theme::t();
        let tools = self.tool_rects();
        // The buttons float on the photo: the page colour under their (translucent) boxes keeps them looking the
        // same whatever is behind.
        let page = rgb(if self.viewer.current.is_none() { theme::plain(t.content) } else { t.content });
        // A joined group's box: drawn once under its segments, at 45 % when none of them can be used.
        let box_of = |tool: Tool| {
            let mut members = tools.iter().filter(|(x, _, _)| Self::tool_joined(*x) && Self::tool_group(*x) == Self::tool_group(tool));
            let first = members.next()?.2;
            Some(members.fold(first, |a, (_, _, r)| D2D_RECT_F { left: a.left.min(r.left), right: a.right.max(r.right), ..a }))
        };
        let mut boxes: Vec<(u8, D2D_RECT_F)> = Vec::new();
        for &(tool, _, _) in &tools {
            if Self::tool_joined(tool) && !boxes.iter().any(|(gr, _)| *gr == Self::tool_group(tool)) {
                if let Some(b) = box_of(tool) {
                    let enabled = tools.iter().any(|(x, _, _)| Self::tool_group(*x) == Self::tool_group(tool) && self.tool_enabled(*x));
                    g.fill_round(b, 5.0, page);
                    ui::group_frame(g, b, enabled);
                    boxes.push((Self::tool_group(tool), b));
                }
            }
        }
        for &(tool, glyph, r) in &tools {
            let enabled = self.tool_enabled(tool);
            let id = Hit::Tool(tool);
            self.hits.add(id, r);
            let st = self.state(id, enabled);
            if let Some(&(_, b)) = boxes.iter().find(|(gr, _)| Self::tool_joined(tool) && *gr == Self::tool_group(tool)) {
                let on = match tool {
                    Tool::Info => self.info_shown(),
                    Tool::Strip => self.strip_shown(),
                    Tool::Explorer => self.explorer_shown(),
                    _ => false,
                };
                let fg = ui::segment(g, b, r, on, &st);
                if tool == Tool::Zoom {
                    if self.model_active() {
                        g.text(&[0xE9A6], &g.fonts.icons, r, fg, Align::Center);
                    } else {
                        let level = if self.photo_up() { format!("{:.0}%", self.view.zoom_percent()) } else { "\u{2013}".into() };
                        g.text_tabular(&wide(&level), &g.fonts.body, r, fg, Align::Center);
                    }
                } else {
                    g.text(&[glyph], &g.fonts.icons, r, fg, Align::Center);
                }
                continue;
            }
            g.fill_round(r, 5.0, page);
            if tool == Tool::SaveRotation {
                let fg = ui::button_frame(g, r, ui::Kind::Accent, &st);
                g.text(&wide("Save"), &g.fonts.body, r, fg, Align::Center);
                continue;
            }
            let kind = match tool {
                Tool::Settings => ui::Kind::Toggle(self.page_open()),
                // A drop-down button is picked out while its menu is open.
                Tool::Sort => ui::Kind::Toggle(matches!(self.menu, Some((menus::MenuKind::Sort, _)))),
                Tool::More => ui::Kind::Toggle(matches!(self.menu, Some((menus::MenuKind::More, _)))),
                _ => ui::Kind::Standard,
            };
            let fg = ui::button_frame(g, r, kind, &st);
            if tool == Tool::Sort {
                // Icon, label and chevron from the left, the same padding at both ends (the chevron a pixel right:
                // its ink sits that far in from its cell). A narrow window drops the label.
                let pad = if r.right - r.left >= SORT_LABEL_W { 12.0 } else { 14.0 };
                g.text(&[glyph], &g.fonts.icons, rect(r.left + pad, r.top, 16.0, BUTTON_H), fg, Align::Center);
                let chevron = r.right - pad - 12.0 + 1.0;
                if r.right - r.left >= SORT_LABEL_W {
                    let x = r.left + pad + 16.0 + 6.0;
                    g.text(&wide(self.sort_label()), &g.fonts.body, rect(x, r.top, (chevron - 4.0 - x).max(0.0), BUTTON_H), fg, Align::Left);
                }
                g.text(&[0xE70D], &g.fonts.caption_icons, rect(chevron, r.top, 12.0, BUTTON_H), fg, Align::Center);
            } else {
                g.text(&[glyph], &g.fonts.icons, r, fg, Align::Center);
            }
            // The accent dot: a newer version is waiting (Settings > Updates).
            if tool == Tool::Settings && self.update == crate::store::UpdateStatus::Available {
                g.fill_round(rect(r.right - 9.0, r.top + 3.0, 6.0, 6.0), 3.0, rgb(t.accent));
            }
        }
        // The joined boxes' outlines and the hairlines between their segments, over the segments' fills.
        for (group, b) in boxes {
            let mut segs: Vec<(D2D_RECT_F, bool)> = tools
                .iter()
                .filter(|(x, _, _)| Self::tool_group(*x) == group)
                .map(|&(x, _, r)| (r, matches!(x, Tool::Info if self.info_shown()) || matches!(x, Tool::Strip if self.strip_shown()) || matches!(x, Tool::Explorer if self.explorer_shown())))
                .collect();
            segs.sort_by(|a, b| a.0.left.total_cmp(&b.0.left));
            let enabled = tools.iter().any(|(x, _, _)| Self::tool_group(*x) == group && self.tool_enabled(*x));
            ui::group_edges(g, b, &segs, enabled);
        }
    }

    /// The toolbar's zoom level clicked: fit or 100 % about the middle of the photo's area.
    pub(super) fn status_zoom_click(&mut self) {
        if !self.photo_up() {
            return;
        }
        let (vw, vh) = self.view_size();
        self.view.toggle_fit_actual(vw / 2.0, vh / 2.0);
        self.after_zoom();
    }

    pub(super) fn draw_status(&mut self, g: &Gfx) {
        let (w, h) = self.size_dip();
        // A photo's camera and exposure come from its EXIF (read once per file, as the info card does).
        if self.placeholder.is_none() && self.viewer.current_entry().is_some_and(|c| !matches!(c.format, format::Format::Video | format::Format::Audio | format::Format::Model | format::Format::Text)) {
            self.request_exif();
        }
        // Centred in the whole row (an inset box sat the text 1.5 px high).
        let y = h - status_h();
        let row_h = status_h();
        let (text, link) = self.status_line(g);
        if text.is_empty() {
            return;
        }
        let rank = self.status_rank();
        // The title bar's colour (dimmed with it when the window isn't active).
        let label = if self.active { rgb(theme::t().text) } else { rgb(theme::t().placeholder) };
        let counter_w = rank.as_ref().map_or(0.0, |s| g.measure_tabular(&wide(s), &g.fonts.caption) + 24.0);
        g.text_tabular(&wide(&text), &g.fonts.caption, rect(SIDE_PAD, y, (w - SIDE_PAD * 2.0 - counter_w).max(0.0), row_h), label, Align::Left);
        if let Some(r) = link {
            self.hits.add(Hit::Reveal, r);
            let st = self.state(Hit::Reveal, true);
            ui::link_fill(g, D2D_RECT_F { left: r.left - 6.0, top: r.top + 5.0, right: r.right + 6.0, bottom: r.bottom - 5.0 }, st.hover, st.pressed);
            g.text_tabular(&wide(STATUS_LINK), &g.fonts.caption, r, ui::link_color(st.hover), Align::Left);
        }
        if let Some(text) = rank {
            g.text_tabular(&wide(&text), &g.fonts.caption, rect(SIDE_PAD, y, (w - SIDE_PAD * 2.0).max(0.0), row_h), label, Align::Right);
        }
    }

    pub(super) fn draw_viewport(&mut self, g: &mut Gfx, ui_k: f32) {
        let v = self.viewport();
        self.hits.add(Hit::Viewport, v);
        if self.viewer.current.is_none() {
            // The home screen's page carries no tint (Faye, 2026-11-10): the theme's page colour as a grey.
            g.begin_ui(Some(theme::plain(self.theme().content)));
            self.draw_landing(g);
            return;
        }
        let bg = self.photo_layer();
        if self.audio_shown() {
            // An audio file sits on the page itself (its cover is a picture of its own, not a photo on a canvas).
            g.fill(bg, g.photo_rgb(self.theme().content));
        } else if self.fullscreen.is_some() && self.video_shown() {
            // A video in fullscreen: pure black around it, no checkerboard, like a cinema (Faye, 2026-11-11).
            g.fill(bg, g.photo_rgb(0x000000));
        } else if self.checkerboard_on() {
            g.checkerboard(bg);
        } else {
            g.fill(bg, g.photo_rgb(gfx::checker_plain()));
        }
        self.last_drawn = None;
        let doc = self.viewer.shown.clone().filter(|e| e.layout.is_some() && self.turns == 0);
        if doc.is_none() {
            self.want_pages(Vec::new());
        }
        // A file Looker can't open covers the photo: the photo isn't drawn at all (it would show around the
        // cover, behind the floating bars).
        let model = self.draw_model(g) || self.draw_audio_photo(g) || self.text_shown() || self.archive_shown();
        if let Some(c) = self.viewer.shown.clone().filter(|_| self.placeholder.is_none() && !model) {
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
                    g.dev.dc.PushAxisAlignedClip(&bg, windows::Win32::Graphics::Direct2D::D2D1_ANTIALIAS_MODE_ALIASED);
                }
                let frames = c.frames.borrow();
                let frame = &frames[self.viewer.anim_frame.min(frames.len() - 1)].0;
                // A full-size photo at 100 % and up is drawn as hard pixels: lined up with the screen's own, or it
                // shows lines at some window sizes (`Gfx::pixel_rect`). `dest` is its turned bounds.
                let dest = if doc.is_none() && c.full_size() {
                    let px = unsafe { frame.GetPixelSize() };
                    let (tw, th) = if self.turns & 1 == 1 { (px.height, px.width) } else { (px.width, px.height) };
                    g.pixel_rect(dest, tw, th).unwrap_or(dest)
                } else {
                    dest
                };
                if self.edit.is_some() {
                    self.draw_edit_photo(g, frame);
                } else if let Some(d) = &doc {
                    self.draw_pages(g, d, dest, bg);
                } else if let Some((v, sdr)) = self.video_bitmap() {
                    // A video's picture: upright and at the still's size, so the same box.
                    g.draw_photo(&v, dest, 1.0, sdr);
                    self.last_drawn = Some((v, dest, sdr));
                } else if self.turns == 0 {
                    g.draw_photo_px(frame, dest, 1.0, c.sdr(), c.full_size());
                    self.last_drawn = Some((frame.clone(), dest, c.sdr()));
                } else {
                    // `dest` is the turned image's bounds: draw the unturned frame into the box that lands on
                    // it once turned about the centre (width and height swap for odd turns).
                    let (cx, cy) = ((dest.left + dest.right) / 2.0, (dest.top + dest.bottom) / 2.0);
                    let (w, h) = (dest.right - dest.left, dest.bottom - dest.top);
                    let (dw, dh) = if self.turns & 1 == 1 { (h, w) } else { (w, h) };
                    let unturned = rect(cx - dw / 2.0, cy - dh / 2.0, dw, dh);
                    g.with_transform(&quarter_turns(self.turns, cx, cy), || g.draw_photo_px(frame, unturned, 1.0, c.sdr(), c.full_size()));
                }
                unsafe {
                    g.dev.dc.PopAxisAlignedClip();
                }
                if !self.first_image_traced {
                    self.first_image_traced = true;
                    crate::trace::mark("first frame with the image drawn");
                }
            }
        }
        if self.placeholder.is_none() {
            self.draw_fade(g, bg);
        }
        // The photo layer ends here (see Gfx::begin_ui).
        g.begin_ui(None);
        self.draw_audio(g);
        self.draw_text_view(g);
        self.draw_archive(g);
        // Not under a placeholder (a folder, or a file Looker can't open, picked in the explorer): its own card
        // says what it is, and the viewer's error is the last file's (the text sat across the card's icon).
        if self.viewer.shown.is_none() && self.viewer.error.is_some() && !self.archive_shown() && self.placeholder.is_none() {
            self.draw_cant_display(g);
        }
        self.draw_placeholder(g);
        if ui_k > 0.0 {
            g.push_fade(ui_k, 1.0, 0.0, 0.0);
            self.draw_scrims(g, bg);
            g.pop_fade();
        }
        if self.placeholder.is_none() {
            self.draw_subtitles(g, bg);
        }
    }

    /// Gradients behind the bars, so they read on a bright photo (black in the dark themes, the page colour in
    /// the light one, whose text is dark): down from the top to the bottom of the toolbar's buttons, and up from
    /// the bottom to the top of the status text or, while the strip shows, of its cells (following them as it
    /// slides).
    fn draw_scrims(&self, g: &Gfx, bg: D2D_RECT_F) {
        let t = theme::t();
        let (colour, alpha) = if t.dark { (0x000000, 0.7) } else { (t.content, 0.85) };
        // Fading from the window's edge all the way: alpha falls as (1 - t)^1.5 over the gradient.
        let edge_out: Vec<_> =
            [0.0f32, 0.25, 0.5, 0.75, 1.0].iter().map(|&k| (k, gfx::rgba(colour, alpha * (1.0 - k).powf(1.5)))).collect();
        // Solid status row (Appearance): filled with the theme's bar colour (neutral, Black's black) and no
        // gradient under it; the title bar and toolbar keep theirs (Faye, 2026-11-10: the status row only).
        let solid = self.chrome() && self.settings.bars_opaque;
        if solid {
            g.fill(D2D_RECT_F { top: self.content_bottom(), ..bg }, rgb(t.bar));
        }
        if self.chrome() {
            let buttons_bottom = Self::toolbar_y() + BUTTON_H;
            g.fill_vgradient(D2D_RECT_F { bottom: bg.top + buttons_bottom, ..bg }, &edge_out);
        }
        // The status text: 12 px, centred in its row (~16 px line).
        let text_top = (self.chrome() && !solid).then(|| bg.bottom - status_h() / 2.0 - 8.0);
        let top = match (text_top, self.strip_cells_top()) {
            (Some(a), Some(b)) => a.min(b),
            (a, b) => match a.or(b) {
                Some(t) => t,
                None => return,
            },
        };
        if top < bg.bottom {
            let edge_in: Vec<_> = edge_out.iter().rev().map(|&(t, c)| (1.0 - t, c)).collect();
            g.fill_vgradient(D2D_RECT_F { top, ..bg }, &edge_in);
        }
    }

    /// "Can't display this file", and when the reason is a Windows extension that isn't installed, which one and
    /// a button to its Store page (as the Photos app does).
    fn draw_cant_display(&mut self, g: &Gfx) {
        let a = self.image_area();
        let Some(path) = self.current_path().map(Path::to_path_buf) else { return };
        if self.missing_extension.as_ref().is_none_or(|(p, _)| *p != path) {
            let ext = crate::store::missing_extension(format::sniff_file(&path));
            self.missing_extension = Some((path, ext));
        }
        let why = self.missing_extension.as_ref().and_then(|(_, e)| e.as_ref()).map(|e| e.why);
        let cy = (a.top + a.bottom) / 2.0;
        let Some(why) = why else {
            g.text(&wide("Can't display this file"), &g.fonts.body, a, rgb(theme::t().label), Align::Center);
            return;
        };
        let cx = (a.left + a.right) / 2.0;
        let w = (a.right - a.left - 32.0).clamp(1.0, 480.0);
        g.text(&wide("Can't display this file"), &g.fonts.title, rect(cx - w / 2.0, cy - 60.0, w, 22.0), rgb(theme::t().text), Align::Center);
        g.text(&wide(why), &g.fonts.body, rect(cx - w / 2.0, cy - 30.0, w, 20.0), rgb(theme::t().label), Align::Center);
        let label = "Get it from the Microsoft Store";
        let bw = (g.measure(&wide(label), &g.fonts.body) + 32.0).ceil();
        let b = rect(g.snap(cx - bw / 2.0), g.snap(cy + 6.0), bw, 30.0);
        self.hits.add(Hit::GetExtension, b);
        let st = self.state(Hit::GetExtension, true);
        let fg = ui::button_frame(g, b, ui::Kind::Accent, &st);
        g.text(&wide(label), &g.fonts.body, b, fg, Align::Center);
    }

    pub(super) fn get_extension(&mut self) {
        if let Some((_, Some(e))) = &self.missing_extension {
            page::shell_open(&format!("ms-windows-store://pdp/?ProductId={}", e.product_id));
        }
    }

    /// The toast, menus, dialogs and tooltips, above everything. Returns whether any of them is moving.
    fn draw_overlays(&mut self, g: &Gfx) -> bool {
        let mut moving = false;
        if self.explorer_shown() {
            moving |= self.draw_crumb_popup(g);
        }
        moving |= self.draw_drag_label(g);
        if let Some(t) = &self.toast {
            // Top middle of the whole window, in the toolbar's row (fullscreen: 16 in from the top); a player's
            // readout centred just above its seek bar (Faye, 2026-11-11).
            let (w, _) = self.size_dip();
            let top = if self.chrome() { TITLE_H + (TOOLBAR_H - ui::Toast::H) / 2.0 } else { 16.0 };
            let mut at = (w / 2.0, top);
            if t.seek && self.video_shown() {
                if let Some(bar) = self.seek_bar_rect(g) {
                    let floor = self.viewport().top + 4.0;
                    at = ((bar.left + bar.right) / 2.0, (bar.top - 10.0 - ui::Toast::H).max(floor));
                }
            }
            match t.phase() {
                ui::ToastPhase::Fading(a) => {
                    t.draw(g, at.0, at.1, a);
                    moving = true;
                }
                ui::ToastPhase::Holding(ms) => {
                    t.draw(g, at.0, at.1, 1.0);
                    unsafe {
                        SetTimer(Some(self.hwnd), TIMER_TOAST, ms, None);
                    }
                }
                ui::ToastPhase::Done => self.toast = None,
            }
        }
        self.draw_rip(g);
        self.draw_extract(g);
        // A submenu row under the pointer opens its menu beside it; another row of the menu closes it.
        let hovered = match self.hover {
            Some(Hit::MenuItem(i)) => Some(i),
            _ => None,
        };
        let (w, h) = self.size_dip();
        if let Some((_, m)) = &mut self.menu {
            match hovered {
                Some(i) if i < ui::MENU_SUB && m.is_sub(i) => {
                    if m.sub.as_ref().map(|s| s.0) != Some(i) {
                        m.open_sub(g, i, rect(0.0, 0.0, w, h));
                    }
                }
                Some(i) if i < ui::MENU_SUB => m.close_sub(),
                _ => {}
            }
        }
        // Menus that closed, playing their opening backwards; never hit-tested.
        self.menus_out.retain(|m| !m.gone());
        for m in &self.menus_out {
            m.draw(g, None, None);
            moving = true;
        }
        if let Some((_, m)) = &self.menu {
            self.hits.add(Hit::MenuSurface, m.bounds());
            for (i, r) in m.item_rects() {
                self.hits.add(Hit::MenuItem(i), r);
            }
            for (i, k, r) in m.swatch_rects() {
                self.hits.add(Hit::MenuSwatch(i, k), r);
            }
            if let Some((_, s)) = &m.sub {
                self.hits.add(Hit::MenuSurface, s.bounds());
                for (i, r) in s.item_rects() {
                    self.hits.add(Hit::MenuItem(ui::MENU_SUB + i), r);
                }
            }
            let hover = hovered.filter(|&i| i < ui::MENU_SUB);
            let swatch = match self.hover {
                Some(Hit::MenuSwatch(i, k)) => Some((i, k)),
                _ => None,
            };
            m.draw(g, hover, swatch);
            moving |= m.moving();
            if let Some((_, s)) = &m.sub {
                s.draw(g, hovered.filter(|&i| i >= ui::MENU_SUB).map(|i| i - ui::MENU_SUB), None);
                moving |= s.moving();
            }
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
            let mut choice_states = Vec::new();
            for &(i, j, r) in &layout.choice_rows {
                self.hits.add(Hit::DialogChoice(i, j), r);
                choice_states.push(self.state(Hit::DialogChoice(i, j), true));
            }
            if let Some((_, d)) = &mut self.dialog {
                self.caret_rect = d.draw(g, &layout, window, &states, field_hover, clear, &choice_states);
                moving |= d.moving();
            }
        }
        if let Some(t) = self.tooltip {
            if let (Some(text), Some(anchor)) = (self.tooltip_for(t), self.explorer_tip_anchor(t)) {
                let (w, h) = self.size_dip();
                ui::tooltip(g, &text, anchor, rect(0.0, 0.0, w, h));
            }
        }
        moving
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
