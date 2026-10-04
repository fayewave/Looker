//! Startup timeline: milliseconds since process creation for each phase, buffered in memory and
//! appended to `%TEMP%\looker-native-startup.log` in one write (the native twin of `Helpers/StartupTrace.cs`).

use std::io::Write;
use std::sync::{Mutex, OnceLock};

use windows::Win32::Foundation::FILETIME;
use windows::Win32::System::SystemInformation::GetSystemTimePreciseAsFileTime;
use windows::Win32::System::Threading::{GetCurrentProcess, GetProcessTimes};

static START: OnceLock<u64> = OnceLock::new();
static LINES: Mutex<Vec<String>> = Mutex::new(Vec::new());

fn ticks(f: FILETIME) -> u64 {
    ((f.dwHighDateTime as u64) << 32) | f.dwLowDateTime as u64
}

/// Milliseconds since the OS created this process (includes loader time before `main`).
pub fn now_ms() -> f64 {
    let start = *START.get_or_init(|| unsafe {
        let (mut c, mut e, mut k, mut u) = Default::default();
        let _ = GetProcessTimes(GetCurrentProcess(), &mut c, &mut e, &mut k, &mut u);
        ticks(c)
    });
    let now = unsafe { ticks(GetSystemTimePreciseAsFileTime()) };
    now.saturating_sub(start) as f64 / 10_000.0
}

pub fn mark(what: impl AsRef<str>) {
    let t = now_ms();
    let thread = std::thread::current();
    let name = thread.name().unwrap_or("?");
    if let Ok(mut lines) = LINES.lock() {
        lines.push(format!("{t:8.1} ms  [{name}] {}", what.as_ref()));
    }
}

/// Appends everything marked so far to the log file and clears the buffer.
pub fn flush(reason: &str) {
    let lines = match LINES.lock() {
        Ok(mut l) => std::mem::take(&mut *l),
        Err(_) => return,
    };
    if lines.is_empty() {
        return;
    }
    let path = std::env::temp_dir().join("looker-native-startup.log");
    let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) else {
        return;
    };
    static HEADER: std::sync::Once = std::sync::Once::new();
    let mut out = String::new();
    HEADER.call_once(|| {
        let args: Vec<String> = std::env::args().skip(1).collect();
        out.push_str(&format!("\n=== pid {}  args {:?} ===\n", std::process::id(), args));
    });
    for l in lines {
        out.push_str(&l);
        out.push('\n');
    }
    out.push_str(&format!("-- flushed: {reason}\n"));
    let _ = f.write_all(out.as_bytes());
}
