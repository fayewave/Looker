//! Windows' spell checker (the one Notepad and Edge use: `ISpellChecker`), in the user's language (else US
//! English): the misspelt words in a piece of text, suggestions for one, its autocorrections (the checker's
//! "replace" answers, "teh" to "the"), and Add to dictionary / Ignore.

use windows::Win32::Globalization::*;
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance, CoTaskMemFree};
use windows::core::{HSTRING, PWSTR};

pub struct Spell {
    checker: ISpellChecker,
}

/// A misspelt word: its UTF-16 range in the text checked, and the checker's own replacement when it has one
/// it's sure of (an autocorrection).
#[derive(Clone, Debug, PartialEq)]
pub struct Miss {
    pub start: usize,
    pub end: usize,
    pub replace: Option<String>,
}

impl Spell {
    pub fn new() -> Option<Spell> {
        unsafe {
            let factory: ISpellCheckerFactory = CoCreateInstance(&SpellCheckerFactory, None, CLSCTX_INPROC_SERVER).ok()?;
            let mut name = [0u16; 85];
            let n = GetUserDefaultLocaleName(&mut name);
            let user = (n > 1).then(|| String::from_utf16_lossy(&name[..(n - 1) as usize]));
            for lang in user.iter().map(String::as_str).chain(["en-US"]) {
                let tag = HSTRING::from(lang);
                if factory.IsSupported(&tag).is_ok_and(|b| b.as_bool()) {
                    if let Ok(checker) = factory.CreateSpellChecker(&tag) {
                        return Some(Spell { checker });
                    }
                }
            }
            None
        }
    }

    /// The misspelt words in `text` (UTF-16).
    pub fn check(&self, text: &[u16]) -> Vec<Miss> {
        let mut out = Vec::new();
        if text.is_empty() {
            return out;
        }
        unsafe {
            let s = HSTRING::from_wide(text);
            let Ok(errors) = self.checker.ComprehensiveCheck(&s) else { return out };
            loop {
                let mut e: Option<ISpellingError> = None;
                if errors.Next(&mut e).0 != 0 {
                    break;
                }
                let Some(e) = e else { break };
                let (Ok(start), Ok(len)) = (e.StartIndex(), e.Length()) else { continue };
                let replace = match e.CorrectiveAction() {
                    Ok(a) if a == CORRECTIVE_ACTION_REPLACE => e.Replacement().ok().map(|p| {
                        let s = p.to_string().unwrap_or_default();
                        CoTaskMemFree(Some(p.0 as _));
                        s
                    }),
                    Ok(a) if a == CORRECTIVE_ACTION_NONE => continue,
                    _ => None,
                };
                out.push(Miss { start: start as usize, end: (start + len) as usize, replace });
                if out.len() > 2000 {
                    break;
                }
            }
        }
        out
    }

    /// Up to `max` suggestions for a word.
    pub fn suggest(&self, word: &str, max: usize) -> Vec<String> {
        let mut out = Vec::new();
        unsafe {
            let Ok(list) = self.checker.Suggest(&HSTRING::from(word)) else { return out };
            while out.len() < max {
                let mut one = [PWSTR::null()];
                let mut got = 0u32;
                if list.Next(&mut one, Some(&mut got)).0 != 0 || got == 0 {
                    break;
                }
                out.push(one[0].to_string().unwrap_or_default());
                CoTaskMemFree(Some(one[0].0 as _));
            }
        }
        out
    }

    pub fn add(&self, word: &str) {
        unsafe {
            let _ = self.checker.Add(&HSTRING::from(word));
        }
    }

    pub fn ignore(&self, word: &str) {
        unsafe {
            let _ = self.checker.Ignore(&HSTRING::from(word));
        }
    }
}

/// Posted to the window when the worker has results waiting (`Worker::take`).
pub const WM_SPELLED: u32 = windows::Win32::UI::WindowsAndMessaging::WM_APP + 40;

/// A line to check: which document and version it belongs to, its index and its text.
pub struct Job {
    pub doc: u64,
    pub version: u64,
    pub line: usize,
    pub text: Vec<u16>,
}

/// A checked line.
pub struct Done {
    pub doc: u64,
    pub version: u64,
    pub line: usize,
    pub misses: Vec<Miss>,
}

#[derive(Default)]
struct Shared {
    /// The lines wanted, first first; replaced whole by each `want` (lines scrolled away are dropped).
    queue: Vec<Job>,
    /// The line being checked now (doc, version, line).
    busy: Option<(u64, u64, usize)>,
    done: Vec<Done>,
    posted: bool,
    quit: bool,
}

/// Spell check off the UI thread: Windows' checker takes up to a few milliseconds a line, and a screenful of new
/// lines (a long log scrolled) cost a frame 10-75 ms (2026-11-11). Its own checker, on its own MTA thread.
pub struct Worker {
    shared: std::sync::Arc<(std::sync::Mutex<Shared>, std::sync::Condvar)>,
}

impl Worker {
    pub fn start(hwnd: windows::Win32::Foundation::HWND) -> Worker {
        let shared = std::sync::Arc::new((std::sync::Mutex::new(Shared::default()), std::sync::Condvar::new()));
        let s = shared.clone();
        let hwnd = hwnd.0 as usize;
        let _ = std::thread::Builder::new().name("spell".into()).spawn(move || {
            unsafe {
                let _ = windows::Win32::System::Com::CoInitializeEx(None, windows::Win32::System::Com::COINIT_MULTITHREADED);
            }
            let checker = Spell::new();
            let (lock, cv) = &*s;
            loop {
                let job = {
                    let mut g = lock.lock().unwrap();
                    loop {
                        if g.quit {
                            return;
                        }
                        if !g.queue.is_empty() {
                            let j = g.queue.remove(0);
                            g.busy = Some((j.doc, j.version, j.line));
                            break j;
                        }
                        g = cv.wait(g).unwrap();
                    }
                };
                let misses = checker.as_ref().map(|c| c.check(&job.text)).unwrap_or_default();
                let post = {
                    let mut g = lock.lock().unwrap();
                    g.busy = None;
                    g.done.push(Done { doc: job.doc, version: job.version, line: job.line, misses });
                    !std::mem::replace(&mut g.posted, true)
                };
                if post {
                    unsafe {
                        let h = windows::Win32::Foundation::HWND(hwnd as _);
                        let _ = windows::Win32::UI::WindowsAndMessaging::PostMessageW(Some(h), WM_SPELLED, Default::default(), Default::default());
                    }
                }
            }
        });
        Worker { shared }
    }

    /// The lines to check, in order, replacing any not started yet. A line being checked now is left out.
    pub fn want(&self, mut jobs: Vec<Job>) {
        let (lock, cv) = &*self.shared;
        let mut g = lock.lock().unwrap();
        if let Some(b) = g.busy {
            jobs.retain(|j| (j.doc, j.version, j.line) != b);
        }
        g.queue = jobs;
        if !g.queue.is_empty() {
            cv.notify_one();
        }
    }

    /// The lines checked since the last call.
    pub fn take(&self) -> Vec<Done> {
        let mut g = self.shared.0.lock().unwrap();
        g.posted = false;
        std::mem::take(&mut g.done)
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        let (lock, cv) = &*self.shared;
        lock.lock().unwrap().quit = true;
        cv.notify_one();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_a_misspelling_and_suggests() {
        unsafe {
            let _ = windows::Win32::System::Com::CoInitializeEx(None, windows::Win32::System::Com::COINIT_MULTITHREADED);
        }
        let Some(s) = Spell::new() else { return };
        let text: Vec<u16> = "This sentense is fine.".encode_utf16().collect();
        let m = s.check(&text);
        assert_eq!(m.len(), 1, "{m:?}");
        assert_eq!((m[0].start, m[0].end), (5, 13));
        assert!(s.suggest("sentense", 5).iter().any(|w| w == "sentence"));
    }

    #[test]
    fn worker_checks_off_thread() {
        unsafe {
            let _ = windows::Win32::System::Com::CoInitializeEx(None, windows::Win32::System::Com::COINIT_MULTITHREADED);
        }
        let w = Worker::start(windows::Win32::Foundation::HWND::default());
        let text: Vec<u16> = "This sentense is fine.".encode_utf16().collect();
        w.want(vec![Job { doc: 1, version: 2, line: 3, text }]);
        let t = std::time::Instant::now();
        let done = loop {
            let d = w.take();
            if !d.is_empty() || t.elapsed().as_secs() > 10 {
                break d;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        };
        assert_eq!(done.len(), 1);
        assert_eq!((done[0].doc, done[0].version, done[0].line), (1, 2, 3));
        // No checker for the language on this machine: nothing found, but still answered.
        if Spell::new().is_some() {
            assert_eq!(done[0].misses.len(), 1);
        }
    }
}
