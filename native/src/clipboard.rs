//! The clipboard: a path as text, and "Copy image" as both the file (pastes into Explorer) and a bitmap
//! (pastes into Paint, chat apps, documents), as the C# app's DataPackage did. The clipboard owns each
//! global block once it is set.

use std::path::Path;

use windows::Win32::Foundation::{GlobalFree, HANDLE, HGLOBAL, HWND};
use windows::Win32::Graphics::Gdi::{BI_BITFIELDS, BITMAPV5HEADER};
use windows::Win32::System::DataExchange::{CloseClipboard, EmptyClipboard, OpenClipboard, RegisterClipboardFormatW, SetClipboardData};
use windows::Win32::System::Memory::{GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalUnlock};
use windows::Win32::System::Ole::{CF_DIBV5, CF_HDROP, CF_UNICODETEXT};
use windows::Win32::UI::Shell::DROPFILES;
use windows::core::w;

use crate::fileops::Image;

/// `LCS_sRGB`: the bitmap's colours are sRGB (what every decode ends in).
const LCS_SRGB: u32 = 0x7352_4742;
const DROPEFFECT_COPY: u32 = 1;

/// A movable global block holding `bytes`.
fn global(bytes: &[u8]) -> Option<HGLOBAL> {
    unsafe {
        let mem = GlobalAlloc(GMEM_MOVEABLE, bytes.len()).ok()?;
        let p = GlobalLock(mem) as *mut u8;
        if p.is_null() {
            let _ = GlobalFree(Some(mem));
            return None;
        }
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), p, bytes.len());
        let _ = GlobalUnlock(mem);
        Some(mem)
    }
}

/// Opens and empties the clipboard, runs `fill`, closes it.
fn with_clipboard(hwnd: HWND, fill: impl FnOnce() -> bool) -> bool {
    unsafe {
        if OpenClipboard(Some(hwnd)).is_err() {
            return false;
        }
        let _ = EmptyClipboard();
        let ok = fill();
        let _ = CloseClipboard();
        ok
    }
}

fn put(format: u32, bytes: &[u8]) -> bool {
    let Some(mem) = global(bytes) else { return false };
    unsafe {
        if SetClipboardData(format, Some(HANDLE(mem.0))).is_ok() {
            true
        } else {
            let _ = GlobalFree(Some(mem));
            false
        }
    }
}

fn utf16z(s: &str) -> Vec<u8> {
    s.encode_utf16().chain([0]).flat_map(u16::to_le_bytes).collect()
}

pub fn set_text(hwnd: HWND, text: &str) -> bool {
    with_clipboard(hwnd, || put(CF_UNICODETEXT.0 as u32, &utf16z(text)))
}

/// The file (CF_HDROP, marked as a copy) and, when it decoded, its pixels (CF_DIBV5).
pub fn set_image(hwnd: HWND, path: &Path, image: Option<&Image>) -> bool {
    let drop = hdrop(path);
    let dib = image.map(dibv5);
    with_clipboard(hwnd, || {
        let mut ok = put(CF_HDROP.0 as u32, &drop);
        unsafe {
            let effect = RegisterClipboardFormatW(w!("Preferred DropEffect"));
            if effect != 0 {
                put(effect, &DROPEFFECT_COPY.to_le_bytes());
            }
        }
        if let Some(d) = &dib {
            ok &= put(CF_DIBV5.0 as u32, d);
        }
        ok
    })
}

/// DROPFILES followed by the wide path and a double NUL.
fn hdrop(path: &Path) -> Vec<u8> {
    let header = DROPFILES { pFiles: size_of::<DROPFILES>() as u32, fWide: true.into(), ..Default::default() };
    let mut out = unsafe { std::slice::from_raw_parts(&header as *const _ as *const u8, size_of::<DROPFILES>()) }.to_vec();
    out.extend(utf16z(&path.to_string_lossy()));
    out.extend([0, 0]);
    out
}

/// A bottom-up 32-bit DIB with straight alpha and explicit channel masks.
pub fn dibv5(img: &Image) -> Vec<u8> {
    let (w, h) = (img.width as usize, img.height as usize);
    let header = BITMAPV5HEADER {
        bV5Size: size_of::<BITMAPV5HEADER>() as u32,
        bV5Width: w as i32,
        bV5Height: h as i32,
        bV5Planes: 1,
        bV5BitCount: 32,
        bV5Compression: BI_BITFIELDS,
        bV5SizeImage: (w * h * 4) as u32,
        bV5RedMask: 0x00FF_0000,
        bV5GreenMask: 0x0000_FF00,
        bV5BlueMask: 0x0000_00FF,
        bV5AlphaMask: 0xFF00_0000,
        bV5CSType: LCS_SRGB,
        bV5Intent: 4, // LCS_GM_IMAGES
        ..Default::default()
    };
    let mut out = Vec::with_capacity(size_of::<BITMAPV5HEADER>() + w * h * 4);
    out.extend_from_slice(unsafe { std::slice::from_raw_parts(&header as *const _ as *const u8, size_of::<BITMAPV5HEADER>()) });
    for y in (0..h).rev() {
        for p in img.pixels[y * w * 4..(y + 1) * w * 4].chunks_exact(4) {
            let a = p[3] as u32;
            let un = |c: u8| if a == 0 { 0 } else { ((c as u32 * 255 + a / 2) / a).min(255) as u8 };
            out.extend([un(p[0]), un(p[1]), un(p[2]), p[3]]);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dib_is_bottom_up_with_straight_alpha() {
        // 1 x 2: top pixel half-transparent premultiplied red, bottom opaque blue.
        let img = Image { width: 1, height: 2, pixels: vec![0, 0, 128, 128, 255, 0, 0, 255] };
        let d = dibv5(&img);
        let px = &d[size_of::<BITMAPV5HEADER>()..];
        assert_eq!(&px[0..4], &[255, 0, 0, 255]); // bottom row first
        assert_eq!(&px[4..8], &[0, 0, 255, 128]);
    }

    #[test]
    fn hdrop_ends_in_a_double_nul() {
        let d = hdrop(Path::new("C:\\a.jpg"));
        assert_eq!(u32::from_le_bytes(d[0..4].try_into().unwrap()), size_of::<DROPFILES>() as u32);
        assert!(d.ends_with(&[0, 0, 0, 0]));
    }
}
