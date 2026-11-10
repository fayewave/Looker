//! Subtitles (native/PLAN.md N6e): which there are for a video (its own streams, and sidecar files beside it), a
//! reader thread per track that keeps the events around the playhead, and the parts that are plain logic: the
//! text's character encoding, sidecar names, ASS override tags, and which events show at a moment.
//!
//! A track is read on its own (looker_video.c's LvSubs: a demuxer of its own on the file), not by the player:
//! it reads ahead of the pictures, and after a seek it reads again from a little before, so an event that began
//! before the seek point still shows. Text sidecars are read whole at once. Times are the player's clock (the
//! video stream's start is 0); the UI asks for the playhead minus the delay.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::Duration;

use crate::imaging::video::{self, SubEvent, SubInfo, SubRead, SubRect, Subs, SubsInterrupter};

/// Sidecar extensions, lower case. `.sub` is VobSub's pictures beside an `.idx` of the same name, else a text
/// format (MicroDVD, SubViewer...).
pub const SIDECAR_EXTENSIONS: [&str; 9] = ["srt", "ass", "ssa", "vtt", "sub", "idx", "smi", "sami", "mpl"];
/// Subfolders a sidecar may be in (VLC looks there too).
const SIDECAR_DIRS: [&str; 3] = ["subs", "subtitles", "sub"];

// --- Character encodings -------------------------------------------------------------------------------------

/// A text subtitle file's bytes as text: UTF-8/16/32 by their BOMs, UTF-8 when it's valid UTF-8, UTF-16 without a
/// BOM when every other byte is zero, else a Windows code page: the one the file name's language implies
/// (`movie.ru.srt`: 1251), else the one that reads it best (`detect_code_page`).
pub fn decode_text(bytes: &[u8], lang: Option<&str>) -> String {
    if let Some(rest) = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]) {
        return String::from_utf8_lossy(rest).into_owned();
    }
    if bytes.starts_with(&[0xFF, 0xFE, 0, 0]) {
        return utf32(&bytes[4..], false);
    }
    if bytes.starts_with(&[0, 0, 0xFE, 0xFF]) {
        return utf32(&bytes[4..], true);
    }
    if let Some(rest) = bytes.strip_prefix(&[0xFF, 0xFE]) {
        return utf16(rest, false);
    }
    if let Some(rest) = bytes.strip_prefix(&[0xFE, 0xFF]) {
        return utf16(rest, true);
    }
    // Before UTF-8: UTF-16 of ASCII text is valid UTF-8 (with a NUL every other byte).
    if let Some(be) = bomless_utf16(bytes) {
        return utf16(bytes, be);
    }
    if let Ok(s) = std::str::from_utf8(bytes) {
        return s.to_string();
    }
    let cp = lang.and_then(code_page_for_language).unwrap_or_else(|| detect_code_page(bytes));
    decode_code_page(bytes, cp).unwrap_or_else(|| String::from_utf8_lossy(bytes).into_owned())
}

fn utf16(b: &[u8], be: bool) -> String {
    let units: Vec<u16> = b.chunks_exact(2).map(|c| if be { u16::from_be_bytes([c[0], c[1]]) } else { u16::from_le_bytes([c[0], c[1]]) }).collect();
    String::from_utf16_lossy(&units)
}

fn utf32(b: &[u8], be: bool) -> String {
    b.chunks_exact(4)
        .map(|c| {
            let v = if be { u32::from_be_bytes([c[0], c[1], c[2], c[3]]) } else { u32::from_le_bytes([c[0], c[1], c[2], c[3]]) };
            char::from_u32(v).unwrap_or('\u{FFFD}')
        })
        .collect()
}

/// UTF-16 without a BOM: in the first 4 KB, zero bytes at nearly all the odd places (little-endian) or the even
/// ones (big-endian) and hardly any at the others. Some(true) for big-endian.
fn bomless_utf16(b: &[u8]) -> Option<bool> {
    let n = b.len().min(4096) & !1;
    if n < 8 {
        return None;
    }
    let pairs = n / 2;
    let (mut even, mut odd) = (0usize, 0usize);
    for i in (0..n).step_by(2) {
        even += (b[i] == 0) as usize;
        odd += (b[i + 1] == 0) as usize;
    }
    if odd * 10 >= pairs * 7 && even * 10 <= pairs {
        Some(false)
    } else if even * 10 >= pairs * 7 && odd * 10 <= pairs {
        Some(true)
    } else {
        None
    }
}

/// The Windows code page a language's old subtitles were usually saved in (ISO 639-1 or -2 tags).
pub fn code_page_for_language(tag: &str) -> Option<u32> {
    let t = tag.to_ascii_lowercase();
    let t = t.split(['-', '_']).next().unwrap_or("");
    Some(match t {
        "ru" | "rus" | "uk" | "ukr" | "bg" | "bul" | "sr" | "srp" | "mk" | "mac" | "mkd" | "be" | "bel" => 1251,
        "pl" | "pol" | "cs" | "cze" | "ces" | "sk" | "slo" | "slk" | "hu" | "hun" | "ro" | "rum" | "ron" | "hr" | "hrv" | "sl" | "slv" | "bs" | "bos" | "sq" | "alb" | "sqi" => 1250,
        "el" | "gre" | "ell" => 1253,
        "tr" | "tur" => 1254,
        "he" | "heb" => 1255,
        "ar" | "ara" | "fa" | "per" | "fas" | "ur" | "urd" => 1256,
        "et" | "est" | "lv" | "lav" | "lt" | "lit" => 1257,
        "vi" | "vie" => 1258,
        "th" | "tha" => 874,
        "ja" | "jpn" => 932,
        "zh" | "chi" | "zho" | "chs" => 936,
        "cht" => 950,
        "ko" | "kor" => 949,
        "en" | "eng" | "fr" | "fre" | "fra" | "de" | "ger" | "deu" | "es" | "spa" | "it" | "ita" | "pt" | "por" | "nl" | "dut" | "nld" | "sv" | "swe" | "da" | "dan" | "no" | "nor" | "nb" | "nob" | "fi" | "fin" | "id" | "ind" => 1252,
        _ => return None,
    })
}

/// The bytes through Windows' converter for code page `cp`; None if it refuses them.
pub fn decode_code_page(bytes: &[u8], cp: u32) -> Option<String> {
    use windows::Win32::Globalization::{MULTI_BYTE_TO_WIDE_CHAR_FLAGS, MultiByteToWideChar};
    if bytes.is_empty() {
        return Some(String::new());
    }
    unsafe {
        let n = MultiByteToWideChar(cp, MULTI_BYTE_TO_WIDE_CHAR_FLAGS(0), bytes, None);
        if n <= 0 {
            return None;
        }
        let mut w = vec![0u16; n as usize];
        let n = MultiByteToWideChar(cp, MULTI_BYTE_TO_WIDE_CHAR_FLAGS(0), bytes, Some(&mut w));
        if n <= 0 {
            return None;
        }
        w.truncate(n as usize);
        Some(String::from_utf16_lossy(&w))
    }
}

/// Whether the bytes are valid in a double-byte code page (MB_ERR_INVALID_CHARS).
fn valid_in(bytes: &[u8], cp: u32) -> bool {
    use windows::Win32::Globalization::{MB_ERR_INVALID_CHARS, MultiByteToWideChar};
    unsafe { MultiByteToWideChar(cp, MB_ERR_INVALID_CHARS, bytes, None) > 0 }
}

/// The code page that reads non-UTF-8 text best. Text that is mostly bytes past ASCII is a non-Latin script: the
/// double-byte pages (Shift-JIS, GBK, Big5, Korean) where the bytes are valid in them and read as that script, else
/// the single-byte page whose script's commonest letters the bytes are (Cyrillic, Greek, Hebrew, Arabic). Latin text
/// with a few accented letters: the page under which bytes inside words read as letters (Central European 1250,
/// Turkish 1254, Baltic 1257), with Western 1252 winning ties.
pub fn detect_code_page(bytes: &[u8]) -> u32 {
    let sample = &bytes[..bytes.len().min(64 * 1024)];
    let high = sample.iter().filter(|&&b| b >= 0x80).count();
    let letters = sample.iter().filter(|b| b.is_ascii_alphabetic()).count();
    if high == 0 {
        return 1252;
    }
    if high > letters {
        // Mostly not ASCII: a non-Latin script.
        let mut best = (0.0f64, 1251u32);
        for cp in [932u32, 936, 949, 950] {
            if !valid_in(sample, cp) {
                continue;
            }
            let Some(s) = decode_code_page(sample, cp) else { continue };
            let total = s.chars().filter(|c| !c.is_ascii()).count().max(1);
            let script = s.chars().filter(|&c| in_script(c, cp)).count();
            let score = script as f64 / total as f64;
            if score > best.0 {
                best = (score, cp);
            }
        }
        if best.0 > 0.9 {
            return best.1;
        }
        let mut best = (-1.0f64, 1251u32);
        for (cp, common) in [(1251u32, "оеаинтсрвлкмдпу"), (1253, "αοιετσνηκρυπμλ"), (1255, "יוהלרמתאבנשעכד"), (1256, "اليمونرتبعهسدفك")] {
            let Some(s) = decode_code_page(sample, cp) else { continue };
            let total = s.chars().filter(|c| !c.is_ascii()).count().max(1);
            let hits = s.chars().filter(|&c| common.contains(c)).count();
            let score = hits as f64 / total as f64;
            if score > best.0 {
                best = (score, cp);
            }
        }
        return best.1;
    }
    // Latin with some accents: count high bytes inside words that read as letters.
    let mut best = (0usize, 1252u32);
    for cp in [1252u32, 1250, 1254, 1257] {
        let Some(s) = decode_code_page(sample, cp) else { continue };
        let chars: Vec<char> = s.chars().collect();
        let mut score = 0;
        for i in 0..chars.len() {
            let c = chars[i];
            if c.is_ascii() {
                continue;
            }
            let before = i > 0 && chars[i - 1].is_alphabetic();
            let after = i + 1 < chars.len() && chars[i + 1].is_alphabetic();
            if (before || after) && c.is_alphabetic() {
                score += 1;
            }
        }
        if score > best.0 {
            best = (score, cp);
        }
    }
    best.1
}

/// Whether `c` is in the script code page `cp` is for (kana and kanji, hanzi, hangul).
fn in_script(c: char, cp: u32) -> bool {
    let u = c as u32;
    let cjk = (0x4E00..=0x9FFF).contains(&u) || (0x3000..=0x303F).contains(&u) || (0xFF00..=0xFFEF).contains(&u);
    match cp {
        932 => cjk || (0x3040..=0x30FF).contains(&u),
        949 => (0xAC00..=0xD7AF).contains(&u) || (0x3130..=0x318F).contains(&u),
        _ => cjk,
    }
}

// --- Sidecar files -------------------------------------------------------------------------------------------

/// A subtitle file beside a video.
#[derive(Clone, Debug, PartialEq)]
pub struct Sidecar {
    pub path: PathBuf,
    /// The language tag in its name ("en" in `movie.en.srt`), or "".
    pub lang: String,
    pub forced: bool,
    pub sdh: bool,
    /// Lower-case extension.
    pub ext: String,
}

impl Sidecar {
    /// How the Subtitles menu names it: its language when its name says one, and its file name
    /// ("English · Movie.en.srt").
    pub fn label(&self) -> String {
        let name = self.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let mut lang = if self.lang.is_empty() { String::new() } else { video::language_name(&self.lang) };
        if !lang.is_empty() && self.forced {
            lang.push_str(", forced");
        }
        if !lang.is_empty() && self.sdh {
            lang.push_str(", SDH");
        }
        if lang.is_empty() { name } else { format!("{lang} \u{b7} {name}") }
    }
}

/// Whether `file` (a file name) is a sidecar of a video named `stem`: the same name with a subtitle extension,
/// optionally with tags between ("Movie.en.srt", "Movie.eng.forced.srt", "Movie.English.SDH.srt", "Movie - en.srt").
/// (language, forced, sdh, extension) when it is.
pub fn sidecar_name(stem: &str, file: &str) -> Option<(String, bool, bool, String)> {
    let (base, ext) = file.rsplit_once('.')?;
    let ext = ext.to_ascii_lowercase();
    if !SIDECAR_EXTENSIONS.contains(&ext.as_str()) {
        return None;
    }
    let (bl, sl) = (base.to_lowercase(), stem.to_lowercase());
    let rest = bl.strip_prefix(&sl)?;
    // Right after the stem: nothing, or a separator (so "Movie2.srt" isn't "Movie"'s), and not a number after a
    // space ("Movie 2.srt" is the sequel's).
    if !(rest.is_empty() || rest.starts_with(['.', '_', '-', ' ', '['])) {
        return None;
    }
    if rest.starts_with(' ') && rest.trim_start().split(['.', ' ']).next().is_some_and(|t| !t.is_empty() && t.chars().all(|c| c.is_ascii_digit())) {
        return None;
    }
    let (mut lang, mut forced, mut sdh) = (String::new(), false, false);
    for tag in rest.split(['.', '_', '-', ' ', '[', ']', '(', ')']).filter(|t| !t.is_empty()) {
        match tag {
            "forced" | "foreign" => forced = true,
            "sdh" | "cc" | "hi" => sdh = true,
            t if lang.is_empty() && language_tag(t).is_some() => lang = language_tag(t).unwrap().to_string(),
            _ => {}
        }
    }
    Some((lang, forced, sdh, ext))
}

/// A language as file names write it ("en", "eng", "en-US", "English", "pt-BR") as a tag `language_name` knows;
/// None when it isn't one.
fn language_tag(t: &str) -> Option<&'static str> {
    const NAMES: [(&str, &str); 24] = [
        ("english", "en"),
        ("french", "fr"),
        ("german", "de"),
        ("spanish", "es"),
        ("italian", "it"),
        ("portuguese", "pt"),
        ("russian", "ru"),
        ("japanese", "ja"),
        ("chinese", "zh"),
        ("korean", "ko"),
        ("dutch", "nl"),
        ("swedish", "sv"),
        ("polish", "pl"),
        ("turkish", "tr"),
        ("arabic", "ar"),
        ("hebrew", "he"),
        ("greek", "el"),
        ("czech", "cs"),
        ("hungarian", "hu"),
        ("danish", "da"),
        ("finnish", "fi"),
        ("norwegian", "no"),
        ("romanian", "ro"),
        ("ukrainian", "uk"),
    ];
    if let Some((_, tag)) = NAMES.iter().find(|(n, _)| *n == t) {
        return Some(tag);
    }
    const TAGS: [&str; 86] = [
        "en", "eng", "fr", "fre", "fra", "de", "ger", "deu", "es", "spa", "it", "ita", "pt", "por", "ru", "rus", "ja", "jpn", "zh", "chi", "zho", "ko", "kor", "nl", "dut",
        "nld", "sv", "swe", "pl", "pol", "tr", "tur", "ar", "ara", "he", "heb", "el", "gre", "ell", "cs", "cze", "ces", "hu", "hun", "da", "dan", "fi", "fin", "no", "nor",
        "nb", "nob", "ro", "rum", "ron", "uk", "ukr", "bg", "bul", "hr", "hrv", "sr", "srp", "sl", "slv", "sk", "slk", "th", "tha", "vi", "vie", "id", "ind", "fa", "per",
        "hi", "hin", "ms", "may", "et", "est", "lv", "lav", "lt", "lit", "chs",
    ];
    let base = t.split(['-', '_']).next().unwrap_or(t);
    // "hi" is Hindi as a tag but "hearing impaired" as a flag: sidecar_name tests the flag first.
    TAGS.iter().find(|&&x| x == base).copied()
}

/// The sidecar files of `video`: beside it, and in a Subs or Subtitles folder beside it. Exact-name files first,
/// then by name; a VobSub `.sub` beside its `.idx` isn't listed on its own.
pub fn find_sidecars(video: &Path) -> Vec<Sidecar> {
    let Some(stem) = video.file_stem().map(|s| s.to_string_lossy().into_owned()) else { return Vec::new() };
    let Some(dir) = video.parent() else { return Vec::new() };
    let mut dirs = vec![dir.to_path_buf()];
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().to_lowercase();
            if SIDECAR_DIRS.contains(&name.as_str()) && e.file_type().is_ok_and(|t| t.is_dir()) {
                dirs.push(e.path());
            }
        }
    }
    let mut out = Vec::new();
    for d in dirs {
        let Ok(rd) = std::fs::read_dir(&d) else { continue };
        let mut names: Vec<String> = rd.flatten().filter(|e| e.file_type().is_ok_and(|t| t.is_file())).map(|e| e.file_name().to_string_lossy().into_owned()).collect();
        names.sort_by_key(|n| n.to_lowercase());
        out.extend(sidecars_among(&stem, &names).into_iter().map(|(name, (lang, forced, sdh, ext))| Sidecar { path: d.join(name), lang, forced, sdh, ext }));
    }
    out
}

/// `find_sidecars` on a list of file names: the sidecars of `stem` among them, in order.
fn sidecars_among(stem: &str, names: &[String]) -> Vec<(String, (String, bool, bool, String))> {
    let mut found: Vec<(String, (String, bool, bool, String))> = names.iter().filter_map(|n| sidecar_name(stem, n).map(|s| (n.clone(), s))).collect();
    // VobSub: the .idx stands for the pair.
    let idx: HashSet<String> = found.iter().filter(|(_, s)| s.3 == "idx").map(|(n, _)| n[..n.len() - 4].to_lowercase()).collect();
    found.retain(|(n, s)| !(s.3 == "sub" && idx.contains(&n[..n.len() - 4].to_lowercase())));
    // The exact name first.
    found.sort_by_key(|(n, _)| n.rsplit_once('.').map(|(b, _)| b.to_lowercase()) != Some(stem.to_lowercase()));
    found
}

// --- ASS override tags ---------------------------------------------------------------------------------------

/// A stretch of text in one style.
#[derive(Clone, Debug, PartialEq)]
pub struct Run {
    pub text: String,
    pub italic: bool,
    pub bold: bool,
    pub underline: bool,
    pub strike: bool,
    /// 0xRRGGBB, None: the default (white).
    pub color: Option<u32>,
}

/// An event's text laid out as lines of runs, and where it goes: ASS's numpad alignment (1-3 bottom, 4-6 middle,
/// 7-9 top; 2, bottom centre, unless the text says otherwise).
#[derive(Clone, Debug, PartialEq)]
pub struct Styled {
    pub lines: Vec<Vec<Run>>,
    pub align: u8,
}

impl Styled {
    /// The text without styles, lines joined by '\n'.
    pub fn plain(&self) -> String {
        self.lines.iter().map(|l| l.iter().map(|r| r.text.as_str()).collect::<String>()).collect::<Vec<_>>().join("\n")
    }
}

/// ASS-tagged text as FFmpeg's decoders give every text format (SubRip's `<i>` is `{\i1}`): \N and \n break lines,
/// \h is a no-break space; \i \b \u \s \c (\1c) style what follows, \r resets, \an and \a place it; drawings (\p1)
/// are dropped, as are tags that don't apply to plain text (\pos, \fad, \k...). Leftover HTML-ish tags (<font>,
/// <i>) are honoured too.
pub fn parse_ass(text: &str) -> Styled {
    #[derive(Clone, Copy, Default)]
    struct St {
        italic: bool,
        bold: bool,
        underline: bool,
        strike: bool,
        color: Option<u32>,
    }
    let mut lines: Vec<Vec<Run>> = vec![Vec::new()];
    let mut st = St::default();
    let mut align = 2u8;
    let mut drawing = false;
    let mut buf = String::new();
    let flush = |lines: &mut Vec<Vec<Run>>, buf: &mut String, st: &St| {
        if buf.is_empty() {
            return;
        }
        let line = lines.last_mut().unwrap();
        if let Some(last) = line.last_mut().filter(|r| r.italic == st.italic && r.bold == st.bold && r.underline == st.underline && r.strike == st.strike && r.color == st.color) {
            last.text.push_str(buf);
        } else {
            line.push(Run { text: std::mem::take(buf), italic: st.italic, bold: st.bold, underline: st.underline, strike: st.strike, color: st.color });
        }
        buf.clear();
    };
    let chars: Vec<char> = text.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == '{' {
            if let Some(end) = chars[i..].iter().position(|&c| c == '}') {
                flush(&mut lines, &mut buf, &st);
                let block: String = chars[i + 1..i + end].iter().collect();
                for tag in block.split('\\').skip(1) {
                    apply_tag(tag.trim(), &mut st.italic, &mut st.bold, &mut st.underline, &mut st.strike, &mut st.color, &mut align, &mut drawing);
                }
                i += end + 1;
                continue;
            }
        }
        if c == '<' {
            if let Some(end) = chars[i..].iter().position(|&c| c == '>') {
                let tag: String = chars[i + 1..i + end].iter().collect::<String>().to_ascii_lowercase();
                // Tried on a copy: what came before the tag keeps the style it had.
                let mut next = st;
                if html_tag(&tag, &mut next.italic, &mut next.bold, &mut next.underline, &mut next.strike, &mut next.color) {
                    flush(&mut lines, &mut buf, &st);
                    st = next;
                    i += end + 1;
                    continue;
                }
                // Not a tag after all ("<3"): it's text.
            }
        }
        if c == '\\' && i + 1 < chars.len() {
            match chars[i + 1] {
                'N' | 'n' => {
                    flush(&mut lines, &mut buf, &st);
                    lines.push(Vec::new());
                    i += 2;
                    continue;
                }
                'h' => {
                    if !drawing {
                        buf.push('\u{a0}');
                    }
                    i += 2;
                    continue;
                }
                _ => {}
            }
        }
        if c == '\r' {
            i += 1;
            continue;
        }
        if c == '\n' {
            flush(&mut lines, &mut buf, &st);
            lines.push(Vec::new());
            i += 1;
            continue;
        }
        if !drawing {
            buf.push(c);
        }
        i += 1;
    }
    flush(&mut lines, &mut buf, &st);
    // Blank lines at either end aren't text (a trailing \N).
    while lines.len() > 1 && lines.last().is_some_and(|l| l.iter().all(|r| r.text.trim().is_empty())) {
        lines.pop();
    }
    while lines.len() > 1 && lines[0].iter().all(|r| r.text.trim().is_empty()) {
        lines.remove(0);
    }
    Styled { lines, align }
}

#[allow(clippy::too_many_arguments)]
fn apply_tag(tag: &str, italic: &mut bool, bold: &mut bool, underline: &mut bool, strike: &mut bool, color: &mut Option<u32>, align: &mut u8, drawing: &mut bool) {
    let on = |v: &str| v.trim().parse::<i32>().map_or(true, |n| n != 0);
    if let Some(v) = tag.strip_prefix("an") {
        if let Ok(n) = v.trim().parse::<u8>() {
            if (1..=9).contains(&n) {
                *align = n;
            }
        }
    } else if let Some(v) = tag.strip_prefix('a').filter(|v| v.chars().all(|c| c.is_ascii_digit()) && !v.is_empty()) {
        // SSA's legacy \a: 1-3 bottom, +4 top, +8 middle.
        if let Ok(n) = v.parse::<u8>() {
            let col = (n & 3).clamp(1, 3);
            *align = if n & 4 != 0 { 6 + col } else if n & 8 != 0 { 3 + col } else { col };
        }
    } else if let Some(v) = tag.strip_prefix("1c").or_else(|| tag.strip_prefix('c').filter(|v| v.starts_with('&') || v.is_empty())) {
        *color = parse_ass_color(v);
    } else if tag.starts_with("2c") || tag.starts_with("3c") || tag.starts_with("4c") {
        // Karaoke, outline and shadow colours: Looker draws its own outline.
    } else if let Some(v) = tag.strip_prefix('p').filter(|v| v.chars().all(|c| c.is_ascii_digit()) && !v.is_empty()) {
        *drawing = v.parse::<i32>().unwrap_or(0) > 0;
    } else if let Some(v) = tag.strip_prefix('i').filter(|v| v.chars().all(|c| c.is_ascii_digit())) {
        *italic = on(v);
    } else if let Some(v) = tag.strip_prefix('b').filter(|v| v.chars().all(|c| c.is_ascii_digit())) {
        // \b1, or a weight: 700 and up is bold.
        *bold = v.parse::<i32>().map_or(true, |n| n == 1 || n >= 600);
    } else if let Some(v) = tag.strip_prefix('u').filter(|v| v.chars().all(|c| c.is_ascii_digit())) {
        *underline = on(v);
    } else if let Some(v) = tag.strip_prefix('s').filter(|v| v.chars().all(|c| c.is_ascii_digit())) {
        *strike = on(v);
    } else if tag == "r" || tag.starts_with('r') && !tag.starts_with("ri") {
        *italic = false;
        *bold = false;
        *underline = false;
        *strike = false;
        *color = None;
    }
}

/// `&HBBGGRR&` (or with alpha, `&HAABBGGRR&`) as 0xRRGGBB; None when there's no value (\c alone resets).
fn parse_ass_color(v: &str) -> Option<u32> {
    let hex: String = v.trim_matches(|c| c == '&' || c == 'H' || c == 'h' || c == ' ').chars().take_while(|c| c.is_ascii_hexdigit()).collect();
    if hex.is_empty() {
        return None;
    }
    let bgr = u32::from_str_radix(&hex, 16).ok()? & 0xFF_FFFF;
    Some(((bgr & 0xFF) << 16) | (bgr & 0xFF00) | (bgr >> 16))
}

/// An HTML-ish tag a subtitle file left in (`i`, `/b`, `font color="#ff0000"`): applied, and true; false when
/// it's none of those (the '<' is text then).
fn html_tag(tag: &str, italic: &mut bool, bold: &mut bool, underline: &mut bool, strike: &mut bool, color: &mut Option<u32>) -> bool {
    let t = tag.trim();
    match t {
        "i" | "em" => *italic = true,
        "/i" | "/em" => *italic = false,
        "b" | "strong" => *bold = true,
        "/b" | "/strong" => *bold = false,
        "u" => *underline = true,
        "/u" => *underline = false,
        "s" | "strike" => *strike = true,
        "/s" | "/strike" => *strike = false,
        "/font" => *color = None,
        "br" | "br/" | "br /" => {}
        _ if t.starts_with("font") => {
            if let Some(i) = t.find("color") {
                let v = t[i + 5..].trim_start_matches([' ', '=', '"', '\'']);
                let hex: String = v.trim_start_matches('#').chars().take_while(|c| c.is_ascii_hexdigit()).collect();
                if hex.len() == 6 {
                    *color = u32::from_str_radix(&hex, 16).ok();
                } else if let Some(c) = named_color(v.split(['"', '\'', ' ']).next().unwrap_or("")) {
                    *color = Some(c);
                }
            }
        }
        _ => return false,
    }
    true
}

fn named_color(n: &str) -> Option<u32> {
    Some(match n {
        "white" => 0xFFFFFF,
        "yellow" => 0xFFFF00,
        "red" => 0xFF0000,
        "green" => 0x00FF00,
        "lime" => 0x00FF00,
        "blue" => 0x0000FF,
        "cyan" | "aqua" => 0x00FFFF,
        "magenta" | "fuchsia" => 0xFF00FF,
        "black" => 0x000000,
        "gray" | "grey" => 0x808080,
        "silver" => 0xC0C0C0,
        "orange" => 0xFFA500,
        _ => return None,
    })
}

// --- Events and when they show -------------------------------------------------------------------------------

/// A text event with no end shows this long at most (or until the next one).
const OPEN_TEXT_US: i64 = 10_000_000;

/// One event as kept.
#[derive(Clone)]
pub struct Event {
    /// Unique in its store: what the UI's caches key on.
    pub id: u64,
    pub start_us: i64,
    pub end_us: Option<i64>,
    /// Text events: their text, styled. ASS events have none here (libass draws them).
    pub styled: Option<Arc<Styled>>,
    pub rects: Vec<SubRect>,
    pub canvas: (i32, i32),
    /// An ASS event: libass has it, this keeps its times.
    pub ass: bool,
}

impl Event {
    /// Nothing to show: a bitmap stream's clear.
    pub fn is_empty(&self) -> bool {
        !self.ass && self.rects.is_empty() && self.styled.as_ref().is_none_or(|s| s.plain().trim().is_empty())
    }
}

/// The events read so far, by start time.
#[derive(Default)]
pub struct Store {
    events: Vec<Event>,
    keys: HashSet<(i64, i64, u64)>,
    next_id: u64,
    /// ASS: its events' times only (to know when something shows).
    ass: bool,
}

fn hash_of(s: &str, rects: &[SubRect]) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    s.hash(&mut h);
    for r in rects {
        (r.x, r.y, r.w, r.h).hash(&mut h);
    }
    h.finish()
}

impl Store {
    pub fn new(ass: bool) -> Store {
        Store { ass, ..Default::default() }
    }

    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.events.len()
    }

    /// Adds an event read; false when it's one already kept (read again after a seek).
    pub fn insert(&mut self, e: SubEvent) -> bool {
        let key_text = if self.ass { e.read_order.to_string() + &e.text } else { e.text.clone() };
        let key = (e.start_us, e.end_us.unwrap_or(-1), hash_of(&key_text, &e.rects));
        if !self.keys.insert(key) {
            return false;
        }
        let styled = (!self.ass && !e.text.is_empty()).then(|| Arc::new(parse_ass(&e.text)));
        let ev = Event { id: self.next_id, start_us: e.start_us, end_us: e.end_us, styled, rects: e.rects, canvas: e.canvas, ass: self.ass };
        self.next_id += 1;
        let at = self.events.partition_point(|x| x.start_us <= ev.start_us);
        self.events.insert(at, ev);
        true
    }

    /// When the event at `i` stops showing: its end, else the next event's start (later than its own), else (a
    /// text event) a while after it starts, else never (yet).
    fn end_of(&self, i: usize) -> i64 {
        let e = &self.events[i];
        if let Some(end) = e.end_us {
            return end;
        }
        let next = self.events[i + 1..].iter().find(|n| n.start_us > e.start_us).map(|n| n.start_us);
        let text = e.rects.is_empty() && (e.styled.is_some() || e.ass);
        match next {
            Some(n) if text => n.min(e.start_us + OPEN_TEXT_US),
            Some(n) => n,
            None if text => e.start_us + OPEN_TEXT_US,
            None => i64::MAX,
        }
    }

    /// The events showing at `t`, oldest first; a clear (an empty event) shows nothing.
    pub fn active(&self, t: i64) -> Vec<Event> {
        let n = self.events.partition_point(|e| e.start_us <= t);
        (0..n).filter(|&i| t < self.end_of(i) && !self.events[i].is_empty()).map(|i| self.events[i].clone()).collect()
    }

    /// Whether anything shows at `t` (for ASS: whether libass needs asking).
    pub fn any_active(&self, t: i64) -> bool {
        let n = self.events.partition_point(|e| e.start_us <= t);
        (0..n).rev().any(|i| t < self.end_of(i) && !self.events[i].is_empty())
    }

    /// Drops pictures that ended before `from` or start after `to` (bitmap streams: their pixels add up). Text is
    /// kept: it's small.
    pub fn evict(&mut self, from: i64, to: i64) {
        if self.events.iter().all(|e| e.rects.is_empty()) {
            return;
        }
        let ends: Vec<i64> = (0..self.events.len()).map(|i| self.end_of(i)).collect();
        let mut i = 0;
        let keys = &mut self.keys;
        self.events.retain(|e| {
            let end = ends[i];
            i += 1;
            let keep = e.rects.is_empty() || (end >= from && e.start_us <= to);
            if !keep {
                keys.retain(|k| k.0 != e.start_us);
            }
            keep
        });
    }
}

// --- The reader ----------------------------------------------------------------------------------------------

/// Where a track comes from.
#[derive(Clone, Debug, PartialEq)]
pub enum Source {
    /// One of the video's own streams (`Track::stream`).
    Stream(i32),
    /// A subtitle file.
    File(PathBuf),
}

/// How far ahead of the playhead the reader keeps, and how far before a seek point it starts reading again.
const AHEAD_US: i64 = 30_000_000;
const PREROLL_US: i64 = 15_000_000;
/// Pictures kept behind the playhead.
const BEHIND_US: i64 = 30_000_000;
/// The playhead moving by more than this (or back) between two looks is a seek.
const JUMP_US: i64 = 3_000_000;

struct ReaderState {
    store: Store,
    /// The playhead (delay applied) the UI last gave.
    want: i64,
    last_want: Option<i64>,
    /// A seek for the reader: where to.
    seek_to: Option<i64>,
    /// The UI stopped the read in progress for it: the reader must seek (the shim's flag stays up till then).
    interrupted: bool,
    /// What's been read: from where reading last began to the last packet's time.
    from: i64,
    to: i64,
    eof: bool,
    quit: bool,
    info: Option<Result<SubInfo, String>>,
}

struct Shared {
    state: Mutex<ReaderState>,
    wake: Condvar,
    interrupter: Mutex<Option<SubsInterrupter>>,
    subs: Mutex<Option<Arc<Subs>>>,
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, ReaderState> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// A subtitle track being read, for as long as it's chosen.
pub struct Track {
    shared: Arc<Shared>,
    pub source: Source,
}

impl Track {
    /// Starts reading `source` of the video at `video` (a stream of its own, or a file) on its own thread.
    /// `zero_us`: the player's `start_us`; `fps`: the video's frame rate (MicroDVD counts frames).
    pub fn open(video: &Path, source: Source, zero_us: i64, fps: f64) -> Track {
        let shared = Arc::new(Shared {
            state: Mutex::new(ReaderState { store: Store::default(), want: 0, last_want: None, seek_to: None, interrupted: false, from: 0, to: i64::MIN, eof: false, quit: false, info: None }),
            wake: Condvar::new(),
            interrupter: Mutex::new(None),
            subs: Mutex::new(None),
        });
        let (s, v, src) = (shared.clone(), video.to_path_buf(), source.clone());
        std::thread::Builder::new().name("subtitles".into()).spawn(move || reader(s, &v, src, zero_us, fps)).ok();
        Track { shared, source }
    }

    /// What was opened, once it has been (`Err` when it couldn't be).
    pub fn info(&self) -> Option<Result<SubInfo, String>> {
        self.shared.lock().info.clone()
    }

    /// The playhead (delay applied): the reader keeps what's around it, and a jump is a seek for it.
    pub fn set_time(&self, us: i64) {
        let mut st = self.shared.lock();
        st.want = us;
        let jumped = st.last_want.is_none_or(|l| us < l - 300_000 || us > l + JUMP_US);
        st.last_want = Some(us);
        if jumped {
            let covered = st.eof && us >= st.from || (us >= st.from && us <= st.to + JUMP_US);
            if !covered {
                st.seek_to = Some(us);
                if !st.interrupted {
                    st.interrupted = true;
                    if let Some(i) = self.shared.interrupter.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
                        i.interrupt();
                    }
                }
            }
        }
        self.shared.wake.notify_all();
    }

    /// Whether what shows at `us` is known: the track is open (or failed) and has been read past `us` (or to its
    /// end). Until then the UI keeps drawing, so events that arrive while paused appear.
    pub fn settled(&self, us: i64) -> bool {
        let st = self.shared.lock();
        match &st.info {
            None => false,
            Some(Err(_)) => true,
            Some(Ok(_)) => st.seek_to.is_none() && (st.eof || st.to >= us),
        }
    }

    /// The events showing at `us`.
    pub fn active(&self, us: i64) -> Vec<Event> {
        self.shared.lock().store.active(us)
    }

    /// Whether something shows at `us`.
    pub fn any_active(&self, us: i64) -> bool {
        self.shared.lock().store.any_active(us)
    }

    /// libass's picture (ASS tracks): see `Subs::render`.
    pub fn render(&self, us: i64, w: u32, h: u32, video_w: u32, video_h: u32, scale: f64) -> Option<Option<video::SubImage>> {
        let subs = self.shared.subs.lock().unwrap_or_else(|e| e.into_inner()).clone()?;
        subs.render(us, w, h, video_w, video_h, scale).ok()?
    }
}

impl Drop for Track {
    fn drop(&mut self) {
        self.shared.lock().quit = true;
        if let Some(i) = self.shared.interrupter.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
            i.interrupt();
        }
        self.shared.wake.notify_all();
    }
}

/// Opens the source: a text file is read here and turned into UTF-8 (FFmpeg has no iconv in Looker's build).
fn open_source(video: &Path, source: &Source, zero_us: i64, fps: f64) -> Result<(Subs, SubInfo, bool), String> {
    match source {
        Source::Stream(s) => Subs::open(video, Some(*s), zero_us, fps).map(|(a, b)| (a, b, false)),
        Source::File(path) => {
            let ext = path.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
            if ext == "idx" {
                return Subs::open(path, None, 0, fps).map(|(a, b)| (a, b, false));
            }
            let bytes = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
            // A ".sub" may be VobSub's pictures without its .idx: MPEG-PS starts 00 00 01 BA.
            if bytes.starts_with(&[0, 0, 1, 0xBA]) {
                return Subs::open(path, None, 0, fps).map(|(a, b)| (a, b, false));
            }
            let stem = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
            let lang = stem.rsplit(['.', '_', '-', ' ']).find_map(language_tag);
            let text = decode_text(&bytes, lang);
            Subs::open_text(text.as_bytes(), path, fps).map(|(a, b)| (a, b, true))
        }
    }
}

fn reader(shared: Arc<Shared>, video: &Path, source: Source, zero_us: i64, fps: f64) {
    let opened = open_source(video, &source, zero_us, fps);
    let (subs, whole) = match opened {
        Ok((subs, info, whole)) => {
            crate::trace::mark(format!("subtitles: opened {:?}: {} ({} fonts)", source, info.codec(), info.fonts));
            let mut st = shared.lock();
            st.store = Store::new(info.kind == video::SUB_ASS);
            st.info = Some(Ok(info));
            let subs = Arc::new(subs);
            *shared.interrupter.lock().unwrap_or_else(|e| e.into_inner()) = Some(subs.interrupter());
            *shared.subs.lock().unwrap_or_else(|e| e.into_inner()) = Some(subs.clone());
            (subs, whole)
        }
        Err(e) => {
            crate::trace::mark(format!("subtitles: {source:?}: {e}"));
            shared.lock().info = Some(Err(e));
            return;
        }
    };
    loop {
        {
            let mut st = shared.lock();
            loop {
                if st.quit {
                    return;
                }
                if let Some(t) = st.seek_to.take() {
                    let covered = st.eof && t >= st.from || (t >= st.from && t <= st.to + JUMP_US);
                    if !whole && (st.interrupted || !covered) {
                        st.interrupted = false;
                        drop(st);
                        let from = (t - PREROLL_US).max(0);
                        let ok = subs.seek(from).is_ok();
                        st = shared.lock();
                        if ok {
                            st.from = from;
                            st.to = from;
                            st.eof = false;
                        }
                        let w = st.want;
                        st.store.evict(w - BEHIND_US, w + AHEAD_US + PREROLL_US);
                        continue;
                    }
                    st.interrupted = false;
                }
                if !st.eof && (whole || st.to < st.want + AHEAD_US) {
                    break;
                }
                st = shared.wake.wait_timeout(st, Duration::from_millis(500)).unwrap_or_else(|e| e.into_inner()).0;
            }
        }
        match subs.next() {
            Ok(SubRead::Event(e)) => {
                let mut st = shared.lock();
                st.to = st.to.max(e.read_us);
                st.store.insert(e);
                if !whole {
                    let w = st.want;
                    st.store.evict(w - BEHIND_US, w + AHEAD_US + PREROLL_US);
                }
            }
            Ok(SubRead::End) => {
                shared.lock().eof = true;
            }
            Ok(SubRead::Interrupted) => {
                // A seek (or quit) is waiting: the loop takes it.
                let mut st = shared.lock();
                if st.seek_to.is_none() && !st.quit {
                    let w = st.want;
                    st.seek_to = Some(w);
                    st.interrupted = true;
                }
            }
            Err(e) => {
                crate::trace::mark(format!("subtitles: {e}"));
                shared.lock().eof = true;
            }
        }
    }
}

// --- Choosing a track ----------------------------------------------------------------------------------------

/// What shows, as the user picked it (or Looker did).
#[derive(Clone, Debug, PartialEq)]
pub enum Choice {
    Off,
    Stream(i32),
    File(PathBuf),
}

impl Choice {
    /// As `subs.txt` keeps it.
    pub fn to_text(&self) -> String {
        match self {
            Choice::Off => "off".into(),
            Choice::Stream(s) => format!("s:{s}"),
            Choice::File(p) => format!("f:{}", p.display()),
        }
    }

    pub fn from_text(t: &str) -> Option<Choice> {
        if t == "off" {
            return Some(Choice::Off);
        }
        if let Some(s) = t.strip_prefix("s:") {
            return s.parse().ok().map(Choice::Stream);
        }
        t.strip_prefix("f:").map(|p| Choice::File(PathBuf::from(p)))
    }
}

/// An embedded track as `pick_default` weighs it: (stream, language tag, default, forced).
pub type StreamFacts = (i32, String, bool, bool);

/// What shows when nothing was picked for this file: a sidecar named after it (the one in `ui_lang` first; not a
/// forced-only one); else the file's forced track (in the audio's language first); else the track it marks
/// default; else nothing.
pub fn pick_default(streams: &[StreamFacts], sidecars: &[Sidecar], audio_lang: &str, ui_lang: &str) -> Choice {
    let same = |a: &str, b: &str| !a.is_empty() && !b.is_empty() && (a.eq_ignore_ascii_case(b) || video::language_name(a) == video::language_name(b));
    let full: Vec<&Sidecar> = sidecars.iter().filter(|s| !s.forced).collect();
    if let Some(s) = full.iter().find(|s| same(&s.lang, ui_lang)).or(full.first()) {
        return Choice::File(s.path.clone());
    }
    let forced: Vec<&StreamFacts> = streams.iter().filter(|s| s.3).collect();
    if let Some(s) = forced.iter().find(|s| same(&s.1, audio_lang)).or(forced.first()) {
        return Choice::Stream(s.0);
    }
    if let Some(s) = streams.iter().find(|s| s.2) {
        return Choice::Stream(s.0);
    }
    Choice::Off
}

/// The Windows display language's tag ("en" from "en-GB").
pub fn ui_language() -> String {
    use windows::Win32::Globalization::GetUserDefaultLocaleName;
    let mut buf = [0u16; 85];
    let n = unsafe { GetUserDefaultLocaleName(&mut buf) };
    if n <= 1 {
        return String::new();
    }
    let s = String::from_utf16_lossy(&buf[..n as usize - 1]);
    s.split('-').next().unwrap_or("").to_string()
}

/// A delay as the toast says it: "Subtitle delay 0 ms", "+250 ms", "−1.5 s".
pub fn delay_text(us: i64) -> String {
    let ms = us / 1000;
    let sign = if ms > 0 { "+" } else if ms < 0 { "\u{2212}" } else { "" };
    let a = ms.abs();
    if a >= 1000 && a % 100 == 0 {
        format!("Subtitle delay {sign}{} s", (a as f64 / 1000.0))
    } else {
        format!("Subtitle delay {sign}{a} ms")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(start: i64, end: Option<i64>, text: &str) -> SubEvent {
        SubEvent { start_us: start, end_us: end, read_us: start, text: text.into(), rects: Vec::new(), canvas: (0, 0), read_order: 0 }
    }

    fn pic(start: i64) -> SubEvent {
        let r = SubRect { x: 0, y: 0, w: 1, h: 1, bgra: Arc::new(vec![255; 4]) };
        SubEvent { start_us: start, end_us: None, read_us: start, text: String::new(), rects: vec![r], canvas: (1920, 1080), read_order: 0 }
    }

    #[test]
    fn encodings() {
        assert_eq!(decode_text(b"\xEF\xBB\xBFcaf\xC3\xA9", None), "café");
        assert_eq!(decode_text(&[0xFF, 0xFE, b'h', 0, b'i', 0], None), "hi");
        assert_eq!(decode_text(&[0xFE, 0xFF, 0, b'h', 0, b'i'], None), "hi");
        assert_eq!(decode_text(&[0xFF, 0xFE, 0, 0, b'h', 0, 0, 0], None), "h");
        assert_eq!(decode_text("Grüße".as_bytes(), None), "Grüße");
        // UTF-16 without a BOM.
        let u16le: Vec<u8> = "1\r\n00:00:01,000 --> 00:00:02,000\r\nHello\r\n".encode_utf16().flat_map(|u| u.to_le_bytes()).collect();
        assert_eq!(decode_text(&u16le, None), "1\r\n00:00:01,000 --> 00:00:02,000\r\nHello\r\n");
        let u16be: Vec<u8> = "Hello there, world".encode_utf16().flat_map(|u| u.to_be_bytes()).collect();
        assert_eq!(decode_text(&u16be, None), "Hello there, world");
        // Windows-1252: é is E9, invalid UTF-8.
        assert_eq!(decode_text(b"caf\xE9 cr\xE8me", None), "café crème");
        // The file name's language decides.
        assert_eq!(decode_text(b"\xcf\xf0\xe8\xe2\xe5\xf2", Some("ru")), "Привет");
        // Without one: Cyrillic by its commonest letters ("Привет, как дела? Хорошо, спасибо." in 1251).
        let cyr = b"\xcf\xf0\xe8\xe2\xe5\xf2, \xea\xe0\xea \xe4\xe5\xeb\xe0? \xd5\xee\xf0\xee\xf8\xee, \xf1\xef\xe0\xf1\xe8\xe1\xee.";
        assert_eq!(detect_code_page(cyr), 1251);
        assert_eq!(decode_text(cyr, None), "Привет, как дела? Хорошо, спасибо.");
        // Greek in 1253: "Καλημέρα, τι κάνεις; Πολύ καλά."
        let greek = decode_code_page_rev("Καλημέρα, τι κάνεις; Πολύ καλά, ευχαριστώ.", 1253);
        assert_eq!(detect_code_page(&greek), 1253);
        // Polish in 1250: ł and ą read as letters there, as ³ and ¹ in 1252.
        let pl = decode_code_page_rev("Zażółć gęślą jaźń. Właśnie tak, łódź płynie.", 1250);
        assert_eq!(detect_code_page(&pl), 1250);
        assert_eq!(detect_code_page(b"Fran\xe7ais, \xe9t\xe9, na\xefve"), 1252);
        // Japanese in Shift-JIS.
        let sjis = decode_code_page_rev("こんにちは、元気ですか。今日はいい天気です。", 932);
        assert_eq!(detect_code_page(&sjis), 932);
        assert_eq!(code_page_for_language("pt-BR"), Some(1252));
        assert_eq!(code_page_for_language("zz"), None);
    }

    /// Text into code page `cp` (for building test bytes).
    fn decode_code_page_rev(s: &str, cp: u32) -> Vec<u8> {
        use windows::Win32::Globalization::{WC_COMPOSITECHECK, WideCharToMultiByte};
        let w: Vec<u16> = s.encode_utf16().collect();
        let _ = WC_COMPOSITECHECK;
        unsafe {
            let n = WideCharToMultiByte(cp, 0, &w, None, windows::core::PCSTR::null(), None);
            let mut out = vec![0u8; n as usize];
            WideCharToMultiByte(cp, 0, &w, Some(&mut out), windows::core::PCSTR::null(), None);
            out
        }
    }

    #[test]
    fn sidecar_names() {
        let s = |f: &str| sidecar_name("Movie (2020)", f);
        assert_eq!(s("Movie (2020).srt"), Some((String::new(), false, false, "srt".into())));
        assert_eq!(s("movie (2020).EN.srt"), Some(("en".into(), false, false, "srt".into())));
        assert_eq!(s("Movie (2020).eng.forced.srt"), Some(("eng".into(), true, false, "srt".into())));
        assert_eq!(s("Movie (2020).English.SDH.srt"), Some(("en".into(), false, true, "srt".into())));
        assert_eq!(s("Movie (2020).pt-BR.ass"), Some(("pt".into(), false, false, "ass".into())));
        assert_eq!(s("Movie (2020)_fr.vtt"), Some(("fr".into(), false, false, "vtt".into())));
        assert_eq!(s("Movie (2020).hi.srt"), Some((String::new(), false, true, "srt".into())));
        assert_eq!(s("Movie (2020) 2.srt"), None);
        assert_eq!(s("Movie (2020) - English.srt"), Some(("en".into(), false, false, "srt".into())));
        assert_eq!(sidecar_name("Movie", "Movie 2.srt"), None);
        assert_eq!(sidecar_name("Movie", "Movie2.srt"), None);
        assert_eq!(s("Movie (2020).txt"), None);
        assert_eq!(s("Movie (2020).mkv"), None);
        assert_eq!(s("Other.srt"), None);
        let names: Vec<String> = ["Movie.de.srt", "Movie.idx", "Movie.srt", "Movie.sub", "Movie.mkv", "Other.srt"].iter().map(|s| s.to_string()).collect();
        let got: Vec<String> = sidecars_among("Movie", &names).into_iter().map(|(n, _)| n).collect();
        assert_eq!(got, ["Movie.idx", "Movie.srt", "Movie.de.srt"]);
        // A .sub alone is a text file.
        let names: Vec<String> = ["Movie.sub".to_string()].to_vec();
        assert_eq!(sidecars_among("Movie", &names).len(), 1);
        let sc = Sidecar { path: PathBuf::from(r"C:\v\Movie.en.forced.srt"), lang: "en".into(), forced: true, sdh: false, ext: "srt".into() };
        assert_eq!(sc.label(), "English, forced \u{b7} Movie.en.forced.srt");
    }

    #[test]
    fn sidecars_on_disk() {
        let dir = std::env::temp_dir().join(format!("looker-subs-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("Subs")).unwrap();
        for f in ["clip.mp4", "clip.srt", "clip.fr.ass", "clipper.srt"] {
            std::fs::write(dir.join(f), b"x").unwrap();
        }
        std::fs::write(dir.join("Subs").join("clip.de.vtt"), b"x").unwrap();
        let found: Vec<String> = find_sidecars(&dir.join("clip.mp4")).iter().map(|s| s.path.strip_prefix(&dir).unwrap().display().to_string()).collect();
        assert_eq!(found, ["clip.srt", "clip.fr.ass", r"Subs\clip.de.vtt"]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn run(text: &str) -> Run {
        Run { text: text.into(), italic: false, bold: false, underline: false, strike: false, color: None }
    }

    #[test]
    fn ass_tags() {
        let s = parse_ass(r"{\i1}Hello{\i0} there\Nsecond line");
        assert_eq!(s.align, 2);
        assert_eq!(s.lines.len(), 2);
        assert_eq!(s.lines[0], vec![Run { italic: true, ..run("Hello") }, run(" there")]);
        assert_eq!(s.lines[1], vec![run("second line")]);
        let s = parse_ass(r"{\an8}{\c&H00FFFF&}Top{\c} white");
        assert_eq!(s.align, 8);
        assert_eq!(s.lines[0], vec![Run { color: Some(0xFFFF00), ..run("Top") }, run(" white")]);
        let s = parse_ass(r"{\1c&HFF0000&\b1}blue bold{\r}plain");
        assert_eq!(s.lines[0], vec![Run { color: Some(0x0000FF), bold: true, ..run("blue bold") }, run("plain")]);
        // Positioning and effects are dropped, drawings too.
        let s = parse_ass(r"{\pos(10,20)\fad(200,200)}Hi{\p1}m 0 0 l 100 0 100 100{\p0} there\h!");
        assert_eq!(s.plain(), "Hi there\u{a0}!");
        assert_eq!(parse_ass(r"{\a6}x").align, 8);
        assert_eq!(parse_ass(r"{\a10}x").align, 5); // SSA: 9-11 are the middle row
        // HTML a decoder left, and a '<' that's text.
        let s = parse_ass("<i>it</i> <font color=\"#FF0000\">red</font> <3");
        assert_eq!(s.lines[0], vec![Run { italic: true, ..run("it") }, run(" "), Run { color: Some(0xFF0000), ..run("red") }, run(" <3")]);
        // Trailing line breaks aren't lines.
        assert_eq!(parse_ass(r"one\N").lines.len(), 1);
        assert_eq!(parse_ass("a\r\nb").plain(), "a\nb");
        assert_eq!(parse_ass(r"{\u1}u{\u0}{\s1}s").lines[0], vec![Run { underline: true, ..run("u") }, Run { strike: true, ..run("s") }]);
    }

    #[test]
    fn timing() {
        let mut st = Store::new(false);
        assert!(st.insert(ev(1_000_000, Some(3_000_000), "one")));
        assert!(st.insert(ev(2_000_000, Some(4_000_000), "two")));
        assert!(!st.insert(ev(1_000_000, Some(3_000_000), "one")), "a repeat after a seek is dropped");
        assert_eq!(st.len(), 2);
        fn texts_of(st: &Store, t: i64) -> Vec<String> {
            st.active(t).iter().map(|e| e.styled.as_ref().unwrap().plain()).collect()
        }
        assert!(texts_of(&st, 999_999).is_empty());
        assert_eq!(texts_of(&st, 1_000_000), ["one"]);
        assert_eq!(texts_of(&st, 2_500_000), ["one", "two"], "overlapping events both show, oldest first");
        assert_eq!(texts_of(&st, 3_000_000), ["two"], "the end is exclusive");
        assert!(texts_of(&st, 4_000_000).is_empty());
        // A text event with no end: until the next one, at most 10 s.
        st.insert(ev(10_000_000, None, "open"));
        assert_eq!(texts_of(&st, 19_999_999), ["open"]);
        assert!(texts_of(&st, 20_000_000).is_empty());
        st.insert(ev(12_000_000, Some(13_000_000), "next"));
        assert_eq!(texts_of(&st, 11_999_999), ["open"]);
        assert_eq!(texts_of(&st, 12_000_000), ["next"]);
        assert!(st.any_active(12_500_000));
        assert!(!st.any_active(14_000_000));
    }

    #[test]
    fn pictures_until_the_next() {
        let mut st = Store::new(false);
        st.insert(pic(1_000_000));
        assert_eq!(st.active(50_000_000).len(), 1, "a picture shows until something follows it");
        // PGS's clear: an empty display set.
        st.insert(SubEvent { rects: Vec::new(), ..pic(3_000_000) });
        assert_eq!(st.active(2_999_999).len(), 1);
        assert!(st.active(3_000_000).is_empty());
        st.insert(pic(5_000_000));
        st.insert(pic(60_000_000));
        // Far from the playhead, pictures go (and could be read again).
        st.evict(40_000_000, 100_000_000);
        assert_eq!(st.len(), 3, "the clear stays (no pixels); the picture at 5 s shows until 60 s, so it stays");
        assert!(st.insert(pic(1_000_000)), "an evicted picture can come back");
    }

    #[test]
    fn defaults() {
        let side = |name: &str, lang: &str, forced: bool| Sidecar { path: PathBuf::from(name), lang: lang.into(), forced, sdh: false, ext: "srt".into() };
        let streams: Vec<StreamFacts> = vec![(2, "eng".into(), true, false), (3, "fre".into(), false, true), (4, "eng".into(), false, true)];
        // A sidecar beats the file's own tracks; the one in Windows' language first.
        let sc = [side("m.fr.srt", "fr", false), side("m.en.srt", "en", false)];
        assert_eq!(pick_default(&streams, &sc, "eng", "en"), Choice::File("m.en.srt".into()));
        assert_eq!(pick_default(&streams, &sc, "eng", "de"), Choice::File("m.fr.srt".into()));
        // Forced only sidecars don't count; the forced track in the audio's language does.
        assert_eq!(pick_default(&streams, &[side("m.forced.srt", "", true)], "eng", "en"), Choice::Stream(4));
        assert_eq!(pick_default(&streams, &[], "fre", "en"), Choice::Stream(3));
        assert_eq!(pick_default(&streams, &[], "jpn", "en"), Choice::Stream(3));
        // Else the default track, else none.
        assert_eq!(pick_default(&streams[..1], &[], "eng", "en"), Choice::Stream(2));
        assert_eq!(pick_default(&[(2, "eng".into(), false, false)], &[], "eng", "en"), Choice::Off);
        for c in [Choice::Off, Choice::Stream(5), Choice::File(r"C:\a b\x.srt".into())] {
            assert_eq!(Choice::from_text(&c.to_text()), Some(c));
        }
        assert_eq!(delay_text(0), "Subtitle delay 0 ms");
        assert_eq!(delay_text(250_000), "Subtitle delay +250 ms");
        assert_eq!(delay_text(-1_500_000), "Subtitle delay \u{2212}1.5 s");
    }

    /// Waits (up to 10 s) for the reader to have what `ok` wants at `t`.
    fn wait_for(track: &Track, t: i64, ok: impl Fn(&[Event]) -> bool) -> Vec<Event> {
        let start = std::time::Instant::now();
        loop {
            track.set_time(t);
            let a = track.active(t);
            if ok(&a) {
                return a;
            }
            assert!(start.elapsed() < Duration::from_secs(10), "nothing right at {t} µs: {:?}", a.iter().map(|e| e.styled.as_ref().map(|s| s.plain())).collect::<Vec<_>>());
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    fn texts(a: &[Event]) -> Vec<String> {
        a.iter().filter_map(|e| e.styled.as_ref().map(|s| s.plain())).collect()
    }

    /// The test media made by `make.sh` (kept beside this note in the side session's notes): embedded SubRip,
    /// ASS (a font attached) and PGS in Matroska, mov_text in MP4, sidecars in Windows-1252, -1251 and WebVTT, and
    /// a VobSub pair. `LOOKER_SUBS=<folder> LOOKER_CODECS=<codecs> cargo test --release -- --ignored --nocapture
    /// real_subtitles`.
    #[test]
    #[ignore]
    fn real_subtitles() {
        let Some(dir) = std::env::var_os("LOOKER_SUBS").map(PathBuf::from) else { panic!("set LOOKER_SUBS") };
        let mkv = dir.join("clip.mkv");
        let (dec, info) = video::Decoder::open(&mkv, std::ptr::null_mut(), false).expect("clip.mkv opens");
        let tracks = dec.sub_tracks();
        let labels: Vec<String> = tracks.iter().enumerate().map(|(n, t)| t.sub_label(n + 1)).collect();
        println!("tracks: {labels:?}");
        assert_eq!(labels, ["English (SRT)", "Styled \u{b7} English (ASS)", "French, forced (PGS)"]);
        assert_eq!(tracks[0].is_default, 1);
        let zero = dec.start_us();
        let fps = info.fps().unwrap_or(0.0);
        drop(dec);

        // SubRip: italics kept, overlaps both shown, and after a seek an event that began before it.
        let srt = Track::open(&mkv, Source::Stream(tracks[0].stream), zero, fps);
        let a = wait_for(&srt, 2_000_000, |a| !a.is_empty());
        assert_eq!(texts(&a), ["Plain first line\nand a second one"]);
        assert!(a[0].styled.as_ref().unwrap().lines[1].iter().any(|r| r.italic && r.text == "second"));
        let a = wait_for(&srt, 7_500_000, |a| a.len() == 2);
        assert_eq!(texts(&a), ["Yellow and bold", "Overlapping, at the top"]);
        assert_eq!(a[0].styled.as_ref().unwrap().lines[0][0].color, Some(0xFFFF00));
        assert_eq!(a[1].styled.as_ref().unwrap().align, 8);
        assert!(srt.active(11_000_000).is_empty());
        let a = wait_for(&srt, 35_000_000, |a| !a.is_empty());
        assert_eq!(texts(&a), ["A long one across the seek point (30-40 s)"]);
        let a = wait_for(&srt, 13_000_000, |a| !a.is_empty());
        assert_eq!(texts(&a), ["Café, naïve, Grüße — and 日本語 too"]);
        drop(srt);

        // ASS: libass draws it, with the attached font.
        let ass = Track::open(&mkv, Source::Stream(tracks[1].stream), zero, fps);
        wait_for(&ass, 3_000_000, |_| ass.any_active(3_000_000));
        let i = ass.info().unwrap().unwrap();
        assert_eq!((i.kind, i.fonts), (video::SUB_ASS, 1));
        let img = ass.render(3_000_000, 1280, 720, 1280, 720, 1.0).expect("a new picture").expect("something shows");
        println!("ASS at 3 s: {}x{} at {},{}", img.w, img.h, img.x, img.y);
        assert!(img.w > 100 && img.h > 20);
        assert!(img.bgra.chunks(4).any(|p| p[3] == 255), "opaque ink");
        assert!(ass.render(3_000_000, 1280, 720, 1280, 720, 1.0).is_none(), "the same moment again: unchanged");
        let big = ass.render(3_000_000, 1280, 720, 1280, 720, 2.0).unwrap().unwrap();
        let ink = |i: &video::SubImage| i.bgra.chunks(4).map(|p| p[3] as u64).sum::<u64>();
        assert!(ink(&big) > ink(&img) * 2, "twice the size: more ink");
        wait_for(&ass, 35_000_000, |_| ass.any_active(35_000_000));
        let moving = ass.render(35_000_000, 1280, 720, 1280, 720, 1.0).unwrap().unwrap();
        let later = ass.render(38_000_000, 1280, 720, 1280, 720, 1.0).unwrap().unwrap();
        assert!(later.x > moving.x + 100, "\\move goes right: {} then {}", moving.x, later.x);
        assert!(!ass.any_active(20_000_000));
        drop(ass);

        // PGS: pictures on a 1280 x 720 canvas, shown until the next display set.
        let pgs = Track::open(&mkv, Source::Stream(tracks[2].stream), zero, fps);
        let a = wait_for(&pgs, 2_000_000, |a| !a.is_empty());
        assert_eq!(a[0].canvas, (1280, 720));
        assert!(!a[0].rects.is_empty() && a[0].rects[0].w > 10);
        println!("PGS at 2 s: {} rects, first {}x{} at {},{}", a[0].rects.len(), a[0].rects[0].w, a[0].rects[0].h, a[0].rects[0].x, a[0].rects[0].y);
        drop(pgs);

        // MP4's mov_text.
        let mp4 = dir.join("clip.mp4");
        let (dec, _) = video::Decoder::open(&mp4, std::ptr::null_mut(), false).unwrap();
        let t = dec.sub_tracks();
        assert_eq!(t.len(), 1);
        assert_eq!(t[0].sub_label(1), "English (Timed Text)");
        let tx3g = Track::open(&mp4, Source::Stream(t[0].stream), dec.start_us(), fps);
        drop(dec);
        let a = wait_for(&tx3g, 13_000_000, |a| !a.is_empty());
        assert_eq!(texts(&a), ["Café, naïve, Grüße — and 日本語 too"]);

        // Sidecars: found by name, their encodings read.
        let side = dir.join("side.mkv");
        let found: Vec<String> = find_sidecars(&side).iter().map(|s| s.label()).collect();
        assert_eq!(found, ["side.srt", "side.vtt", "Russian \u{b7} side.ru.srt"], "exact names first");
        let open = |name: &str, t: i64| {
            let tr = Track::open(&side, Source::File(dir.join(name)), 0, fps);
            texts(&wait_for(&tr, t, |a| !a.is_empty()))
        };
        assert_eq!(open("side.srt", 2_000_000), ["Café crème, naïve (Windows-1252)"]);
        assert_eq!(open("side.ru.srt", 2_000_000), ["Привет, как дела?"]);
        assert_eq!(open("side.vtt", 3_000_000), ["WebVTT bold cue"]);
        let pick = pick_default(&[], &find_sidecars(&side), "", "en");
        assert_eq!(pick, Choice::File(dir.join("side.srt")));

        // VobSub's .idx and .sub, at the first subtitle (1:43.6).
        let vob = dir.join("vobsub.mkv");
        let sc = find_sidecars(&vob);
        assert_eq!(sc.iter().map(|s| s.label()).collect::<Vec<_>>(), ["vobsub.idx"]);
        let tr = Track::open(&vob, Source::File(sc[0].path.clone()), 0, 25.0);
        let a = wait_for(&tr, 104_000_000, |a| !a.is_empty());
        println!("VobSub at 1:44: {} rects on {:?}", a[0].rects.len(), a[0].canvas);
        assert!(!a[0].rects.is_empty());
    }
}
