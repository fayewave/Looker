//! Single instance: a second launch (a double-clicked photo while Looker is open) hands its file to the
//! running window, brings that window to the front and exits, so there is only ever one Looker. Runs first
//! in `main`, before any decode starts: a named mutex costs microseconds, and only the losing launch looks
//! for the window.
//!
//! `LOOKER_NEW_INSTANCE=1` in the environment opts out (the test driver uses it so a test window never
//! lands in the user's own).

use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::path::Path;

use windows::Win32::Foundation::{ERROR_ALREADY_EXISTS, GetLastError, LPARAM, WPARAM};
use windows::Win32::System::DataExchange::COPYDATASTRUCT;
use windows::Win32::System::Threading::CreateMutexW;
use windows::Win32::UI::WindowsAndMessaging::{
    FindWindowW, IsIconic, SMTO_ABORTIFHUNG, SW_RESTORE, SendMessageTimeoutW, SetForegroundWindow, ShowWindow, WM_COPYDATA,
};
use windows::core::w;

/// `COPYDATASTRUCT::dwData` of an "open this" hand-over ("LOOK"); the payload is the UTF-16 path, empty
/// for a launch without one (which only activates the window).
pub const COPYDATA_OPEN: usize = 0x4C4F_4F4B;

/// True when this process should go on and be Looker; false when the running one took the launch.
pub fn claim_or_forward(path: Option<&Path>) -> bool {
    if std::env::var_os("LOOKER_NEW_INSTANCE").is_some() {
        return true;
    }
    unsafe {
        let Ok(mutex) = CreateMutexW(None, false, w!("Local\\Looker.Native.Instance")) else { return true };
        if GetLastError() != ERROR_ALREADY_EXISTS {
            let _ = mutex; // never closed: held for the life of the process
            return true;
        }
    }
    let mut payload: Vec<u16> = path
        .and_then(|p| std::path::absolute(p).ok())
        .map(|p| p.as_os_str().encode_wide().collect())
        .unwrap_or_default();
    payload.push(0);
    // The first instance may still be starting: give its window a few seconds to appear and take messages.
    for _ in 0..60 {
        if forward(&payload) {
            return false;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    true // it never answered (hung, or closing): be the app instead of dropping the launch
}

fn forward(payload: &[u16]) -> bool {
    unsafe {
        let Ok(hwnd) = FindWindowW(w!("LookerWindow"), None) else { return false };
        if hwnd.is_invalid() {
            return false;
        }
        let cds = COPYDATASTRUCT { dwData: COPYDATA_OPEN, cbData: (payload.len() * 2) as u32, lpData: payload.as_ptr() as _ };
        let mut answer = 0usize;
        let sent = SendMessageTimeoutW(hwnd, WM_COPYDATA, WPARAM(0), LPARAM(&cds as *const _ as isize), SMTO_ABORTIFHUNG, 3000, Some(&mut answer));
        // 1 = taken; 0 = the window exists but its app isn't attached yet.
        if sent.0 == 0 || answer != 1 {
            return false;
        }
        // This process is the one the user just launched, so it may hand the foreground over.
        if IsIconic(hwnd).as_bool() {
            let _ = ShowWindow(hwnd, SW_RESTORE);
        }
        let _ = SetForegroundWindow(hwnd);
        true
    }
}

/// The path in a hand-over, if it is one (`None` = not ours).
pub fn received(cds: &COPYDATASTRUCT) -> Option<Option<std::path::PathBuf>> {
    if cds.dwData != COPYDATA_OPEN || cds.lpData.is_null() {
        return None;
    }
    let units = unsafe { std::slice::from_raw_parts(cds.lpData as *const u16, cds.cbData as usize / 2) };
    let units = units.split(|&u| u == 0).next().unwrap_or(&[]);
    if units.is_empty() {
        return Some(None);
    }
    Some(Some(std::path::PathBuf::from(std::ffi::OsString::from_wide(units))))
}
