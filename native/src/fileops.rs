//! File actions, ported from `Services/FileOperationsService.cs`: rotate-and-save, recycle, set as wallpaper,
//! and the pixels "Copy image" puts on the clipboard. Each runs on its own short-lived thread (they touch the
//! disk, and recycling or setting the wallpaper can take a while) and posts a [`Done`] back to the window.
//! Every action is failure-tolerant: a denied or impossible operation reports `false`/`None`, never panics.

use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};

use windows::Foundation::{PropertyType, PropertyValue};
use windows::Graphics::Imaging::{
    BitmapAlphaMode, BitmapDecoder, BitmapEncoder, BitmapPixelFormat, BitmapPropertySet, BitmapRotation, BitmapTypedValue,
};
use windows::Storage::FileAccessMode;
use windows::Storage::Streams::FileRandomAccessStream;
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::Graphics::Imaging::{CLSID_WICImagingFactory2, IWICImagingFactory};
use windows::Win32::Storage::FileSystem::{REPLACE_FILE_FLAGS, ReplaceFileW};
use windows::Win32::System::Com::{
    CLSCTX_ALL, CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE, COINIT_MULTITHREADED, CoCreateInstance,
    CoInitializeEx,
};
use windows::Win32::UI::Shell::*;
use windows::Win32::UI::WindowsAndMessaging::{
    PostMessageW, SPI_SETDESKWALLPAPER, SPIF_SENDCHANGE, SPIF_UPDATEINIFILE, SystemParametersInfoW, WM_APP,
};
use windows::core::{HSTRING, PCWSTR};

use crate::format::{self, Format};
use crate::imaging;

/// Posted with a boxed [`Done`] in the LPARAM.
pub const WM_FILE_OP: u32 = WM_APP + 4;

/// Full-size premultiplied BGRA, for the clipboard.
pub struct Image {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>,
}

pub enum Done {
    Rotated { path: PathBuf, ok: bool },
    Recycled { path: PathBuf, ok: bool },
    Wallpaper { ok: bool },
    Renamed { from: PathBuf, to: Result<PathBuf, String> },
    /// The decode for "Copy image" (None when the file couldn't be decoded: the file still goes on the
    /// clipboard, just without a bitmap).
    Copied { path: PathBuf, image: Option<Image> },
    /// The explorer's rename (inline), so it can be undone.
    RenamedItem { from: PathBuf, to: Result<PathBuf, String>, origin: Origin },
    /// The explorer's moves and copies (drag and drop, paste), and their undos.
    Transferred(Transfer),
    /// The explorer's delete of a selection: each path and whether it went. `undo_copy`: these were the copies
    /// a copy made, recycled to undo it (their pairs, to redo).
    RecycledMany { paths: Vec<(PathBuf, bool)>, undo_copy: Option<(Vec<(PathBuf, PathBuf)>, Origin)> },
    /// "New folder", and its undo (`removed`).
    NewFolder { path: Result<PathBuf, String>, origin: Origin },
    FolderRemoved { path: PathBuf, ok: bool, origin: Origin },
    /// Rip DVD: the disc read (or why it can't be ripped), and a finished rip.
    DvdOpened(Result<Box<crate::dvd::Disc>, String>),
    Ripped { out: PathBuf, result: Result<(), String> },
    /// Archives extracted from the explorer's menu (app/extract.rs).
    Extracted(crate::app::ExtractResult),
    /// The info card's Ctrl+S: each file, the edits written into it, and how it went.
    MetaWritten { results: Vec<(PathBuf, Vec<(crate::metawrite::Field, Option<String>)>, Result<(), String>)> },
    /// An edit saved (app/edit.rs): the photo, the file written (or why not), and whether it was a copy.
    Edited { path: PathBuf, out: Result<PathBuf, String>, copy: bool },
    /// Paste edits onto photos picked in the explorer (app/batchedit.rs).
    EditsPasted(crate::app::PasteResult),
    /// Print: the files for Windows' Print Pictures (the photos, or PNGs made of those it can't read).
    PrintReady(Result<Vec<PathBuf>, String>),
}

/// Runs `job` on a new thread with COM initialised and posts its result to `hwnd`. `sta` for shell work
/// that may show UI (recycling a file that can't go to the Recycle Bin asks first).
pub fn spawn(hwnd: HWND, sta: bool, job: impl FnOnce() -> Done + Send + 'static) {
    let hwnd = hwnd.0 as isize;
    std::thread::Builder::new()
        .name("file-op".into())
        .spawn(move || {
            unsafe {
                let _ = CoInitializeEx(None, if sta { COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE } else { COINIT_MULTITHREADED });
            }
            let ptr = Box::into_raw(Box::new(job()));
            unsafe {
                if PostMessageW(Some(HWND(hwnd as _)), WM_FILE_OP, WPARAM(0), LPARAM(ptr as isize)).is_err() {
                    drop(Box::from_raw(ptr));
                }
            }
        })
        .ok();
}

fn wic() -> Option<IWICImagingFactory> {
    unsafe { CoCreateInstance(&CLSID_WICImagingFactory2, None, CLSCTX_INPROC_SERVER).ok() }
}

fn err(e: windows::core::Error) -> String {
    e.message().to_string()
}

// --- Recycle ----------------------------------------------------------------------------------------

/// Moves the file to the Recycle Bin. A file that can't be recycled (a network share) is not deleted
/// silently: the shell asks first (`FOF_WANTNUKEWARNING`), with the window as the owner.
pub fn recycle(path: &Path, owner: isize) -> bool {
    let r = (|| -> windows::core::Result<bool> {
        unsafe {
            let op: IFileOperation = CoCreateInstance(&FileOperation, None, CLSCTX_ALL)?;
            op.SetOperationFlags(FILEOPERATION_FLAGS(
                FOF_ALLOWUNDO.0 | FOF_NOCONFIRMATION.0 | FOF_WANTNUKEWARNING.0 | FOF_NOERRORUI.0 | FOF_SILENT.0 | FOFX_RECYCLEONDELETE.0,
            ))?;
            let _ = op.SetOwnerWindow(HWND(owner as _));
            let item: IShellItem = SHCreateItemFromParsingName(&HSTRING::from(path.as_os_str()), None)?;
            op.DeleteItem(&item, None)?;
            op.PerformOperations()?;
            Ok(!op.GetAnyOperationsAborted()?.as_bool())
        }
    })();
    match r {
        Ok(done) => done && !path.exists(),
        Err(e) => {
            crate::trace::mark(format!("recycle failed: {}", err(e)));
            false
        }
    }
}

// --- Rename -----------------------------------------------------------------------------------------

/// The path a rename to `new_name` would produce: trimmed, with the original extension kept when the new
/// name has none (the dialog pre-selects only the base name). Err with a message the toast can show.
pub fn rename_target(path: &Path, new_name: &str) -> Result<PathBuf, String> {
    let name = new_name.trim().trim_end_matches('.');
    if name.is_empty() {
        return Err("Type a name".into());
    }
    if name.chars().any(|c| c.is_control() || r#"<>:"/\|?*"#.contains(c)) {
        return Err("A file name can't contain any of these characters: \\ / : * ? \" < > |".into());
    }
    let mut name = name.to_string();
    if Path::new(&name).extension().is_none_or(|e| e.is_empty()) {
        if let Some(ext) = path.extension().filter(|e| !e.is_empty()) {
            name.push('.');
            name.push_str(&ext.to_string_lossy());
        }
    }
    Ok(path.with_file_name(name))
}

/// Renames in place. A case-only change is a real rename; an existing other file is never replaced.
pub fn rename(path: &Path, new_name: &str) -> Result<PathBuf, String> {
    let to = rename_target(path, new_name)?;
    if to == path {
        return Ok(to);
    }
    let same_file = to.to_string_lossy().to_lowercase() == path.to_string_lossy().to_lowercase();
    if !same_file && std::fs::symlink_metadata(&to).is_ok() {
        return Err(format!("There's already a file called \u{201C}{}\u{201D}", to.file_name().unwrap_or_default().to_string_lossy()));
    }
    std::fs::rename(path, &to).map_err(|e| format!("Couldn't rename: {e}"))?;
    Ok(to)
}

/// Renames a file or folder. A folder's name is taken as typed (no extension carried over).
pub fn rename_item(path: &Path, new_name: &str) -> Result<PathBuf, String> {
    if !path.is_dir() {
        return rename(path, new_name);
    }
    let name = new_name.trim().trim_end_matches('.');
    let probe = path.with_file_name("x");
    let to = rename_target(&probe, name).map(|_| path.with_file_name(name))?;
    if to == path {
        return Ok(to);
    }
    let same = to.to_string_lossy().to_lowercase() == path.to_string_lossy().to_lowercase();
    if !same && std::fs::symlink_metadata(&to).is_ok() {
        return Err(format!("There's already something called \u{201C}{name}\u{201D}"));
    }
    std::fs::rename(path, &to).map_err(|e| format!("Couldn't rename: {e}"))?;
    Ok(to)
}

// --- Moves, copies, new folders (the explorer's) ------------------------------------------------------

/// Who asked for a file operation: the user, or undo / redo replaying one (the result goes on the other
/// stack).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Origin {
    User,
    Undo,
    Redo,
}

/// What a move or copy did: every item that went, from where to where, and what didn't.
pub struct Transfer {
    pub copy: bool,
    pub pairs: Vec<(PathBuf, PathBuf)>,
    pub failed: Vec<(PathBuf, String)>,
    pub origin: Origin,
}

fn copy_tree(from: &Path, to: &Path) -> std::io::Result<()> {
    if std::fs::symlink_metadata(from)?.is_dir() {
        std::fs::create_dir(to)?;
        for e in std::fs::read_dir(from)? {
            let e = e?;
            copy_tree(&e.path(), &to.join(e.file_name()))?;
        }
        Ok(())
    } else {
        std::fs::copy(from, to).map(|_| ())
    }
}

/// Moves (or copies) each of `items` into `dest`, a taken name becoming "name (2)". A move to another drive
/// copies and then deletes the original. A folder never goes into itself, and an item already in `dest`
/// isn't moved onto itself.
pub fn transfer(items: &[PathBuf], dest: &Path, copy: bool, origin: Origin) -> Transfer {
    let mut out = Transfer { copy, pairs: Vec::new(), failed: Vec::new(), origin };
    for from in items {
        if crate::explorer::within(dest, from) {
            out.failed.push((from.clone(), "A folder can't go into itself".into()));
            continue;
        }
        if !copy && from.parent().is_some_and(|p| crate::explorer::same_path(p, dest)) {
            continue;
        }
        let name = crate::explorer::folder_name(from);
        let to = crate::explorer::free_name(dest, &name);
        let r = if copy {
            copy_tree(from, &to)
        } else {
            std::fs::rename(from, &to).or_else(|e| {
                // ERROR_NOT_SAME_DEVICE: across drives a move is a copy, then the original goes.
                if e.raw_os_error() == Some(17) {
                    copy_tree(from, &to)?;
                    if from.is_dir() { std::fs::remove_dir_all(from) } else { std::fs::remove_file(from) }
                } else {
                    Err(e)
                }
            })
        };
        match r {
            Ok(()) => out.pairs.push((from.clone(), to)),
            Err(e) => out.failed.push((from.clone(), e.to_string())),
        }
    }
    out
}

/// Puts moved items back where they came from (undoing a move): each `to` goes back to its `from`, or a free
/// name beside it should that be taken now.
pub fn transfer_back(pairs: &[(PathBuf, PathBuf)], origin: Origin) -> Transfer {
    let mut out = Transfer { copy: false, pairs: Vec::new(), failed: Vec::new(), origin };
    for (from, to) in pairs {
        let back = match from.parent() {
            Some(dir) if std::fs::symlink_metadata(from).is_ok() => crate::explorer::free_name(dir, &crate::explorer::folder_name(from)),
            _ => from.clone(),
        };
        let r = std::fs::rename(to, &back).or_else(|e| {
            if e.raw_os_error() == Some(17) {
                copy_tree(to, &back)?;
                if to.is_dir() { std::fs::remove_dir_all(to) } else { std::fs::remove_file(to) }
            } else {
                Err(e)
            }
        });
        match r {
            Ok(()) => out.pairs.push((to.clone(), back)),
            Err(e) => out.failed.push((to.clone(), e.to_string())),
        }
    }
    out
}

/// Makes "New folder" (or "New folder (2)"…) in `dir`.
pub fn new_folder(dir: &Path) -> Result<PathBuf, String> {
    let p = crate::explorer::free_name(dir, "New folder");
    std::fs::create_dir(&p).map(|_| p).map_err(|e| format!("Couldn't make a folder: {e}"))
}

/// Removes a folder only if it's still empty (undoing "New folder").
pub fn remove_empty_folder(p: &Path) -> bool {
    std::fs::read_dir(p).is_ok_and(|mut d| d.next().is_none()) && std::fs::remove_dir(p).is_ok()
}

// --- Rotate -----------------------------------------------------------------------------------------

fn encoder_for(path: &Path) -> Option<windows::core::GUID> {
    let ext = path.extension()?.to_string_lossy().to_ascii_lowercase();
    match ext.as_str() {
        "jpg" | "jpeg" | "jpe" | "jfif" => BitmapEncoder::JpegEncoderId().ok(),
        "png" => BitmapEncoder::PngEncoderId().ok(),
        "tif" | "tiff" => BitmapEncoder::TiffEncoderId().ok(),
        "bmp" | "dib" => BitmapEncoder::BmpEncoderId().ok(),
        _ => None,
    }
}

/// Rotates the file by `degrees` clockwise (a multiple of 90) for the encodable formats (JPEG, PNG, TIFF,
/// BMP). An upright file (no EXIF orientation) is transcoded, which keeps all its metadata; an EXIF-oriented
/// one has its orientation plus the turn baked into fresh pixels (the tag would otherwise compose with the
/// turn). Writes a sibling temp file and swaps it in, then bumps the write time so the cache key changes.
pub fn rotate(path: &Path, degrees: i32) -> Result<(), String> {
    let delta = degrees.rem_euclid(360);
    if delta == 0 {
        return Err("no rotation".into());
    }
    let encoder = encoder_for(path).ok_or("format can't be re-encoded")?;
    let name = path.file_name().ok_or("no file name")?.to_string_lossy().into_owned();
    let temp = path.with_file_name(format!(".{name}.looker.tmp"));
    let f = wic().ok_or("WIC unavailable")?;
    let orientation = imaging::wic::orientation(&f, path);
    let result = (|| {
        if orientation == 1 {
            transcode_rotate(path, &temp, delta)?;
        } else {
            bake_rotate(&f, path, &temp, encoder, delta)?;
        }
        replace_file(&temp, path)
    })();
    let _ = std::fs::remove_file(&temp);
    result?;
    // A new cache key even if the swap preserved the write time.
    if let Ok(file) = std::fs::File::options().write(true).open(path) {
        let _ = file.set_modified(std::time::SystemTime::now());
    }
    Ok(())
}

fn rotation(delta: i32) -> BitmapRotation {
    match delta {
        90 => BitmapRotation::Clockwise90Degrees,
        180 => BitmapRotation::Clockwise180Degrees,
        270 => BitmapRotation::Clockwise270Degrees,
        _ => BitmapRotation::None,
    }
}

pub(crate) fn open_stream(path: &Path, mode: FileAccessMode) -> Result<windows::Storage::Streams::IRandomAccessStream, String> {
    let absolute = std::path::absolute(path).map_err(|e| e.to_string())?;
    FileRandomAccessStream::OpenAsync(&HSTRING::from(absolute.as_os_str()), mode).and_then(|op| op.join()).map_err(err)
}

fn transcode_rotate(path: &Path, temp: &Path, delta: i32) -> Result<(), String> {
    std::fs::File::create(temp).map_err(|e| e.to_string())?;
    let src = open_stream(path, FileAccessMode::Read)?;
    let dst = open_stream(temp, FileAccessMode::ReadWrite)?;
    let decoder = BitmapDecoder::CreateAsync(&src).and_then(|op| op.join()).map_err(err)?;
    // Transcoding copies all metadata; the transform rotates the raster.
    let encoder = BitmapEncoder::CreateForTranscodingAsync(&dst, &decoder).and_then(|op| op.join()).map_err(err)?;
    encoder.BitmapTransform().and_then(|t| t.SetRotation(rotation(delta))).map_err(err)?;
    encoder.FlushAsync().and_then(|op| op.join()).map_err(err)?;
    Ok(())
}

fn bake_rotate(f: &IWICImagingFactory, path: &Path, temp: &Path, encoder_id: windows::core::GUID, delta: i32) -> Result<(), String> {
    // Display-oriented, sRGB, premultiplied pixels at full size; then turn the buffer ourselves.
    let d = imaging::wic::decode(f, imaging::wic::Source::File(path), 0, 0).map_err(err)?;
    let (w, h) = (d.width, d.height);
    let (pixels, nw, nh) = rotate_bgra(&d.frames[0].pixels, w, h, delta);
    std::fs::File::create(temp).map_err(|e| e.to_string())?;
    let dst = open_stream(temp, FileAccessMode::ReadWrite)?;
    let encoder = if encoder_id == BitmapEncoder::JpegEncoderId().map_err(err)? {
        let options = BitmapPropertySet::new().map_err(err)?;
        let quality = BitmapTypedValue::Create(&PropertyValue::CreateSingle(0.95).map_err(err)?, PropertyType::Single).map_err(err)?;
        options.Insert(&HSTRING::from("ImageQuality"), &quality).map_err(err)?;
        BitmapEncoder::CreateWithEncodingOptionsAsync(encoder_id, &dst, &options).and_then(|op| op.join()).map_err(err)?
    } else {
        BitmapEncoder::CreateAsync(encoder_id, &dst).and_then(|op| op.join()).map_err(err)?
    };
    encoder
        .SetPixelData(BitmapPixelFormat::Bgra8, BitmapAlphaMode::Premultiplied, nw, nh, 96.0, 96.0, &pixels)
        .map_err(err)?;
    encoder.FlushAsync().and_then(|op| op.join()).map_err(err)?;
    Ok(())
}

/// Rotates a tightly packed 4-byte-per-pixel buffer by 90/180/270° clockwise. Returns the new size.
pub fn rotate_bgra(src: &[u8], w: u32, h: u32, delta: i32) -> (Vec<u8>, u32, u32) {
    let (w, h) = (w as usize, h as usize);
    let mut dst = vec![0u8; src.len()];
    let px = |i: usize| &src[i * 4..i * 4 + 4];
    match delta {
        180 => {
            for y in 0..h {
                for x in 0..w {
                    let d = (y * w + x) * 4;
                    dst[d..d + 4].copy_from_slice(px((h - 1 - y) * w + (w - 1 - x)));
                }
            }
            (dst, w as u32, h as u32)
        }
        270 => {
            // Counter-clockwise: the new row `dy` is the old column `w - 1 - dy`, read top to bottom.
            for dy in 0..w {
                for dx in 0..h {
                    let d = (dy * h + dx) * 4;
                    dst[d..d + 4].copy_from_slice(px(dx * w + (w - 1 - dy)));
                }
            }
            (dst, h as u32, w as u32)
        }
        _ => {
            // Clockwise: the new row `dy` is the old column `dy`, read bottom to top.
            for dy in 0..w {
                for dx in 0..h {
                    let d = (dy * h + dx) * 4;
                    dst[d..d + 4].copy_from_slice(px((h - 1 - dx) * w + dy));
                }
            }
            (dst, h as u32, w as u32)
        }
    }
}

/// Swaps `temp` in for `dest`, keeping dest's attributes; an overwrite copy where the filesystem can't
/// (some network shares, FAT).
pub(crate) fn replace_file(temp: &Path, dest: &Path) -> Result<(), String> {
    let ok = unsafe {
        ReplaceFileW(&HSTRING::from(dest.as_os_str()), &HSTRING::from(temp.as_os_str()), PCWSTR::null(), REPLACE_FILE_FLAGS(0), None, None)
            .is_ok()
    };
    if ok {
        return Ok(());
    }
    std::fs::copy(temp, dest).map(|_| ()).map_err(|e| e.to_string())
}

// --- Pixels for the clipboard and the wallpaper -----------------------------------------------------

/// The whole image (first frame) at full size, capped at the largest bitmap edge; for a document, `page`.
/// Straight RGBA as `Image` holds pixels: premultiplied BGRA.
pub fn premultiplied_bgra(mut px: Vec<u8>) -> Vec<u8> {
    for p in px.chunks_exact_mut(4) {
        let a = p[3] as u32;
        let pm = |c: u8| ((c as u32 * a + 127) / 255) as u8;
        let (r, g, b) = (pm(p[0]), pm(p[1]), pm(p[2]));
        p[0] = b;
        p[1] = g;
        p[2] = r;
    }
    px
}

pub fn full_image(path: &Path, page: usize) -> Option<Image> {
    let f = wic()?;
    if format::sniff_file(path) == Format::Pdf {
        let (width, height, pixels) = imaging::pdf::render_page_of(&f, path, page)
            .map_err(|e| crate::trace::mark(format!("page render failed: {e}")))
            .ok()?;
        return Some(Image { width, height, pixels });
    }
    let d = imaging::decode(&f, path, imaging::MAX_EDGE, imaging::MAX_EDGE, true)
        .map_err(|e| crate::trace::mark(format!("full decode failed: {e}")))
        .ok()?;
    let pixels = d.frames.into_iter().next()?.pixels;
    Some(Image { width: d.width, height: d.height, pixels })
}

/// The full-size picture an edit is saved from: in the file's own colour space with its ICC profile when it has
/// an RGB one, else sRGB (no profile). A colour space named only by EXIF (Adobe RGB) is converted to sRGB.
pub fn full_image_own_space(path: &Path) -> Option<(Image, Option<Vec<u8>>)> {
    let f = wic()?;
    if format::sniff_file(path) == Format::Pdf {
        return full_image(path, 0).map(|i| (i, None));
    }
    let d = imaging::decode_own_space(&f, path).map_err(|e| crate::trace::mark(format!("full decode failed: {e}"))).ok()?;
    let (width, height) = (d.width, d.height);
    match d.colour {
        imaging::Colour::Srgb => Some((Image { width, height, pixels: d.frames.into_iter().next()?.pixels }, None)),
        imaging::Colour::Icc(p) => Some((Image { width, height, pixels: d.frames.into_iter().next()?.pixels }, Some(p.to_vec()))),
        _ => full_image(path, 0).map(|i| (i, None)),
    }
}

// --- Wallpaper --------------------------------------------------------------------------------------

/// Sets the desktop wallpaper. JPEG, PNG and BMP are used as they are; anything else is rendered to a PNG in
/// Looker's data folder first, so every format Looker opens works.
pub fn set_wallpaper(path: &Path, page: usize) -> bool {
    let image_path = match format::sniff_file(path) {
        Format::Jpeg | Format::Png | Format::Bmp => path.to_path_buf(),
        _ => {
            let Some(dest) = crate::settings::data_dir().map(|d| d.join("wallpaper.png")) else { return false };
            let (Some(img), Some(f)) = (full_image(path, page), wic()) else { return false };
            if let Some(dir) = dest.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            if let Err(e) = imaging::wic::save_png(&f, &dest, img.width, img.height, &img.pixels) {
                crate::trace::mark(format!("wallpaper png failed: {}", err(e)));
                return false;
            }
            dest
        }
    };
    let Ok(absolute) = std::path::absolute(&image_path) else { return false };
    let mut w: Vec<u16> = absolute.as_os_str().encode_wide().collect();
    w.push(0);
    unsafe { SystemParametersInfoW(SPI_SETDESKWALLPAPER, 0, Some(w.as_mut_ptr() as _), SPIF_UPDATEINIFILE | SPIF_SENDCHANGE).is_ok() }
}

// --- Print ------------------------------------------------------------------------------------------

/// The files to give Windows' Print Pictures for these photos: JPEG, PNG, BMP, GIF and TIFF as they are (it
/// reads those), anything else rendered to a PNG in `%TEMP%\looker-print` first, as the wallpaper is, so every
/// format Looker opens prints. Those PNGs stay a day (the wizard reads them while it's open), then go.
pub fn print_files(paths: &[PathBuf]) -> Result<Vec<PathBuf>, String> {
    let dir = std::env::temp_dir().join("looker-print");
    if let Ok(old) = std::fs::read_dir(&dir) {
        for e in old.flatten() {
            if e.metadata().and_then(|m| m.modified()).is_ok_and(|t| t.elapsed().is_ok_and(|age| age.as_secs() > 86_400)) {
                let _ = std::fs::remove_file(e.path());
            }
        }
    }
    let mut out = Vec::new();
    for (i, path) in paths.iter().enumerate() {
        match format::sniff_file(path) {
            Format::Jpeg | Format::Png | Format::Bmp | Format::Gif | Format::Tiff => out.push(path.clone()),
            _ => {
                let name = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "photo".into());
                let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0);
                let dest = dir.join(format!("{name} {stamp}-{i}.png"));
                let (Some(img), Some(f)) = (full_image(path, 0), wic()) else { return Err(format!("Couldn't read \u{201c}{}\u{201d}", file_name(path))) };
                let _ = std::fs::create_dir_all(&dir);
                imaging::wic::save_png(&f, &dest, img.width, img.height, &img.pixels).map_err(|e| format!("Couldn't prepare \u{201c}{}\u{201d}: {}", file_name(path), err(e)))?;
                out.push(dest);
            }
        }
    }
    Ok(out)
}

fn file_name(path: &Path) -> String {
    path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    // 3 x 2 image, one byte per pixel tagged in channel 0: 1 2 3 / 4 5 6
    fn sample() -> Vec<u8> {
        (1..=6u8).flat_map(|v| [v, 0, 0, 0]).collect()
    }

    fn tags(px: &[u8]) -> Vec<u8> {
        px.chunks_exact(4).map(|c| c[0]).collect()
    }

    #[test]
    fn rotates_quarter_turns() {
        let (cw, w, h) = rotate_bgra(&sample(), 3, 2, 90);
        assert_eq!((w, h), (2, 3));
        assert_eq!(tags(&cw), [4, 1, 5, 2, 6, 3]);
        let (ccw, _, _) = rotate_bgra(&sample(), 3, 2, 270);
        assert_eq!(tags(&ccw), [3, 6, 2, 5, 1, 4]);
        let (half, w, h) = rotate_bgra(&sample(), 3, 2, 180);
        assert_eq!((w, h), (3, 2));
        assert_eq!(tags(&half), [6, 5, 4, 3, 2, 1]);
    }

    /// The wallpaper/clipboard render of `LOOKER_DECODE`, written as a PNG next to the system temp folder
    /// (doesn't touch the desktop or the clipboard). `cargo test --release -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn renders_a_png_for_the_wallpaper() {
        unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        }
        let src = PathBuf::from(std::env::var("LOOKER_DECODE").expect("set LOOKER_DECODE"));
        let img = full_image(&src, 0).expect("decodes");
        let out = std::env::temp_dir().join("looker-wallpaper-test.png");
        imaging::wic::save_png(&wic().unwrap(), &out, img.width, img.height, &img.pixels).unwrap();
        println!("{}x{} -> {}", img.width, img.height, out.display());
    }

    #[test]
    fn rename_keeps_the_extension_unless_given_one() {
        let p = Path::new(r"C:\pics\holiday.jpg");
        assert_eq!(rename_target(p, " beach ").unwrap(), Path::new(r"C:\pics\beach.jpg"));
        assert_eq!(rename_target(p, "beach.").unwrap(), Path::new(r"C:\pics\beach.jpg"));
        assert_eq!(rename_target(p, "beach.png").unwrap(), Path::new(r"C:\pics\beach.png"));
        assert!(rename_target(p, "  ").is_err());
        assert!(rename_target(p, "a/b").is_err());
    }

    #[test]
    fn only_encodable_formats_rotate() {
        assert!(encoder_for(Path::new("a.JPG")).is_some());
        assert!(encoder_for(Path::new("a.tiff")).is_some());
        assert!(encoder_for(Path::new("a.heic")).is_none());
        assert!(rotate(Path::new("a.heic"), 90).is_err());
    }
}
