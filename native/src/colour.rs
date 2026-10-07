//! Which colour space photos are drawn in, from the monitor the window is on:
//!
//! - **SDR**, the usual case: Windows shows an 8-bit swap chain's pixels on the panel as they are. Photos are
//!   converted to the monitor's colour profile when one is set (Settings > Display > Color profile), else to
//!   sRGB, on the decode thread (`imaging::wic`).
//! - **Advanced colour** (HDR on, or Auto Color Management on an SDR panel): the desktop composes in linear
//!   scRGB, an 8-bit swap chain counts as sRGB and anything outside sRGB is lost. The photo layer is drawn in a
//!   16-bit float scRGB swap chain instead (`gfx`), photos keep their own profile to the GPU, and HDR images
//!   go above SDR white.
//!
//! The decode pool reads [`current`] for every job; each change bumps a generation so decodes made for the
//! previous space are dropped instead of shown.

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, RwLock};

use windows::Win32::Devices::Display::*;
use windows::Win32::Foundation::{ERROR_SUCCESS, LUID};
use windows::Win32::Graphics::Gdi::{CreateDCW, DeleteDC, GetMonitorInfoW, HMONITOR, MONITORINFOEXW};
use windows::Win32::UI::ColorSystem::GetICMProfileW;
use windows::core::{PCWSTR, PWSTR, w};

#[derive(Clone, Debug, PartialEq)]
pub enum Space {
    /// 8-bit, sRGB.
    Srgb,
    /// 8-bit, the monitor's own ICC profile (an SDR monitor with a profile that isn't sRGB).
    Icc(Arc<Vec<u8>>),
    /// 16-bit float scRGB (1.0 = 80 nits): HDR or Auto Color Management.
    Scrgb {
        /// Where SDR white (a photo's 255) sits, in nits: Settings' "SDR content brightness" under HDR, 80
        /// (scRGB 1.0) under Auto Color Management.
        white_nits: f32,
        /// HDR is on (above SDR white there is room up to [`peak_nits`]).
        hdr: bool,
    },
}

impl Space {
    pub fn is_scrgb(&self) -> bool {
        matches!(self, Space::Scrgb { .. })
    }

    /// SDR white as a multiple of scRGB 1.0.
    pub fn white_scale(&self) -> f32 {
        match self {
            Space::Scrgb { white_nits, .. } => white_nits / 80.0,
            _ => 1.0,
        }
    }

    /// How far above SDR white the display can go (1.0 when it can't).
    pub fn headroom(&self) -> f32 {
        match self {
            Space::Scrgb { white_nits, hdr: true } => (peak_nits() / white_nits).max(1.0),
            _ => 1.0,
        }
    }
}

/// The panel's peak brightness in nits (bits of an f32; 0 = not read yet). Not part of the space: asking DXGI
/// costs ~12 ms the first time in a process, too much before the first frame, and only HDR photos use it.
static PEAK: AtomicU32 = AtomicU32::new(0);
/// The monitor [`query`] last looked at (its handle as an integer), whose peak `PEAK` is.
static MONITOR: std::sync::atomic::AtomicIsize = std::sync::atomic::AtomicIsize::new(0);

/// The peak of the monitor last queried, read now if [`refresh_peak`] hasn't yet (1000 nits if DXGI can't say).
pub fn peak_nits() -> f32 {
    if PEAK.load(Ordering::Relaxed) == 0 {
        let m = MONITOR.load(Ordering::Relaxed);
        if m != 0 {
            refresh_peak(HMONITOR(m as _));
        }
    }
    match PEAK.load(Ordering::Relaxed) {
        0 => 1000.0,
        b => f32::from_bits(b),
    }
}

/// Reads the peak of `monitor` (on whatever thread can spare the time).
pub fn refresh_peak(monitor: HMONITOR) {
    if let Some(p) = peak(monitor) {
        PEAK.store(p.to_bits(), Ordering::Relaxed);
    }
}

pub struct Output {
    pub space: Space,
    pub generation: u32,
}

static OUTPUT: RwLock<Option<Arc<Output>>> = RwLock::new(None);
static GENERATION: AtomicU32 = AtomicU32::new(0);

/// The space photos are decoded for now (sRGB until the first [`set`]).
pub fn current() -> Arc<Output> {
    if let Some(o) = OUTPUT.read().unwrap().as_ref() {
        return o.clone();
    }
    Arc::new(Output { space: Space::Srgb, generation: 0 })
}

/// Switches the space; false when it is already this one.
pub fn set(space: Space) -> bool {
    let mut o = OUTPUT.write().unwrap();
    if o.as_ref().is_some_and(|o| o.space == space) {
        return false;
    }
    let generation = GENERATION.fetch_add(1, Ordering::Relaxed) + 1;
    crate::trace::mark(format!("colour: {}", describe(&space)));
    *o = Some(Arc::new(Output { space, generation }));
    true
}

pub fn describe(s: &Space) -> String {
    match s {
        Space::Srgb => "sRGB".into(),
        Space::Icc(p) => format!("monitor profile ({} bytes)", p.len()),
        Space::Scrgb { white_nits, hdr } => format!("scRGB, SDR white {white_nits:.0} nits{}", if *hdr { ", HDR" } else { "" }),
    }
}

/// `LOOKER_COLOR=srgb`, `scrgb` (HDR with SDR white at 200 nits) or the path of an ICC profile (an SDR monitor
/// with that profile) overrides the monitor.
fn forced() -> Option<Space> {
    let v = std::env::var("LOOKER_COLOR").ok()?;
    match v.to_ascii_lowercase().as_str() {
        "srgb" => Some(Space::Srgb),
        "scrgb" => Some(Space::Scrgb { white_nits: 200.0, hdr: true }),
        _ => std::fs::read(&v).ok().map(|p| Space::Icc(Arc::new(p))),
    }
}

/// DISPLAYCONFIG_GET_ADVANCED_COLOR_INFO_2 (Windows 11 24H2), not in the bindings yet.
#[repr(C)]
#[derive(Default)]
struct AdvancedColorInfo2 {
    header: DISPLAYCONFIG_DEVICE_INFO_HEADER,
    flags: u32,
    color_encoding: i32,
    bits_per_channel: u32,
    /// 0 SDR, 1 WCG (Auto Color Management), 2 HDR.
    active_color_mode: i32,
}

#[derive(Debug, PartialEq)]
enum Mode {
    Sdr,
    Wcg,
    Hdr,
}

/// The space for photos on `monitor`.
pub fn query(monitor: HMONITOR) -> Space {
    if MONITOR.swap(monitor.0 as isize, Ordering::Relaxed) != monitor.0 as isize {
        PEAK.store(0, Ordering::Relaxed);
    }
    if let Some(s) = forced() {
        return s;
    }
    let Some(device) = device_name(monitor) else { return Space::Srgb };
    match target_of(&device) {
        Some((adapter, id)) => match mode(adapter, id) {
            Mode::Hdr => Space::Scrgb { white_nits: sdr_white(adapter, id).unwrap_or(80.0), hdr: true },
            Mode::Wcg => Space::Scrgb { white_nits: 80.0, hdr: false },
            Mode::Sdr => monitor_profile(&device).map_or(Space::Srgb, |p| Space::Icc(Arc::new(p))),
        },
        None => monitor_profile(&device).map_or(Space::Srgb, |p| Space::Icc(Arc::new(p))),
    }
}

fn device_name(monitor: HMONITOR) -> Option<[u16; 32]> {
    unsafe {
        let mut mi = MONITORINFOEXW::default();
        mi.monitorInfo.cbSize = size_of::<MONITORINFOEXW>() as u32;
        GetMonitorInfoW(monitor, &mut mi as *mut _ as *mut _).as_bool().then_some(mi.szDevice)
    }
}

fn wide_eq(a: &[u16], b: &[u16]) -> bool {
    let end = |s: &[u16]| s.iter().position(|&c| c == 0).unwrap_or(s.len());
    a[..end(a)] == b[..end(b)]
}

/// The display target (adapter, id) behind a GDI device name (`\\.\DISPLAY1`).
fn target_of(device: &[u16; 32]) -> Option<(LUID, u32)> {
    unsafe {
        let (mut np, mut nm) = (0u32, 0u32);
        if GetDisplayConfigBufferSizes(QDC_ONLY_ACTIVE_PATHS, &mut np, &mut nm) != ERROR_SUCCESS {
            return None;
        }
        let mut paths = vec![DISPLAYCONFIG_PATH_INFO::default(); np as usize];
        let mut modes = vec![DISPLAYCONFIG_MODE_INFO::default(); nm as usize];
        if QueryDisplayConfig(QDC_ONLY_ACTIVE_PATHS, &mut np, paths.as_mut_ptr(), &mut nm, modes.as_mut_ptr(), None) != ERROR_SUCCESS {
            return None;
        }
        paths.truncate(np as usize);
        for p in &paths {
            let mut name = DISPLAYCONFIG_SOURCE_DEVICE_NAME::default();
            name.header = DISPLAYCONFIG_DEVICE_INFO_HEADER {
                r#type: DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME,
                size: size_of::<DISPLAYCONFIG_SOURCE_DEVICE_NAME>() as u32,
                adapterId: p.sourceInfo.adapterId,
                id: p.sourceInfo.id,
            };
            if DisplayConfigGetDeviceInfo(&mut name.header) == 0 && wide_eq(&name.viewGdiDeviceName, device) {
                return Some((p.targetInfo.adapterId, p.targetInfo.id));
            }
        }
        None
    }
}

fn mode(adapter: LUID, id: u32) -> Mode {
    unsafe {
        let mut info = AdvancedColorInfo2::default();
        info.header = DISPLAYCONFIG_DEVICE_INFO_HEADER {
            r#type: DISPLAYCONFIG_DEVICE_INFO_TYPE(15),
            size: size_of::<AdvancedColorInfo2>() as u32,
            adapterId: adapter,
            id,
        };
        if DisplayConfigGetDeviceInfo(&mut info.header) == 0 {
            return match info.active_color_mode {
                2 => Mode::Hdr,
                1 => Mode::Wcg,
                _ => Mode::Sdr,
            };
        }
        // Before 24H2 advanced colour meant HDR.
        let mut old = DISPLAYCONFIG_GET_ADVANCED_COLOR_INFO::default();
        old.header = DISPLAYCONFIG_DEVICE_INFO_HEADER {
            r#type: DISPLAYCONFIG_DEVICE_INFO_GET_ADVANCED_COLOR_INFO,
            size: size_of::<DISPLAYCONFIG_GET_ADVANCED_COLOR_INFO>() as u32,
            adapterId: adapter,
            id,
        };
        if DisplayConfigGetDeviceInfo(&mut old.header) == 0 && old.Anonymous.value & 0b10 != 0 { Mode::Hdr } else { Mode::Sdr }
    }
}

/// Settings' "SDR content brightness", in nits.
fn sdr_white(adapter: LUID, id: u32) -> Option<f32> {
    unsafe {
        let mut w = DISPLAYCONFIG_SDR_WHITE_LEVEL::default();
        w.header = DISPLAYCONFIG_DEVICE_INFO_HEADER {
            r#type: DISPLAYCONFIG_DEVICE_INFO_GET_SDR_WHITE_LEVEL,
            size: size_of::<DISPLAYCONFIG_SDR_WHITE_LEVEL>() as u32,
            adapterId: adapter,
            id,
        };
        // In thousandths of 80 nits.
        (DisplayConfigGetDeviceInfo(&mut w.header) == 0 && w.SDRWhiteLevel > 0).then(|| w.SDRWhiteLevel as f32 / 1000.0 * 80.0)
    }
}

/// The panel's peak brightness as DXGI reports it (the HDR calibration app's value when it has run).
fn peak(monitor: HMONITOR) -> Option<f32> {
    use windows::Win32::Graphics::Dxgi::*;
    use windows::core::Interface;
    unsafe {
        let f: IDXGIFactory1 = CreateDXGIFactory1().ok()?;
        let mut a = 0;
        while let Ok(adapter) = f.EnumAdapters1(a) {
            let mut o = 0;
            while let Ok(out) = adapter.EnumOutputs(o) {
                if let Ok(o6) = out.cast::<IDXGIOutput6>() {
                    if let Ok(d) = o6.GetDesc1() {
                        if d.Monitor == monitor && d.MaxLuminance > 0.0 {
                            return Some(d.MaxLuminance);
                        }
                    }
                }
                o += 1;
            }
            a += 1;
        }
        None
    }
}

/// The monitor's default colour profile, unless it is (or there is only) the sRGB one.
fn monitor_profile(device: &[u16; 32]) -> Option<Vec<u8>> {
    unsafe {
        let dc = CreateDCW(w!("DISPLAY"), PCWSTR(device.as_ptr()), PCWSTR::null(), None);
        if dc.is_invalid() {
            return None;
        }
        let mut buf = [0u16; 260];
        let mut len = buf.len() as u32;
        let ok = GetICMProfileW(dc, &mut len, Some(PWSTR(buf.as_mut_ptr()))).as_bool();
        let _ = DeleteDC(dc);
        if !ok {
            return None;
        }
        let path = String::from_utf16_lossy(&buf[..buf.iter().position(|&c| c == 0).unwrap_or(0)]);
        let name = std::path::Path::new(&path).file_name()?.to_string_lossy().to_ascii_lowercase();
        if name.starts_with("srgb") {
            return None;
        }
        let bytes = std::fs::read(&path).ok()?;
        crate::trace::mark(format!("colour: monitor profile {path}"));
        Some(bytes)
    }
}

/// sRGB-encoded 0..1 to linear.
pub fn srgb_to_linear(c: f32) -> f32 {
    if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
}

/// f32 to IEEE half (round to nearest; enough for pixels in 0..65504).
pub fn f16(v: f32) -> u16 {
    let b = v.to_bits();
    let sign = ((b >> 16) & 0x8000) as u16;
    let exp = ((b >> 23) & 0xFF) as i32 - 127 + 15;
    let man = b & 0x7F_FFFF;
    if v.is_nan() {
        return sign | 0x7E00;
    }
    if exp >= 31 {
        return sign | 0x7C00;
    }
    if exp <= 0 {
        if exp < -10 {
            return sign;
        }
        let m = (man | 0x80_0000) >> (1 - exp);
        return sign | ((m + 0x1000) >> 13) as u16;
    }
    let h = ((exp as u32) << 10) | (man >> 13);
    // Round half up on the dropped bits (a carry into the exponent is still correct).
    sign | (h + ((man >> 12) & 1)) as u16
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn halves() {
        assert_eq!(f16(0.0), 0);
        assert_eq!(f16(1.0), 0x3C00);
        assert_eq!(f16(-2.0), 0xC000);
        assert_eq!(f16(0.5), 0x3800);
        assert_eq!(f16(65504.0), 0x7BFF);
        assert_eq!(f16(1e6), 0x7C00);
        assert_eq!(f16(6.1035156e-5), 0x0400); // smallest normal
        assert_eq!(f16(5.9604645e-8), 0x0001); // smallest subnormal
    }

    #[test]
    fn srgb_curve() {
        assert_eq!(srgb_to_linear(0.0), 0.0);
        assert!((srgb_to_linear(1.0) - 1.0).abs() < 1e-6);
        assert!((srgb_to_linear(0.5) - 0.214).abs() < 1e-3);
    }

    /// Times each step of `query` on the primary monitor. `cargo test --release -- --ignored --nocapture query_cost`
    #[test]
    #[ignore]
    fn query_cost() {
        use windows::Win32::Foundation::POINT;
        use windows::Win32::Graphics::Gdi::{MONITOR_DEFAULTTOPRIMARY, MonitorFromPoint};
        let t = std::time::Instant::now();
        let ms = |t: &std::time::Instant| t.elapsed().as_secs_f64() * 1000.0;
        let m = unsafe { MonitorFromPoint(POINT { x: 0, y: 0 }, MONITOR_DEFAULTTOPRIMARY) };
        let dev = device_name(m).unwrap();
        println!("device name {:.2} ms", ms(&t));
        let t = std::time::Instant::now();
        let (a, id) = target_of(&dev).unwrap();
        println!("target {:.2} ms", ms(&t));
        let t = std::time::Instant::now();
        println!("mode {:?} {:.2} ms", mode(a, id), ms(&t));
        let t = std::time::Instant::now();
        println!("white {:?} {:.2} ms", sdr_white(a, id), ms(&t));
        let t = std::time::Instant::now();
        println!("peak {:?} {:.2} ms", peak(m), ms(&t));
        let t = std::time::Instant::now();
        println!("profile {:?} {:.2} ms", monitor_profile(&dev).map(|p| p.len()), ms(&t));
        let t = std::time::Instant::now();
        println!("query {:?} {:.2} ms", query(m), ms(&t));
    }

    #[test]
    fn white_and_headroom() {
        let s = Space::Scrgb { white_nits: 250.0, hdr: true };
        assert_eq!(s.white_scale(), 3.125);
        assert_eq!(s.headroom(), 4.0); // the 1000-nit default peak
        assert_eq!(Space::Scrgb { white_nits: 80.0, hdr: false }.headroom(), 1.0);
        assert_eq!(Space::Srgb.headroom(), 1.0);
    }
}
