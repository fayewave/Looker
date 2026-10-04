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
            self.invalidate();
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
                    if let Some((px, py)) = self.drag {
                        self.view.pan((x - px) as f64, (y - py) as f64);
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
                    Some(LRESULT(0))
                }
                WM_SETCURSOR if (lp.0 & 0xFFFF) as u32 == HTCLIENT => {
                    let cursor = if self.hover == Some(Hit::Reveal) { IDC_HAND } else { IDC_ARROW };
                    SetCursor(LoadCursorW(None, cursor).ok());
                    Some(LRESULT(1))
                }
                WM_LBUTTONDOWN => {
                    let (x, y) = self.dip_from_lparam(lp);
                    SetCapture(self.hwnd);
                    let h = self.hit(x, y);
                    self.pressed = h;
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
                    let h = self.hit(x, y);
                    if pressed.is_some() && pressed == h {
                        match h {
                            Some(Hit::Tool(t)) => self.act(t),
                            Some(Hit::Reveal) => self.reveal(),
                            Some(Hit::Open) => self.open_dialog(),
                            _ => {}
                        }
                    }
                    if was_drag {
                        self.schedule_upgrade();
                    }
                    self.invalidate();
                    Some(LRESULT(0))
                }
                WM_LBUTTONDBLCLK => {
                    let (x, y) = self.dip_from_lparam(lp);
                    if self.hit(x, y) == Some(Hit::Viewport) && self.view.has_content() {
                        let v = self.viewport();
                        self.view.toggle_fit_actual((x - v.left) as f64, (y - v.top) as f64);
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
                    None
                }
                WM_MOUSEWHEEL => {
                    let delta = ((wp.0 >> 16) & 0xFFFF) as i16 as f64;
                    let (x, y) = self.screen_to_dip(lp);
                    if self.hit(x, y) == Some(Hit::Viewport) && self.view.has_content() {
                        let v = self.viewport();
                        self.view.zoom_at(1.2f64.powf(delta / 120.0), (x - v.left) as f64, (y - v.top) as f64);
                        self.after_zoom();
                    }
                    Some(LRESULT(0))
                }
                WM_KEYDOWN => {
                    let ctrl = GetKeyState(VK_CONTROL.0 as i32) < 0;
                    let vk = VIRTUAL_KEY(wp.0 as u16);
                    match vk {
                        VK_LEFT => self.step(-1),
                        VK_RIGHT => self.step(1),
                        VK_HOME => self.jump(false),
                        VK_END => self.jump(true),
                        VK_F11 => self.toggle_fullscreen(),
                        VK_ESCAPE if self.fullscreen.is_some() => self.toggle_fullscreen(),
                        VK_F if !ctrl => self.act(Tool::Fit),
                        VK_0 if ctrl => self.act(Tool::Fit),
                        VK_1 => {
                            self.view.actual_size_at(
                                ((self.viewport().right - self.viewport().left) / 2.0) as f64,
                                ((self.viewport().bottom - self.viewport().top) / 2.0) as f64,
                            );
                            self.after_zoom();
                        }
                        VK_OEM_PLUS | VK_ADD if ctrl => self.act(Tool::ZoomIn),
                        VK_OEM_MINUS | VK_SUBTRACT if ctrl => self.act(Tool::ZoomOut),
                        VK_O if ctrl => self.open_dialog(),
                        VK_E if ctrl => self.reveal(),
                        _ => return None,
                    }
                    Some(LRESULT(0))
                }
                WM_TIMER => {
                    match wp.0 {
                        TIMER_UPGRADE => self.upgrade(),
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
                    if self.client_w > 0 && self.client_h > 0 {
                        if let Some(g) = &mut self.gfx {
                            let _ = g.resize(self.client_w, self.client_h, self.dpi as f32);
                        }
                        let v = self.viewport();
                        let s = self.scale() as f64;
                        self.view.set_viewport((v.right - v.left) as f64, (v.bottom - v.top) as f64, s);
                        let (bw, bh) = self.fit_box();
                        self.viewer.set_fit_box(bw, bh);
                        self.render();
                        self.schedule_upgrade();
                    }
                    Some(LRESULT(0))
                }
                WM_DPICHANGED => {
                    self.dpi = (wp.0 & 0xFFFF) as u32;
                    let r = &*(lp.0 as *const RECT);
                    let _ = SetWindowPos(self.hwnd, None, r.left, r.top, r.right - r.left, r.bottom - r.top, SWP_NOZORDER | SWP_NOACTIVATE);
                    Some(LRESULT(0))
                }
                WM_GETMINMAXINFO => {
                    let mmi = &mut *(lp.0 as *mut MINMAXINFO);
                    mmi.ptMinTrackSize = POINT { x: (520.0 * self.scale()) as i32, y: (380.0 * self.scale()) as i32 };
                    Some(LRESULT(0))
                }
                WM_ACTIVATE => {
                    self.active = (wp.0 & 0xFFFF) as u32 != WA_INACTIVE;
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
                WM_MOVE | WM_EXITSIZEMOVE => {
                    self.track_normal_rect();
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
