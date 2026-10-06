//! The Microsoft Store side (ports of `UpdateService.cs` and `DefaultAppsService.cs`). Updates are installed by
//! Windows in the background, from the Store or, for the GitHub release, from its `.appinstaller` file; this
//! asks whether a newer version is published, so the UI can say "an update is waiting" and deep-link to the
//! Store (or install it at once, for the GitHub release). A packaged app can't change file associations itself, so
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
    /// A GitHub install is installing the update (Looker closes and restarts when it's in).
    Installing,
    /// Installing it failed; Windows tries again the next time Looker starts.
    Failed,
}

impl UpdateStatus {
    pub fn from_u8(v: u8) -> UpdateStatus {
        match v {
            1 => UpdateStatus::Checking,
            2 => UpdateStatus::UpToDate,
            3 => UpdateStatus::Available,
            4 => UpdateStatus::Installing,
            5 => UpdateStatus::Failed,
            _ => UpdateStatus::Unknown,
        }
    }
}

/// Running with package identity (the MSIX). `LOOKER_PACKAGED_UI=1` shows the packaged-only UI in a dev
/// build, for checking its layout.
pub fn packaged() -> bool {
    std::env::var_os("LOOKER_PACKAGED_UI").is_some() || family_name().is_some()
}

/// The package family name when running packaged (a kernel call, microseconds: fine at startup).
pub fn family_name() -> Option<String> {
    use windows::Win32::Storage::Packaging::Appx::GetCurrentPackageFamilyName;
    let mut len = 0u32;
    unsafe {
        // Unpackaged: APPMODEL_ERROR_NO_PACKAGE. Packaged: ERROR_INSUFFICIENT_BUFFER with the length.
        let _ = GetCurrentPackageFamilyName(&mut len, None);
        if len == 0 {
            return None;
        }
        let mut buf = vec![0u16; len as usize];
        if GetCurrentPackageFamilyName(&mut len, Some(windows::core::PWSTR(buf.as_mut_ptr()))).is_err() {
            return None;
        }
        Some(String::from_utf16_lossy(&buf[..len.saturating_sub(1) as usize]))
    }
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

/// Installed from the GitHub release's `.appinstaller` rather than the Store: Windows checks that file on every
/// launch and installs a newer version in the background, and "Update now" asks it to right away. Its address,
/// when so (asked once: a WinRT call).
pub fn appinstaller_uri() -> Option<String> {
    static URI: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
    URI.get_or_init(|| {
        let uri = Package::Current().and_then(|p| p.GetAppInstallerInfo()).and_then(|i| i.Uri()).and_then(|u| u.AbsoluteUri());
        uri.ok().map(|u| u.to_string_lossy()).filter(|u| !u.is_empty())
    })
    .clone()
}

/// Installs the update from the `.appinstaller` now, on a worker thread. Windows closes Looker to do it and
/// starts it again (on `reopen`, the photo that was up) through the restart registration; a failure is posted
/// as [`UpdateStatus::Failed`].
pub fn install_update(hwnd: HWND, reopen: Option<&std::path::Path>) {
    use windows::Management::Deployment::{AddPackageByAppInstallerOptions, PackageManager, PackageVolume};
    use windows::Win32::System::Recovery::{RESTART_NO_CRASH, RESTART_NO_HANG, RESTART_NO_REBOOT, RegisterApplicationRestart, UnregisterApplicationRestart};
    let Some(uri) = appinstaller_uri() else { return };
    let args = reopen.map(|p| format!("\"{}\"", p.display())).unwrap_or_default();
    let h = hwnd.0 as isize;
    let _ = std::thread::Builder::new().name("appinstaller".into()).spawn(move || {
        unsafe {
            let _ = windows::Win32::System::Com::CoInitializeEx(None, windows::Win32::System::Com::COINIT_MULTITHREADED);
            // Only an update restarts it (a crash, hang or reboot doesn't).
            let wide: Vec<u16> = args.encode_utf16().chain(Some(0)).collect();
            let _ = RegisterApplicationRestart(windows::core::PCWSTR(wide.as_ptr()), RESTART_NO_CRASH | RESTART_NO_HANG | RESTART_NO_REBOOT);
        }
        let done = (|| -> windows::core::Result<()> {
            let uri = windows::Foundation::Uri::CreateUri(&windows::core::HSTRING::from(uri.as_str()))?;
            let op = PackageManager::new()?.AddPackageByAppInstallerFileAsync(&uri, AddPackageByAppInstallerOptions::ForceTargetAppShutdown, None::<&PackageVolume>)?;
            let r = op.join()?;
            r.ExtendedErrorCode()?.ok()
        })();
        // Still here: nothing was installed (installing closes Looker).
        crate::trace::mark(format!("appinstaller update: {done:?}"));
        unsafe {
            let _ = UnregisterApplicationRestart();
            let _ = PostMessageW(Some(HWND(h as _)), WM_UPDATE_STATUS, WPARAM(UpdateStatus::Failed as usize), LPARAM(0));
        }
    });
}

/// A Windows extension from the Microsoft Store that a format needs: what to call it, why, and its Store id.
pub struct Extension {
    pub why: &'static str,
    pub product_id: &'static str,
}

/// The extension a file of this format failed for want of, if that is the reason: camera RAW without the
/// (free) Raw Image Extension, HEIC/AVIF only when the bundled libheif is missing too.
pub fn missing_extension(format: crate::format::Format) -> Option<Extension> {
    use crate::format::Format;
    match format {
        Format::Raw if !raw_extension_installed() => {
            Some(Extension { why: "Windows needs the free Raw Image Extension to open camera RAW files.", product_id: "9NCTDW2W1BH8" })
        }
        Format::Heif if !crate::imaging::heif::available() => {
            Some(Extension { why: "Windows needs the HEVC Video Extensions to open HEIC photos.", product_id: "9NMZLZ57R3T7" })
        }
        Format::Avif if !crate::imaging::heif::available() => {
            Some(Extension { why: "Windows needs the free AV1 Video Extension to open AVIF images.", product_id: "9MVZQVXJBQ9V" })
        }
        _ => None,
    }
}

fn raw_extension_installed() -> bool {
    use windows::Win32::Graphics::Imaging::IWICBitmapDecoder;
    use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance};
    unsafe { CoCreateInstance::<_, IWICBitmapDecoder>(&crate::imaging::wic::RAW_IMAGE_DECODER, None, CLSCTX_INPROC_SERVER).is_ok() }
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
