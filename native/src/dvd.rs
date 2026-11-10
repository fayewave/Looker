//! DVD-Video discs and VIDEO_TS folders, for "Rip DVD" (native/PLAN.md N8). Reads the IFOs the way libdvdread
//! does (the titles from VIDEO_TS.IFO's title table, each title's program chains, cells, chapters, audio and
//! subpicture streams from its VTS_nn_0.IFO) and plays a title's cells off its VOB files as one MPEG program
//! stream for `imaging::video::rip`. Angle blocks take angle 1, and interleaved cells are walked unit by unit
//! through the DSI packets, as libdvdnav does. Only unencrypted discs: a title whose packs are CSS-scrambled is
//! refused (`Title::scrambled`), Looker carries no libdvdcss.

use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::os::windows::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

pub const SECTOR: usize = 2048;

/// A disc (or a folder holding one).
pub struct Disc {
    /// The VIDEO_TS folder.
    pub root: PathBuf,
    /// The volume label of a disc, else the name of the folder above VIDEO_TS: the default file name.
    pub name: String,
    pub titles: Vec<Title>,
}

pub struct Title {
    /// 1-based, as players number them.
    pub number: usize,
    pub vts: u8,
    pub duration_ms: u64,
    /// Each chapter's start, from 0.
    pub chapters_ms: Vec<u64>,
    pub angles: u8,
    pub cells: Vec<Cell>,
    pub audio: Vec<Track>,
    pub subs: Vec<Track>,
    pub width: u32,
    pub height: u32,
    pub wide: bool,
    pub pal: bool,
    /// The subpicture palette (16 entries of 0x00YYCrCb), from the title's first program chain.
    pub palette: [u32; 16],
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cell {
    /// Sectors from the start of VTS_nn_1.VOB, inclusive.
    pub first: u32,
    pub last: u32,
    /// Its sectors are shared with other angles or branches: walk it by its interleaved units.
    pub interleaved: bool,
}

#[derive(Clone, Debug)]
pub struct Track {
    /// Its id in the program stream (0x80+n AC-3, 0x88+n DTS, 0xA0+n LPCM, 0x1C0+n MPEG, 0x20+n subpicture).
    pub id: i32,
    /// ISO 639-2 ("eng"), or "".
    pub lang: String,
    /// "English, Dolby Digital 5.1", for the dialog.
    pub label: String,
}

impl Title {
    pub fn duration_text(&self) -> String {
        let s = self.duration_ms / 1000;
        if s >= 3600 { format!("{}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60) } else { format!("{}:{:02}", s / 60, s % 60) }
    }

    /// "1:52:03 · 24 chapters · English, Dolby Digital 5.1 + 2 more".
    pub fn summary(&self) -> String {
        let mut s = self.duration_text();
        let n = self.chapters_ms.len();
        if n > 1 {
            s += &format!(" \u{00B7} {n} chapters");
        }
        if let Some(a) = self.audio.first() {
            s += &format!(" \u{00B7} {}", a.label);
            if self.audio.len() > 1 {
                s += &format!(" +{}", self.audio.len() - 1);
            }
        }
        if !self.subs.is_empty() {
            s += &format!(" \u{00B7} {} subtitle{}", self.subs.len(), if self.subs.len() == 1 { "" } else { "s" });
        }
        if self.angles > 1 {
            s += &format!(" \u{00B7} {} angles", self.angles);
        }
        s += if self.pal { " \u{00B7} PAL" } else { " \u{00B7} NTSC" };
        s += if self.wide { " 16:9" } else { " 4:3" };
        s
    }

    /// The bytes the title's VOBs hold for it, for progress.
    pub fn bytes(&self) -> u64 {
        self.cells.iter().map(|c| (c.last.saturating_sub(c.first) as u64 + 1) * SECTOR as u64).sum()
    }

    /// The display aspect ratio.
    pub fn dar(&self) -> (i32, i32) {
        if self.wide { (16, 9) } else { (4, 3) }
    }

    /// VobSub's header for Matroska ("size: 720x576\npalette: rrggbb, ...\n"), with the palette turned to RGB.
    pub fn vobsub_header(&self) -> String {
        let pal: Vec<String> = self.palette.iter().map(|&v| {
            let (y, cr, cb) = (((v >> 16) & 0xFF) as f32, ((v >> 8) & 0xFF) as f32 - 128.0, (v & 0xFF) as f32 - 128.0);
            let y = (y - 16.0) * 1.164;
            let c = |x: f32| x.round().clamp(0.0, 255.0) as u8;
            let (r, g, b) = (c(y + 1.596 * cr), c(y - 0.813 * cr - 0.391 * cb), c(y + 2.018 * cb));
            format!("{r:02x}{g:02x}{b:02x}")
        }).collect();
        format!("size: {}x{}\npalette: {}\n", self.width, self.height, pal.join(", "))
    }
}

/// The VIDEO_TS folder `path` means: the folder itself, VIDEO_TS inside it (a drive's root), or the folder of
/// a VIDEO_TS.IFO.
pub fn video_ts(path: &Path) -> Option<PathBuf> {
    let is_root = |p: &Path| p.join("VIDEO_TS.IFO").is_file() || p.join("VIDEO_TS.BUP").is_file();
    if path.is_file() {
        let name = path.file_name()?.to_string_lossy().to_ascii_uppercase();
        return (name == "VIDEO_TS.IFO" || name == "VIDEO_TS.BUP").then(|| path.parent().map(Path::to_path_buf)).flatten();
    }
    if path.file_name().is_some_and(|n| n.to_string_lossy().eq_ignore_ascii_case("VIDEO_TS")) && is_root(path) {
        return Some(path.to_path_buf());
    }
    let inner = path.join("VIDEO_TS");
    is_root(&inner).then_some(inner)
}

/// Whether `path` (a folder or drive) holds a DVD-Video: cheap enough for a context menu.
pub fn is_dvd(path: &Path) -> bool {
    video_ts(path).is_some()
}

// --- IFO reading ---

struct Ifo(Vec<u8>);

impl Ifo {
    /// The IFO, or its backup copy when the IFO is unreadable (a scratched disc).
    fn read(root: &Path, stem: &str) -> io::Result<Ifo> {
        let r = std::fs::read(root.join(format!("{stem}.IFO"))).or_else(|_| std::fs::read(root.join(format!("{stem}.BUP"))))?;
        Ok(Ifo(r))
    }
    fn u8(&self, at: usize) -> io::Result<u8> {
        self.0.get(at).copied().ok_or_else(short)
    }
    fn u16(&self, at: usize) -> io::Result<u16> {
        Ok(u16::from_be_bytes(self.0.get(at..at + 2).ok_or_else(short)?.try_into().unwrap()))
    }
    fn u32(&self, at: usize) -> io::Result<u32> {
        Ok(u32::from_be_bytes(self.0.get(at..at + 4).ok_or_else(short)?.try_into().unwrap()))
    }
}

fn short() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, "IFO too short")
}

fn bad(what: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, what.to_string())
}

/// A BCD playback time (hh mm ss ff, the frame byte's top bits the rate) in ms.
fn bcd_ms(b: [u8; 4]) -> u64 {
    let d = |x: u8| ((x >> 4) * 10 + (x & 0xF)) as u64;
    let fps = match b[3] >> 6 {
        1 => 25.0,
        3 => 29.97,
        _ => 0.0,
    };
    let frames = d(b[3] & 0x3F);
    (d(b[0]) * 3600 + d(b[1]) * 60 + d(b[2])) * 1000 + if fps > 0.0 { (frames as f64 * 1000.0 / fps) as u64 } else { 0 }
}

struct TitleEntry {
    angles: u8,
    vts: u8,
    vts_ttn: u8,
}

struct Pgc {
    time_ms: u64,
    audio_ctl: [u16; 8],
    sub_ctl: [u32; 32],
    palette: [u32; 16],
    /// Each program's entry cell (1-based).
    programs: Vec<u8>,
    cells: Vec<PgcCell>,
}

struct PgcCell {
    block_mode: u8,
    block_type: u8,
    interleaved: bool,
    time_ms: u64,
    first: u32,
    last: u32,
}

fn read_pgc(ifo: &Ifo, at: usize) -> io::Result<Pgc> {
    let nprog = ifo.u8(at + 2)? as usize;
    let ncell = ifo.u8(at + 3)? as usize;
    let time_ms = bcd_ms([ifo.u8(at + 4)?, ifo.u8(at + 5)?, ifo.u8(at + 6)?, ifo.u8(at + 7)?]);
    let mut audio_ctl = [0u16; 8];
    for (i, a) in audio_ctl.iter_mut().enumerate() {
        *a = ifo.u16(at + 0x0C + i * 2)?;
    }
    let mut sub_ctl = [0u32; 32];
    for (i, s) in sub_ctl.iter_mut().enumerate() {
        *s = ifo.u32(at + 0x1C + i * 4)?;
    }
    let mut palette = [0u32; 16];
    for (i, p) in palette.iter_mut().enumerate() {
        *p = ifo.u32(at + 0xA4 + i * 4)?;
    }
    let prog_at = ifo.u16(at + 0xE6)? as usize;
    let play_at = ifo.u16(at + 0xE8)? as usize;
    let mut programs = Vec::with_capacity(nprog);
    if prog_at != 0 {
        for i in 0..nprog {
            programs.push(ifo.u8(at + prog_at + i)?);
        }
    }
    let mut cells = Vec::with_capacity(ncell);
    if play_at != 0 {
        for i in 0..ncell {
            let c = at + play_at + i * 24;
            let b0 = ifo.u8(c)?;
            cells.push(PgcCell {
                block_mode: b0 >> 6,
                block_type: (b0 >> 4) & 3,
                interleaved: b0 & 0x04 != 0,
                time_ms: bcd_ms([ifo.u8(c + 4)?, ifo.u8(c + 5)?, ifo.u8(c + 6)?, ifo.u8(c + 7)?]),
                first: ifo.u32(c + 8)?,
                last: ifo.u32(c + 20)?,
            });
        }
    }
    Ok(Pgc { time_ms, audio_ctl, sub_ctl, palette, programs, cells })
}

impl Pgc {
    /// Whether cell `i` (0-based) plays at angle 1: every cell outside an angle block, and an angle block's first.
    fn plays(&self, i: usize) -> bool {
        let c = &self.cells[i];
        c.block_type != 1 || c.block_mode == 1 || c.block_mode == 0
    }
}

pub fn open(path: &Path) -> io::Result<Disc> {
    let root = video_ts(path).ok_or_else(|| bad("No DVD-Video here"))?;
    let vmg = Ifo::read(&root, "VIDEO_TS")?;
    if vmg.0.get(..12) != Some(b"DVDVIDEO-VMG") {
        return Err(bad("VIDEO_TS.IFO isn't a DVD-Video manager"));
    }
    let tt = vmg.u32(0xC4)? as usize * SECTOR;
    let ntitles = vmg.u16(tt)? as usize;
    let mut entries = Vec::with_capacity(ntitles);
    for i in 0..ntitles {
        let e = tt + 8 + i * 12;
        entries.push(TitleEntry { angles: vmg.u8(e + 1)?, vts: vmg.u8(e + 6)?, vts_ttn: vmg.u8(e + 7)? });
    }
    let mut vts_cache: Vec<(u8, Ifo)> = Vec::new();
    let mut titles = Vec::new();
    for (i, e) in entries.iter().enumerate() {
        if !vts_cache.iter().any(|(n, _)| *n == e.vts) {
            match Ifo::read(&root, &format!("VTS_{:02}_0", e.vts)) {
                Ok(ifo) if ifo.0.get(..12) == Some(b"DVDVIDEO-VTS") => vts_cache.push((e.vts, ifo)),
                _ => continue, // a title in a missing set: leave it out
            }
        }
        let ifo = &vts_cache.iter().find(|(n, _)| *n == e.vts).unwrap().1;
        if let Ok(t) = read_title(ifo, i + 1, e) {
            if !t.cells.is_empty() {
                titles.push(t);
            }
        }
    }
    if titles.is_empty() {
        return Err(bad("This DVD has no titles Looker can read"));
    }
    Ok(Disc { name: disc_name(&root), root, titles })
}

fn read_title(ifo: &Ifo, number: usize, e: &TitleEntry) -> io::Result<Title> {
    let ptt = ifo.u32(0xC8)? as usize * SECTOR;
    let pgci = ifo.u32(0xCC)? as usize * SECTOR;
    // This title's chapters: (program chain, program) pairs.
    let nttn = ifo.u16(ptt)? as usize;
    let ttn = e.vts_ttn as usize;
    if ttn == 0 || ttn > nttn {
        return Err(bad("title outside its VTS"));
    }
    let ptt_end = ifo.u32(ptt + 4)? as usize + 1;
    let start = ifo.u32(ptt + 8 + (ttn - 1) * 4)? as usize;
    let end = if ttn < nttn { ifo.u32(ptt + 8 + ttn * 4)? as usize } else { ptt_end };
    let mut parts = Vec::new();
    let mut at = start;
    while at + 4 <= end {
        parts.push((ifo.u16(ptt + at)?, ifo.u16(ptt + at + 2)?));
        at += 4;
    }
    if parts.is_empty() {
        return Err(bad("title without chapters"));
    }
    // The program chains it runs through, each once, in order.
    let npgc = ifo.u16(pgci)? as usize;
    let mut chains: Vec<(u16, Pgc)> = Vec::new();
    for &(pgcn, _) in &parts {
        if chains.iter().any(|(n, _)| *n == pgcn) {
            continue;
        }
        if pgcn == 0 || pgcn as usize > npgc {
            return Err(bad("chapter in a missing program chain"));
        }
        let off = ifo.u32(pgci + 8 + (pgcn as usize - 1) * 8 + 4)? as usize;
        chains.push((pgcn, read_pgc(ifo, pgci + off)?));
    }
    // Cells in play order, with each one's start time, so chapters can be placed.
    let mut cells = Vec::new();
    let mut cell_start: Vec<(u16, usize, u64)> = Vec::new(); // (pgcn, cell 1-based, start ms)
    let mut t = 0u64;
    for (pgcn, pgc) in &chains {
        for (i, c) in pgc.cells.iter().enumerate() {
            if !pgc.plays(i) {
                continue;
            }
            cell_start.push((*pgcn, i + 1, t));
            t += c.time_ms;
            if c.last >= c.first {
                cells.push(Cell { first: c.first, last: c.last, interleaved: c.interleaved || c.block_type == 1 });
            }
        }
    }
    let duration_ms = if t > 0 { t } else { chains.iter().map(|(_, p)| p.time_ms).sum() };
    let mut chapters_ms = Vec::new();
    for &(pgcn, pgn) in &parts {
        let Some((_, pgc)) = chains.iter().find(|(n, _)| *n == pgcn) else { continue };
        let Some(&entry) = pgc.programs.get(pgn as usize - 1) else { continue };
        // Angle cells don't play: the next cell that does.
        let ms = cell_start.iter().find(|&&(n, c, _)| n == pgcn && c >= entry as usize).map(|&(_, _, ms)| ms);
        if let Some(ms) = ms {
            if chapters_ms.last().is_none_or(|&l| ms > l) {
                chapters_ms.push(ms);
            }
        }
    }
    if chapters_ms.first() != Some(&0) {
        chapters_ms.insert(0, 0);
    }

    // The title set's streams, through the first chain's control tables (which hold the program stream numbers).
    let pgc = &chains[0].1;
    let v = ifo.u16(0x200)?;
    let pal = (v >> 12) & 3 == 1;
    let wide = (v >> 10) & 3 == 3;
    let height = if pal { 576 } else { 480 };
    // libdvdread's video_attr_t: picture_size in bits 3-2 of the second byte.
    let size = (v >> 2) & 3;
    let width = match size {
        1 => 704,
        2 | 3 => 352,
        _ => 720,
    };
    let height = if size == 3 { height / 2 } else { height };
    let mut audio = Vec::new();
    let naudio = (ifo.u16(0x202)? as usize).min(8);
    for k in 0..naudio {
        let ctl = pgc.audio_ctl[k];
        if ctl & 0x8000 == 0 {
            continue;
        }
        let n = ((ctl >> 8) & 7) as i32;
        let a = 0x204 + k * 8;
        let b0 = ifo.u8(a)?;
        let b1 = ifo.u8(a + 1)?;
        let channels = (b1 & 7) + 1;
        let (id, codec) = match b0 >> 5 {
            0 => (0x80 + n, "Dolby Digital"),
            2 | 3 => (0x1C0 + n, "MPEG audio"),
            4 => (0xA0 + n, "PCM"),
            6 => (0x88 + n, "DTS"),
            _ => continue,
        };
        let lang = if (b0 >> 2) & 3 == 1 { lang3([ifo.u8(a + 2)?, ifo.u8(a + 3)?]) } else { String::new() };
        let ch = match channels {
            1 => "mono".to_string(),
            2 => "stereo".to_string(),
            6 => "5.1".to_string(),
            8 => "7.1".to_string(),
            n => format!("{n} ch"),
        };
        let extra = match ifo.u8(a + 5)? {
            2 | 3 | 4 => " (commentary)",
            _ => "",
        };
        audio.push(Track { id, label: format!("{}, {codec} {ch}{extra}", lang_name(&lang)), lang });
    }
    let mut subs = Vec::new();
    let nsubs = (ifo.u16(0x254)? as usize).min(32);
    for k in 0..nsubs {
        let ctl = pgc.sub_ctl[k];
        if ctl & 0x8000_0000 == 0 {
            continue;
        }
        // 4:3 stream number in the top byte, wide in the next.
        let n = if wide { (ctl >> 16) & 0x1F } else { (ctl >> 24) & 0x1F } as i32;
        if subs.iter().any(|s: &Track| s.id == 0x20 + n) {
            continue;
        }
        let a = 0x256 + k * 6;
        let lang = if ifo.u8(a)? & 3 == 1 { lang3([ifo.u8(a + 2)?, ifo.u8(a + 3)?]) } else { String::new() };
        let extra = match ifo.u8(a + 5)? {
            2 | 6 => " (large)",
            3 | 7 => " (children)",
            5 => " (captions)",
            9 => " (forced)",
            13..=15 => " (commentary)",
            _ => "",
        };
        subs.push(Track { id: 0x20 + n, label: format!("{}{extra}", lang_name(&lang)), lang });
    }
    Ok(Title {
        number,
        vts: e.vts,
        duration_ms,
        chapters_ms,
        angles: e.angles.max(1),
        cells,
        audio,
        subs,
        width,
        height,
        wide,
        pal,
        palette: pgc.palette,
    })
}

fn disc_name(root: &Path) -> String {
    let above = root.parent().unwrap_or(root);
    // A drive's root: its volume label.
    if above.parent().is_none() {
        use windows::Win32::Storage::FileSystem::GetVolumeInformationW;
        use windows::core::HSTRING;
        let mut label = [0u16; 261];
        let mut s = above.to_string_lossy().into_owned();
        if !s.ends_with('\\') {
            s.push('\\');
        }
        if unsafe { GetVolumeInformationW(&HSTRING::from(s), Some(&mut label), None, None, None, None) }.is_ok() {
            let n = label.iter().position(|&c| c == 0).unwrap_or(0);
            let l = String::from_utf16_lossy(&label[..n]);
            if !l.trim().is_empty() {
                return tidy_label(&l);
            }
        }
        return "DVD".into();
    }
    above.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "DVD".into())
}

/// "THE_BIG_FILM" -> "The Big Film".
fn tidy_label(l: &str) -> String {
    let spaced = l.trim().replace('_', " ");
    if spaced.chars().any(|c| c.is_lowercase()) {
        return spaced;
    }
    spaced.split_whitespace().map(|w| {
        let mut c = w.chars();
        c.next().map(|f| f.to_uppercase().chain(c.flat_map(char::to_lowercase)).collect::<String>()).unwrap_or_default()
    }).collect::<Vec<_>>().join(" ")
}

const LANGS: &[(&str, &str, &str)] = &[
    ("en", "eng", "English"), ("fr", "fre", "French"), ("de", "ger", "German"), ("es", "spa", "Spanish"),
    ("it", "ita", "Italian"), ("nl", "dut", "Dutch"), ("pt", "por", "Portuguese"), ("sv", "swe", "Swedish"),
    ("no", "nor", "Norwegian"), ("da", "dan", "Danish"), ("fi", "fin", "Finnish"), ("is", "ice", "Icelandic"),
    ("pl", "pol", "Polish"), ("cs", "cze", "Czech"), ("sk", "slo", "Slovak"), ("hu", "hun", "Hungarian"),
    ("ro", "rum", "Romanian"), ("bg", "bul", "Bulgarian"), ("hr", "hrv", "Croatian"), ("sr", "srp", "Serbian"),
    ("sl", "slv", "Slovenian"), ("el", "gre", "Greek"), ("tr", "tur", "Turkish"), ("ru", "rus", "Russian"),
    ("uk", "ukr", "Ukrainian"), ("he", "heb", "Hebrew"), ("iw", "heb", "Hebrew"), ("ar", "ara", "Arabic"),
    ("fa", "per", "Persian"), ("hi", "hin", "Hindi"), ("th", "tha", "Thai"), ("vi", "vie", "Vietnamese"),
    ("id", "ind", "Indonesian"), ("ms", "may", "Malay"), ("zh", "chi", "Chinese"), ("ja", "jpn", "Japanese"),
    ("ko", "kor", "Korean"), ("ca", "cat", "Catalan"), ("eu", "baq", "Basque"), ("gl", "glg", "Galician"),
    ("et", "est", "Estonian"), ("lv", "lav", "Latvian"), ("lt", "lit", "Lithuanian"), ("ga", "gle", "Irish"),
    ("cy", "wel", "Welsh"), ("la", "lat", "Latin"), ("ta", "tam", "Tamil"), ("te", "tel", "Telugu"),
];

/// The IFO's two-letter ISO 639-1 code as ISO 639-2 (Matroska's and MP4's), "" if unknown.
fn lang3(b: [u8; 2]) -> String {
    let two = String::from_utf8_lossy(&b).to_ascii_lowercase();
    LANGS.iter().find(|l| l.0 == two).map(|l| l.1.to_string()).unwrap_or_default()
}

fn lang_name(three: &str) -> &'static str {
    LANGS.iter().find(|l| l.1 == three).map(|l| l.2).unwrap_or("Unknown language")
}

// --- Reading a title ---

/// A title set's VOBs (VTS_nn_1.VOB ...) as one run of sectors.
struct Vobs {
    files: Vec<(File, u64)>, // each and its size in sectors
}

impl Vobs {
    fn open(root: &Path, vts: u8) -> io::Result<Vobs> {
        let mut files = Vec::new();
        for i in 1..=9 {
            let p = root.join(format!("VTS_{vts:02}_{i}.VOB"));
            // Shared for delete, as Looker's other readers: the user can still move things around.
            let Ok(f) = std::fs::OpenOptions::new().read(true).share_mode(7).open(&p) else { break };
            let n = f.metadata()?.len() / SECTOR as u64;
            files.push((f, n));
        }
        if files.is_empty() {
            return Err(bad(&format!("VTS_{vts:02}_1.VOB is missing")));
        }
        Ok(Vobs { files })
    }

    fn read(&mut self, mut lsn: u64, buf: &mut [u8]) -> io::Result<()> {
        debug_assert!(buf.len() % SECTOR == 0);
        let mut done = 0;
        while done < buf.len() {
            let mut base = 0u64;
            let Some(i) = self.files.iter().position(|(_, n)| {
                let hit = lsn < base + n;
                if !hit {
                    base += n;
                }
                hit
            }) else {
                return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "past the end of the title's VOBs"));
            };
            let (f, n) = &mut self.files[i];
            let within = lsn - base;
            let take = ((*n - within) as usize * SECTOR).min(buf.len() - done);
            f.seek(SeekFrom::Start(within * SECTOR as u64))?;
            f.read_exact(&mut buf[done..done + take])?;
            done += take;
            lsn += (take / SECTOR) as u64;
        }
        Ok(())
    }
}

/// A title's program stream: its cells' sectors in play order.
pub struct TitleReader {
    vobs: Vobs,
    cells: Vec<Cell>,
    cell: usize,
    /// The run being read (inclusive), and where the next one starts in an interleaved cell.
    pos: u64,
    run_end: u64,
    next: Option<u64>,
    started: bool,
    in_run: bool,
    buf: Vec<u8>,
    at: usize,
    pub read_bytes: u64,
}

impl TitleReader {
    pub fn new(disc: &Disc, t: &Title) -> io::Result<TitleReader> {
        Ok(TitleReader {
            vobs: Vobs::open(&disc.root, t.vts)?,
            cells: t.cells.clone(),
            cell: 0,
            pos: 0,
            run_end: 0,
            next: None,
            started: false,
            in_run: false,
            buf: Vec::new(),
            at: 0,
            read_bytes: 0,
        })
    }

    /// The next run of sectors to read, or None at the end of the title.
    fn next_run(&mut self) -> io::Result<Option<(u64, u64)>> {
        loop {
            let Some(&c) = self.cells.get(self.cell) else { return Ok(None) };
            let (first, last) = (c.first as u64, c.last as u64);
            let at = if !self.started {
                self.started = true;
                Some(first)
            } else {
                self.next.take()
            };
            let Some(s) = at.filter(|&s| s <= last) else {
                self.cell += 1;
                self.started = false;
                continue;
            };
            if !c.interleaved {
                self.next = None;
                return Ok(Some((s, last)));
            }
            // An interleaved unit: its NAV pack says where it ends and where this angle's next one starts.
            let mut nav = vec![0u8; SECTOR];
            self.vobs.read(s, &mut nav)?;
            match dsi(&nav) {
                Some(d) if d.ilvu => {
                    let end = (s + d.ilvu_ea as u64).min(last);
                    self.next = (d.next_ilvu != 0 && d.next_ilvu != 0x7FFF_FFFF).then(|| s + d.next_ilvu as u64);
                    return Ok(Some((s, end)));
                }
                Some(d) => {
                    let end = (s + d.vobu_ea as u64).min(last);
                    self.next = Some(end + 1);
                    return Ok(Some((s, end)));
                }
                // Not a NAV pack where one should be: read the rest of the cell straight.
                None => {
                    self.next = None;
                    return Ok(Some((s, last)));
                }
            }
        }
    }

    /// Fills the buffer with up to 256 sectors.
    fn fill(&mut self) -> io::Result<bool> {
        if !self.in_run || self.pos > self.run_end {
            match self.next_run()? {
                Some((a, b)) => {
                    self.pos = a;
                    self.run_end = b;
                    self.in_run = true;
                }
                None => return Ok(false),
            }
        }
        let n = (self.run_end - self.pos + 1).min(256) as usize;
        self.buf.resize(n * SECTOR, 0);
        self.vobs.read(self.pos, &mut self.buf)?;
        self.pos += n as u64;
        self.at = 0;
        Ok(true)
    }

    /// The first `n` sectors of the title, for the encryption check.
    fn sample(&mut self, n: usize) -> io::Result<Vec<u8>> {
        let mut out = Vec::new();
        while out.len() < n * SECTOR {
            if self.at >= self.buf.len() && !self.fill()? {
                break;
            }
            out.extend_from_slice(&self.buf[self.at..]);
            self.at = self.buf.len();
        }
        Ok(out)
    }
}

impl Read for TitleReader {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        if self.at >= self.buf.len() && !self.fill()? {
            return Ok(0);
        }
        let n = out.len().min(self.buf.len() - self.at);
        out[..n].copy_from_slice(&self.buf[self.at..self.at + n]);
        self.at += n;
        self.read_bytes += n as u64;
        Ok(n)
    }
}

struct Dsi {
    vobu_ea: u32,
    ilvu: bool,
    ilvu_ea: u32,
    next_ilvu: u32,
}

/// A NAV pack's DSI (libdvdread's dsi_gi and sml_pbi), or None if this sector isn't a NAV pack.
fn dsi(s: &[u8]) -> Option<Dsi> {
    if s.len() < SECTOR || s[..4] != [0, 0, 1, 0xBA] || s[0x26..0x2A] != [0, 0, 1, 0xBF] || s[0x400..0x404] != [0, 0, 1, 0xBF] || s[0x406] != 1 {
        return None;
    }
    let d = &s[0x407..];
    let u32_at = |i: usize| u32::from_be_bytes(d[i..i + 4].try_into().unwrap());
    let category = u16::from_be_bytes([d[0x20], d[0x21]]);
    Some(Dsi { vobu_ea: u32_at(8), ilvu: category & 0x4000 != 0, ilvu_ea: u32_at(0x22), next_ilvu: u32_at(0x26) })
}

/// Whether the title's opening packs are CSS-scrambled, as libdvdcss tells (the PES scrambling bits of a video
/// or audio pack). A disc drive that hasn't been unlocked refuses to read the scrambled sectors at all, which
/// says the same.
pub fn scrambled(disc: &Disc, t: &Title) -> io::Result<bool> {
    let mut r = TitleReader::new(disc, t)?;
    let data = match r.sample(1024) {
        Ok(d) => d,
        Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Err(e),
        Err(_) => return Ok(true),
    };
    Ok(data.chunks_exact(SECTOR).any(|s| {
        s[..4] == [0, 0, 1, 0xBA] && s[0x0E..0x11] == [0, 0, 1] && matches!(s[0x11], 0xBD | 0xC0..=0xEF) && s[0x14] & 0x30 != 0
    }))
}

/// The message for a copy-protected disc.
pub const PROTECTED: &str = "This DVD is copy-protected (CSS): Looker can only rip unprotected discs.";

/// Rips title `t` to `out` (MP4: H.264 + AAC; else Matroska with every stream copied). Blocks. `done` counts the
/// title's bytes read, against `t.bytes()`; `cancel` set non-zero stops it. A failed or cancelled rip leaves no
/// file behind.
pub fn rip(disc: &Disc, t: &Title, out: &Path, mp4: bool, cancel: &std::sync::atomic::AtomicI32, done: &std::sync::atomic::AtomicU64) -> Result<(), String> {
    if scrambled(disc, t).map_err(|e| e.to_string())? {
        return Err(PROTECTED.into());
    }
    let spec = crate::imaging::video::RipSpec {
        out,
        mp4,
        audio: t.audio.iter().map(|a| (a.id, a.lang.clone())).collect(),
        subs: if mp4 { Vec::new() } else { t.subs.iter().map(|s| (s.id, s.lang.clone())).collect() },
        sub_header: if t.subs.is_empty() { String::new() } else { t.vobsub_header() },
        chapters_us: t.chapters_ms.iter().map(|&ms| ms as i64 * 1000).collect(),
        duration_us: t.duration_ms as i64 * 1000,
        dar: t.dar(),
    };
    struct Counted<'a> {
        r: TitleReader,
        done: &'a std::sync::atomic::AtomicU64,
    }
    impl Read for Counted<'_> {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            let n = self.r.read(buf)?;
            self.done.fetch_add(n as u64, std::sync::atomic::Ordering::Relaxed);
            Ok(n)
        }
    }
    let mut reader = Counted { r: TitleReader::new(disc, t).map_err(|e| e.to_string())?, done };
    let r = crate::imaging::video::rip(&spec, &mut reader, cancel);
    if r.is_err() {
        let _ = std::fs::remove_file(out);
    }
    r
}

/// The file name a rip of `t` gets by default: the disc's name, "(Title 3)" after it for any but the longest.
pub fn default_name(disc: &Disc, t: &Title) -> String {
    let main = disc.titles.iter().max_by_key(|x| x.duration_ms).map(|x| x.number) == Some(t.number);
    let clean: String = disc.name.chars().map(|c| if "<>:\"/\\|?*".contains(c) || c.is_control() { '_' } else { c }).collect();
    if main { clean } else { format!("{clean} (Title {})", t.number) }
}

/// `folder\base.ext`, or `base (2).ext`... if that is taken.
pub fn unique_path(folder: &Path, base: &str, ext: &str) -> PathBuf {
    let mut p = folder.join(format!("{base}.{ext}"));
    let mut n = 2;
    while p.exists() {
        p = folder.join(format!("{base} ({n}).{ext}"));
        n += 1;
    }
    p
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bcd_times() {
        assert_eq!(bcd_ms([0x01, 0x52, 0x03, 0x40 | 0x12]), (3600 + 52 * 60 + 3) * 1000 + 480);
        assert_eq!(bcd_ms([0, 0, 0x10, 0xC0 | 0x15]), 10_000 + (15.0 * 1000.0 / 29.97) as u64);
    }

    #[test]
    fn labels() {
        assert_eq!(tidy_label("THE_BIG_FILM"), "The Big Film");
        assert_eq!(tidy_label("My Film"), "My Film");
        assert_eq!(lang3(*b"en"), "eng");
        assert_eq!(lang3(*b"xx"), "");
    }

    /// `LOOKER_DVD=<VIDEO_TS folder> cargo test --release -- --ignored --nocapture lists_titles`
    #[test]
    #[ignore]
    fn lists_titles() {
        let p = std::env::var("LOOKER_DVD").expect("LOOKER_DVD");
        let d = open(Path::new(&p)).unwrap();
        println!("{} ({})", d.name, d.root.display());
        for t in &d.titles {
            println!("Title {}: {} | {:?} | {}x{} | scrambled {:?}", t.number, t.summary(), t.chapters_ms, t.width, t.height, scrambled(&d, t).ok());
            for a in &t.audio {
                println!("  audio {:#x} {} {}", a.id, a.lang, a.label);
            }
            for s in &t.subs {
                println!("  sub {:#x} {} {}", s.id, s.lang, s.label);
            }
            println!("  cells {:?}", t.cells);
        }
    }

    /// `LOOKER_DVD=<VIDEO_TS folder> cargo test --release -- --ignored --nocapture rips_titles`: the longest title
    /// to both formats in %TEMP%.
    #[test]
    #[ignore]
    fn rips_titles() {
        let p = std::env::var("LOOKER_DVD").expect("LOOKER_DVD");
        let d = open(Path::new(&p)).unwrap();
        let t = d.titles.iter().max_by_key(|t| t.duration_ms).unwrap();
        for mp4 in [false, true] {
            let out = unique_path(&std::env::temp_dir(), &default_name(&d, t), if mp4 { "mp4" } else { "mkv" });
            let cancel = std::sync::atomic::AtomicI32::new(0);
            let done = std::sync::atomic::AtomicU64::new(0);
            let t0 = std::time::Instant::now();
            let r = rip(&d, t, &out, mp4, &cancel, &done);
            println!("{} -> {r:?} in {:?}, read {} of {}", out.display(), t0.elapsed(), done.into_inner(), t.bytes());
            assert!(r.is_ok());
        }
    }
}
