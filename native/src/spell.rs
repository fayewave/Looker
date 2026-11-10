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
}
