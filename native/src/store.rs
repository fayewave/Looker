//! The Microsoft Store side (ports of `UpdateService.cs` and `DefaultAppsService.cs`). Updates are installed by
//! Windows in the background, from the Store or, for the GitHub release, from its `.appinstaller` file; this
//! asks whether a newer version is published, so the UI can say "an update is waiting" and deep-link to the
//! Store (or install it at once, for the GitHub release). A packaged app can't change file associations itself, so
//! "set as default" opens Settings on Looker's own Default apps page.
//!
//! All of it needs package identity; unpackaged (dev builds) the update check answers Unknown ("Couldn't
//! check"), as a dev-registered layout of the C# app does.

use windows::ApplicationModel::{AppInfo, Package};
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
            if let Some(uri) = appinstaller_uri() {
                return check_appinstaller(&uri);
            }
            // The Store's own answer (Package.CheckUpdateAvailabilityAsync is for .appinstaller installs).
            // Asking shows no UI, so no owner window is needed.
            let updates = windows::Services::Store::StoreContext::GetDefault()?.GetAppAndOptionalStorePackageUpdatesAsync()?.join()?;
            Ok(if updates.Size()? > 0 { UpdateStatus::Available } else { UpdateStatus::UpToDate })
        })()
        .unwrap_or(UpdateStatus::Unknown);
        crate::trace::mark(format!("store update check: {status:?}"));
        unsafe {
            let _ = PostMessageW(Some(HWND(h as _)), WM_UPDATE_STATUS, WPARAM(status as usize), LPARAM(0));
        }
    });
}

/// The Store's publisher: every other one is the GitHub release (signed with Trusted Signing).
const STORE_PUBLISHER: &str = "CN=75645224-1CB5-47AA-A845-256EA45E9908";
/// Where the GitHub release's `.appinstaller` always is (Publish-GitHubRelease.ps1).
const GITHUB_APPINSTALLER: &str = "https://github.com/fayewave/Looker/releases/latest/download/Looker.appinstaller";

/// The GitHub release: the `.appinstaller` it updates from, when this copy is one (asked once: WinRT calls).
/// Installed through that file, Windows checks it on every launch and updates in the background. Installed
/// from the `.msix` directly, Windows knows of no such file: Looker checks it itself, and "Update now"
/// installs through it, after which Windows keeps that copy up to date too. The Store's copies (and dev
/// registrations, which carry its publisher) are None.
pub fn appinstaller_uri() -> Option<String> {
    static URI: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
    URI.get_or_init(|| {
        let p = Package::Current().ok()?;
        let known = p.GetAppInstallerInfo().and_then(|i| i.Uri()).and_then(|u| u.AbsoluteUri());
        if let Some(u) = known.ok().map(|u| u.to_string_lossy()).filter(|u| !u.is_empty()) {
            return Some(u);
        }
        let publisher = p.Id().and_then(|id| id.Publisher()).ok()?.to_string_lossy();
        (publisher != STORE_PUBLISHER).then(|| GITHUB_APPINSTALLER.to_string())
    })
    .clone()
}

/// Reads the `.appinstaller` and compares its package version with this one's.
fn check_appinstaller(uri: &str) -> windows::core::Result<UpdateStatus> {
    let client = windows::Web::Http::HttpClient::new()?;
    let uri = windows::Foundation::Uri::CreateUri(&windows::core::HSTRING::from(uri))?;
    let xml = client.GetStringAsync(&uri)?.join()?.to_string_lossy();
    let Some(latest) = appinstaller_version(&xml) else { return Ok(UpdateStatus::Unknown) };
    let v = Package::Current()?.Id()?.Version()?;
    let current = [v.Major, v.Minor, v.Build, v.Revision];
    Ok(if latest > current { UpdateStatus::Available } else { UpdateStatus::UpToDate })
}

/// The `Version` of an `.appinstaller`'s `MainPackage`.
fn appinstaller_version(xml: &str) -> Option<[u16; 4]> {
    let main = &xml[xml.find("<MainPackage")?..];
    let main = &main[..main.find('>')?];
    let start = main.find(" Version=\"")? + " Version=\"".len();
    let text = &main[start..start + main[start..].find('"')?];
    let mut v = [0u16; 4];
    let mut parts = text.split('.');
    for f in &mut v {
        *f = parts.next()?.trim().parse().ok()?;
    }
    Some(v)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn appinstaller_version_reads_the_main_package() {
        let xml = r#"<?xml version="1.0" encoding="utf-8"?>
<AppInstaller xmlns="http://schemas.microsoft.com/appx/appinstaller/2021" Version="1.0.0.0" Uri="https://x/Looker.appinstaller">
  <MainPackage Name="fayewave.Looker-PhotoViewer" Publisher="CN=A, O=B" Version="1.2.3.0" ProcessorArchitecture="x64" Uri="https://x/a.msix" />
</AppInstaller>"#;
        assert_eq!(appinstaller_version(xml), Some([1, 2, 3, 0]));
        assert!(appinstaller_version(xml).unwrap() > [1, 1, 9, 0]);
        assert_eq!(appinstaller_version("<AppInstaller Version=\"1.0.0.0\"/>"), None);
    }
}
