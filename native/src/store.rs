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
