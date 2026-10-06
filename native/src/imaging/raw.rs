//! Camera RAW through LibRaw (bundled), for machines without Microsoft's Raw Image Extension: the DLL ships in the
//! package's `codecs\` folder (scripts/Build-Codecs.ps1, `raw_r.dll`) and loads the first time a RAW needs it.
//! Its one entry point, `looker_raw_decode`, is Looker's own (native/codecs/ports/libraw/looker_raw.cpp): the
//! camera's white balance, sRGB, upright, and half-size (no demosaic, ~4x faster) when that still covers the box.

use std::ffi::{CStr, c_char, c_int, c_void};
use std::sync::OnceLock;

use windows::Win32::System::LibraryLoader::{GetProcAddress, LOAD_LIBRARY_SEARCH_DEFAULT_DIRS, LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR, LoadLibraryExW};
use windows::core::{HSTRING, PCSTR};

type DecodeFn = unsafe extern "C" fn(*const c_void, usize, c_int, c_int, *mut c_int, *mut c_int, *mut c_int, *mut c_int, *mut i64, *mut *mut u8) -> c_int;

struct Lib {
    decode: DecodeFn,
    free: unsafe extern "C" fn(*mut u8),
    error: unsafe extern "C" fn(c_int) -> *const c_char,
}

static LIB: OnceLock<Result<Lib, String>> = OnceLock::new();

fn load() -> Result<Lib, String> {
    let dll = super::heif::codecs_dir().ok_or("no exe path")?.join("raw_r.dll");
    unsafe {
        let m = LoadLibraryExW(&HSTRING::from(dll.as_os_str()), None, LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR | LOAD_LIBRARY_SEARCH_DEFAULT_DIRS)
            .map_err(|e| format!("{}: {}", dll.display(), e.message()))?;
        macro_rules! sym {
            ($name:literal) => {
                std::mem::transmute(GetProcAddress(m, PCSTR(concat!($name, "\0").as_ptr())).ok_or(concat!("raw_r.dll lacks ", $name))?)
            };
        }
        Ok(Lib { decode: sym!("looker_raw_decode"), free: sym!("looker_raw_free"), error: sym!("looker_raw_error") })
    }
}

fn lib() -> Result<&'static Lib, String> {
    LIB.get_or_init(|| {
        let r = load();
        if let Err(e) = &r {
            crate::trace::mark(format!("LibRaw unavailable: {e}"));
        }
        r
    })
    .as_ref()
    .map_err(|e| e.clone())
}

/// Whether the bundled LibRaw is there (decides between decoding a RAW and the get-the-extension hint).
pub fn available() -> bool {
    lib().is_ok()
}

/// A decoded RAW: straight RGBA, upright, plus the full (oriented) size it was decoded from (a half-size decode
/// is smaller).
pub struct Raw {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
    pub full_width: u32,
    pub full_height: u32,
    /// When it was taken (the camera's clock), as Unix seconds.
    pub taken: Option<i64>,
}

/// Decodes `bytes` for a `box_w × box_h` box (`(0, 0)` = full size).
pub fn decode(bytes: &[u8], box_w: u32, box_h: u32) -> Result<Raw, String> {
    let l = lib()?;
    let (mut w, mut h, mut fw, mut fh, mut taken) = (0, 0, 0, 0, 0i64);
    let mut px: *mut u8 = std::ptr::null_mut();
    let r = unsafe { (l.decode)(bytes.as_ptr() as _, bytes.len(), box_w as c_int, box_h as c_int, &mut w, &mut h, &mut fw, &mut fh, &mut taken, &mut px) };
    if r != 0 || px.is_null() {
        let msg = unsafe { CStr::from_ptr((l.error)(r)) }.to_string_lossy().into_owned();
        return Err(format!("LibRaw {r}: {msg}"));
    }
    let n = w as usize * h as usize * 4;
    let rgba = unsafe { std::slice::from_raw_parts(px, n) }.to_vec();
    unsafe { (l.free)(px) };
    Ok(Raw { width: w as u32, height: h as u32, rgba, full_width: fw as u32, full_height: fh as u32, taken: (taken > 0).then_some(taken) })
}

#[cfg(test)]
mod tests {
    /// Decodes every RAW sample through LibRaw (this machine's Raw Image Extension would take them otherwise).
    /// `LOOKER_CODECS=%LOCALAPPDATA%\Looker\codecs cargo test --release -- --ignored --nocapture libraw`
    #[test]
    #[ignore]
    fn libraw_decodes_the_samples() {
        let dir = std::path::PathBuf::from(std::env::var("USERPROFILE").unwrap()).join("Pictures").join("Looker Test Photos");
        let mut paths: Vec<_> = std::fs::read_dir(&dir).unwrap().flatten().map(|e| e.path()).filter(|p| crate::format::sniff_file(p) == crate::format::Format::Raw).collect();
        paths.sort();
        let mut failed = Vec::new();
        for p in &paths {
            let bytes = std::fs::read(p).unwrap();
            for (bw, bh) in [(1280, 800), (0, 0)] {
                let t = std::time::Instant::now();
                match super::decode(&bytes, bw, bh) {
                    Ok(r) => {
                        assert_eq!(r.rgba.len(), r.width as usize * r.height as usize * 4);
                        println!(
                            "{:<40} box {bw}x{bh}: {}x{} of {}x{} in {:.0} ms",
                            p.file_name().unwrap().to_string_lossy(),
                            r.width,
                            r.height,
                            r.full_width,
                            r.full_height,
                            t.elapsed().as_secs_f64() * 1000.0
                        )
                    }
                    Err(e) => {
                        println!("{:<40} box {bw}x{bh}: FAIL {e}", p.file_name().unwrap().to_string_lossy());
                        failed.push(p.clone());
                    }
                }
            }
        }
        assert!(!paths.is_empty());
        println!("{} RAW files, {} failed decodes", paths.len(), failed.len());
    }
}
