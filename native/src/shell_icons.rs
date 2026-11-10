//! Windows' own icons for folders, drives and This PC (the solid, coloured ones File Explorer shows), as
//! Transient's Browser tab draws them (`veil-waveform/src/icons.rs`): fetched on one background STA thread
//! from the shell's system image lists and kept as premultiplied BGRA pixels; the explorer card draws them
//! through `Gfx::draw_icon`, which makes each a GPU bitmap once.
//!
//! An ordinary folder is looked up by attributes only (`SHGFI_USEFILEATTRIBUTES`), which never touches the
//! disk, so cloud placeholders stay online-only. Drives, special folders (Desktop, Downloads, …), bookmarks,
//! history and folders marked customised (read-only, as Windows marks a folder with a `desktop.ini`) are
//! looked up by their real path, for their own icons. Each lookup asks for the system image list index, and
//! an index is extracted once per list: a thousand folders are one icon.

use std::collections::{HashMap, HashSet, VecDeque};
use std::os::windows::ffi::OsStrExt;
use std::path::PathBuf;
use std::sync::atomic::{AtomicIsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock};

use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BI_RGB, BITMAP, BITMAPINFO, BITMAPINFOHEADER, DIB_RGB_COLORS, DeleteObject, GetDC, GetDIBits, GetObjectW, HBITMAP, HGDIOBJ, ReleaseDC,
};
use windows::Win32::Storage::FileSystem::{FILE_ATTRIBUTE_DIRECTORY, FILE_FLAGS_AND_ATTRIBUTES};
use windows::Win32::System::Com::{COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE, CoInitializeEx, CoTaskMemFree};
use windows::Win32::UI::Controls::{IImageList, ILD_TRANSPARENT};
use windows::Win32::UI::Shell::{
    FOLDERID_ComputerFolder, KF_FLAG_DEFAULT, SHFILEINFOW, SHGFI_FLAGS, SHGFI_PIDL, SHGFI_SYSICONINDEX, SHGFI_USEFILEATTRIBUTES, SHGetFileInfoW,
    SHGetImageList, SHGetKnownFolderIDList, SHIL_EXTRALARGE, SHIL_LARGE, SHIL_SMALL,
};
use windows::Win32::UI::WindowsAndMessaging::{DestroyIcon, GetIconInfo, HICON, ICONINFO, PostMessageW, WM_APP};
use windows::core::PCWSTR;

/// Posted to the window when icons arrive: draw again.
pub const WM_SHELL_ICONS: u32 = WM_APP + 24;

#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub enum Kind {
    /// Any ordinary folder (one shared icon, never read from disk).
    Folder,
    /// This very folder or drive, by path (special folders and drives have their own icons).
    Path(PathBuf),
    /// This PC.
    Computer,
}

pub struct Icon {
    pub w: u32,
    pub h: u32,
    /// Premultiplied BGRA, top-down rows.
    pub bgra: Vec<u8>,
}

/// What's asked for: the icon, and how many device pixels across it will be drawn.
type Key = (Kind, u32);

#[derive(Default)]
struct State {
    /// None: looked up and Windows had nothing (the caller draws its glyph).
    done: HashMap<Key, Option<Arc<Icon>>>,
    queue: VecDeque<Key>,
    queued: HashSet<Key>,
    started: bool,
}

struct Shared {
    state: Mutex<State>,
    cv: Condvar,
    hwnd: AtomicIsize,
}

static SHARED: OnceLock<Shared> = OnceLock::new();

/// The window to wake when icons arrive.
pub fn init(hwnd: HWND) {
    let sh = SHARED.get_or_init(|| Shared { state: Mutex::new(State::default()), cv: Condvar::new(), hwnd: AtomicIsize::new(0) });
    sh.hwnd.store(hwnd.0 as isize, Ordering::Release);
}

pub enum Lookup {
    Ready(Arc<Icon>),
    /// Being fetched (the window wakes when it arrives): draw nothing yet.
    Pending,
    /// Windows has none: draw a stand-in.
    Missing,
}

/// The icon `px` device pixels across if it's ready; otherwise asks for it. Never waits: drawing calls this
/// every frame.
pub fn get(kind: &Kind, px: u32) -> Lookup {
    let Some(sh) = SHARED.get() else { return Lookup::Missing };
    let Ok(mut st) = sh.state.try_lock() else { return Lookup::Pending };
    let key = (kind.clone(), px);
    if let Some(found) = st.done.get(&key) {
        return match found {
            Some(i) => Lookup::Ready(i.clone()),
            None => Lookup::Missing,
        };
    }
    if st.queued.insert(key.clone()) {
        st.queue.push_back(key);
        if !st.started {
            st.started = true;
            if std::thread::Builder::new().name("shell-icons".into()).spawn(worker).is_err() {
                st.started = false;
            }
        }
        sh.cv.notify_one();
    }
    Lookup::Pending
}

/// The system image lists (small, large, extra-large: 16, 32 and 48 px at 100 %, scaled with the system DPI)
/// and the size each holds.
struct Lists(Vec<(IImageList, u32)>);

impl Lists {
    fn open() -> Lists {
        let mut out = Vec::new();
        for id in [SHIL_SMALL, SHIL_LARGE, SHIL_EXTRALARGE] {
            unsafe {
                if let Ok(l) = SHGetImageList::<IImageList>(id as i32) {
                    let (mut cx, mut cy) = (0, 0);
                    if l.GetIconSize(&mut cx, &mut cy).is_ok() && cx > 0 {
                        out.push((l, cx as u32));
                    }
                }
            }
        }
        Lists(out)
    }

    /// The smallest list at least `px` across (so it's drawn 1:1 or scaled down, never up), else the largest.
    fn pick(&self, px: u32) -> Option<usize> {
        self.0.iter().position(|(_, s)| *s >= px).or_else(|| self.0.len().checked_sub(1))
    }
}

fn worker() {
    let sh = SHARED.get().unwrap();
    unsafe {
        // The shell's icon lookups want a single-threaded apartment.
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE);
    }
    let lists = Lists::open();
    // Each image list index is extracted once per list, whatever asked for it.
    let mut extracted: HashMap<(usize, i32), Option<Arc<Icon>>> = HashMap::new();
    loop {
        let key = {
            let mut st = sh.state.lock().unwrap();
            loop {
                if let Some(k) = st.queue.pop_front() {
                    break k;
                }
                st = sh.cv.wait(st).unwrap();
            }
        };
        let icon = lists.pick(key.1).and_then(|li| {
            let index = icon_index(&key.0)?;
            extracted.entry((li, index)).or_insert_with(|| unsafe { extract(&lists.0[li].0, index) }.map(Arc::new)).clone()
        });
        {
            let mut st = sh.state.lock().unwrap();
            st.queued.remove(&key);
            st.done.insert(key, icon);
        }
        let h = sh.hwnd.load(Ordering::Acquire);
        if h != 0 {
            unsafe {
                let _ = PostMessageW(Some(HWND(h as _)), WM_SHELL_ICONS, WPARAM(0), LPARAM(0));
            }
        }
    }
}

/// The icon's index in the system image lists.
fn icon_index(kind: &Kind) -> Option<i32> {
    let mut sfi = SHFILEINFOW::default();
    let size = std::mem::size_of::<SHFILEINFOW>() as u32;
    unsafe {
        let ok = match kind {
            Kind::Folder => {
                let path = wide("folder");
                SHGetFileInfoW(PCWSTR(path.as_ptr()), FILE_ATTRIBUTE_DIRECTORY, Some(&mut sfi), size, SHGFI_SYSICONINDEX | SHGFI_USEFILEATTRIBUTES)
            }
            Kind::Path(p) => {
                let path: Vec<u16> = p.as_os_str().encode_wide().chain(Some(0)).collect();
                SHGetFileInfoW(PCWSTR(path.as_ptr()), FILE_FLAGS_AND_ATTRIBUTES(0), Some(&mut sfi), size, SHGFI_SYSICONINDEX)
            }
            Kind::Computer => {
                let pidl = SHGetKnownFolderIDList(&FOLDERID_ComputerFolder, KF_FLAG_DEFAULT.0 as u32, None).ok()?;
                let r = SHGetFileInfoW(PCWSTR(pidl as *const u16), FILE_FLAGS_AND_ATTRIBUTES(0), Some(&mut sfi), size, SHGFI_FLAGS(SHGFI_SYSICONINDEX.0 | SHGFI_PIDL.0));
                CoTaskMemFree(Some(pidl as _));
                r
            }
        };
        (ok != 0).then_some(sfi.iIcon)
    }
}

unsafe fn extract(list: &IImageList, index: i32) -> Option<Icon> {
    unsafe {
        let hicon = list.GetIcon(index, ILD_TRANSPARENT.0).ok()?;
        let icon = pixels(hicon);
        let _ = DestroyIcon(hicon);
        icon
    }
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

/// An HICON's pixels as premultiplied BGRA (an icon's colour bitmap has straight alpha). Icons without an
/// alpha channel (old 24-bit ones) get it from their mask.
unsafe fn pixels(hicon: HICON) -> Option<Icon> {
    unsafe {
        let mut info = ICONINFO::default();
        GetIconInfo(hicon, &mut info).ok()?;
        let mut bm = BITMAP::default();
        let got = if info.hbmColor.is_invalid() { 0 } else { GetObjectW(HGDIOBJ(info.hbmColor.0), size_of::<BITMAP>() as i32, Some(&mut bm as *mut _ as _)) };
        let (w, h) = (bm.bmWidth, bm.bmHeight.abs());
        let read = |hbm: HBITMAP| -> Option<Vec<u8>> {
            let mut bmi = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: w,
                    biHeight: -h, // top-down
                    biPlanes: 1,
                    biBitCount: 32,
                    biCompression: BI_RGB.0,
                    ..Default::default()
                },
                ..Default::default()
            };
            let mut buf = vec![0u8; (w * h * 4) as usize];
            let dc = GetDC(None);
            let lines = GetDIBits(dc, hbm, 0, h as u32, Some(buf.as_mut_ptr() as _), &mut bmi, DIB_RGB_COLORS);
            ReleaseDC(None, dc);
            (lines == h).then_some(buf)
        };
        let color = if got != 0 && w > 0 && h > 0 { read(info.hbmColor) } else { None };
        let mask = if color.as_ref().is_some_and(|c| c.chunks_exact(4).all(|p| p[3] == 0)) { read(info.hbmMask) } else { None };
        if !info.hbmColor.is_invalid() {
            let _ = DeleteObject(HGDIOBJ(info.hbmColor.0));
        }
        if !info.hbmMask.is_invalid() {
            let _ = DeleteObject(HGDIOBJ(info.hbmMask.0));
        }
        let mut bgra = color?;
        premultiply(&mut bgra, mask.as_deref());
        Some(Icon { w: w as u32, h: h as u32, bgra })
    }
}

/// Straight BGRA to premultiplied; with a mask (white is transparent) its alpha replaces the pixels'.
fn premultiply(bgra: &mut [u8], mask: Option<&[u8]>) {
    for (i, p) in bgra.chunks_exact_mut(4).enumerate() {
        let a = match mask {
            Some(m) => {
                if m[i * 4] == 0 { 255 } else { 0 }
            }
            None => p[3],
        };
        for c in &mut p[..3] {
            *c = ((*c as u32 * a as u32 + 127) / 255) as u8;
        }
        p[3] = a;
    }
}

/// `icon` recoloured in `tint` (0xRRGGBB), its shading kept and its hue replaced (as Transient does a selected
/// folder on the accent): the lightness against a folder's usual (about 0.7) scales the tint.
pub fn tinted(icon: &Icon, tint: u32) -> Vec<u8> {
    let t = [(tint & 0xFF) as f32, ((tint >> 8) & 0xFF) as f32, ((tint >> 16) & 0xFF) as f32];
    icon.bgra
        .chunks_exact(4)
        .flat_map(|p| {
            let a = p[3] as f32;
            if a == 0.0 {
                return [0u8; 4];
            }
            let l = (0.114 * p[0] as f32 + 0.587 * p[1] as f32 + 0.299 * p[2] as f32) / a;
            let k = (l / 0.7).min(1.35);
            let c = |i: usize| ((t[i] * k).min(255.0) * a / 255.0).round() as u8;
            [c(0), c(1), c(2), p[3]]
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folder_drive_and_computer_icons_come_back() {
        unsafe {
            let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        }
        let lists = Lists::open();
        assert!(!lists.0.is_empty(), "no image lists");
        let small = lists.pick(1).unwrap();
        for kind in [Kind::Folder, Kind::Path(PathBuf::from("C:\\")), Kind::Computer] {
            let index = icon_index(&kind).unwrap_or_else(|| panic!("no index for {kind:?}"));
            let icon = unsafe { extract(&lists.0[small].0, index) }.unwrap_or_else(|| panic!("no icon for {kind:?}"));
            assert!(icon.w >= 16 && icon.h >= 16, "{kind:?} {}x{}", icon.w, icon.h);
            assert!(icon.bgra.chunks_exact(4).any(|p| p[3] > 0), "{kind:?} is blank");
            assert!(icon.bgra.chunks_exact(4).all(|p| p[0] <= p[3] && p[1] <= p[3] && p[2] <= p[3]), "{kind:?} isn't premultiplied");
        }
        // The drive's own icon, not the folder's.
        assert_ne!(icon_index(&Kind::Folder), icon_index(&Kind::Path(PathBuf::from("C:\\"))));
    }

    #[test]
    fn mask_alpha_and_premultiplying() {
        let mut px = vec![200, 100, 50, 128, 10, 20, 30, 0];
        premultiply(&mut px, None);
        assert_eq!(px, [100, 50, 25, 128, 0, 0, 0, 0]);
        let mut px = vec![200, 100, 50, 0, 10, 20, 30, 0];
        premultiply(&mut px, Some(&[0, 0, 0, 0, 255, 255, 255, 0]));
        assert_eq!(px, [200, 100, 50, 255, 0, 0, 0, 0]);
    }
}
