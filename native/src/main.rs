//! Looker, native. Startup order is the whole point:
//! 1. the launch photo's decode starts on the pool before anything else, sized for the window that will open;
//! 2. a WARP device + fonts are built on another thread meanwhile (see gfx.rs for why not the GPU yet);
//! 3. the UI thread creates the window, then draws the first frame with the image already in it when the
//!    decode is fast enough, and only then shows the window;
//! 4. the hardware device is built in the background and swapped in.

#![windows_subsystem = "windows"]

mod app;
mod decode;
mod folder;
mod gfx;
mod trace;
mod view;

use std::path::PathBuf;

use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx};
use windows::Win32::UI::HiDpi::{DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext};

fn main() {
    trace::mark("main");
    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }
    let path = std::env::args_os().nth(1).map(PathBuf::from).filter(|p| p.is_file());
    let placement = app::initial_placement();

    let workers = std::thread::available_parallelism().map_or(2, |n| n.get().saturating_sub(1).clamp(2, 4));
    let pool = decode::Pool::start(workers);
    if let Some(p) = &path {
        let (bw, bh) = placement.viewport_px();
        pool.submit(decode::Job { path: p.clone(), box_w: bw, box_h: bh, priority: decode::Priority::Current });
    }
    trace::mark("launch decode submitted");

    let icon_px = (16.0 * placement.dpi as f32 / 96.0).round() as u32;
    let gfx_thread = std::thread::Builder::new()
        .name("gfx-init".into())
        .spawn(move || {
            unsafe {
                let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
            }
            let ready = gfx::create_device(true).and_then(|d| gfx::init_text().map(|t| (d, t)));
            let (dev, text) = match ready {
                Ok(r) => r,
                Err(e) => {
                    trace::mark(format!("gfx init failed: {e}"));
                    return None;
                }
            };
            let icon = decode::decode_icon(include_bytes!("../../src/Looker/Assets/AppIcon.ico"), icon_px).ok();
            Some((gfx::Sendable((dev, text)), icon))
        })
        .expect("spawn gfx init");

    app::run(path, placement, pool, gfx_thread);
}
