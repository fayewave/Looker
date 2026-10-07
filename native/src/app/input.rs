//! Window messages: the custom frame (caption removal, hit-testing, caption buttons), pointer and keyboard
//! input, timers, and the cross-thread notifications from the decode pool and the folder lister.

use super::*;

impl App {
    // --- Window messages ----------------------------------------------------------------------------

    pub(super) fn dip_from_lparam(&self, lp: LPARAM) -> (f32, f32) {
        let x = (lp.0 & 0xFFFF) as i16 as f32;
        let y = ((lp.0 >> 16) & 0xFFFF) as i16 as f32;
        (x / self.scale(), y / self.scale())
    }

    /// Where a pointer zoom aims, in image-area coordinates: the pointer, or the middle (Settings).
    pub(super) fn zoom_anchor(&self, x: f32, y: f32) -> (f64, f64) {
        let a = self.image_area();
        if self.settings.zoom_center {
            (((a.right - a.left) / 2.0) as f64, ((a.bottom - a.top) / 2.0) as f64)
        } else {
            ((x - a.left) as f64, (y - a.top) as f64)
        }
    }

    pub(super) fn screen_to_dip(&self, lp: LPARAM) -> (f32, f32) {
        let mut pt = POINT { x: (lp.0 & 0xFFFF) as i16 as i32, y: ((lp.0 >> 16) & 0xFFFF) as i16 as i32 };
        unsafe {
            let _ = ScreenToClient(self.hwnd, &mut pt);
        }
        (pt.x as f32 / self.scale(), pt.y as f32 / self.scale())
    }

    pub(super) fn set_hover(&mut self, h: Option<Hit>) {
        if self.hover != h {
            self.hover = h;
            self.hide_tooltip();
            if h.and_then(|h| self.tooltip_for(h)).is_some() && self.menu.is_none() {
                unsafe {
                    SetTimer(Some(self.hwnd), TIMER_TOOLTIP, ui::TOOLTIP_DELAY_MS, None);
                }
            }
            self.invalidate();
        }
    }

    pub(super) fn hide_tooltip(&mut self) {
        unsafe {
            let _ = KillTimer(Some(self.hwnd), TIMER_TOOLTIP);
        }
        if self.tooltip.take().is_some() {
            self.invalidate();
        }
    }

    /// Keys while a menu is open: it has them all (arrows, Enter, Escape), like a WinUI flyout.
    fn menu_key(&mut self, vk: VIRTUAL_KEY) {
        match vk {
            VK_ESCAPE => self.close_menu(),
            VK_UP | VK_DOWN => {
                if let Some((_, m)) = &mut self.menu {
                    m.move_cursor(vk == VK_DOWN);
                }
                self.invalidate();
            }
            VK_RETURN | VK_SPACE => {
                if let Some(i) = self.menu.as_ref().and_then(|(_, m)| m.cursor) {
                    self.activate_menu(i);
                }
            }
            _ => {}
        }
    }

    pub(super) fn set_cap_hover(&mut self, c: Option<Caption>) {
        if self.cap_hover != c {
            self.cap_hover = c;
            self.invalidate();
        }
    }

    pub(super) unsafe fn handle(&mut self, msg: u32, wp: WPARAM, lp: LPARAM) -> Option<LRESULT> {
        unsafe {
            match msg {
                WM_NCCALCSIZE if wp.0 != 0 && self.fullscreen.is_none() => {
                    let params = &mut *(lp.0 as *mut NCCALCSIZE_PARAMS);
                    let top = params.rgrc[0].top;
                    DefWindowProcW(self.hwnd, msg, wp, lp);
                    params.rgrc[0].top = top;
                    if IsZoomed(self.hwnd).as_bool() {
                        params.rgrc[0].top += frame_px(self.dpi).1;
                    }
                    Some(LRESULT(0))
                }
                WM_NCHITTEST => {
                    let r = DefWindowProcW(self.hwnd, msg, wp, lp);
                    if r.0 as u32 != HTCLIENT || self.fullscreen.is_some() {
                        return Some(r);
                    }
                    let (x, y) = self.screen_to_dip(lp);
                    let (_, fy) = frame_px(self.dpi);
                    let border = fy as f32 / self.scale();
                    if !IsZoomed(self.hwnd).as_bool() && y < border {
                        let (w, _) = self.size_dip();
                        return Some(LRESULT(if x < border * 2.0 {
                            HTTOPLEFT
                        } else if x > w - border * 2.0 {
                            HTTOPRIGHT
                        } else {
                            HTTOP
                        } as isize));
                    }
                    for (c, ht) in [(Caption::Close, HTCLOSE), (Caption::Max, HTMAXBUTTON), (Caption::Min, HTMINBUTTON)] {
                        if contains(&self.caption_rect(c), x, y) {
                            return Some(LRESULT(ht as isize));
                        }
                    }
                    if y < TITLE_H {
                        return Some(LRESULT(HTCAPTION as isize));
                    }
                    Some(LRESULT(HTCLIENT as isize))
                }
                WM_NCMOUSEMOVE => {
                    let c = caption_for(wp.0 as u32);
                    self.set_cap_hover(c);
                    self.set_hover(None);
                    if c.is_some() {
                        let mut tme = TRACKMOUSEEVENT {
                            cbSize: size_of::<TRACKMOUSEEVENT>() as u32,
                            dwFlags: TME_LEAVE | TME_NONCLIENT,
                            hwndTrack: self.hwnd,
                            dwHoverTime: 0,
                        };
                        let _ = TrackMouseEvent(&mut tme);
                        self.tracking = false;
                        return Some(LRESULT(0));
                    }
                    None
                }
                WM_NCMOUSELEAVE => {
                    self.set_cap_hover(None);
                    if self.cap_pressed.take().is_some() {
                        self.invalidate();
                    }
                    None
                }
                WM_NCLBUTTONDOWN | WM_NCLBUTTONDBLCLK => {
                    if let Some(c) = caption_for(wp.0 as u32) {
                        self.cap_pressed = Some(c);
                        self.invalidate();
                        return Some(LRESULT(0));
                    }
                    None
                }
                WM_NCLBUTTONUP => {
                    if let Some(c) = caption_for(wp.0 as u32) {
                        if self.cap_pressed.take() == Some(c) {
                            match c {
                                Caption::Min => {
                                    let _ = ShowWindow(self.hwnd, SW_MINIMIZE);
                                }
                                Caption::Max => {
                                    let _ = ShowWindow(self.hwnd, if self.maximized { SW_RESTORE } else { SW_MAXIMIZE });
                                }
                                Caption::Close => {
                                    let _ = PostMessageW(Some(self.hwnd), WM_CLOSE, WPARAM(0), LPARAM(0));
                                }
                            }
                        }
                        self.invalidate();
                        return Some(LRESULT(0));
                    }
                    None
                }
                WM_MOUSEMOVE => {
                    let (x, y) = self.dip_from_lparam(lp);
                    if !self.tracking {
                        let mut tme = TRACKMOUSEEVENT {
                            cbSize: size_of::<TRACKMOUSEEVENT>() as u32,
                            dwFlags: TME_LEAVE,
                            hwndTrack: self.hwnd,
                            dwHoverTime: 0,
                        };
                        let _ = TrackMouseEvent(&mut tme);
                        self.tracking = true;
                    }
                    self.set_cap_hover(None);
                    if self.text_drag.is_some() {
                        self.field_drag(x);
                    }
                    if self.info_card.drag.is_some() {
                        self.info_grip_drag(x);
                    }
                    if self.strip.drag.is_some() {
                        self.strip_grip_drag(y);
                    }
                    if self.explorer.drag.is_some() {
                        self.explorer_grip_drag(x);
                    }
                    if let Some((px, py)) = self.drag {
                        self.view.pan((x - px) as f64, (y - py) as f64);
                        self.sync_page_to_view(); // dragging across a document moves the page bar along
                        self.drag = Some((x, y));
                        self.invalidate();
                    }
                    self.mouse = (x, y);
                    let h = self.hit(x, y);
                    self.set_hover(h);
                    Some(LRESULT(0))
                }
                WM_MOUSELEAVE => {
                    self.tracking = false;
                    self.set_hover(None);
                    self.hide_tooltip();
                    Some(LRESULT(0))
                }
                WM_SETCURSOR if (lp.0 & 0xFFFF) as u32 == HTCLIENT => {
                    let cursor = match self.hover {
                        Some(Hit::Reveal | Hit::InfoPath) => IDC_HAND,
                        Some(Hit::InfoGrip | Hit::ExplorerGrip) => IDC_SIZEWE,
                        Some(Hit::StripGrip) => IDC_SIZENS,
                        Some(Hit::DialogField) => IDC_IBEAM,
                        _ => IDC_ARROW,
                    };
                    SetCursor(LoadCursorW(None, cursor).ok());
                    Some(LRESULT(1))
                }
                WM_LBUTTONDOWN => {
                    let (x, y) = self.dip_from_lparam(lp);
                    self.hide_tooltip();
                    let h = self.hit(x, y);
                    // Light dismiss: a press outside an open menu only closes it.
                    if self.menu.is_some() && !matches!(h, Some(Hit::MenuItem(_) | Hit::MenuSurface)) {
                        let reopening_sort = h == Some(Hit::Tool(Tool::Sort));
                        self.close_menu();
                        if !reopening_sort {
                            return Some(LRESULT(0));
                        }
                    }
                    SetCapture(self.hwnd);
                    self.pressed = h;
                    if h == Some(Hit::DialogField) {
                        self.field_press(x, false);
                    }
                    if h == Some(Hit::InfoGrip) {
                        self.info_grip_press(x);
                    }
                    if h == Some(Hit::StripGrip) {
                        self.strip_grip_press(y);
                    }
                    if h == Some(Hit::ExplorerGrip) {
                        self.explorer_grip_press(x);
                    }
                    if h == Some(Hit::Viewport) && self.view.has_content() {
                        self.drag = Some((x, y));
                    }
                    self.invalidate();
                    Some(LRESULT(0))
                }
                WM_LBUTTONUP => {
                    let _ = ReleaseCapture();
                    let (x, y) = self.dip_from_lparam(lp);
                    let pressed = self.pressed.take();
                    let was_drag = self.drag.take().is_some();
                    self.text_drag = None;
                    self.info_grip_release();
                    self.strip_grip_release();
                    self.explorer_grip_release();
                    let h = self.hit(x, y);
                    if pressed.is_some() && pressed == h {
                        match h {
                            Some(Hit::Tool(t)) => self.act(t),
                            Some(Hit::Reveal | Hit::InfoPath) => self.reveal(),
                            Some(Hit::StripCell(i)) => self.strip_click(i),
                            Some(Hit::ExplorerRow(i)) => self.explorer_click(i),
                            Some(Hit::ExplorerBack) => self.explorer_back(),
                            Some(Hit::ExplorerForward) => self.explorer_forward(),
                            Some(Hit::Crumb(i)) => self.crumb_click(i),
                            Some(Hit::CrumbMore) => self.open_crumb_menu(),
                            Some(Hit::Open) => self.open_dialog(),
                            Some(Hit::PageBack) => self.close_page(),
                            Some(Hit::Combo(s)) => self.open_combo(s),
                            Some(Hit::Toggle(s)) => self.toggle_setting(s),
                            Some(Hit::PageLink(i)) => self.page_link(i),
                            Some(Hit::LandingFolder) => self.open_folder_dialog(),
                            Some(Hit::RecentRow(i)) => self.open_recent(i),
                            Some(Hit::RecentClear) => self.clear_recents(),
                            Some(Hit::DefaultHint) => self.page_link(5),
                            Some(Hit::GetExtension) => self.get_extension(),
                            Some(Hit::DefaultHintClose) => self.dismiss_default_hint(),
                            Some(Hit::MenuItem(i)) => self.activate_menu(i),
                            Some(Hit::DialogButton(i)) => self.dialog_click(i),
                            Some(Hit::DialogFieldClear) => self.field_clear(),
                            Some(Hit::PagePrevious) => self.turn_page(-1),
                            Some(Hit::PageNext) => self.turn_page(1),
                            _ => {}
                        }
                    }
                    if was_drag {
                        self.schedule_upgrade();
                    }
                    self.invalidate();
                    Some(LRESULT(0))
                }
                WM_RBUTTONUP => {
                    if self.dialog.is_some() {
                        return Some(LRESULT(0));
                    }
                    let (x, y) = self.dip_from_lparam(lp);
                    match self.hit(x, y) {
                        Some(Hit::ExplorerRow(i)) => {
                            self.close_menu();
                            self.explorer_menu(i, x, y);
                        }
                        Some(Hit::RecentRow(i)) => {
                            self.close_menu();
                            self.open_recent_menu(i, x, y);
                        }
                        Some(Hit::Strip | Hit::StripCell(_) | Hit::StripGrip) => {
                            self.close_menu();
                            self.open_strip_menu(x, y);
                        }
                        Some(Hit::Viewport) | Some(Hit::MenuSurface) | Some(Hit::MenuItem(_)) if self.viewer.current.is_some() => {
                            self.open_context_menu(x, y);
                        }
                        _ => self.close_menu(),
                    }
                    Some(LRESULT(0))
                }
                WM_LBUTTONDBLCLK => {
                    let (x, y) = self.dip_from_lparam(lp);
                    if self.hit(x, y) == Some(Hit::InfoGrip) {
                        self.info_grip_reset();
                        return Some(LRESULT(0));
                    }
                    if self.hit(x, y) == Some(Hit::StripGrip) {
                        self.strip_grip_reset();
                        return Some(LRESULT(0));
                    }
                    if self.hit(x, y) == Some(Hit::ExplorerGrip) {
                        self.explorer_grip_reset();
                        return Some(LRESULT(0));
                    }
                    if let Some(Hit::ExplorerRow(i)) = self.hit(x, y) {
                        if self.menu.is_none() && self.dialog.is_none() {
                            self.explorer_double_click(i);
                            return Some(LRESULT(0));
                        }
                    }
                    if self.hit(x, y) == Some(Hit::DialogField) {
                        self.pressed = Some(Hit::DialogField);
                        SetCapture(self.hwnd);
                        self.field_press(x, true);
                        return Some(LRESULT(0));
                    }
                    if self.menu.is_some() || self.dialog.is_some() {
                        // Treat as a fresh press: it may dismiss the menu or pick an item.
                        return self.handle(WM_LBUTTONDOWN, wp, lp);
                    }
                    if self.hit(x, y) == Some(Hit::Viewport) && self.view.has_content() {
                        let (ax, ay) = self.zoom_anchor(x, y);
                        self.view.toggle_fit_actual(ax, ay);
                        self.after_zoom();
                    } else {
                        // A double-click on a button is two clicks.
                        let h = self.hit(x, y);
                        self.pressed = h;
                        SetCapture(self.hwnd);
                    }
                    Some(LRESULT(0))
                }
                WM_CAPTURECHANGED => {
                    self.drag = None;
                    self.info_grip_release();
                    self.strip_grip_release();
                    self.explorer_grip_release();
                    None
                }
                WM_MOUSEWHEEL => {
                    if self.dialog.is_some() {
                        return Some(LRESULT(0));
                    }
                    self.close_menu();
                    self.hide_tooltip();
                    let delta = ((wp.0 >> 16) & 0xFFFF) as i16 as f64;
                    let (x, y) = self.screen_to_dip(lp);
                    if self.page.is_some() {
                        self.scroll_page(delta as f32);
                        return Some(LRESULT(0));
                    }
                    if self.viewer.current.is_none() {
                        self.scroll_landing(delta as f32);
                        return Some(LRESULT(0));
                    }
                    if matches!(self.hit(x, y), Some(Hit::Strip | Hit::StripCell(_) | Hit::StripGrip)) {
                        self.scroll_strip(delta as f32);
                        return Some(LRESULT(0));
                    }
                    if matches!(
                        self.hit(x, y),
                        Some(Hit::ExplorerCard | Hit::ExplorerRow(_) | Hit::ExplorerGrip | Hit::ExplorerBack | Hit::ExplorerForward | Hit::Crumb(_) | Hit::CrumbMore)
                    ) {
                        self.scroll_explorer(delta as f32);
                        return Some(LRESULT(0));
                    }
                    if matches!(self.hit(x, y), Some(Hit::InfoCard | Hit::InfoPath | Hit::InfoGrip)) {
                        self.scroll_info(delta as f32);
                        return Some(LRESULT(0));
                    }
                    if self.hit(x, y) == Some(Hit::Viewport) && self.view.has_content() {
                        let ctrl = GetKeyState(VK_CONTROL.0 as i32) < 0;
                        if self.settings.wheel_navigates && !ctrl {
                            // A notch a step: down is next. Fine-grained wheels add up to a notch first.
                            self.wheel_acc += delta;
                            while self.wheel_acc.abs() >= 120.0 {
                                let dir = -self.wheel_acc.signum();
                                self.wheel_acc += dir * 120.0;
                                self.step(dir as isize);
                            }
                            return Some(LRESULT(0));
                        }
                        let (ax, ay) = self.zoom_anchor(x, y);
                        self.view.zoom_at(1.2f64.powf(delta / 120.0), ax, ay);
                        self.after_zoom();
                    }
                    Some(LRESULT(0))
                }
                WM_KEYDOWN => {
                    let ctrl = GetKeyState(VK_CONTROL.0 as i32) < 0;
                    let shift = GetKeyState(VK_SHIFT.0 as i32) < 0;
                    let vk = VIRTUAL_KEY(wp.0 as u16);
                    if self.dialog.is_some() {
                        self.dialog_key(vk, shift, ctrl);
                        return Some(LRESULT(0));
                    }
                    if self.menu.is_some() {
                        self.menu_key(vk);
                        return Some(LRESULT(0));
                    }
                    self.hide_tooltip();
                    // A page has the keyboard to itself: Escape closes it, nothing else reaches the viewer.
                    if self.page.is_some() {
                        if vk == VK_ESCAPE || (vk == VK_OEM_COMMA && ctrl) {
                            self.close_page();
                        }
                        return Some(LRESULT(0));
                    }
                    match vk {
                        VK_LEFT => self.step(-1),
                        VK_RIGHT => self.step(1),
                        // Page Up / Down: a PDF's pages while there is one that way, else the top / bottom
                        // of the explorer's list, else the folder's first / last photo.
                        VK_PRIOR if self.page_count() > 1 && self.pdf_page > 0 => self.turn_page(-1),
                        VK_NEXT if self.pdf_page + 1 < self.page_count() => self.turn_page(1),
                        VK_PRIOR if self.explorer_shown() => self.explorer_jump(false),
                        VK_NEXT if self.explorer_shown() => self.explorer_jump(true),
                        VK_PRIOR => self.jump(false),
                        VK_NEXT => self.jump(true),
                        VK_HOME => self.jump(false),
                        VK_END => self.jump(true),
                        VK_F11 => self.toggle_fullscreen(),
                        // Escape steps out: the slideshow, then fullscreen, then the window.
                        VK_ESCAPE if self.slideshow.running => self.stop_slideshow(),
                        VK_ESCAPE if self.fullscreen.is_some() => self.toggle_fullscreen(),
                        VK_ESCAPE => {
                            let _ = PostMessageW(Some(self.hwnd), WM_CLOSE, WPARAM(0), LPARAM(0));
                        }
                        VK_F5 => self.toggle_slideshow(),
                        VK_SPACE => {
                            // Pauses a slideshow, else an animation.
                            if !self.slideshow_play_pause() && self.viewer.toggle_animation_pause() {
                                self.invalidate();
                            }
                        }
                        VK_F if !ctrl => self.act(Tool::Fit),
                        VK_0 if ctrl => self.act(Tool::Fit),
                        VK_1 => {
                            let a = self.image_area();
                            self.view.actual_size_at(((a.right - a.left) / 2.0) as f64, ((a.bottom - a.top) / 2.0) as f64);
                            self.after_zoom();
                        }
                        VK_OEM_PLUS | VK_ADD if ctrl => self.act(Tool::ZoomIn),
                        VK_OEM_MINUS | VK_SUBTRACT if ctrl => self.act(Tool::ZoomOut),
                        VK_OEM_COMMA if ctrl => self.open_page(),
                        VK_O if ctrl && shift => self.open_folder_dialog(),
                        VK_O if ctrl => self.open_dialog(),
                        VK_E if ctrl => self.reveal(),
                        VK_C if ctrl && shift => self.copy_path(),
                        VK_C if ctrl => self.copy_image(),
                        VK_R if ctrl => self.rotate_preview(!shift),
                        VK_S if ctrl => self.save_rotation(),
                        VK_DELETE => self.confirm_delete(true),
                        VK_F2 => self.begin_rename(true),
                        VK_I if !ctrl => self.toggle_info(),
                        VK_T if !ctrl => self.toggle_strip(),
                        VK_E if !ctrl => self.toggle_explorer(),
                        VK_UP if self.explorer_shown() => self.explorer_move(-1),
                        VK_DOWN if self.explorer_shown() => self.explorer_move(1),
                        VK_RETURN if self.explorer_shown() => {
                            if !self.explorer_enter() {
                                return None;
                            }
                        }
                        VK_APPS => {
                            let v = self.viewport();
                            self.open_context_menu((v.left + v.right) / 2.0, (v.top + v.bottom) / 2.0);
                        }
                        _ => return None,
                    }
                    Some(LRESULT(0))
                }
                WM_TIMER => {
                    match wp.0 {
                        TIMER_UPGRADE => self.upgrade(),
                        TIMER_CARET => self.blink_caret(),
                        TIMER_EXPLORER => self.explorer_refresh(),
                        TIMER_FOLDER => self.refresh_folder(),
                        slideshow::TIMER_SLIDESHOW => self.on_slideshow_timer(),
                        TIMER_UPDATES => {
                            let _ = KillTimer(Some(self.hwnd), TIMER_UPDATES);
                            self.check_updates();
                        }
                        TIMER_THUMBS => {
                            let _ = KillTimer(Some(self.hwnd), TIMER_THUMBS);
                            self.invalidate();
                        }
                        TIMER_TOAST => {
                            let _ = KillTimer(Some(self.hwnd), TIMER_TOAST);
                            self.invalidate();
                        }
                        TIMER_TOOLTIP => {
                            let _ = KillTimer(Some(self.hwnd), TIMER_TOOLTIP);
                            if self.menu.is_none() && self.pressed.is_none() {
                                self.tooltip = self.hover;
                                self.invalidate();
                            }
                        }
                        viewer::TIMER_ANIM => {
                            if self.viewer.next_frame() {
                                self.invalidate();
                            }
                        }
                        viewer::TIMER_SETTLE => self.viewer.on_settle_timer(),
                        TIMER_TRACE => {
                            let _ = KillTimer(Some(self.hwnd), TIMER_TRACE);
                            crate::trace::flush("1.5 s after the first frame");
                        }
                        _ => {}
                    }
                    Some(LRESULT(0))
                }
                decode::WM_DECODED => {
                    self.on_decoded();
                    Some(LRESULT(0))
                }
                WM_CHAR => {
                    if self.dialog.is_some() {
                        self.dialog_char(wp.0 as u16);
                    }
                    Some(LRESULT(0))
                }
                WM_IME_STARTCOMPOSITION | WM_IME_COMPOSITION => {
                    self.place_ime();
                    None
                }
                crate::explorer::WM_EXPLORER_LISTED => {
                    let l = Box::from_raw(lp.0 as *mut explorer::Listed);
                    self.on_explorer_listed(*l);
                    Some(LRESULT(0))
                }
                crate::explorer::WM_FOLDER_CHANGED => {
                    if self.folder_watch.is_some() {
                        SetTimer(Some(self.hwnd), TIMER_FOLDER, explorer::REFRESH_MS, None);
                    }
                    Some(LRESULT(0))
                }
                crate::explorer::WM_EXPLORER_CHANGED => {
                    self.on_explorer_changed();
                    Some(LRESULT(0))
                }
                // Mouse buttons 4 and 5: Back / Forward through the explorer's folders.
                WM_XBUTTONUP if self.explorer_shown() && self.dialog.is_none() => {
                    match (wp.0 >> 16) & 0xFFFF {
                        1 => self.explorer_back(),
                        2 => self.explorer_forward(),
                        _ => {}
                    }
                    Some(LRESULT(1))
                }
                WM_DROPFILES => {
                    let drop = HDROP(wp.0 as _);
                    let n = DragQueryFileW(drop, u32::MAX, None);
                    let mut paths = Vec::new();
                    for i in 0..n {
                        let len = DragQueryFileW(drop, i, None) as usize;
                        let mut buf = vec![0u16; len + 1];
                        DragQueryFileW(drop, i, Some(&mut buf));
                        buf.truncate(len);
                        paths.push(PathBuf::from(<std::ffi::OsString as std::os::windows::ffi::OsStringExt>::from_wide(&buf)));
                    }
                    DragFinish(drop);
                    self.dropped(paths);
                    Some(LRESULT(0))
                }
                crate::store::WM_UPDATE_STATUS => {
                    self.update = crate::store::UpdateStatus::from_u8(wp.0 as u8);
                    self.invalidate();
                    Some(LRESULT(0))
                }
                WM_COPYDATA => {
                    let cds = &*(lp.0 as *const windows::Win32::System::DataExchange::COPYDATASTRUCT);
                    let Some(path) = crate::instance::received(cds) else { return None };
                    self.activated(path);
                    Some(LRESULT(1))
                }
                crate::pages::WM_PAGE_RENDERED => {
                    self.on_pages_rendered();
                    Some(LRESULT(0))
                }
                crate::thumbs::WM_THUMBS => {
                    self.on_thumbs();
                    Some(LRESULT(0))
                }
                landing::WM_RECENTS => {
                    let list = Box::from_raw(lp.0 as *mut Vec<landing::Recent>);
                    self.on_recents(*list);
                    Some(LRESULT(0))
                }
                info::WM_EXIF => {
                    let r = Box::from_raw(lp.0 as *mut info::ExifResult);
                    self.on_exif(*r);
                    Some(LRESULT(0))
                }
                crate::fileops::WM_FILE_OP => {
                    let done = Box::from_raw(lp.0 as *mut crate::fileops::Done);
                    self.on_file_op(*done);
                    Some(LRESULT(0))
                }
                WM_GPU_READY => {
                    self.on_gpu_ready();
                    Some(LRESULT(0))
                }
                WM_LISTED => {
                    let listing = Box::from_raw(lp.0 as *mut Listing);
                    self.on_listed(*listing);
                    Some(LRESULT(0))
                }
                WM_SIZE => {
                    self.client_w = (lp.0 & 0xFFFF) as u32;
                    self.client_h = ((lp.0 >> 16) & 0xFFFF) as u32;
                    self.maximized = wp.0 as u32 == SIZE_MAXIMIZED;
                    self.track_normal_rect();
                    self.menu = None;
                    self.tooltip = None;
                    if self.client_w > 0 && self.client_h > 0 {
                        if let Some(g) = &mut self.gfx {
                            let _ = g.resize(self.client_w, self.client_h, self.dpi as f32);
                        }
                        self.layout_changed();
                        self.render();
                    }
                    Some(LRESULT(0))
                }
                WM_DPICHANGED if super::dpi_override().is_some() => Some(LRESULT(0)),
                WM_DPICHANGED => {
                    self.dpi = (wp.0 & 0xFFFF) as u32;
                    let r = &*(lp.0 as *const RECT);
                    let _ = SetWindowPos(self.hwnd, None, r.left, r.top, r.right - r.left, r.bottom - r.top, SWP_NOZORDER | SWP_NOACTIVATE);
                    self.check_colour();
                    Some(LRESULT(0))
                }
                WM_GETMINMAXINFO => {
                    let mmi = &mut *(lp.0 as *mut MINMAXINFO);
                    mmi.ptMinTrackSize = POINT { x: (520.0 * self.scale()) as i32, y: (380.0 * self.scale()) as i32 };
                    Some(LRESULT(0))
                }
                WM_ACTIVATE => {
                    self.active = (wp.0 & 0xFFFF) as u32 != WA_INACTIVE;
                    if !self.active {
                        self.close_menu();
                        self.hide_tooltip();
                    } else {
                        // SDR content brightness has no message of its own.
                        self.check_colour();
                    }
                    self.invalidate();
                    None
                }
                WM_PAINT => {
                    let mut ps = PAINTSTRUCT::default();
                    BeginPaint(self.hwnd, &mut ps);
                    self.render();
                    let _ = EndPaint(self.hwnd, &ps);
                    Some(LRESULT(0))
                }
                WM_ERASEBKGND => Some(LRESULT(1)),
                WM_MOVE => {
                    self.track_normal_rect();
                    None
                }
                WM_EXITSIZEMOVE => {
                    self.track_normal_rect();
                    self.check_colour();
                    None
                }
                WM_DISPLAYCHANGE => {
                    self.check_colour();
                    None
                }
                WM_CLOSE => {
                    self.save_placement();
                    None
                }
                WM_DESTROY => {
                    crate::trace::flush("window closed");
                    PostQuitMessage(0);
                    Some(LRESULT(0))
                }
                _ => None,
            }
        }
    }
}

fn caption_for(ht: u32) -> Option<Caption> {
    match ht {
        HTMINBUTTON => Some(Caption::Min),
        HTMAXBUTTON => Some(Caption::Max),
        HTCLOSE => Some(Caption::Close),
        _ => None,
    }
}
