//! HEIC/HEIF and AVIF through libheif (with libde265 for HEVC and dav1d for AV1), for machines where WIC can't
//! decode them: Windows needs the HEVC Video Extension for HEIC, which is a paid Store add-on, and the AV1
//! Video Extension for AVIF. The DLLs ship in the package's `codecs\` folder (scripts/Build-Codecs.ps1) and are
//! loaded the first time a file needs them, so startup never pays for them; functions are looked up by name,
//! with no import library.
//!
//! libheif applies the file's own rotation and mirroring (`irot`/`imir`) while decoding, so the pixels come out
//! upright; a HEIF's EXIF orientation must not be applied on top.

use std::ffi::{CStr, c_char, c_int, c_void};
use std::sync::OnceLock;

use windows::Win32::System::LibraryLoader::{GetModuleFileNameW, GetProcAddress, LOAD_LIBRARY_SEARCH_DEFAULT_DIRS, LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR, LoadLibraryExW};
use windows::core::{HSTRING, PCSTR};

#[repr(C)]
struct HeifError {
    code: c_int,
    subcode: c_int,
    message: *const c_char,
}

impl HeifError {
    fn check(self) -> Result<(), String> {
        if self.code == 0 {
            return Ok(());
        }
        let msg = if self.message.is_null() { String::new() } else { unsafe { CStr::from_ptr(self.message) }.to_string_lossy().into_owned() };
        Err(format!("libheif {}/{}: {msg}", self.code, self.subcode))
    }
}

// From libheif's headers (heif_image.h / heif_library.h).
const COLORSPACE_RGB: c_int = 1;
const CHROMA_INTERLEAVED_RGBA: c_int = 11;
const CHANNEL_INTERLEAVED: c_int = 10;

type Ctx = c_void;
type Handle = c_void;
type Image = c_void;

struct Lib {
    context_alloc: unsafe extern "C" fn() -> *mut Ctx,
    context_free: unsafe extern "C" fn(*mut Ctx),
    read_from_memory_without_copy: unsafe extern "C" fn(*mut Ctx, *const c_void, usize, *const c_void) -> HeifError,
    get_primary_image_handle: unsafe extern "C" fn(*mut Ctx, *mut *mut Handle) -> HeifError,
    handle_release: unsafe extern "C" fn(*const Handle),
    decode_image: unsafe extern "C" fn(*const Handle, *mut *mut Image, c_int, c_int, *const c_void) -> HeifError,
    image_get_width: unsafe extern "C" fn(*const Image, c_int) -> c_int,
    image_get_height: unsafe extern "C" fn(*const Image, c_int) -> c_int,
    image_get_plane_readonly: unsafe extern "C" fn(*const Image, c_int, *mut c_int) -> *const u8,
    image_release: unsafe extern "C" fn(*const Image),
}

static LIB: OnceLock<Result<Lib, String>> = OnceLock::new();

/// The codecs folder beside the running exe (`LOOKER_CODECS` overrides it, for tests).
fn codecs_dir() -> Option<std::path::PathBuf> {
    if let Some(d) = std::env::var_os("LOOKER_CODECS") {
        return Some(d.into());
    }
    let mut buf = vec![0u16; 1024];
    let n = unsafe { GetModuleFileNameW(None, &mut buf) } as usize;
    let exe = std::path::PathBuf::from(String::from_utf16_lossy(&buf[..n]));
    Some(exe.parent()?.join("codecs"))
}

fn load() -> Result<Lib, String> {
    let dll = codecs_dir().ok_or("no exe path")?.join("heif.dll");
    unsafe {
        // DLL_LOAD_DIR: libheif's own imports (libde265, dav1d) resolve from the same folder.
        let m = LoadLibraryExW(&HSTRING::from(dll.as_os_str()), None, LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR | LOAD_LIBRARY_SEARCH_DEFAULT_DIRS)
            .map_err(|e| format!("{}: {}", dll.display(), e.message()))?;
        macro_rules! sym {
            ($name:literal) => {
                std::mem::transmute(GetProcAddress(m, PCSTR(concat!($name, "\0").as_ptr())).ok_or(concat!("libheif lacks ", $name))?)
            };
        }
        let init: unsafe extern "C" fn(*const c_void) -> HeifError = sym!("heif_init");
        init(std::ptr::null()).check()?;
        Ok(Lib {
            context_alloc: sym!("heif_context_alloc"),
            context_free: sym!("heif_context_free"),
            read_from_memory_without_copy: sym!("heif_context_read_from_memory_without_copy"),
            get_primary_image_handle: sym!("heif_context_get_primary_image_handle"),
            handle_release: sym!("heif_image_handle_release"),
            decode_image: sym!("heif_decode_image"),
            image_get_width: sym!("heif_image_get_width"),
            image_get_height: sym!("heif_image_get_height"),
            image_get_plane_readonly: sym!("heif_image_get_plane_readonly"),
            image_release: sym!("heif_image_release"),
        })
    }
}

fn lib() -> Result<&'static Lib, String> {
    LIB.get_or_init(|| {
        let r = load();
        if let Err(e) = &r {
            crate::trace::mark(format!("libheif unavailable: {e}"));
        }
        r
    })
    .as_ref()
    .map_err(|e| e.clone())
}

/// Whether the bundled decoders are there (decides between "decoded by libheif" and the get-the-extension hint).
pub fn available() -> bool {
    lib().is_ok()
}

/// The primary image, upright, as straight RGBA.
pub fn decode(bytes: &[u8]) -> Result<(u32, u32, Vec<u8>), String> {
    let l = lib()?;
    unsafe {
        let ctx = (l.context_alloc)();
        if ctx.is_null() {
            return Err("libheif: out of memory".into());
        }
        // Freed on every path out, in reverse order of creation.
        struct Guard<'a> {
            l: &'a Lib,
            ctx: *mut Ctx,
            handle: *mut Handle,
            image: *mut Image,
        }
        impl Drop for Guard<'_> {
            fn drop(&mut self) {
                unsafe {
                    if !self.image.is_null() {
                        (self.l.image_release)(self.image);
                    }
                    if !self.handle.is_null() {
                        (self.l.handle_release)(self.handle);
                    }
                    (self.l.context_free)(self.ctx);
                }
            }
        }
        let mut g = Guard { l, ctx, handle: std::ptr::null_mut(), image: std::ptr::null_mut() };
        (l.read_from_memory_without_copy)(ctx, bytes.as_ptr() as _, bytes.len(), std::ptr::null()).check()?;
        (l.get_primary_image_handle)(ctx, &mut g.handle).check()?;
        (l.decode_image)(g.handle, &mut g.image, COLORSPACE_RGB, CHROMA_INTERLEAVED_RGBA, std::ptr::null()).check()?;
        let w = (l.image_get_width)(g.image, CHANNEL_INTERLEAVED);
        let h = (l.image_get_height)(g.image, CHANNEL_INTERLEAVED);
        let mut stride: c_int = 0;
        let plane = (l.image_get_plane_readonly)(g.image, CHANNEL_INTERLEAVED, &mut stride);
        if w <= 0 || h <= 0 || plane.is_null() || stride < w * 4 {
            return Err("libheif: no RGBA plane".into());
        }
        let (w, h, stride) = (w as usize, h as usize, stride as usize);
        let mut rgba = Vec::with_capacity(w * h * 4);
        for y in 0..h {
            rgba.extend_from_slice(std::slice::from_raw_parts(plane.add(y * stride), w * 4));
        }
        Ok((w as u32, h as u32, rgba))
    }
}

#[cfg(test)]
mod tests {
    /// Decodes the HEIC/AVIF samples through libheif itself (this machine's WIC would take them otherwise).
    /// `LOOKER_CODECS=%LOCALAPPDATA%\Looker\codecs cargo test --release -- --ignored --nocapture heif`
    #[test]
    #[ignore]
    fn heif_decodes_the_samples() {
        let dir = std::path::PathBuf::from(std::env::var("USERPROFILE").unwrap()).join("Pictures").join("Looker Test Photos");
        let mut seen = 0;
        for e in std::fs::read_dir(&dir).unwrap().flatten() {
            let p = e.path();
            let ext = p.extension().map(|x| x.to_string_lossy().to_lowercase()).unwrap_or_default();
            if !matches!(ext.as_str(), "heic" | "heif" | "hif" | "avif") {
                continue;
            }
            let t = std::time::Instant::now();
            let (w, h, px) = super::decode(&std::fs::read(&p).unwrap()).unwrap_or_else(|e| panic!("{}: {e}", p.display()));
            assert_eq!(px.len(), w as usize * h as usize * 4);
            println!("{}: {w}x{h} in {:.0} ms", p.file_name().unwrap().to_string_lossy(), t.elapsed().as_secs_f64() * 1000.0);
            seen += 1;
        }
        assert!(seen > 0, "no HEIC/AVIF samples in {}", dir.display());
    }
}
