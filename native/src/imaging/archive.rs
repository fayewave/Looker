//! Archives through libarchive (`codecs\archive.dll`, BSD): zip, 7z, RAR, tar and its compressed forms, gzip/bzip2/
//! xz/zstd on their own, cab, ISO, cpio, lha... Looker lists an archive's entries, reads the ones it wants (in the
//! archive's order: a solid 7z or RAR can only be read through), and extracts them itself rather than through
//! libarchive's disk writer, so no entry can land outside the folder asked for (`..`, absolute paths, drive letters
//! and Windows' reserved names are refused or made safe).

use std::ffi::{c_char, c_int, c_void};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use windows::Win32::System::LibraryLoader::{GetProcAddress, LOAD_LIBRARY_SEARCH_DEFAULT_DIRS, LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR, LoadLibraryExW};
use windows::core::{HSTRING, PCSTR};

const ARCHIVE_OK: c_int = 0;
const ARCHIVE_EOF: c_int = 1;
const ARCHIVE_WARN: c_int = -20;
const AE_IFMT: u32 = 0o170000;
const AE_IFDIR: u32 = 0o040000;
const AE_IFREG: u32 = 0o100000;

struct Lib {
    read_new: unsafe extern "C" fn() -> *mut c_void,
    support_filter_all: unsafe extern "C" fn(*mut c_void) -> c_int,
    support_format_all: unsafe extern "C" fn(*mut c_void) -> c_int,
    support_format_raw: unsafe extern "C" fn(*mut c_void) -> c_int,
    open_filename_w: unsafe extern "C" fn(*mut c_void, *const u16, usize) -> c_int,
    next_header: unsafe extern "C" fn(*mut c_void, *mut *mut c_void) -> c_int,
    read_data: unsafe extern "C" fn(*mut c_void, *mut c_void, usize) -> isize,
    data_skip: unsafe extern "C" fn(*mut c_void) -> c_int,
    read_free: unsafe extern "C" fn(*mut c_void) -> c_int,
    error_string: unsafe extern "C" fn(*mut c_void) -> *const c_char,
    format_name: unsafe extern "C" fn(*mut c_void) -> *const c_char,
    entry_pathname_w: unsafe extern "C" fn(*mut c_void) -> *const u16,
    entry_pathname_utf8: unsafe extern "C" fn(*mut c_void) -> *const c_char,
    entry_size: unsafe extern "C" fn(*mut c_void) -> i64,
    entry_mtime: unsafe extern "C" fn(*mut c_void) -> i64,
    entry_filetype: unsafe extern "C" fn(*mut c_void) -> u32,
    entry_is_encrypted: unsafe extern "C" fn(*mut c_void) -> c_int,
    // Writing (adding files: the archive is written anew).
    format: unsafe extern "C" fn(*mut c_void) -> c_int,
    filter_count: unsafe extern "C" fn(*mut c_void) -> c_int,
    filter_code: unsafe extern "C" fn(*mut c_void, c_int) -> c_int,
    write_new: unsafe extern "C" fn() -> *mut c_void,
    write_set_format: unsafe extern "C" fn(*mut c_void, c_int) -> c_int,
    write_add_filter: unsafe extern "C" fn(*mut c_void, c_int) -> c_int,
    write_open_filename_w: unsafe extern "C" fn(*mut c_void, *const u16) -> c_int,
    write_header: unsafe extern "C" fn(*mut c_void, *mut c_void) -> c_int,
    write_data: unsafe extern "C" fn(*mut c_void, *const c_void, usize) -> isize,
    write_close: unsafe extern "C" fn(*mut c_void) -> c_int,
    write_free: unsafe extern "C" fn(*mut c_void) -> c_int,
    entry_new: unsafe extern "C" fn() -> *mut c_void,
    entry_free: unsafe extern "C" fn(*mut c_void),
    entry_copy_pathname_w: unsafe extern "C" fn(*mut c_void, *const u16),
    entry_set_size: unsafe extern "C" fn(*mut c_void, i64),
    entry_set_filetype: unsafe extern "C" fn(*mut c_void, u32),
    entry_set_perm: unsafe extern "C" fn(*mut c_void, u32),
    entry_set_mtime: unsafe extern "C" fn(*mut c_void, i64, std::ffi::c_long),
}

static LIB: OnceLock<Result<Lib, String>> = OnceLock::new();

fn lib() -> Result<&'static Lib, String> {
    LIB.get_or_init(|| unsafe {
        let dll = super::heif::codecs_dir().ok_or("no exe path")?.join("archive.dll");
        let m = LoadLibraryExW(&HSTRING::from(dll.as_os_str()), None, LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR | LOAD_LIBRARY_SEARCH_DEFAULT_DIRS)
            .map_err(|e| format!("{}: {}", dll.display(), e.message()))?;
        macro_rules! sym {
            ($name:literal) => {
                std::mem::transmute(GetProcAddress(m, PCSTR(concat!($name, "\0").as_ptr())).ok_or(concat!("archive.dll lacks ", $name))?)
            };
        }
        Ok(Lib {
            read_new: sym!("archive_read_new"),
            support_filter_all: sym!("archive_read_support_filter_all"),
            support_format_all: sym!("archive_read_support_format_all"),
            support_format_raw: sym!("archive_read_support_format_raw"),
            open_filename_w: sym!("archive_read_open_filename_w"),
            next_header: sym!("archive_read_next_header"),
            read_data: sym!("archive_read_data"),
            data_skip: sym!("archive_read_data_skip"),
            read_free: sym!("archive_read_free"),
            error_string: sym!("archive_error_string"),
            format_name: sym!("archive_format_name"),
            entry_pathname_w: sym!("archive_entry_pathname_w"),
            entry_pathname_utf8: sym!("archive_entry_pathname_utf8"),
            entry_size: sym!("archive_entry_size"),
            entry_mtime: sym!("archive_entry_mtime"),
            entry_filetype: sym!("archive_entry_filetype"),
            entry_is_encrypted: sym!("archive_entry_is_encrypted"),
            format: sym!("archive_format"),
            filter_count: sym!("archive_filter_count"),
            filter_code: sym!("archive_filter_code"),
            write_new: sym!("archive_write_new"),
            write_set_format: sym!("archive_write_set_format"),
            write_add_filter: sym!("archive_write_add_filter"),
            write_open_filename_w: sym!("archive_write_open_filename_w"),
            write_header: sym!("archive_write_header"),
            write_data: sym!("archive_write_data"),
            write_close: sym!("archive_write_close"),
            write_free: sym!("archive_write_free"),
            entry_new: sym!("archive_entry_new"),
            entry_free: sym!("archive_entry_free"),
            entry_copy_pathname_w: sym!("archive_entry_copy_pathname_w"),
            entry_set_size: sym!("archive_entry_set_size"),
            entry_set_filetype: sym!("archive_entry_set_filetype"),
            entry_set_perm: sym!("archive_entry_set_perm"),
            entry_set_mtime: sym!("archive_entry_set_mtime"),
        })
    })
    .as_ref()
    .map_err(|e| e.clone())
}

/// One entry of an archive.
#[derive(Clone, Debug, PartialEq)]
pub struct Entry {
    /// Its path inside the archive, `/`-separated, no leading `/` or `./`, no trailing `/` (a folder's too).
    pub path: String,
    pub size: i64,
    /// Unix seconds, 0 when the archive doesn't say.
    pub mtime: i64,
    pub dir: bool,
    pub encrypted: bool,
}

impl Entry {
    /// Its name (the path's last part).
    pub fn name(&self) -> &str {
        self.path.rsplit('/').next().unwrap_or(&self.path)
    }
}

/// An open archive, read front to back once.
struct Reader {
    l: &'static Lib,
    a: *mut c_void,
    /// A plain .gz/.bz2/.xz file: libarchive's raw format names its one entry "data"; this is what it's called.
    raw_name: Option<String>,
}

impl Drop for Reader {
    fn drop(&mut self) {
        unsafe { (self.l.read_free)(self.a) };
    }
}

fn cstr(p: *const c_char) -> String {
    if p.is_null() {
        return String::new();
    }
    unsafe { std::ffi::CStr::from_ptr(p) }.to_string_lossy().into_owned()
}

impl Reader {
    fn open(path: &Path) -> Result<Reader, String> {
        let l = lib()?;
        let a = unsafe { (l.read_new)() };
        if a.is_null() {
            return Err("archive_read_new failed".into());
        }
        let mut r = Reader { l, a, raw_name: None };
        unsafe {
            (l.support_filter_all)(a);
            (l.support_format_all)(a);
            // Last resort, for a single compressed file (photo.jpg.gz): its one entry.
            (l.support_format_raw)(a);
        }
        let w: Vec<u16> = path.as_os_str().encode_wide_nul();
        if unsafe { (l.open_filename_w)(a, w.as_ptr(), 1 << 16) } != ARCHIVE_OK {
            return Err(r.error());
        }
        // The raw format's entry is named after the file, its compression's extension dropped.
        let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("data").to_string();
        r.raw_name = Some(stem);
        Ok(r)
    }

    fn error(&self) -> String {
        let e = cstr(unsafe { (self.l.error_string)(self.a) });
        if e.is_empty() { "the archive couldn't be read".into() } else { e }
    }

    /// The next entry, or None at the end.
    fn next(&mut self) -> Result<Option<Entry>, String> {
        let mut e: *mut c_void = std::ptr::null_mut();
        let r = unsafe { (self.l.next_header)(self.a, &mut e) };
        if r == ARCHIVE_EOF {
            return Ok(None);
        }
        if (r != ARCHIVE_OK && r != ARCHIVE_WARN) || e.is_null() {
            return Err(self.error());
        }
        let raw = cstr(unsafe { (self.l.format_name)(self.a) }) == "raw";
        let w = unsafe { (self.l.entry_pathname_w)(e) };
        let mut path = if !w.is_null() {
            let n = (0..).take_while(|&i| unsafe { *w.add(i) } != 0).count();
            String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(w, n) })
        } else {
            cstr(unsafe { (self.l.entry_pathname_utf8)(e) })
        };
        if raw {
            path = self.raw_name.clone().unwrap_or(path);
        }
        let kind = unsafe { (self.l.entry_filetype)(e) } & AE_IFMT;
        let path = path.replace('\\', "/");
        let dir = kind == AE_IFDIR || path.ends_with('/');
        let path = path.trim_start_matches("./").trim_start_matches('/').trim_end_matches('/').to_string();
        Ok(Some(Entry {
            path,
            size: unsafe { (self.l.entry_size)(e) }.max(0),
            mtime: unsafe { (self.l.entry_mtime)(e) },
            dir: dir || (kind != AE_IFREG && kind != 0 && !raw),
            encrypted: unsafe { (self.l.entry_is_encrypted)(e) } > 0,
        }))
    }

    /// The current entry's data, in pieces handed to `f` (false stops early).
    fn data(&mut self, f: &mut dyn FnMut(&[u8]) -> Result<bool, String>) -> Result<(), String> {
        let mut buf = vec![0u8; 1 << 18];
        loop {
            let n = unsafe { (self.l.read_data)(self.a, buf.as_mut_ptr() as *mut c_void, buf.len()) };
            if n < 0 {
                return Err(self.error());
            }
            if n == 0 || !f(&buf[..n as usize])? {
                return Ok(());
            }
        }
    }

    fn skip(&mut self) {
        unsafe { (self.l.data_skip)(self.a) };
    }
}

trait WideNul {
    fn encode_wide_nul(&self) -> Vec<u16>;
}

impl WideNul for std::ffi::OsStr {
    fn encode_wide_nul(&self) -> Vec<u16> {
        use std::os::windows::ffi::OsStrExt;
        self.encode_wide().chain(Some(0)).collect()
    }
}

/// Every entry of the archive at `path`, with the folders its files imply added (zips often list none), in the
/// archive's order.
pub fn list(path: &Path) -> Result<Vec<Entry>, String> {
    let mut r = Reader::open(path)?;
    let mut out: Vec<Entry> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    while let Some(e) = r.next()? {
        r.skip();
        if e.path.is_empty() || !seen.insert(e.path.clone()) {
            continue;
        }
        // The folders above it.
        let mut at = 0;
        while let Some(i) = e.path[at..].find('/') {
            let d = &e.path[..at + i];
            if seen.insert(d.to_string()) {
                out.push(Entry { path: d.to_string(), size: 0, mtime: e.mtime, dir: true, encrypted: false });
            }
            at += i + 1;
        }
        out.push(e);
    }
    Ok(out)
}

/// Reads the files `want` picks, in the archive's order, handing each to `f` with its bytes (up to `max` bytes; a
/// larger one is passed over). `f` returns false to stop.
pub fn read_files(path: &Path, max: i64, want: &dyn Fn(&Entry) -> bool, f: &mut dyn FnMut(&Entry, Vec<u8>) -> bool) -> Result<(), String> {
    let mut r = Reader::open(path)?;
    while let Some(e) = r.next()? {
        if e.dir || e.encrypted || !want(&e) || e.size > max {
            r.skip();
            continue;
        }
        let mut bytes = Vec::with_capacity(e.size.max(0) as usize);
        let res = r.data(&mut |b| {
            bytes.extend_from_slice(b);
            Ok((bytes.len() as i64) <= max)
        });
        if res.is_err() || bytes.len() as i64 > max {
            continue; // a damaged or too-large entry: the rest may still read
        }
        if !f(&e, bytes) {
            break;
        }
    }
    Ok(())
}

/// A safe place under `dest` for the archive path `p` with `strip` taken off its front: each part kept only if it's
/// an ordinary name (no `..`, no drive or root, no device names like CON or NUL; characters Windows refuses become
/// `_`). None when nothing is left.
pub fn safe_target(dest: &Path, p: &str, strip: &str) -> Option<PathBuf> {
    let rel = p.strip_prefix(strip).unwrap_or(p).trim_start_matches('/');
    let mut out = dest.to_path_buf();
    let mut any = false;
    for part in rel.split('/') {
        let part = part.trim_end_matches(['.', ' ']);
        if part.is_empty() || part == "." || part == ".." {
            continue;
        }
        let mut clean: String = part.chars().map(|c| if c < ' ' || "<>:\"|?*\\".contains(c) { '_' } else { c }).collect();
        let stem = clean.split('.').next().unwrap_or("").to_ascii_uppercase();
        let reserved = matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL") || (stem.len() == 4 && (stem.starts_with("COM") || stem.starts_with("LPT")) && stem.as_bytes()[3].is_ascii_digit());
        if reserved {
            clean.insert(0, '_');
        }
        out.push(clean);
        any = true;
    }
    any.then_some(out)
}

/// What an extraction did.
#[derive(Default, Debug)]
pub struct Extracted {
    pub files: usize,
    pub skipped_encrypted: usize,
    pub failed: usize,
}

/// Extracts the entries `pick` chooses into `dest`, each at its path with `strip` (a folder prefix, or "") taken off
/// the front. An existing file of the same name isn't overwritten: the new one gets " (2)" and so on. `progress` is
/// told the bytes written so far and each file's name; returning false from it stops.
pub fn extract(path: &Path, dest: &Path, strip: &str, pick: &dyn Fn(&Entry) -> bool, progress: &mut dyn FnMut(u64, &str) -> bool) -> Result<Extracted, String> {
    let mut r = Reader::open(path)?;
    let mut done = Extracted::default();
    let mut written = 0u64;
    std::fs::create_dir_all(dest).map_err(|e| format!("{}: {e}", dest.display()))?;
    while let Some(e) = r.next()? {
        if !pick(&e) {
            r.skip();
            continue;
        }
        let Some(target) = safe_target(dest, &e.path, strip) else {
            r.skip();
            continue;
        };
        if e.dir {
            let _ = std::fs::create_dir_all(&target);
            r.skip();
            continue;
        }
        if e.encrypted {
            done.skipped_encrypted += 1;
            r.skip();
            continue;
        }
        if let Some(parent) = target.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let target = unique(&target);
        let Ok(mut file) = std::fs::File::create(&target) else {
            done.failed += 1;
            r.skip();
            continue;
        };
        let name = e.name().to_string();
        let mut stop = false;
        let res = r.data(&mut |b| {
            use std::io::Write;
            file.write_all(b).map_err(|e| e.to_string())?;
            written += b.len() as u64;
            if !progress(written, &name) {
                stop = true;
                return Ok(false);
            }
            Ok(true)
        });
        if res.is_err() {
            done.failed += 1;
            drop(file);
            let _ = std::fs::remove_file(&target);
        } else {
            if e.mtime > 0 {
                let t = std::time::UNIX_EPOCH + std::time::Duration::from_secs(e.mtime as u64);
                let _ = file.set_modified(t);
            }
            done.files += 1;
        }
        if stop {
            break;
        }
    }
    Ok(done)
}

/// Whether WIC decodes the picture `path` (by name) from memory: what an archive's thumbnail and its rows'
/// thumbnails can show.
pub fn is_picture(path: &str) -> bool {
    use crate::format::Format;
    let ext = Path::new(path).extension().and_then(|e| e.to_str());
    matches!(crate::format::from_extension(ext), Format::Jpeg | Format::Png | Format::Gif | Format::Bmp | Format::Tiff | Format::Webp | Format::JpegXr | Format::Ico)
}

/// The decode pool's still for an archive: its first picture (by the archive's order, up to 64 MB), so a zip of
/// photos or a comic book shows its first page. An archive with none is declined (its type's icon shows).
pub fn still(f: &windows::Win32::Graphics::Imaging::IWICImagingFactory, path: &Path, box_w: u32, box_h: u32) -> Result<super::Decoded, String> {
    let mut found = None;
    read_files(path, 64 << 20, &|e| is_picture(&e.path) && !e.name().starts_with('.'), &mut |_, bytes| {
        found = Some(bytes);
        false
    })?;
    let bytes = found.ok_or("no picture inside")?;
    let mut d = super::wic::decode(f, super::wic::Source::Memory(&bytes), box_w, box_h).map_err(|e| e.message().to_string())?;
    d.taken = None;
    Ok(d)
}

/// The format a rewrite writes, for an archive read as `code` (libarchive's ARCHIVE_FORMAT_*): zip, 7z, tar (any
/// kind, written as POSIX pax) and cpio. None for what libarchive reads but can't write (RAR, CAB, LHA, ISO...).
fn writable(code: c_int) -> Option<c_int> {
    match code & 0xFF0000 {
        0x50000 => Some(0x50000),        // zip
        0xE0000 => Some(0xE0000),        // 7z
        0x30000 => Some(0x30003),        // tar -> pax restricted
        0x10000 => Some(0x10001),        // cpio -> POSIX odc
        _ => None,
    }
}

/// Whether files can be added to the archive at `path` (its format is one libarchive writes, and nothing in it is
/// encrypted).
pub fn can_add(path: &Path) -> Result<(), String> {
    let r = Reader::open(path)?;
    let mut e: *mut c_void = std::ptr::null_mut();
    let first = unsafe { (r.l.next_header)(r.a, &mut e) };
    if first != ARCHIVE_OK && first != ARCHIVE_WARN && first != ARCHIVE_EOF {
        return Err(r.error());
    }
    let code = unsafe { (r.l.format)(r.a) };
    let raw = cstr(unsafe { (r.l.format_name)(r.a) }) == "raw";
    if raw || writable(code).is_none() {
        let name = cstr(unsafe { (r.l.format_name)(r.a) });
        return Err(format!("files can't be added to a {} archive", if name.is_empty() { "this kind of".into() } else { name }));
    }
    Ok(())
}

/// Adds `files` (folders with all inside) to the archive at `path`, in its folder `dir` ("" its top): the archive is
/// written anew beside it in the same format and compression, every entry copied across except those the new files
/// replace, then put in its place. Returns how many files went in.
pub fn add(path: &Path, files: &[PathBuf], dir: &str, progress: &mut dyn FnMut(u64, &str) -> bool) -> Result<usize, String> {
    // What goes in, and where: each file at dir/name, a folder's files under dir/folder/...
    let mut items: Vec<(String, PathBuf)> = Vec::new();
    fn walk(base: &str, p: &Path, out: &mut Vec<(String, PathBuf)>) {
        let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("file");
        let at = if base.is_empty() { name.to_string() } else { format!("{base}/{name}") };
        if p.is_dir() {
            out.push((at.clone(), p.to_path_buf()));
            if let Ok(rd) = std::fs::read_dir(p) {
                let mut kids: Vec<PathBuf> = rd.flatten().map(|e| e.path()).collect();
                kids.sort();
                for k in kids {
                    walk(&at, &k, out);
                }
            }
        } else {
            out.push((at, p.to_path_buf()));
        }
    }
    for f in files {
        walk(dir.trim_matches('/'), f, &mut items);
    }
    let replaced: std::collections::HashSet<&str> = items.iter().map(|(a, _)| a.as_str()).collect();

    let l = lib()?;
    let r = Reader::open(path)?;
    let tmp = path.with_extension(format!("{}.looker-new", path.extension().and_then(|e| e.to_str()).unwrap_or("")));
    let w = unsafe { (l.write_new)() };
    if w.is_null() {
        return Err("archive_write_new failed".into());
    }
    struct W(&'static Lib, *mut c_void);
    impl Drop for W {
        fn drop(&mut self) {
            unsafe { (self.0.write_free)(self.1) };
        }
    }
    let w = W(l, w);
    let werr = |w: &W| {
        let e = cstr(unsafe { (l.error_string)(w.1) });
        if e.is_empty() { "the archive couldn't be written".to_string() } else { e }
    };
    let mut opened = false;
    let mut written = 0u64;
    let mut buf = vec![0u8; 1 << 18];
    loop {
        let mut e: *mut c_void = std::ptr::null_mut();
        let rc = unsafe { (l.next_header)(r.a, &mut e) };
        if rc == ARCHIVE_EOF && opened {
            break;
        }
        if !opened {
            // The format and compression are known once the first header is read (an empty archive: none).
            let code = unsafe { (l.format)(r.a) };
            let raw = cstr(unsafe { (l.format_name)(r.a) }) == "raw";
            let Some(fmt) = writable(code).filter(|_| !raw) else {
                return Err(format!("files can't be added to a {} archive", cstr(unsafe { (l.format_name)(r.a) })));
            };
            if unsafe { (l.write_set_format)(w.1, fmt) } != ARCHIVE_OK {
                return Err(werr(&w));
            }
            let n = unsafe { (l.filter_count)(r.a) };
            for i in 0..n {
                let f = unsafe { (l.filter_code)(r.a, i) };
                if f != 0 {
                    unsafe { (l.write_add_filter)(w.1, f) };
                }
            }
            let t: Vec<u16> = tmp.as_os_str().encode_wide_nul();
            if unsafe { (l.write_open_filename_w)(w.1, t.as_ptr()) } != ARCHIVE_OK {
                return Err(werr(&w));
            }
            opened = true;
            if rc == ARCHIVE_EOF {
                break;
            }
        }
        if (rc != ARCHIVE_OK && rc != ARCHIVE_WARN) || e.is_null() {
            let _ = std::fs::remove_file(&tmp);
            return Err(r.error());
        }
        if unsafe { (l.entry_is_encrypted)(e) } > 0 {
            let _ = std::fs::remove_file(&tmp);
            return Err("it has encrypted files, which Looker can't copy across".into());
        }
        // Its path as the listing names it: replaced ones are left out.
        let w16 = unsafe { (l.entry_pathname_w)(e) };
        let p = if w16.is_null() {
            cstr(unsafe { (l.entry_pathname_utf8)(e) })
        } else {
            let n = (0..).take_while(|&i| unsafe { *w16.add(i) } != 0).count();
            String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(w16, n) })
        };
        let key = p.replace('\\', "/");
        let key = key.trim_start_matches("./").trim_matches('/');
        if replaced.contains(key) {
            unsafe { (l.data_skip)(r.a) };
            continue;
        }
        if unsafe { (l.write_header)(w.1, e) } < ARCHIVE_WARN {
            let _ = std::fs::remove_file(&tmp);
            return Err(werr(&w));
        }
        loop {
            let n = unsafe { (l.read_data)(r.a, buf.as_mut_ptr() as *mut c_void, buf.len()) };
            if n < 0 {
                let _ = std::fs::remove_file(&tmp);
                return Err(r.error());
            }
            if n == 0 {
                break;
            }
            if unsafe { (l.write_data)(w.1, buf.as_ptr() as *const c_void, n as usize) } < 0 {
                let _ = std::fs::remove_file(&tmp);
                return Err(werr(&w));
            }
        }
    }
    drop(r);
    // The new files.
    let mut added = 0;
    for (at, src) in &items {
        let e = unsafe { (l.entry_new)() };
        let pw: Vec<u16> = std::ffi::OsStr::new(at).encode_wide_nul();
        let meta = std::fs::metadata(src).map_err(|err| format!("{}: {err}", src.display()))?;
        let mtime = meta.modified().ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map_or(0, |d| d.as_secs() as i64);
        unsafe {
            (l.entry_copy_pathname_w)(e, pw.as_ptr());
            (l.entry_set_mtime)(e, mtime, 0);
            if meta.is_dir() {
                (l.entry_set_filetype)(e, AE_IFDIR);
                (l.entry_set_perm)(e, 0o755);
                (l.entry_set_size)(e, 0);
            } else {
                (l.entry_set_filetype)(e, AE_IFREG);
                (l.entry_set_perm)(e, 0o644);
                (l.entry_set_size)(e, meta.len() as i64);
            }
        }
        let ok = unsafe { (l.write_header)(w.1, e) } >= ARCHIVE_WARN;
        unsafe { (l.entry_free)(e) };
        if !ok {
            let _ = std::fs::remove_file(&tmp);
            return Err(werr(&w));
        }
        if meta.is_dir() {
            continue;
        }
        use std::io::Read;
        let mut f = std::fs::File::open(src).map_err(|err| format!("{}: {err}", src.display()))?;
        let name = src.file_name().and_then(|n| n.to_str()).unwrap_or("").to_string();
        loop {
            let n = f.read(&mut buf).map_err(|err| err.to_string())?;
            if n == 0 {
                break;
            }
            if unsafe { (l.write_data)(w.1, buf.as_ptr() as *const c_void, n) } < 0 {
                let _ = std::fs::remove_file(&tmp);
                return Err(werr(&w));
            }
            written += n as u64;
            if !progress(written, &name) {
                let _ = std::fs::remove_file(&tmp);
                return Err("cancelled".into());
            }
        }
        added += 1;
    }
    if unsafe { (l.write_close)(w.1) } != ARCHIVE_OK {
        let _ = std::fs::remove_file(&tmp);
        return Err(werr(&w));
    }
    drop(w);
    // In its place (MoveFileEx replaces): the old one is gone only once the new one is whole.
    std::fs::rename(&tmp, path).map_err(|err| {
        let _ = std::fs::remove_file(&tmp);
        format!("{}: {err}", path.display())
    })?;
    Ok(added)
}

/// `p`, or "name (2).ext", "name (3).ext"... when it's taken.
fn unique(p: &Path) -> PathBuf {
    if !p.exists() {
        return p.to_path_buf();
    }
    let stem = p.file_stem().and_then(|s| s.to_str()).unwrap_or("file").to_string();
    let ext = p.extension().and_then(|e| e.to_str()).map(|e| format!(".{e}")).unwrap_or_default();
    (2..).map(|n| p.with_file_name(format!("{stem} ({n}){ext}"))).find(|q| !q.exists()).unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn targets_stay_inside() {
        let d = Path::new(r"C:\out");
        assert_eq!(safe_target(d, "a/b.txt", ""), Some(PathBuf::from(r"C:\out\a\b.txt")));
        assert_eq!(safe_target(d, "../../evil.exe", ""), Some(PathBuf::from(r"C:\out\evil.exe")));
        assert_eq!(safe_target(d, "/abs/x", ""), Some(PathBuf::from(r"C:\out\abs\x")));
        assert_eq!(safe_target(d, "C:/Windows/x.dll", ""), Some(PathBuf::from(r"C:\out\C_\Windows\x.dll")));
        assert_eq!(safe_target(d, "con.txt", ""), Some(PathBuf::from(r"C:\out\_con.txt")));
        assert_eq!(safe_target(d, "a/b/c.jpg", "a/b"), Some(PathBuf::from(r"C:\out\c.jpg")));
        assert_eq!(safe_target(d, "..", ""), None);
    }

    /// Adds a file to a zip, a 7z and a tar.gz written by Windows' tar, and reads it back out.
    #[test]
    #[ignore]
    fn adds_files() {
        let dir = std::env::temp_dir().join(format!("looker-archive-add-{}", std::process::id()));
        let src = dir.join("src");
        std::fs::create_dir_all(src.join("inner")).unwrap();
        std::fs::write(src.join("a.txt"), b"first").unwrap();
        std::fs::write(src.join("inner").join("b.txt"), b"second").unwrap();
        let extra = dir.join("added.txt");
        std::fs::write(&extra, b"added later").unwrap();
        for (name, flags) in [("t.zip", "-acf"), ("t.7z", "-acf"), ("t.tar.gz", "-czf")] {
            let ar = dir.join(name);
            let ok = std::process::Command::new("tar").current_dir(&src).args([flags, ar.to_str().unwrap(), "a.txt", "inner"]).status().unwrap();
            assert!(ok.success(), "tar made {name}");
            can_add(&ar).unwrap();
            assert_eq!(add(&ar, &[extra.clone()], "inner", &mut |_, _| true).unwrap(), 1, "{name}");
            let names: Vec<String> = list(&ar).unwrap().into_iter().map(|e| e.path).collect();
            assert!(names.contains(&"inner/added.txt".to_string()) && names.contains(&"a.txt".to_string()) && names.contains(&"inner/b.txt".to_string()), "{name}: {names:?}");
            let mut got = None;
            read_files(&ar, 1 << 20, &|e| e.path == "inner/added.txt", &mut |_, b| {
                got = Some(b);
                false
            })
            .unwrap();
            assert_eq!(got.as_deref(), Some(&b"added later"[..]), "{name}");
            // Again: it replaces, not doubles.
            add(&ar, &[extra.clone()], "inner", &mut |_, _| true).unwrap();
            assert_eq!(list(&ar).unwrap().iter().filter(|e| e.path == "inner/added.txt").count(), 1, "{name}");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// LOOKER_ARCHIVE=<file>: lists it, reads its first image, and extracts it all into %TEMP%.
    /// `LOOKER_CODECS=...\native-target\release\codecs LOOKER_ARCHIVE=x.zip cargo test --release -- --ignored --nocapture lists_and_extracts`
    #[test]
    #[ignore]
    fn lists_and_extracts() {
        let Some(p) = std::env::var_os("LOOKER_ARCHIVE") else { return };
        let p = PathBuf::from(p);
        let t = std::time::Instant::now();
        let list = list(&p).expect("lists");
        println!("{} entries in {:.1} ms", list.len(), t.elapsed().as_secs_f64() * 1000.0);
        for e in list.iter().take(12) {
            println!("  {}{} {} bytes {}", e.path, if e.dir { "/" } else { "" }, e.size, if e.encrypted { "(encrypted)" } else { "" });
        }
        let mut first = None;
        read_files(&p, 64 << 20, &|e| crate::format::from_extension(Path::new(&e.path).extension().and_then(|x| x.to_str())) != crate::format::Format::Unknown, &mut |e, b| {
            first = Some((e.path.clone(), b.len()));
            false
        })
        .unwrap();
        println!("first readable file: {first:?}");
        let dest = std::env::temp_dir().join(format!("looker-archive-test-{}", std::process::id()));
        let done = extract(&p, &dest, "", &|_| true, &mut |_, _| true).unwrap();
        println!("extracted {done:?} into {}", dest.display());
        let _ = std::fs::remove_dir_all(&dest);
    }
}
