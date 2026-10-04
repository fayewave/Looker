//! The window: a plain Win32 window with the system caption removed and everything inside drawn by us.
//! Layout (DIPs, as in MainWindow.xaml): title bar 48 / toolbar 40 / viewport / status row 28.

use std::collections::HashMap;
use std::os::windows::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use windows::Win32::Foundation::*;
use windows::Win32::Globalization::{DATE_SHORTDATE, GetDateFormatEx, GetTimeFormatEx, TIME_NOSECONDS};
use windows::Win32::Graphics::Direct2D::Common::D2D_RECT_F;
use windows::Win32::Graphics::Direct2D::ID2D1Bitmap1;
use windows::Win32::Graphics::Dwm::{DWMWA_USE_IMMERSIVE_DARK_MODE, DwmSetWindowAttribute};
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::Com::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Time::{FileTimeToSystemTime, SystemTimeToTzSpecificLocalTime};
use windows::Win32::UI::Controls::WM_MOUSELEAVE;
use windows::Win32::UI::HiDpi::*;
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::Shell::Common::{COMDLG_FILTERSPEC, ITEMIDLIST};
use windows::Win32::UI::Shell::*;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{HSTRING, PCWSTR, w};

use crate::decode::{self, Decoded, Job, Pool, Priority};
use crate::folder::{self, Listing};
use crate::gfx::{self, Align, Gfx, contains, rect, rgb, white};
use crate::view::View;

const TITLE_H: f32 = 48.0;
const TOOLBAR_H: f32 = 40.0;
const STATUS_H: f32 = 28.0;
const CAPTION_W: f32 = 46.0;
const BUTTON_W: f32 = 40.0;
const BUTTON_H: f32 = 32.0;
const SORT_W: f32 = 60.0;
const DEFAULT_W: f32 = 1200.0;
const DEFAULT_H: f32 = 800.0;

const ACCENT: u32 = 0xF52524;
const CLOSE_RED: u32 = 0xC42B1C;
/// TextFillColorSecondary / Tertiary / Disabled on the dark theme.
const TEXT_SECONDARY: u8 = 0xC5;
const TEXT_TERTIARY: u8 = 0x8B;
const TEXT_DISABLED: u8 = 0x5D;

const WM_LISTED: u32 = WM_APP + 2;
const WM_GPU_READY: u32 = WM_APP + 3;
const TIMER_UPGRADE: usize = 1;
const TIMER_TRACE: usize = 2;

static APP_ICON: &[u8] = include_bytes!("../../src/Looker/Assets/AppIcon.ico");

/// The hardware device, built in the background after the first frame (see gfx.rs).
static GPU_DEVICE: std::sync::Mutex<Option<gfx::Sendable<gfx::Device>>> = std::sync::Mutex::new(None);

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Tool {
    Previous,
    Next,
    ZoomOut,
    Fit,
    ZoomIn,
    Fullscreen,
    Rotate,
    Delete,
    Sort,
    Home,
    Explorer,
    Strip,
    Info,
    Settings,
}

const LEFT_TOOLS: &[(Tool, u16)] = &[
    (Tool::Previous, 0xE76B),
    (Tool::Next, 0xE76C),
    (Tool::ZoomOut, 0xE71F),
    (Tool::Fit, 0xE9A6),
    (Tool::ZoomIn, 0xE8A3),
    (Tool::Fullscreen, 0xE740),
    (Tool::Rotate, 0xE7AD),
    (Tool::Delete, 0xE74D),
    (Tool::Sort, 0xE8CB),
];
const RIGHT_TOOLS: &[(Tool, u16)] = &[
    (Tool::Home, 0xE80F),
    (Tool::Explorer, 0xE8B7),
    (Tool::Strip, 0xE8FD),
    (Tool::Info, 0xE946),
    (Tool::Settings, 0xE713),
];

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Hit {
    Tool(Tool),
    Reveal,
    Open,
    Viewport,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Caption {
    Min,
    Max,
    Close,
}

struct Cached {
    bmp: ID2D1Bitmap1,
    /// The decoded pixels, kept only while drawing on WARP so the bitmap can be re-made on the GPU.
    pixels: Option<(u32, u32, Vec<u8>)>,
    long_edge: u32,
    native_w: u32,
    native_h: u32,
    format: &'static str,
    taken: Option<FILETIME>,
}

struct FileInfo {
    size: u64,
    modified: FILETIME,
}

pub struct Placement {
    pub rect: RECT,
    pub dpi: u32,
}

impl Placement {
    /// The viewport the first image will be fitted into, in device pixels.
    pub fn viewport_px(&self) -> (u32, u32) {
        let s = self.dpi as f32 / 96.0;
        let (w, h) = client_size_for(self);
        let vh = h as f32 - (TITLE_H + TOOLBAR_H + STATUS_H) * s;
        (w, vh.max(1.0) as u32)
    }
}

fn frame_px(dpi: u32) -> (i32, i32) {
    unsafe {
        let pad = GetSystemMetricsForDpi(SM_CXPADDEDBORDER, dpi);
        (GetSystemMetricsForDpi(SM_CXFRAME, dpi) + pad, GetSystemMetricsForDpi(SM_CYFRAME, dpi) + pad)
    }
}

/// Client size for a window rect: the side and bottom frames stay (invisible resize borders), the top goes.
fn client_size_for(p: &Placement) -> (u32, u32) {
    let (fx, fy) = frame_px(p.dpi);
    (((p.rect.right - p.rect.left) - 2 * fx).max(1) as u32, ((p.rect.bottom - p.rect.top) - fy).max(1) as u32)
}

/// Default window: 1200 × 800 DIPs centred on the primary monitor's work area.
pub fn initial_placement() -> Placement {
    unsafe {
        let mon = MonitorFromPoint(POINT { x: 0, y: 0 }, MONITOR_DEFAULTTOPRIMARY);
        let mut mi = MONITORINFO { cbSize: size_of::<MONITORINFO>() as u32, ..Default::default() };
        let _ = GetMonitorInfoW(mon, &mut mi);
        let (mut dx, mut dy) = (96u32, 96u32);
        let _ = GetDpiForMonitor(mon, MDT_EFFECTIVE_DPI, &mut dx, &mut dy);
        let s = dx as f32 / 96.0;
        let (fx, fy) = frame_px(dx);
        let work = mi.rcWork;
        let ww = work.right - work.left;
        let wh = work.bottom - work.top;
        let w = ((DEFAULT_W * s) as i32 + 2 * fx).min(ww * 9 / 10);
        let h = ((DEFAULT_H * s) as i32 + fy).min(wh * 9 / 10);
        let x = work.left + (ww - w) / 2;
        let y = work.top + (wh - h) / 2;
        Placement { rect: RECT { left: x, top: y, right: x + w, bottom: y + h }, dpi: dx }
    }
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().collect()
}

fn file_name(p: &Path) -> String {
    p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
}

fn format_bytes(b: u64) -> String {
    if b >= 1 << 20 {
        format!("{:.1} MB", b as f64 / (1u64 << 20) as f64)
    } else if b >= 1 << 10 {
        format!("{:.1} KB", b as f64 / 1024.0)
    } else {
        format!("{b} B")
    }
}

/// UTC FILETIME → the user's short date + short time, like .NET's `{0:g}`.
fn format_time(ft: FILETIME) -> String {
    unsafe {
        let mut utc = SYSTEMTIME::default();
        let mut local = SYSTEMTIME::default();
        if FileTimeToSystemTime(&ft, &mut utc).is_err() || SystemTimeToTzSpecificLocalTime(None, &utc, &mut local).is_err() {
            return String::new();
        }
        let mut d = [0u16; 64];
        let mut t = [0u16; 64];
        let dn = GetDateFormatEx(PCWSTR::null(), DATE_SHORTDATE, Some(&local), PCWSTR::null(), Some(&mut d), PCWSTR::null());
        let tn = GetTimeFormatEx(PCWSTR::null(), TIME_NOSECONDS, Some(&local), PCWSTR::null(), Some(&mut t));
        let ds = String::from_utf16_lossy(&d[..(dn.max(1) - 1) as usize]);
        let ts = String::from_utf16_lossy(&t[..(tn.max(1) - 1) as usize]);
        format!("{ds} {ts}")
    }
}

pub struct App {
    hwnd: HWND,
    gfx: Option<Gfx>,
    dpi: u32,
    client_w: u32,
    client_h: u32,
    active: bool,
    maximized: bool,
    fullscreen: Option<WINDOWPLACEMENT>,

    pool: Arc<Pool>,
    listing: Option<Listing>,
    index: Option<usize>,
    current: Option<PathBuf>,
    info: Option<FileInfo>,
    cache: HashMap<PathBuf, Cached>,
    failed: HashMap<PathBuf, String>,
    pending: HashMap<PathBuf, (u32, u32)>,
    view: View,
    view_for: Option<PathBuf>,
    first_image_traced: bool,

    icon: Option<ID2D1Bitmap1>,
    icon_pixels: Option<Decoded>,
    mouse: (f32, f32),
    tracking: bool,
    hover: Option<Hit>,
    pressed: Option<Hit>,
    drag: Option<(f32, f32)>,
    cap_hover: Option<Caption>,
    cap_pressed: Option<Caption>,
}

impl App {
    fn scale(&self) -> f32 {
        self.dpi as f32 / 96.0
    }
    fn size_dip(&self) -> (f32, f32) {
        (self.client_w as f32 / self.scale(), self.client_h as f32 / self.scale())
    }
    fn chrome(&self) -> bool {
        self.fullscreen.is_none()
    }

    fn viewport(&self) -> D2D_RECT_F {
        let (w, h) = self.size_dip();
        if self.chrome() {
            D2D_RECT_F { left: 0.0, top: TITLE_H + TOOLBAR_H, right: w, bottom: (h - STATUS_H).max(TITLE_H + TOOLBAR_H) }
        } else {
            D2D_RECT_F { left: 0.0, top: 0.0, right: w, bottom: h }
        }
    }

    fn caption_rect(&self, c: Caption) -> D2D_RECT_F {
        let (w, _) = self.size_dip();
        let i = match c {
            Caption::Close => 1.0,
            Caption::Max => 2.0,
            Caption::Min => 3.0,
        };
        rect(w - CAPTION_W * i, 0.0, CAPTION_W, TITLE_H)
    }

    fn tool_rects(&self) -> Vec<(Tool, u16, D2D_RECT_F)> {
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

    fn tool_enabled(&self, t: Tool) -> bool {
        let has = self.current.is_some();
        match t {
            Tool::Settings => true,
            Tool::Previous | Tool::Next => has && self.listing.as_ref().is_some_and(|l| l.images.len() > 1),
            _ => has,
        }
    }

    fn status_parts(&self) -> String {
        let Some(path) = &self.current else { return String::new() };
        let mut parts: Vec<String> = Vec::new();
        let cached = self.cache.get(path);
        if let Some(c) = cached {
            parts.push(c.format.to_string());
            parts.push(format!("{} × {}", c.native_w, c.native_h));
            parts.push(format!("{:.1} MP", c.native_w as f64 * c.native_h as f64 / 1_000_000.0));
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

    fn reveal_rect(&self) -> Option<D2D_RECT_F> {
        let g = self.gfx.as_ref()?;
        self.current.as_ref()?;
        let (_, h) = self.size_dip();
        let tw = g.measure(&wide(&self.status_parts()), &g.fonts.caption);
        let lw = g.measure(&wide("Open in Explorer"), &g.fonts.caption);
        Some(rect(12.0 + tw + 16.0, h - STATUS_H + 4.0, lw, STATUS_H - 10.0))
    }

    fn open_rect(&self) -> D2D_RECT_F {
        let v = self.viewport();
        let cx = (v.left + v.right) / 2.0;
        let cy = (v.top + v.bottom) / 2.0;
        rect(cx - 70.0, cy + 8.0, 140.0, 32.0)
    }

    fn hit(&self, x: f32, y: f32) -> Option<Hit> {
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
        if self.current.is_none() && contains(&self.open_rect(), x, y) {
            return Some(Hit::Open);
        }
        if contains(&self.viewport(), x, y) {
            return Some(Hit::Viewport);
        }
        None
    }

    // --- Images -------------------------------------------------------------------------------------

    /// The device-pixel box a fit decode targets.
    fn fit_box(&self) -> (u32, u32) {
        let v = self.viewport();
        let s = self.scale();
        (((v.right - v.left) * s).ceil().max(1.0) as u32, ((v.bottom - v.top) * s).ceil().max(1.0) as u32)
    }

    fn request(&mut self, path: &Path, bw: u32, bh: u32, priority: Priority) {
        if let Some(&(pw, ph)) = self.pending.get(path) {
            let covers = (pw == 0 && ph == 0) || (bw != 0 && pw >= bw && ph >= bh);
            if covers {
                return;
            }
        }
        if self.failed.contains_key(path) {
            return;
        }
        self.pending.insert(path.to_path_buf(), (bw, bh));
        self.pool.submit(Job { path: path.to_path_buf(), box_w: bw, box_h: bh, priority });
    }

    fn show_index(&mut self, i: usize) {
        let Some(l) = &self.listing else { return };
        if i >= l.images.len() {
            return;
        }
        let path = l.images[i].path.clone();
        self.index = Some(i);
        self.show_path(path);
    }

    fn show_path(&mut self, path: PathBuf) {
        self.current = Some(path.clone());
        self.info = std::fs::metadata(&path).ok().map(|m| {
            let t = m.last_write_time();
            FileInfo { size: m.len(), modified: FILETIME { dwLowDateTime: t as u32, dwHighDateTime: (t >> 32) as u32 } }
        });
        unsafe {
            let _ = SetWindowTextW(self.hwnd, &HSTRING::from(file_name(&path)));
        }
        if let Some(c) = self.cache.get(&path) {
            let (nw, nh) = (c.native_w, c.native_h);
            self.reset_view(nw, nh);
            self.view_for = Some(path.clone());
        } else {
            self.view.clear();
            self.view_for = None;
        }
        if !self.cache.contains_key(&path) {
            let (bw, bh) = self.fit_box();
            self.request(&path, bw, bh, Priority::Current);
        }
        self.schedule_preloads();
        self.schedule_upgrade();
        self.invalidate();
    }

    fn reset_view(&mut self, nw: u32, nh: u32) {
        let v = self.viewport();
        self.view.reset((v.right - v.left) as f64, (v.bottom - v.top) as f64, nw as f64, nh as f64, self.scale() as f64);
    }

    /// Preload ±1, ±2 at the fit size; drop queued work for images that left the window and evict the cache.
    fn schedule_preloads(&mut self) {
        let Some(cur) = self.current.clone() else { return };
        let mut wanted: Vec<PathBuf> = vec![cur.clone()];
        let mut preload: Vec<PathBuf> = Vec::new();
        if let (Some(l), Some(i)) = (&self.listing, self.index) {
            for d in [1isize, -1, 2, -2] {
                let j = i as isize + d;
                if j >= 0 && (j as usize) < l.images.len() {
                    let e = &l.images[j as usize];
                    wanted.push(e.path.clone());
                    if !e.cloud {
                        preload.push(e.path.clone());
                    }
                }
            }
            for d in [3isize, -3] {
                let j = i as isize + d;
                if j >= 0 && (j as usize) < l.images.len() {
                    wanted.push(l.images[j as usize].path.clone());
                }
            }
        }
        for p in self.pool.retain(|j| wanted.contains(&j.path)) {
            self.pending.remove(&p);
        }
        self.cache.retain(|p, _| wanted.contains(p));
        let (bw, bh) = self.fit_box();
        for p in preload {
            if !self.cache.contains_key(&p) {
                self.request(&p, bw, bh, Priority::Preload);
            }
        }
    }

    /// Zoomed or resized past what the on-screen bitmap was decoded for: re-decode sharper, debounced.
    fn schedule_upgrade(&self) {
        unsafe {
            SetTimer(Some(self.hwnd), TIMER_UPGRADE, 160, None);
        }
    }

    fn upgrade(&mut self) {
        unsafe {
            let _ = KillTimer(Some(self.hwnd), TIMER_UPGRADE);
        }
        let Some(path) = self.current.clone() else { return };
        let Some(c) = self.cache.get(&path) else { return };
        let r = self.view.target_rect();
        let native_long = c.native_w.max(c.native_h);
        let max = self.gfx.as_ref().map_or(16384, |g| g.max_bitmap_size());
        let needed = ((r.w.max(r.h) * self.scale() as f64).ceil() as u32).min(native_long).min(max);
        if c.long_edge + 1 >= needed {
            return;
        }
        let edge = if needed * 10 >= native_long * 7 && native_long <= max { 0 } else { needed };
        self.request(&path, edge, edge, Priority::Current);
    }

    fn on_decoded(&mut self) {
        let results = self.pool.take_results();
        for d in results {
            if self.pending.get(&d.path) == Some(&(d.box_w, d.box_h)) {
                self.pending.remove(&d.path);
            }
            match d.result {
                Ok(img) => self.adopt(d.path, img),
                Err(e) => {
                    if self.current.as_ref() == Some(&d.path) && !self.cache.contains_key(&d.path) {
                        self.invalidate();
                    }
                    if !self.cache.contains_key(&d.path) {
                        self.failed.insert(d.path, e);
                    }
                }
            }
        }
    }

    fn adopt(&mut self, path: PathBuf, img: Decoded) {
        let long_edge = img.width.max(img.height);
        if self.cache.get(&path).is_some_and(|c| c.long_edge >= long_edge) {
            return;
        }
        let is_current = self.current.as_ref() == Some(&path);
        // A finished preload that has since left the window is not worth a GPU upload.
        if !is_current && !self.listing.as_ref().is_some_and(|l| l.images.iter().any(|e| e.path == path)) {
            return;
        }
        let Some(g) = &self.gfx else { return };
        let Ok(bmp) = g.bitmap(img.width, img.height, &img.pixels) else { return };
        let pixels = if g.is_warp() { Some((img.width, img.height, img.pixels)) } else { None };
        self.cache.insert(
            path.clone(),
            Cached { bmp, pixels, long_edge, native_w: img.native_width, native_h: img.native_height, format: img.format, taken: img.taken },
        );
        if is_current {
            if self.view_for.as_ref() != Some(&path) {
                self.reset_view(img.native_width, img.native_height);
                self.view_for = Some(path);
                self.schedule_upgrade();
            }
            self.invalidate();
        }
    }

    fn step(&mut self, delta: isize) {
        let (Some(l), Some(i)) = (&self.listing, self.index) else { return };
        let n = l.images.len() as isize;
        let j = (i as isize + delta).clamp(0, n - 1) as usize;
        if j != i {
            self.show_index(j);
        }
    }

    fn jump(&mut self, last: bool) {
        let Some(l) = &self.listing else { return };
        if l.images.is_empty() {
            return;
        }
        let j = if last { l.images.len() - 1 } else { 0 };
        if Some(j) != self.index {
            self.show_index(j);
        }
    }

    fn on_listed(&mut self, listing: Listing) {
        let Some(cur) = self.current.clone() else { return };
        if listing.folder.as_path() != cur.parent().unwrap_or(Path::new("")) {
            return;
        }
        self.index = listing.images.iter().position(|e| e.path == cur);
        self.listing = Some(listing);
        self.schedule_preloads();
        self.invalidate();
    }

    fn list_folder_async(&self, folder: PathBuf) {
        let hwnd = self.hwnd.0 as isize;
        std::thread::Builder::new()
            .name("listing".into())
            .spawn(move || {
                let listing = folder::list(&folder);
                crate::trace::mark(format!("folder listed: {} images of {} files", listing.images.len(), listing.total_files));
                let ptr = Box::into_raw(Box::new(listing));
                unsafe {
                    if PostMessageW(Some(HWND(hwnd as _)), WM_LISTED, WPARAM(0), LPARAM(ptr as isize)).is_err() {
                        drop(Box::from_raw(ptr));
                    }
                }
            })
            .ok();
    }

    fn open(&mut self, path: PathBuf) {
        self.listing = None;
        self.index = None;
        self.cache.clear();
        self.failed.clear();
        self.pending.clear();
        let _ = self.pool.retain(|_| false);
        if let Some(folder) = path.parent() {
            self.list_folder_async(folder.to_path_buf());
        }
        self.show_path(path);
    }

    fn close(&mut self) {
        self.listing = None;
        self.index = None;
        self.current = None;
        self.info = None;
        self.cache.clear();
        self.pending.clear();
        let _ = self.pool.retain(|_| false);
        self.view.clear();
        self.view_for = None;
        unsafe {
            let _ = SetWindowTextW(self.hwnd, w!("Looker"));
        }
        self.invalidate();
    }

    fn open_dialog(&mut self) {
        unsafe {
            let Ok(dlg) = CoCreateInstance::<_, IFileOpenDialog>(&FileOpenDialog, None, CLSCTX_INPROC_SERVER) else { return };
            let pattern = w!("*.jpg;*.jpeg;*.jpe;*.jfif;*.png;*.apng;*.bmp;*.dib;*.gif;*.tif;*.tiff;*.webp;*.ico;*.jxr;*.wdp;*.hdp;*.heic;*.heif;*.hif;*.avif;*.jxl;*.dds");
            let filters = [COMDLG_FILTERSPEC { pszName: w!("Images"), pszSpec: pattern }];
            let _ = dlg.SetFileTypes(&filters);
            if dlg.Show(Some(self.hwnd)).is_err() {
                return;
            }
            let Ok(item) = dlg.GetResult() else { return };
            let Ok(name) = item.GetDisplayName(SIGDN_FILESYSPATH) else { return };
            let path = PathBuf::from(name.to_string().unwrap_or_default());
            CoTaskMemFree(Some(name.0 as _));
            self.open(path);
        }
    }

    fn reveal(&self) {
        let Some(path) = &self.current else { return };
        unsafe {
            let mut pidl: *mut ITEMIDLIST = std::ptr::null_mut();
            if SHParseDisplayName(&HSTRING::from(path.as_os_str()), None, &mut pidl, 0, None).is_ok() && !pidl.is_null() {
                let _ = SHOpenFolderAndSelectItems(pidl, None, 0);
                CoTaskMemFree(Some(pidl as _));
            }
        }
    }

    fn toggle_fullscreen(&mut self) {
        unsafe {
            if let Some(wp) = self.fullscreen.take() {
                SetWindowLongPtrW(self.hwnd, GWL_STYLE, (WS_OVERLAPPEDWINDOW | WS_VISIBLE).0 as isize);
                let _ = SetWindowPlacement(self.hwnd, &wp);
                let _ = SetWindowPos(self.hwnd, None, 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_FRAMECHANGED);
            } else {
                let mut wp = WINDOWPLACEMENT { length: size_of::<WINDOWPLACEMENT>() as u32, ..Default::default() };
                let _ = GetWindowPlacement(self.hwnd, &mut wp);
                let mon = MonitorFromWindow(self.hwnd, MONITOR_DEFAULTTONEAREST);
                let mut mi = MONITORINFO { cbSize: size_of::<MONITORINFO>() as u32, ..Default::default() };
                let _ = GetMonitorInfoW(mon, &mut mi);
                self.fullscreen = Some(wp);
                SetWindowLongPtrW(self.hwnd, GWL_STYLE, (WS_POPUP | WS_VISIBLE).0 as isize);
                let r = mi.rcMonitor;
                let _ = SetWindowPos(self.hwnd, Some(HWND_TOP), r.left, r.top, r.right - r.left, r.bottom - r.top, SWP_FRAMECHANGED);
            }
        }
    }

    fn act(&mut self, t: Tool) {
        if !self.tool_enabled(t) {
            return;
        }
        match t {
            Tool::Previous => self.step(-1),
            Tool::Next => self.step(1),
            Tool::ZoomOut => self.zoom_center(1.0 / 1.25),
            Tool::ZoomIn => self.zoom_center(1.25),
            Tool::Fit => {
                self.view.fit();
                self.after_zoom();
            }
            Tool::Fullscreen => self.toggle_fullscreen(),
            Tool::Home => self.close(),
            // Not in the spike yet.
            Tool::Rotate | Tool::Delete | Tool::Sort | Tool::Explorer | Tool::Strip | Tool::Info | Tool::Settings => {}
        }
    }

    fn zoom_center(&mut self, f: f64) {
        self.view.zoom_center(f);
        self.after_zoom();
    }

    fn after_zoom(&mut self) {
        self.schedule_upgrade();
        self.invalidate();
    }

    // --- Devices ------------------------------------------------------------------------------------

    fn start_gpu_device(&self) {
        let hwnd = self.hwnd.0 as isize;
        std::thread::Builder::new()
            .name("gpu-init".into())
            .spawn(move || match gfx::create_device(false) {
                Ok(dev) => {
                    *GPU_DEVICE.lock().unwrap() = Some(gfx::Sendable(dev));
                    unsafe {
                        let _ = PostMessageW(Some(HWND(hwnd as _)), WM_GPU_READY, WPARAM(0), LPARAM(0));
                    }
                }
                Err(e) => crate::trace::mark(format!("hardware device failed, staying on WARP: {e}")),
            })
            .ok();
    }

    /// WARP to hardware: re-make every bitmap on the new device, draw a frame on its swap chain, then show it.
    fn on_gpu_ready(&mut self) {
        let Some(dev) = GPU_DEVICE.lock().unwrap().take() else { return };
        let Some(g) = &mut self.gfx else { return };
        if let Err(e) = g.switch_device(dev.0) {
            crate::trace::mark(format!("device switch failed: {e}"));
            return;
        }
        let mut lost = Vec::new();
        for (path, c) in self.cache.iter_mut() {
            match c.pixels.take().and_then(|(w, h, px)| g.bitmap(w, h, &px).ok()) {
                Some(bmp) => c.bmp = bmp,
                None => lost.push(path.clone()),
            }
        }
        for p in lost {
            self.cache.remove(&p);
        }
        self.icon = self.icon_pixels.as_ref().and_then(|i| g.bitmap(i.width, i.height, &i.pixels).ok());
        self.render();
        if let Some(g) = &self.gfx {
            let _ = g.commit_swap();
        }
        crate::trace::mark("switched to the hardware device");
        // Whatever was current but not re-made (it was mid-decode) is requested again.
        if let Some(cur) = self.current.clone() {
            if !self.cache.contains_key(&cur) {
                self.view_for = None;
                let (bw, bh) = self.fit_box();
                self.request(&cur, bw, bh, Priority::Current);
            }
        }
        self.schedule_preloads();
    }

    // --- Drawing ------------------------------------------------------------------------------------

    fn invalidate(&self) {
        unsafe {
            let _ = InvalidateRect(Some(self.hwnd), None, false);
        }
    }

    fn render(&mut self) {
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

    fn draw_title(&self, g: &Gfx) {
        let (w, _) = self.size_dip();
        if let Some(icon) = &self.icon {
            let x = g.snap(16.0);
            let y = g.snap((TITLE_H - 16.0) / 2.0);
            g.draw_bitmap(icon, rect(x, y, 16.0, 16.0), 1.0);
        }
        let title = self.current.as_ref().map(|p| file_name(p)).unwrap_or_else(|| "Looker".into());
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

    fn draw_toolbar(&self, g: &Gfx) {
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

    fn draw_status(&self, g: &Gfx) {
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
        if let (Some(l), Some(i)) = (&self.listing, self.index) {
            let label = format!("{} / {}", l.rank[i] + 1, l.total_files);
            g.text(&wide(&label), &g.fonts.caption, rect(12.0, y, (w - 24.0).max(0.0), row_h), white(TEXT_SECONDARY), Align::Right);
        }
    }

    fn draw_viewport(&mut self, g: &mut Gfx) {
        let v = self.viewport();
        let Some(path) = self.current.clone() else {
            self.draw_landing(g);
            return;
        };
        g.checkerboard(v);
        if let Some(c) = self.cache.get(&path) {
            if self.view_for.as_ref() == Some(&path) {
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
                g.draw_bitmap(&c.bmp, dest, 1.0);
                unsafe {
                    g.dev.dc.PopAxisAlignedClip();
                }
                if !self.first_image_traced {
                    self.first_image_traced = true;
                    crate::trace::mark("first frame with the image drawn");
                }
            }
        } else if self.failed.contains_key(&path) {
            g.text(&wide("Can't display this file"), &g.fonts.body, v, white(TEXT_SECONDARY), Align::Center);
        }
    }

    fn draw_landing(&self, g: &Gfx) {
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

    // --- Window messages ----------------------------------------------------------------------------

    fn dip_from_lparam(&self, lp: LPARAM) -> (f32, f32) {
        let x = (lp.0 & 0xFFFF) as i16 as f32;
        let y = ((lp.0 >> 16) & 0xFFFF) as i16 as f32;
        (x / self.scale(), y / self.scale())
    }

    fn screen_to_dip(&self, lp: LPARAM) -> (f32, f32) {
        let mut pt = POINT { x: (lp.0 & 0xFFFF) as i16 as i32, y: ((lp.0 >> 16) & 0xFFFF) as i16 as i32 };
        unsafe {
            let _ = ScreenToClient(self.hwnd, &mut pt);
        }
        (pt.x as f32 / self.scale(), pt.y as f32 / self.scale())
    }

    fn set_hover(&mut self, h: Option<Hit>) {
        if self.hover != h {
            self.hover = h;
            self.invalidate();
        }
    }

    fn set_cap_hover(&mut self, c: Option<Caption>) {
        if self.cap_hover != c {
            self.cap_hover = c;
            self.invalidate();
        }
    }

    unsafe fn handle(&mut self, msg: u32, wp: WPARAM, lp: LPARAM) -> Option<LRESULT> {
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
                    if self.client_w > 0 && self.client_h > 0 {
                        if let Some(g) = &mut self.gfx {
                            let _ = g.resize(self.client_w, self.client_h, self.dpi as f32);
                        }
                        let v = self.viewport();
                        let s = self.scale() as f64;
                        self.view.set_viewport((v.right - v.left) as f64, (v.bottom - v.top) as f64, s);
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

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    unsafe {
        let app = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut App;
        if !app.is_null() {
            if let Some(r) = (*app).handle(msg, wp, lp) {
                return r;
            }
        }
        DefWindowProcW(hwnd, msg, wp, lp)
    }
}

/// Picks the ICO entry closest above `size` and makes an HICON from it (entries here are PNG).
fn load_icon(bytes: &[u8], size: i32) -> Option<HICON> {
    let rd16 = |o: usize| u16::from_le_bytes([bytes[o], bytes[o + 1]]);
    let rd32 = |o: usize| u32::from_le_bytes([bytes[o], bytes[o + 1], bytes[o + 2], bytes[o + 3]]);
    let count = rd16(4) as usize;
    let mut best: Option<(i32, usize, usize)> = None;
    for i in 0..count {
        let e = 6 + 16 * i;
        let w = if bytes[e] == 0 { 256 } else { bytes[e] as i32 };
        let len = rd32(e + 8) as usize;
        let off = rd32(e + 12) as usize;
        let better = match best {
            None => true,
            Some((bw, _, _)) => (w >= size && (bw < size || w < bw)) || (bw < size && w > bw),
        };
        if better {
            best = Some((w, off, len));
        }
    }
    let (_, off, len) = best?;
    unsafe { CreateIconFromResourceEx(&bytes[off..off + len], true, 0x0003_0000, size, size, LR_DEFAULTCOLOR).ok() }
}

pub fn run(path: Option<PathBuf>, placement: Placement, pool: Arc<Pool>, gfx_thread: std::thread::JoinHandle<Option<(gfx::Sendable<(gfx::Device, gfx::Text)>, Option<Decoded>)>>) {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE);
        let instance: HINSTANCE = GetModuleHandleW(None).unwrap_or_default().into();
        let dpi = placement.dpi;
        let class = WNDCLASSEXW {
            cbSize: size_of::<WNDCLASSEXW>() as u32,
            style: CS_HREDRAW | CS_VREDRAW | CS_DBLCLKS,
            lpfnWndProc: Some(wndproc),
            hInstance: instance,
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
            hbrBackground: HBRUSH(GetStockObject(BLACK_BRUSH).0),
            lpszClassName: w!("LookerWindow"),
            hIcon: load_icon(APP_ICON, GetSystemMetricsForDpi(SM_CXICON, dpi)).unwrap_or_default(),
            hIconSm: load_icon(APP_ICON, GetSystemMetricsForDpi(SM_CXSMICON, dpi)).unwrap_or_default(),
            ..Default::default()
        };
        RegisterClassExW(&class);
        let r = placement.rect;
        let title = path.as_ref().map(|p| file_name(p)).unwrap_or_else(|| "Looker".into());
        let hwnd = CreateWindowExW(
            WINDOW_EX_STYLE(0),
            w!("LookerWindow"),
            &HSTRING::from(title),
            WS_OVERLAPPEDWINDOW,
            r.left,
            r.top,
            r.right - r.left,
            r.bottom - r.top,
            None,
            None,
            Some(instance),
            None,
        )
        .expect("CreateWindowExW");
        let dark = windows::core::BOOL(1);
        let _ = DwmSetWindowAttribute(hwnd, DWMWA_USE_IMMERSIVE_DARK_MODE, &dark as *const _ as _, size_of::<windows::core::BOOL>() as u32);
        crate::trace::mark("window created");

        let mut client = RECT::default();
        let _ = GetClientRect(hwnd, &mut client);
        let dpi = GetDpiForWindow(hwnd);
        let mut app = Box::new(App {
            hwnd,
            gfx: None,
            dpi,
            client_w: (client.right - client.left) as u32,
            client_h: (client.bottom - client.top) as u32,
            active: true,
            maximized: false,
            fullscreen: None,
            pool: pool.clone(),
            listing: None,
            index: None,
            current: None,
            info: None,
            cache: HashMap::new(),
            failed: HashMap::new(),
            pending: HashMap::new(),
            view: View::new(),
            view_for: None,
            first_image_traced: false,
            icon: None,
            icon_pixels: None,
            mouse: (0.0, 0.0),
            tracking: false,
            hover: None,
            pressed: None,
            drag: None,
            cap_hover: None,
            cap_pressed: None,
        });

        // The launch decode was submitted before the window existed; mark it pending so it isn't requested twice.
        if let Some(p) = &path {
            let (bw, bh) = placement.viewport_px();
            app.pending.insert(p.clone(), (bw, bh));
        }

        if let Some(Some((ready, icon))) = gfx_thread.join().ok() {
            let (dev, text) = ready.0;
            match Gfx::attach(dev, text, hwnd, app.client_w, app.client_h, dpi as f32) {
                Ok(g) => {
                    if let Some(i) = &icon {
                        app.icon = g.bitmap(i.width, i.height, &i.pixels).ok();
                    }
                    app.icon_pixels = icon;
                    app.gfx = Some(g);
                }
                Err(e) => crate::trace::mark(format!("gfx attach failed: {e}")),
            }
        }

        let app_ptr = Box::into_raw(app);
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, app_ptr as isize);
        let app = &mut *app_ptr;
        // Re-run the frame calculation now that the handler is attached (drops the system caption).
        let _ = SetWindowPos(hwnd, None, 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_FRAMECHANGED | SWP_NOACTIVATE);

        if let Some(p) = path {
            // Give a fast decode the chance to land in the very first frame.
            if pool.wait_for(&p, 60) {
                crate::trace::mark("launch decode ready before first frame");
            }
            let folder = p.parent().map(Path::to_path_buf);
            app.show_path(p);
            app.on_decoded();
            if let Some(f) = folder {
                app.list_folder_async(f);
            }
        }
        pool.set_window(hwnd);
        app.render();
        crate::trace::mark("first frame presented");
        let _ = ShowWindow(hwnd, SW_SHOW);
        crate::trace::mark("window shown");
        app.start_gpu_device();
        SetTimer(Some(hwnd), TIMER_TRACE, 1500, None);

        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
        std::process::exit(0);
    }
}
