//! Audio files through FFmpeg: the reader (`la_*` in looker_video.c) the player (audio.rs) and the waveform scan
//! (waveform.rs) each open, the file's tags and cover picture, and the decode pool's still: the cover, or a sketch
//! of the waveform when there's none (thumbnails, the strip).

use std::ffi::{c_char, c_int, c_void};
use std::path::Path;

use windows::Win32::Graphics::Imaging::IWICImagingFactory;

use super::video::{error, lib, text, wide};
use super::{Decoded, wic};

/// `LaInfo` in looker_video.c.
#[repr(C)]
#[derive(Clone)]
pub struct Facts {
    pub duration_us: i64,
    /// From the duration: an estimate until the scan has counted them.
    pub frames: i64,
    pub bit_rate: i64,
    pub rate: c_int,
    pub channels: c_int,
    /// Bits per sample as stored (16, 24...), 0 for lossy codecs.
    pub bits: c_int,
    pub lossless: c_int,
    pub has_cover: c_int,
    pub stream: c_int,
    codec: [c_char; 32],
    profile: [c_char; 32],
    layout: [c_char; 48],
    container: [c_char; 48],
}

impl Facts {
    /// FFmpeg's short codec name ("mp3", "flac", "aac"...).
    pub fn codec(&self) -> String {
        text(&self.codec)
    }
    pub fn profile(&self) -> String {
        text(&self.profile)
    }
    /// "mono", "stereo", "5.1(side)"...
    pub fn layout(&self) -> String {
        text(&self.layout)
    }
}

/// The codec's name as people know it: "MP3", "AAC", "FLAC", "Opus"...
pub fn codec_name(codec: &str) -> String {
    match codec {
        "mp3" | "mp3float" => "MP3",
        "mp2" => "MP2",
        "mp1" => "MP1",
        "aac" | "aac_latm" => "AAC",
        "flac" => "FLAC",
        "alac" => "ALAC",
        "opus" => "Opus",
        "vorbis" => "Vorbis",
        "wmav1" | "wmav2" => "WMA",
        "wmapro" => "WMA Pro",
        "wavpack" => "WavPack",
        "ape" => "Monkey's Audio",
        "ac3" => "Dolby Digital",
        "eac3" => "Dolby Digital Plus",
        "truehd" => "Dolby TrueHD",
        "dca" | "dts" => "DTS",
        c if c.starts_with("pcm_") => "PCM",
        c if c.starts_with("adpcm_") => "ADPCM",
        c => return c.to_uppercase(),
    }
    .to_string()
}

/// "Stereo", "Mono", "5.1", or "6 channels".
pub fn channels_text(channels: i32, layout: &str) -> String {
    match (channels, layout) {
        (1, _) => "Mono".into(),
        (2, _) => "Stereo".into(),
        (_, l) if !l.is_empty() && !l.contains("channels") => l.split('(').next().unwrap_or(l).to_string(),
        (n, _) => format!("{n} channels"),
    }
}

/// "44.1 kHz", "48 kHz", "96 kHz".
pub fn rate_text(rate: i32) -> String {
    let k = format!("{:.1}", rate as f64 / 1000.0);
    format!("{} kHz", k.trim_end_matches(".0"))
}

/// The format line: "FLAC · 24-bit · 96 kHz · Stereo", "MP3 · 320 kbit/s · 44.1 kHz · Stereo".
pub fn format_line(f: &Facts) -> String {
    let mut parts = vec![codec_name(&f.codec())];
    let profile = f.profile();
    if f.codec() == "aac" && profile.starts_with("HE") {
        parts[0] = format!("AAC {profile}");
    }
    if f.bits > 0 {
        parts.push(format!("{}-bit", f.bits));
    } else if f.bit_rate > 0 {
        parts.push(format!("{} kbit/s", (f.bit_rate + 500) / 1000));
    }
    parts.push(rate_text(f.rate));
    parts.push(channels_text(f.channels, &f.layout()));
    parts.join("  \u{b7}  ")
}

/// The info card's Audio and Tags groups.
pub fn facts(f: &Facts, t: &Tags) -> (Vec<(&'static str, String)>, Vec<(&'static str, String)>) {
    let mut a = vec![("Codec", codec_name(&f.codec()))];
    if f.duration_us > 0 {
        a.push(("Duration", super::video::duration_text(f.duration_us)));
    }
    a.push(("Sample rate", rate_text(f.rate)));
    a.push(("Channels", channels_text(f.channels, &f.layout())));
    if f.bits > 0 {
        a.push(("Bit depth", format!("{}-bit", f.bits)));
    }
    if f.bit_rate > 0 {
        a.push(("Bit rate", format!("{} kbit/s", (f.bit_rate + 500) / 1000)));
    }
    let mut g = Vec::new();
    for (label, v) in [
        ("Title", &t.title),
        ("Artist", &t.artist),
        ("Album", &t.album),
        ("Album artist", &t.album_artist),
        ("Year", &t.date),
        ("Genre", &t.genre),
        ("Track", &t.track),
        ("Disc", &t.disc),
        ("Composer", &t.composer),
        ("BPM", &t.bpm),
        ("Key", &t.key),
        ("Comment", &t.comment),
    ] {
        if !v.is_empty() {
            g.push((label, v.clone()));
        }
    }
    (a, g)
}

/// A file's tags, the way people name them.
#[derive(Clone, Default, Debug)]
pub struct Tags {
    pub title: String,
    pub artist: String,
    pub album: String,
    pub album_artist: String,
    pub date: String,
    pub genre: String,
    pub track: String,
    pub disc: String,
    pub composer: String,
    pub comment: String,
    pub bpm: String,
    pub key: String,
}

impl Tags {
    fn parse(raw: &str) -> Tags {
        let mut t = Tags::default();
        for entry in raw.split('\u{1e}') {
            let Some((k, v)) = entry.split_once('\u{1f}') else { continue };
            let v = v.trim();
            if v.is_empty() {
                continue;
            }
            let slot = match k.to_ascii_lowercase().as_str() {
                "title" => &mut t.title,
                "artist" => &mut t.artist,
                "album" => &mut t.album,
                "album_artist" | "albumartist" | "album artist" => &mut t.album_artist,
                "date" | "year" | "originaldate" => &mut t.date,
                "genre" => &mut t.genre,
                "track" | "tracknumber" => &mut t.track,
                "disc" | "discnumber" => &mut t.disc,
                "composer" => &mut t.composer,
                "comment" | "description" => &mut t.comment,
                "tbpm" | "bpm" => &mut t.bpm,
                "tkey" | "initialkey" | "key" => &mut t.key,
                _ => continue,
            };
            if slot.is_empty() {
                *slot = v.to_string();
            }
        }
        t
    }

    /// "Artist — Album", or whichever there is.
    pub fn byline(&self) -> String {
        let artist = if self.artist.is_empty() { &self.album_artist } else { &self.artist };
        match (artist.is_empty(), self.album.is_empty()) {
            (false, false) => format!("{artist}  \u{2014}  {}", self.album),
            (false, true) => artist.clone(),
            (true, false) => self.album.clone(),
            (true, true) => String::new(),
        }
    }
}

/// An open audio stream, read block by block at its own rate and channel count. One thread at a time.
pub struct Reader {
    raw: *mut c_void,
    pub facts: Facts,
}

// SAFETY: the reader is used from one thread at a time (moved to it); la_interrupt is thread-safe.
unsafe impl Send for Reader {}

/// Stops a reader's current or next read from another thread (`Reader::interrupter`).
#[derive(Clone, Copy)]
pub struct Interrupter(*mut c_void);
// SAFETY: la_interrupt only sets an interlocked flag.
unsafe impl Send for Interrupter {}
unsafe impl Sync for Interrupter {}

impl Interrupter {
    pub fn interrupt(&self) {
        if let Ok(l) = lib() {
            unsafe { (l.la_interrupt)(self.0) };
        }
    }
}

pub enum Block<'a> {
    /// Interleaved float, `facts.channels` per frame, starting at this frame.
    Samples(i64, &'a [f32]),
    End,
    Interrupted,
}

impl Reader {
    /// Opens `path`'s audio stream `stream` (None: the best one).
    pub fn open(path: &Path, stream: Option<i32>) -> Result<Reader, String> {
        let l = lib()?;
        let mut facts: Facts = unsafe { std::mem::zeroed() };
        let mut raw = std::ptr::null_mut();
        let r = unsafe { (l.la_open)(wide(path).as_ptr(), stream.unwrap_or(-1), &mut facts, &mut raw) };
        if r < 0 || raw.is_null() {
            return Err(error(l, r));
        }
        Ok(Reader { raw, facts })
    }

    pub fn interrupter(&self) -> Interrupter {
        Interrupter(self.raw)
    }

    /// The next block (valid until the next call).
    pub fn read(&mut self) -> Result<Block<'_>, String> {
        let l = lib()?;
        let (mut p, mut n, mut pos) = (std::ptr::null(), 0, 0i64);
        let r = unsafe { (l.la_read)(self.raw, &mut p, &mut n, &mut pos) };
        const EOF: c_int = -0x20464f45; // AVERROR_EOF: -MKTAG('E','O','F',' ')
        const EXIT: c_int = -0x54495845; // AVERROR_EXIT: -MKTAG('E','X','I','T')
        match r {
            0 if !p.is_null() && n > 0 => Ok(Block::Samples(pos, unsafe { std::slice::from_raw_parts(p, n as usize * self.facts.channels as usize) })),
            0 => Ok(Block::Samples(pos, &[])),
            EOF => Ok(Block::End),
            EXIT => Ok(Block::Interrupted),
            r => Err(error(l, r)),
        }
    }

    /// Goes to `frame` exactly (also clears an interrupt).
    pub fn seek(&mut self, frame: i64) -> Result<(), String> {
        let l = lib()?;
        let r = unsafe { (l.la_seek)(self.raw, frame) };
        if r < 0 { Err(error(l, r)) } else { Ok(()) }
    }

    /// The cover picture's encoded bytes (JPEG or PNG).
    pub fn cover(&self) -> Option<Vec<u8>> {
        let l = lib().ok()?;
        let (mut p, mut n) = (std::ptr::null(), 0);
        let r = unsafe { (l.la_cover)(self.raw, &mut p, &mut n) };
        (r >= 0 && !p.is_null() && n > 0).then(|| unsafe { std::slice::from_raw_parts(p, n as usize) }.to_vec())
    }

    pub fn tags(&self) -> Tags {
        let Ok(l) = lib() else { return Tags::default() };
        let need = unsafe { (l.la_tags)(self.raw, std::ptr::null_mut(), 0) };
        if need <= 0 {
            return Tags::default();
        }
        let mut buf = vec![0u8; need as usize + 1];
        unsafe { (l.la_tags)(self.raw, buf.as_mut_ptr() as *mut c_char, buf.len() as c_int) };
        buf.truncate(need as usize);
        Tags::parse(&String::from_utf8_lossy(&buf))
    }
}

impl Drop for Reader {
    fn drop(&mut self) {
        if let Ok(l) = lib() {
            unsafe { (l.la_close)(self.raw) };
        }
    }
}

/// The facts and tags without decoding (the info card, the explorer's length column).
pub fn probe(path: &Path) -> Result<(Facts, Tags), String> {
    let r = Reader::open(path, None)?;
    Ok((r.facts.clone(), r.tags()))
}

/// Decodes cover bytes to premultiplied BGRA fitted to the box.
pub fn decode_cover(f: &IWICImagingFactory, bytes: &[u8], box_w: u32, box_h: u32) -> Result<Decoded, String> {
    wic::decode(f, wic::Source::Memory(bytes), box_w, box_h).map_err(|e| e.message().to_string())
}

/// The decode pool's still: the cover, else a sketch of the waveform (bars from a few dozen places in the file).
pub fn decode(f: &IWICImagingFactory, path: &Path, box_w: u32, box_h: u32) -> Result<Decoded, String> {
    let mut r = Reader::open(path, None)?;
    if let Some(bytes) = r.cover() {
        match decode_cover(f, &bytes, box_w, box_h) {
            Ok(mut d) => {
                d.taken = None;
                return Ok(d);
            }
            Err(e) => crate::trace::mark(format!("audio cover: {e}")),
        }
    }
    const BARS: usize = 48;
    let frames = r.facts.frames;
    let mut peaks = [0f32; BARS];
    let mut got = false;
    for (i, p) in peaks.iter_mut().enumerate() {
        if frames > 0 {
            if r.seek(frames * i as i64 / BARS as i64).is_err() {
                break;
            }
        }
        // A few blocks: one can be tiny (an Ogg page edge) or silent at a song's very start.
        for _ in 0..3 {
            match r.read()? {
                Block::Samples(_, s) => {
                    for v in s {
                        *p = p.max(v.abs());
                    }
                    got |= !s.is_empty();
                }
                _ => break,
            }
        }
    }
    if !got {
        return Err("no sound".into());
    }
    let (w, h) = if box_w == 0 || box_h == 0 { (512u32, 512u32) } else { (box_w.min(box_h).max(32), box_w.min(box_h).max(32)) };
    Ok(Decoded::still(w, h, sketch(w, h, &peaks), w, h, None))
}

/// Mirrored bars on a dark tile, in Looker's red: the waveform at a glance.
fn sketch(w: u32, h: u32, peaks: &[f32]) -> Vec<u8> {
    let mut px = vec![0u8; (w * h * 4) as usize];
    let max = peaks.iter().cloned().fold(0.0f32, f32::max).max(1e-4);
    let (bg, ink) = ([0x1c, 0x1c, 0x1c], [0x4d, 0x48, 0xe5]); // BGR
    for p in px.chunks_exact_mut(4) {
        p.copy_from_slice(&[bg[0], bg[1], bg[2], 255]);
    }
    let n = peaks.len() as f32;
    let pad = w as f32 * 0.12;
    let pitch = (w as f32 - pad * 2.0) / n;
    let bar = (pitch * 0.6).max(1.0);
    let mid = h as f32 / 2.0;
    for (i, &v) in peaks.iter().enumerate() {
        let half = ((v / max).sqrt() * h as f32 * 0.3).max(1.0);
        let x0 = (pad + i as f32 * pitch) as u32;
        let x1 = ((pad + i as f32 * pitch + bar) as u32).min(w);
        let (y0, y1) = ((mid - half).max(0.0) as u32, ((mid + half) as u32).min(h));
        for y in y0..y1 {
            for x in x0..x1 {
                let o = ((y * w + x) * 4) as usize;
                px[o..o + 3].copy_from_slice(&ink);
            }
        }
    }
    px
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wording() {
        assert_eq!(rate_text(44100), "44.1 kHz");
        assert_eq!(rate_text(48000), "48 kHz");
        assert_eq!(channels_text(2, "stereo"), "Stereo");
        assert_eq!(channels_text(6, "5.1(side)"), "5.1");
        assert_eq!(codec_name("pcm_s24le"), "PCM");
        let t = Tags::parse("title\u{1f}Song\u{1e}ARTIST\u{1f}Band\u{1e}album\u{1f}Record\u{1e}");
        assert_eq!(t.title, "Song");
        assert_eq!(t.byline(), "Band  \u{2014}  Record");
    }

    /// LOOKER_AUDIO=<file>: reads it end to end and prints what it found.
    #[test]
    #[ignore]
    fn reads_a_file() {
        let Some(p) = std::env::var_os("LOOKER_AUDIO") else { return };
        let mut r = Reader::open(Path::new(&p), None).unwrap();
        println!("{}  ({} frames est., cover {})", format_line(&r.facts), r.facts.frames, r.facts.has_cover);
        println!("{:?}", r.tags());
        let mut last = 0;
        let mut n = 0i64;
        loop {
            match r.read().unwrap() {
                Block::Samples(pos, s) => {
                    assert!(pos >= last);
                    last = pos;
                    n += (s.len() / r.facts.channels as usize) as i64;
                }
                _ => break,
            }
        }
        println!("{n} frames read");
        r.seek(r.facts.rate as i64 * 10).unwrap();
        if let Block::Samples(pos, _) = r.read().unwrap() {
            assert_eq!(pos, r.facts.rate as i64 * 10, "exact seek");
        }
    }
}
