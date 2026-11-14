//! Windows' own ways to hand files on, as Explorer's menu offers them: Open with (the apps registered for the
//! file's type, then Windows' app picker), Share (the share sheet) and Print (the Print Pictures wizard that
//! Explorer's Print opens for photos, `photowiz.dll`'s drop target).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use windows::ApplicationModel::DataTransfer::{DataRequestedEventArgs, DataTransferManager};
use windows::Foundation::TypedEventHandler;
use windows::Storage::{IStorageItem, StorageFile};
use windows::Win32::Foundation::{HWND, POINTL};
use windows::Win32::System::Com::{
    CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx, CoTaskMemFree,
    IDataObject,
};
use windows::Win32::System::Ole::{DROPEFFECT, DROPEFFECT_COPY, IDropTarget};
use windows::Win32::System::SystemServices::MK_LBUTTON;
use windows::Win32::UI::Shell::Common::ITEMIDLIST;
use windows::Win32::UI::Shell::{
    ASSOC_FILTER_RECOMMENDED, BHID_DataObject, IAssocHandler, IDataTransferManagerInterop, IShellItemArray, OAIF_ALLOW_REGISTRATION,
    OAIF_EXEC, OPENASINFO, SHAssocEnumHandlers, SHCreateShellItemArrayFromIDLists, SHLoadIndirectString, SHOpenWithDialog,
    SHParseDisplayName,
};
use windows::core::{GUID, HSTRING, Interface, PCWSTR, PWSTR};

// --- Open with --------------------------------------------------------------------------------------

/// One app registered for a file type: its name as Windows shows it, its exe (or, for some packaged apps, a
/// name) and its icon. Plain data, so it's found on a thread of its own and kept: Windows' enumeration costs
/// 50-70 ms, too much for a menu opening.
#[derive(Clone)]
pub struct App {
    pub name: String,
    exe: String,
    pub icon: Option<crate::shell_icons::Kind>,
}

/// The apps for each extension, and when they were looked up.
static APPS: Mutex<Option<HashMap<String, (Instant, Arc<Vec<App>>)>>> = Mutex::new(None);
/// Extensions being looked up on a thread now.
static WARMING: Mutex<Vec<String>> = Mutex::new(Vec::new());
/// How long a list is trusted before it's looked up again (in the background, for next time).
const FRESH: Duration = Duration::from_secs(60);

fn extension(path: &Path) -> Option<String> {
    path.extension().map(|e| format!(".{}", e.to_string_lossy().to_lowercase()))
}

unsafe fn take(p: PWSTR) -> String {
    unsafe {
        let s = p.to_string().unwrap_or_default();
        CoTaskMemFree(Some(p.0 as _));
        s
    }
}

fn cached(ext: &str) -> Option<(Instant, Arc<Vec<App>>)> {
    APPS.lock().unwrap().as_ref().and_then(|m| m.get(ext).cloned())
}

fn store(ext: String, apps: Arc<Vec<App>>) {
    APPS.lock().unwrap().get_or_insert_with(HashMap::new).insert(ext, (Instant::now(), apps));
}

fn look_up(ext: &str) -> Arc<Vec<App>> {
    Arc::new(enumerate(ext).into_iter().map(|(a, _)| a).collect())
}

/// The recommended apps for `path`'s type, Looker left out (it's already open), in Windows' order. Looked up
/// here the first time for a type (when `warm` hasn't), then kept; a stale list is used and looked up again in
/// the background.
pub fn apps_for(path: &Path) -> Arc<Vec<App>> {
    let Some(ext) = extension(path) else { return Arc::new(Vec::new()) };
    if let Some((at, apps)) = cached(&ext) {
        if at.elapsed() > FRESH {
            warm(path);
        }
        return apps;
    }
    let apps = look_up(&ext);
    store(ext, apps.clone());
    apps
}

/// Looks up `path`'s apps on a thread of its own unless they're known and fresh (the viewer asks for each file
/// it shows, so its menu opens at once).
pub fn warm(path: &Path) {
    let Some(ext) = extension(path) else { return };
    if cached(&ext).is_some_and(|(at, _)| at.elapsed() < FRESH) {
        return;
    }
    {
        let mut w = WARMING.lock().unwrap();
        if w.contains(&ext) {
            return;
        }
        w.push(ext.clone());
    }
    let spawned = std::thread::Builder::new().name("open-with".into()).spawn({
        let ext = ext.clone();
        move || {
            unsafe {
                let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE);
            }
            store(ext.clone(), look_up(&ext));
            WARMING.lock().unwrap().retain(|e| *e != ext);
        }
    });
    if spawned.is_err() {
        WARMING.lock().unwrap().retain(|e| *e != ext);
    }
}

fn enumerate(ext: &str) -> Vec<(App, IAssocHandler)> {
    let mut out = Vec::new();
    unsafe {
        let Ok(list) = SHAssocEnumHandlers(&HSTRING::from(ext), ASSOC_FILTER_RECOMMENDED) else { return out };
        loop {
            let mut one = [None];
            let mut got = 0u32;
            if list.Next(&mut one, Some(&mut got)).is_err() || got == 0 {
                break;
            }
            let Some(handler) = one[0].take() else { break };
            let exe = handler.GetName().map(|p| take(p)).unwrap_or_default();
            let name = handler.GetUIName().map(|p| take(p)).unwrap_or_default();
            if name.is_empty() || is_looker(&exe, &name) {
                continue;
            }
            let mut icon_path = PWSTR::null();
            let mut index = 0i32;
            let icon = if handler.GetIconLocation(&mut icon_path, &mut index).is_ok() && !icon_path.is_null() { icon_kind(&take(icon_path), index) } else { None };
            out.push((App { name, exe, icon }, handler));
        }
    }
    out
}

/// An icon location as the registry gives it: a file and index, or a packaged app's `@{package?ms-resource:...}`
/// logo, resolved to the PNG it names.
fn icon_kind(location: &str, index: i32) -> Option<crate::shell_icons::Kind> {
    use crate::shell_icons::Kind;
    if location.is_empty() {
        return None;
    }
    if location.starts_with("@{") {
        let mut buf = [0u16; 1024];
        unsafe { SHLoadIndirectString(&HSTRING::from(location), &mut buf, None) }.ok()?;
        let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
        let path = PathBuf::from(String::from_utf16_lossy(&buf[..len]));
        return path.is_file().then_some(Kind::Picture(path));
    }
    Some(Kind::Location(location.to_string(), index))
}

/// Looker itself, packaged (its family name in the handler's name) or a dev build's exe.
fn is_looker(exe: &str, name: &str) -> bool {
    let exe = exe.to_lowercase();
    exe.contains("looker-photoviewer") || exe.ends_with("\\looker.exe") || name == "Looker"
}

/// The shell's data object for these files, as Explorer hands a selection to a verb.
fn data_object(paths: &[PathBuf]) -> Option<IDataObject> {
    unsafe {
        let mut pidls: Vec<*mut ITEMIDLIST> = Vec::new();
        for p in paths {
            // The shell's parser wants a full path with backslashes (GetFullPathNameW turns / into \).
            let p = std::path::absolute(p).unwrap_or_else(|_| p.clone());
            let mut pidl = std::ptr::null_mut();
            if SHParseDisplayName(&HSTRING::from(p.as_os_str()), None, &mut pidl, 0, None).is_ok() && !pidl.is_null() {
                pidls.push(pidl);
            }
        }
        let array: Option<IShellItemArray> = if pidls.len() == paths.len() && !pidls.is_empty() {
            let consts: Vec<*const ITEMIDLIST> = pidls.iter().map(|&p| p as *const _).collect();
            SHCreateShellItemArrayFromIDLists(&consts).ok()
        } else {
            None
        };
        for p in pidls {
            CoTaskMemFree(Some(p as _));
        }
        array?.BindToHandler(None, &BHID_DataObject).ok()
    }
}

/// Opens `paths` in the app at `index` of `apps_for(paths[0])`. The handler is looked up again here (a handler
/// belongs to the thread that found it), matched by exe and name.
pub fn open_with(index: usize, paths: &[PathBuf]) -> Result<(), String> {
    let first = paths.first().ok_or("Nothing to open")?;
    let want = apps_for(first).get(index).cloned().ok_or("That app has gone")?;
    let ext = extension(first).unwrap_or_default();
    let (_, handler) =
        enumerate(&ext).into_iter().find(|(a, _)| a.exe == want.exe && a.name == want.name).ok_or_else(|| format!("{} isn't there any more", want.name))?;
    let data = data_object(paths).ok_or("Couldn't hand the file over")?;
    unsafe { handler.Invoke(&data) }.map_err(|e| format!("Couldn't open it in {}: {}", want.name, e.message()))
}

/// Windows' "How do you want to open this file?" picker, which opens the file in what's picked (and can make it
/// the default, as Explorer's Choose another app can).
pub fn choose_app(hwnd: HWND, path: &Path) {
    let file = HSTRING::from(path.as_os_str());
    let info = OPENASINFO { pcszFile: PCWSTR(file.as_ptr()), pcszClass: PCWSTR::null(), oaifInFlags: OAIF_ALLOW_REGISTRATION | OAIF_EXEC };
    unsafe {
        let _ = SHOpenWithDialog(Some(hwnd), &info);
    }
}

// --- Print ------------------------------------------------------------------------------------------

/// The Photo Printing Wizard's drop target (`HKCR\SystemFileAssociations\image\shell\print\DropTarget`).
const PRINT_WIZARD: GUID = GUID::from_u128(0x60fd46de_f830_4894_a628_6fa81bc0190d);

/// Windows' Print Pictures dialog (printer, paper, layouts from full page to contact sheet) on these files,
/// which must be ones it reads (`fileops::print_files` makes them so).
pub fn print(paths: &[PathBuf]) -> Result<(), String> {
    let data = data_object(paths).ok_or("Couldn't hand the photos to Windows")?;
    unsafe {
        let target: IDropTarget =
            CoCreateInstance(&PRINT_WIZARD, None, CLSCTX_INPROC_SERVER).map_err(|e| format!("Windows' Print Pictures isn't available: {}", e.message()))?;
        let mut effect: DROPEFFECT = DROPEFFECT_COPY;
        target.DragEnter(&data, MK_LBUTTON, POINTL::default(), &mut effect).map_err(|e| e.message().to_string())?;
        let mut effect: DROPEFFECT = DROPEFFECT_COPY;
        target.Drop(&data, MK_LBUTTON, POINTL::default(), &mut effect).map_err(|e| e.message().to_string())?;
    }
    Ok(())
}

// --- Share ------------------------------------------------------------------------------------------

/// What the share sheet is sharing: set before it's shown, read when it asks (`DataRequested`).
static SHARING: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());
/// The window whose `DataRequested` is hooked (once per window).
static HOOKED: Mutex<isize> = Mutex::new(0);

/// Windows' share sheet (Nearby Share, Mail, Phone Link, apps) for these files.
pub fn share(hwnd: HWND, paths: Vec<PathBuf>) -> Result<(), String> {
    if paths.is_empty() {
        return Ok(());
    }
    *SHARING.lock().unwrap() = paths;
    let err = |e: windows::core::Error| format!("Couldn't open Share: {}", e.message());
    let interop = windows::core::factory::<DataTransferManager, IDataTransferManagerInterop>().map_err(err)?;
    let mut hooked = HOOKED.lock().unwrap();
    if *hooked != hwnd.0 as isize {
        let manager: DataTransferManager = unsafe { interop.GetForWindow(hwnd) }.map_err(err)?;
        manager
            .DataRequested(&TypedEventHandler::<DataTransferManager, DataRequestedEventArgs>::new(|_, args| {
                if let Some(args) = args.as_ref() {
                    fill(args);
                }
                Ok(())
            }))
            .map_err(err)?;
        *hooked = hwnd.0 as isize;
    }
    drop(hooked);
    unsafe { interop.ShowShareUIForWindow(hwnd) }.map_err(err)
}

/// The share sheet asked for the data: its title now, the files once they're looked up (`StorageFile`s come
/// from an async call, made on a thread of its own under the request's deferral so the UI thread never waits).
fn fill(args: &DataRequestedEventArgs) {
    let paths = SHARING.lock().unwrap().clone();
    let Ok(request) = args.Request() else { return };
    let Ok(data) = request.Data() else { return };
    let title = match paths.as_slice() {
        [one] => one.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
        many => format!("{} files", many.len()),
    };
    if let Ok(props) = data.Properties() {
        let _ = props.SetTitle(&HSTRING::from(title));
    }
    let Ok(deferral) = request.GetDeferral() else { return };
    std::thread::spawn(move || {
        unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        }
        let files: Vec<Option<IStorageItem>> = paths
            .iter()
            .filter_map(|p| StorageFile::GetFileFromPathAsync(&HSTRING::from(p.as_os_str())).and_then(|op| op.join()).ok())
            .filter_map(|f| f.cast::<IStorageItem>().ok())
            .map(Some)
            .collect();
        if files.is_empty() {
            let _ = request.FailWithDisplayText(&HSTRING::from("Couldn't read the file"));
        } else {
            let _ = data.SetStorageItems(&windows_collections::IIterable::<IStorageItem>::from(files), true);
        }
        let _ = deferral.Complete();
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The print path up to the wizard's window, which never shows: files made printable (`LOOKER_DECODE`, any
    /// format Looker opens), handed over as Explorer does, and the wizard's drop target accepting them as a copy.
    /// `LOOKER_DECODE=x.webp cargo test --release -- --ignored --nocapture print_wizard_takes_files`.
    #[test]
    #[ignore]
    fn print_wizard_takes_files() {
        unsafe {
            let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        }
        let src = PathBuf::from(std::env::var("LOOKER_DECODE").expect("LOOKER_DECODE"));
        let files = crate::fileops::print_files(&[src]).expect("print files");
        println!("files: {files:?}");
        assert!(files.iter().all(|f| f.is_file()));
        let data = data_object(&files).expect("data object");
        unsafe {
            let target: IDropTarget = CoCreateInstance(&PRINT_WIZARD, None, CLSCTX_INPROC_SERVER).expect("wizard");
            let mut effect: DROPEFFECT = DROPEFFECT_COPY;
            target.DragEnter(&data, MK_LBUTTON, POINTL::default(), &mut effect).expect("drag enter");
            println!("effect: {effect:?}");
            assert!(effect.0 & DROPEFFECT_COPY.0 != 0);
            let _ = target.DragLeave();
        }
    }

    /// Lists the apps for a type, to see what Windows gives: `LOOKER_EXT=.jpg cargo test --release -- --ignored
    /// --nocapture lists_apps`.
    #[test]
    #[ignore]
    fn lists_apps() {
        unsafe {
            let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        }
        let ext = std::env::var("LOOKER_EXT").unwrap_or(".jpg".into());
        let t = Instant::now();
        let apps = enumerate(&ext);
        println!("{} apps in {:?}", apps.len(), t.elapsed());
        for (a, _) in apps.iter() {
            println!("{} | {} | {:?}", a.name, a.exe, a.icon);
        }
    }
}
