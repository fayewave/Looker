//! The window: a plain Win32 window with the system caption removed and everything inside drawn by us.
//! Layout (DIPs, as in MainWindow.xaml): title bar 48 / toolbar 40 / viewport / status row 28.


mod chrome;
mod input;

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

use crate::decode::{self, Decoded, Pool};
use crate::engine::Key;
use crate::folder::{self, Listing};
use crate::format;
use crate::settings::{self, SavedWindow, Settings};
use crate::viewer::{self, Outcome, Viewer};
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

static APP_ICON: &[u8] = include_bytes!("../../../src/Looker/Assets/AppIcon.ico");

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

struct FileInfo {
    size: u64,
    modified: FILETIME,
}

pub struct Placement {
    /// The restored window rect.
    pub rect: RECT,
    pub dpi: u32,
    pub maximized: bool,
    /// The work area of the monitor the window opens on (a maximized window's client area).
    pub work: RECT,
}

impl Placement {
    /// The viewport the first image will be fitted into, in device pixels.
    pub fn viewport_px(&self) -> (u32, u32) {
        let s = self.dpi as f32 / 96.0;
        let (w, h) = if self.maximized {
            ((self.work.right - self.work.left).max(1) as u32, (self.work.bottom - self.work.top).max(1) as u32)
        } else {
            client_size_for(self)
        };
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

/// The saved window placement when it is still on a connected monitor, else the default window.
pub fn initial_placement(settings: &Settings) -> Placement {
    if let Some(w) = settings.window.filter(|_| settings.remember_window) {
        let rect = RECT { left: w.left, top: w.top, right: w.right, bottom: w.bottom };
        unsafe {
            let mon = MonitorFromRect(&rect, MONITOR_DEFAULTTONULL);
            if !mon.is_invalid() {
                let mut mi = MONITORINFO { cbSize: size_of::<MONITORINFO>() as u32, ..Default::default() };
                let _ = GetMonitorInfoW(mon, &mut mi);
                let (mut dx, mut dy) = (96u32, 96u32);
                let _ = GetDpiForMonitor(mon, MDT_EFFECTIVE_DPI, &mut dx, &mut dy);
                return Placement { rect, dpi: dx, maximized: w.maximized, work: mi.rcWork };
            }
        }
    }
    default_placement()
}

/// Default window: 1200 × 800 DIPs centred on the primary monitor's work area.
fn default_placement() -> Placement {
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
        Placement { rect: RECT { left: x, top: y, right: x + w, bottom: y + h }, dpi: dx, maximized: false, work }
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

    viewer: Viewer,
    settings: Settings,
    /// The window rect while restored (not maximized, minimized or fullscreen): what is saved on close.
    normal_rect: RECT,
    info: Option<FileInfo>,
    view: View,
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
    fn current_path(&self) -> Option<&Path> {
        self.viewer.current.as_ref().map(|(p, _)| p.as_path())
    }

    // --- Images -------------------------------------------------------------------------------------

    /// The device-pixel box a fit decode targets.
    fn fit_box(&self) -> (u32, u32) {
        let v = self.viewport();
        let s = self.scale();
        (((v.right - v.left) * s).ceil().max(1.0) as u32, ((v.bottom - v.top) * s).ceil().max(1.0) as u32)
    }

    /// Applies what a viewer call says: a new image resets the zoom (and may want a sharper decode).
    fn apply(&mut self, out: Outcome) {
        if let Some((nw, nh)) = out.reset_view {
            self.reset_view(nw, nh);
            self.schedule_upgrade();
        }
        if out.redraw {
            self.invalidate();
        }
    }

    /// After every navigation: title, status-row file facts.
    fn current_changed(&mut self) {
        let Some(path) = self.current_path().map(Path::to_path_buf) else { return };
        self.info = std::fs::metadata(&path).ok().map(|m| {
            let t = m.last_write_time();
            FileInfo { size: m.len(), modified: FILETIME { dwLowDateTime: t as u32, dwHighDateTime: (t >> 32) as u32 } }
        });
        unsafe {
            let _ = SetWindowTextW(self.hwnd, &HSTRING::from(file_name(&path)));
        }
        self.invalidate();
    }

    fn show_path(&mut self, path: PathBuf) {
        let stamp = crate::engine::stamp(&path);
        let out = self.viewer.show(path, stamp, self.gfx.as_ref());
        self.apply(out);
        self.current_changed();
    }

    fn reset_view(&mut self, nw: u32, nh: u32) {
        let v = self.viewport();
        self.view.reset((v.right - v.left) as f64, (v.bottom - v.top) as f64, nw as f64, nh as f64, self.scale() as f64);
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
        let r = self.view.target_rect();
        let needed = (r.w.max(r.h) * self.scale() as f64).ceil() as u32;
        let max = self.gfx.as_ref().map_or(16384, |g| g.max_bitmap_size());
        let out = self.viewer.upgrade(needed, max);
        self.apply(out);
    }

    fn on_decoded(&mut self) {
        let out = self.viewer.on_results(self.gfx.as_ref());
        self.apply(out);
    }

    fn step(&mut self, delta: isize) {
        let out = self.viewer.step(delta, self.gfx.as_ref());
        self.apply(out);
        self.current_changed();
    }

    fn jump(&mut self, last: bool) {
        let n = self.viewer.image_count();
        if n == 0 {
            return;
        }
        let j = if last { n - 1 } else { 0 };
        if Some(j) != self.viewer.index {
            let out = self.viewer.show_index(j, self.gfx.as_ref());
            self.apply(out);
            self.current_changed();
        }
    }

    fn on_listed(&mut self, listing: Listing) {
        if self.viewer.set_listing(listing) {
            self.invalidate();
        }
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
        self.viewer.close();
        if let Some(folder) = path.parent() {
            self.list_folder_async(folder.to_path_buf());
        }
        self.show_path(path);
    }

    fn close(&mut self) {
        self.viewer.close();
        self.info = None;
        self.view.clear();
        unsafe {
            let _ = SetWindowTextW(self.hwnd, w!("Looker"));
        }
        self.invalidate();
    }

    fn open_dialog(&mut self) {
        unsafe {
            let Ok(dlg) = CoCreateInstance::<_, IFileOpenDialog>(&FileOpenDialog, None, CLSCTX_INPROC_SERVER) else { return };
            let pattern = HSTRING::from(format::EXTENSIONS.iter().map(|e| format!("*.{e}")).collect::<Vec<_>>().join(";"));
            let filters = [COMDLG_FILTERSPEC { pszName: w!("Images"), pszSpec: PCWSTR(pattern.as_ptr()) }];
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
        let Some(path) = self.current_path() else { return };
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
        self.viewer.reupload(g);
        self.icon = self.icon_pixels.as_ref().and_then(|i| g.bitmap(i.width, i.height, &i.frames[0].pixels).ok());
        self.render();
        if let Some(g) = &self.gfx {
            let _ = g.commit_swap();
        }
        crate::trace::mark("switched to the hardware device");
    }

    // --- Placement ----------------------------------------------------------------------------------

    fn track_normal_rect(&mut self) {
        unsafe {
            if self.fullscreen.is_none() && !IsZoomed(self.hwnd).as_bool() && !IsIconic(self.hwnd).as_bool() {
                let mut r = RECT::default();
                if GetWindowRect(self.hwnd, &mut r).is_ok() && r.right > r.left && r.bottom > r.top {
                    self.normal_rect = r;
                }
            }
        }
    }

    /// Saved on close only while "remember window placement" is on.
    fn save_placement(&mut self) {
        if !self.settings.remember_window {
            return;
        }
        let r = self.normal_rect;
        let maximized = self.fullscreen.is_none() && unsafe { IsZoomed(self.hwnd).as_bool() };
        self.settings.window = Some(SavedWindow { left: r.left, top: r.top, right: r.right, bottom: r.bottom, maximized });
        settings::save(&self.settings);
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

pub fn run(path: Option<PathBuf>, launch_keys: Vec<Key>, settings: Settings, placement: Placement, pool: Arc<Pool>, gfx_thread: std::thread::JoinHandle<Option<(gfx::Sendable<(gfx::Device, gfx::Text)>, Option<Decoded>)>>) {
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
            viewer: Viewer::new(hwnd, pool.clone()),
            settings,
            normal_rect: placement.rect,
            info: None,
            view: View::new(),
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

        // The launch decodes were submitted before the window existed; mark them pending so they aren't
        // requested twice.
        app.viewer.adopt_pending(launch_keys);
        // The viewport `main` predicted (a maximized window is still restored-size until it is shown), so the
        // first requests match the launch decodes already running. WM_SIZE corrects it from here on.
        let (bw, bh) = placement.viewport_px();
        app.viewer.set_fit_box(bw, bh);

        if let Some(Some((ready, icon))) = gfx_thread.join().ok() {
            let (dev, text) = ready.0;
            match Gfx::attach(dev, text, hwnd, app.client_w, app.client_h, dpi as f32) {
                Ok(g) => {
                    if let Some(i) = &icon {
                        app.icon = g.bitmap(i.width, i.height, &i.frames[0].pixels).ok();
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
        let _ = ShowWindow(hwnd, if placement.maximized { SW_SHOWMAXIMIZED } else { SW_SHOW });
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
