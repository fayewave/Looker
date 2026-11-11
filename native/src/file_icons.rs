//! Looker's own red file icons (the ones its file associations show, `assets/FileIcons`, one per extension it
//! opens): the explorer shows them for the text types, PDFs and audio rather than other apps' (Faye, 2026-11-11), and
//! the thumbnail strip tags every cell with one. Read from the package's `Assets\FileIcons` (dev runs: the source
//! tree's), decoded once per extension and size on the UI thread (small PNGs).

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, OnceLock};

use crate::shell_icons::Icon;

/// The sizes `scripts/New-FileIcons.ps1` draws each icon at.
const SIZES: [u32; 6] = [16, 24, 32, 48, 64, 256];

/// Where the icons are: beside the exe in a package, else the source tree (dev runs and tests).
fn dir() -> &'static Option<PathBuf> {
    static DIR: OnceLock<Option<PathBuf>> = OnceLock::new();
    DIR.get_or_init(|| {
        let packaged = std::env::current_exe().ok().and_then(|e| e.parent().map(|p| p.join("Assets").join("FileIcons")));
        let source = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("assets").join("FileIcons");
        packaged.filter(|p| p.is_dir()).or_else(|| source.is_dir().then_some(source))
    })
}

thread_local! {
    static CACHE: RefCell<HashMap<(String, u32), Option<Arc<Icon>>>> = RefCell::new(HashMap::new());
}

/// Looker's icon for this extension at `px` device pixels, if Looker opens the type.
pub fn icon(ext: &str, px: u32) -> Option<Arc<Icon>> {
    let ext = ext.to_ascii_lowercase();
    if ext.is_empty() || !ext.bytes().all(|b| b.is_ascii_alphanumeric()) {
        return None;
    }
    let px = px.clamp(8, 256);
    CACHE.with(|c| {
        c.borrow_mut()
            .entry((ext.clone(), px))
            .or_insert_with(|| {
                let dir = dir().as_ref()?;
                // The smallest drawing at least this big (its own tuning at small sizes), scaled to fit exactly.
                let size = SIZES.iter().copied().find(|&s| s >= px).unwrap_or(256);
                let bytes = std::fs::read(dir.join(format!("{ext}.targetsize-{size}.png"))).ok()?;
                let d = crate::imaging::wic::decode_icon(&bytes, px).ok()?;
                let f = d.frames.into_iter().next()?;
                Some(Arc::new(Icon { w: d.width, h: d.height, bgra: f.pixels }))
            })
            .clone()
    })
}

/// The explorer's: Looker's icon for the types whose Windows icon is another app's (text, PDF, audio).
pub fn explorer_icon(ext: &str, px: u32) -> Option<Arc<Icon>> {
    use crate::format::Format;
    match crate::format::from_extension(Some(ext)) {
        Format::Text | Format::Pdf | Format::Audio => icon(ext, px),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn every_extension_has_an_icon() {
        unsafe {
            let _ = windows::Win32::System::Com::CoInitializeEx(None, windows::Win32::System::Com::COINIT_APARTMENTTHREADED);
        }
        for e in crate::format::EXTENSIONS {
            let e = e.trim_start_matches('.');
            for px in [16, 20, 96] {
                let i = super::icon(e, px).unwrap_or_else(|| panic!("{e} at {px}"));
                assert_eq!((i.w, i.h), (px, px));
            }
        }
        assert!(super::explorer_icon("pdf", 16).is_some());
        assert!(super::explorer_icon("mp3", 16).is_some());
        assert!(super::explorer_icon("jpg", 16).is_none());
        assert!(super::icon("exe", 16).is_none());
    }
}
