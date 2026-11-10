//! Looker's own red file icons (the ones its file associations show, `assets/FileIcons`) for the text types it
//! opens, so the explorer and the text tabs show those rather than Notepad's (Faye, 2026-11-11). Embedded, so dev
//! runs have them too; decoded once per size on the UI thread (a few small PNGs).

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::Arc;

use crate::shell_icons::Icon;

macro_rules! sizes {
    ($ext:literal) => {
        [
            (16, include_bytes!(concat!("../assets/FileIcons/", $ext, ".targetsize-16.png")) as &[u8]),
            (24, include_bytes!(concat!("../assets/FileIcons/", $ext, ".targetsize-24.png"))),
            (32, include_bytes!(concat!("../assets/FileIcons/", $ext, ".targetsize-32.png"))),
            (48, include_bytes!(concat!("../assets/FileIcons/", $ext, ".targetsize-48.png"))),
            (64, include_bytes!(concat!("../assets/FileIcons/", $ext, ".targetsize-64.png"))),
            (256, include_bytes!(concat!("../assets/FileIcons/", $ext, ".targetsize-256.png"))),
        ]
    };
}

type Set = [(u32, &'static [u8]); 6];

/// The text types Looker opens (`Format::Text`), each with its icon at every size made.
static TEXT: [(&str, Set); 9] = [
    ("txt", sizes!("txt")),
    ("md", sizes!("md")),
    ("markdown", sizes!("markdown")),
    ("log", sizes!("log")),
    ("ini", sizes!("ini")),
    ("cfg", sizes!("cfg")),
    ("vdf", sizes!("vdf")),
    ("qc", sizes!("qc")),
    ("vmt", sizes!("vmt")),
];

thread_local! {
    static CACHE: RefCell<HashMap<(&'static str, u32), Option<Arc<Icon>>>> = RefCell::new(HashMap::new());
}

/// Looker's icon for this extension (lower case) at `px` device pixels, if it's a text type Looker opens.
pub fn text_icon(ext: &str, px: u32) -> Option<Arc<Icon>> {
    let (name, set) = TEXT.iter().find(|(e, _)| *e == ext)?;
    let px = px.clamp(8, 256);
    CACHE.with(|c| {
        c.borrow_mut()
            .entry((name, px))
            .or_insert_with(|| {
                // The smallest drawing at least this big (its own tuning at small sizes), scaled to fit exactly.
                let bytes = set.iter().find(|(s, _)| *s >= px).unwrap_or(&set[5]).1;
                let d = crate::imaging::wic::decode_icon(bytes, px).ok()?;
                let f = d.frames.into_iter().next()?;
                Some(Arc::new(Icon { w: d.width, h: d.height, bgra: f.pixels }))
            })
            .clone()
    })
}

#[cfg(test)]
mod tests {
    #[test]
    fn every_text_icon_decodes() {
        unsafe {
            let _ = windows::Win32::System::Com::CoInitializeEx(None, windows::Win32::System::Com::COINIT_APARTMENTTHREADED);
        }
        for (e, _) in super::TEXT.iter() {
            for px in [16, 20, 36, 96] {
                let i = super::text_icon(e, px).unwrap_or_else(|| panic!("{e} at {px}"));
                assert_eq!((i.w, i.h), (px, px));
            }
        }
        assert!(super::text_icon("exe", 16).is_none());
    }
}
