//! The Microsoft Store side (ports of `UpdateService.cs` and `DefaultAppsService.cs`). Updates are installed by
//! Windows in the background; this only asks whether a newer version is published, so the UI can say "an
//! update is waiting" and deep-link to the Store. A packaged app can't change file associations itself, so
//! "set as default" opens Settings on Looker's own Default apps page.
//!
//! All of it needs package identity; unpackaged (dev builds) the update check answers Unknown ("Couldn't
//! check"), as a dev-registered layout of the C# app does.

use windows::ApplicationModel::{AppInfo, Package, PackageUpdateAvailability};
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_APP};

/// Posted with the [`UpdateStatus`] (as `u8`) in `wparam` when a check finishes.
pub const WM_UPDATE_STATUS: u32 = WM_APP + 12;

pub const STORE_UPDATES: &str = "ms-windows-store://downloadsandupdates";

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum UpdateStatus {
    /// No check has finished, or the Store couldn't answer (unpackaged, offline).
    #[default]
    Unknown,
    Checking,
    UpToDate,
    Available,
}

impl UpdateStatus {
    pub fn from_u8(v: u8) -> UpdateStatus {
        match v {
            1 => UpdateStatus::Checking,
            2 => UpdateStatus::UpToDate,
            3 => UpdateStatus::Available,
            _ => UpdateStatus::Unknown,
        }
    }
}

/// Running with package identity (the MSIX). `LOOKER_PACKAGED_UI=1` shows the packaged-only UI in a dev
/// build, for checking its layout.
pub fn packaged() -> bool {
    std::env::var_os("LOOKER_PACKAGED_UI").is_some() || Package::Current().is_ok()
}

/// Asks the Store on a worker thread (a network call: never on the startup path); the answer is posted.
pub fn check_updates(hwnd: HWND) {
    let h = hwnd.0 as isize;
    let _ = std::thread::Builder::new().name("store-check".into()).spawn(move || {
        unsafe {
            let _ = windows::Win32::System::Com::CoInitializeEx(None, windows::Win32::System::Com::COINIT_MULTITHREADED);
        }
        let status = (|| -> windows::core::Result<UpdateStatus> {
            let r = Package::Current()?.CheckUpdateAvailabilityAsync()?.join()?;
            let a = r.Availability()?;
            Ok(if a == PackageUpdateAvailability::Available || a == PackageUpdateAvailability::Required {
                UpdateStatus::Available
            } else if a == PackageUpdateAvailability::NoUpdates {
                UpdateStatus::UpToDate
            } else {
                UpdateStatus::Unknown
            })
        })()
        .unwrap_or(UpdateStatus::Unknown);
        crate::trace::mark(format!("store update check: {status:?}"));
        unsafe {
            let _ = PostMessageW(Some(HWND(h as _)), WM_UPDATE_STATUS, WPARAM(status as usize), LPARAM(0));
        }
    });
}

/// The `ms-settings:` link to Looker's own Default apps page (Windows 11), or the general page when there is
/// no identity to name.
pub fn default_apps_uri() -> String {
    const BASE: &str = "ms-settings:defaultapps";
    match AppInfo::Current().and_then(|a| a.AppUserModelId()) {
        Ok(aumid) if !aumid.is_empty() => {
            // The AUMID is "family!App": only '!' needs escaping in a query value here.
            format!("{BASE}?registeredAUMID={}", aumid.to_string_lossy().replace('!', "%21"))
        }
        _ => BASE.to_string(),
    }
}
