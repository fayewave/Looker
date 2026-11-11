//! Video through FFmpeg (bundled, `codecs\`: the FFmpeg DLLs and `looker_video.dll`, scripts/Build-Codecs.ps1).
//! Looker calls only `looker_video.dll`, its own small C API over FFmpeg (native/codecs/ports/looker-video), so
//! FFmpeg's struct layouts never reach Rust. Loaded the first time a video needs it: no startup cost otherwise.
//! For now a video shows its first picture as a still (N6a in native/PLAN.md); playback comes next.

use std::ffi::{c_char, c_int, c_void};
use std::path::Path;
use std::sync::OnceLock;

use windows::Win32::Graphics::Imaging::IWICImagingFactory;
use windows::Win32::System::LibraryLoader::{GetProcAddress, LOAD_LIBRARY_SEARCH_DEFAULT_DIRS, LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR, LoadLibraryExW};
use windows::core::{HSTRING, PCSTR};

use super::{Decoded, wic};

/// `LOOKER_VIDEO_API` in looker_video.c: a DLL built for another version is refused.
const API: c_int = 13;

/// `LvInfo` in looker_video.c.
#[repr(C)]
#[derive(Clone)]
pub struct Info {
    /// Display size: upright, square pixels.
    pub width: c_int,
    pub height: c_int,
    pub coded_width: c_int,
    pub coded_height: c_int,
    /// Clockwise degrees applied to show it upright.
    pub rotation: c_int,
    pub duration_us: i64,
    pub fps_num: c_int,
    pub fps_den: c_int,
    pub bit_rate: i64,
    pub bit_depth: c_int,
    pub has_alpha: c_int,
    /// FFmpeg's AVCOL_PRI_*, AVCOL_TRC_*, AVCOL_SPC_* numbers.
    pub primaries: c_int,
    pub transfer: c_int,
    pub matrix: c_int,
    pub full_range: c_int,
    pub audio_channels: c_int,
    pub audio_rate: c_int,
    /// Unix seconds, 0 when the container doesn't say.
    pub created: i64,
    codec: [c_char; 32],
    profile: [c_char; 48],
    pix_fmt: [c_char; 32],
    audio_codec: [c_char; 32],
}

pub(super) fn text(b: &[c_char]) -> String {
    let bytes: Vec<u8> = b.iter().take_while(|&&c| c != 0).map(|&c| c as u8).collect();
    String::from_utf8_lossy(&bytes).into_owned()
}

impl Info {
    fn zeroed() -> Info {
        // SAFETY: plain C data, all-zero is a valid value.
        unsafe { std::mem::zeroed() }
    }
    /// An audio file's facts in a video's shape, for the controls bar (no picture: 0 x 0, no frame rate).
    pub fn audio_only(duration_us: i64, channels: i32, rate: i32, bit_rate: i64, codec: &str) -> Info {
        let mut i = Info::zeroed();
        i.duration_us = duration_us;
        i.audio_channels = channels;
        i.audio_rate = rate;
        i.bit_rate = bit_rate;
        for (d, s) in i.audio_codec.iter_mut().zip(codec.bytes().take(31)) {
            *d = s as c_char;
        }
        i
    }
    /// FFmpeg's short codec name ("h264", "hevc", "prores"...).
    pub fn codec(&self) -> String {
        text(&self.codec)
    }
    /// The codec profile ("High", "Main 10", "422 HQ"...), or empty.
    pub fn profile(&self) -> String {
        text(&self.profile)
    }
    pub fn pix_fmt(&self) -> String {
        text(&self.pix_fmt)
    }
    pub fn audio_codec(&self) -> String {
        text(&self.audio_codec)
    }
    pub fn fps(&self) -> Option<f64> {
        (self.fps_num > 0 && self.fps_den > 0).then(|| self.fps_num as f64 / self.fps_den as f64)
    }
}

/// The codec's name as people know it.
pub fn codec_name(codec: &str) -> &str {
    match codec {
        "h264" => "H.264",
        "hevc" => "HEVC",
        "av1" => "AV1",
        "vp8" => "VP8",
        "vp9" => "VP9",
        "prores" => "ProRes",
        "dnxhd" => "DNxHD/HR",
        "cfhd" => "CineForm",
        "mpeg1video" => "MPEG-1",
        "mpeg2video" => "MPEG-2",
        "mpeg4" => "MPEG-4",
        "mjpeg" => "Motion JPEG",
        "vc1" => "VC-1",
        "wmv1" | "wmv2" | "wmv3" => "WMV",
        "v210" | "v210x" | "rawvideo" => "Uncompressed",
        c => c,
    }
}

/// Codec and profile the way an editor says them: "ProRes 422 HQ", "DNxHR HQX", "HEVC Main 10".
pub fn codec_label(codec: &str, profile: &str) -> String {
    match (codec, profile) {
        ("prores", p) => {
            let p = match p {
                "Proxy" => "422 Proxy",
                "LT" => "422 LT",
                "Standard" => "422",
                "HQ" => "422 HQ",
                "XQ" => "4444 XQ",
                "" => return "ProRes".into(),
                p => p,
            };
            format!("ProRes {p}")
        }
        ("dnxhd", p) if p.starts_with("DNXHR") => format!("DNxHR{}", &p[5..]),
        ("dnxhd", _) => "DNxHD".into(),
        (c, "") => codec_name(c).to_string(),
        (c, p) => format!("{} {p}", codec_name(c)),
    }
}

/// "25", "29.97", "23.976".
pub fn fps_text(fps: f64) -> String {
    let s = format!("{fps:.3}");
    s.trim_end_matches('0').trim_end_matches('.').to_string()
}

/// "0:07", "12:34", "1:02:03".
pub fn duration_text(us: i64) -> String {
    let s = (us.max(0) as f64 / 1e6).round() as i64;
    let (h, m, s) = (s / 3600, s / 60 % 60, s % 60);
    if h > 0 { format!("{h}:{m:02}:{s:02}") } else { format!("{m}:{s:02}") }
}

/// The status row's video parts, each with how soon it goes when the row is narrow (as `App::status_parts`):
/// "50 fps", "HEVC Main 10", "HDR (PQ)", "2:14", "45.2 Mbit/s".
pub fn status_parts(i: &Info) -> Vec<(u8, String)> {
    let mut out = Vec::new();
    if let Some(f) = i.fps() {
        out.push((1, format!("{} fps", fps_text(f))));
    }
    let codec = codec_label(&i.codec(), &i.profile());
    if !codec.is_empty() {
        out.push((4, codec));
    }
    match i.transfer {
        16 => out.push((3, "HDR (PQ)".into())),
        18 => out.push((3, "HDR (HLG)".into())),
        _ => {}
    }
    if i.duration_us > 0 {
        out.push((2, duration_text(i.duration_us)));
    }
    if i.bit_rate > 0 {
        out.push((7, format!("{:.1} Mbit/s", i.bit_rate as f64 / 1e6)));
    }
    out
}

/// The info card's Video rows.
pub fn facts(i: &Info) -> Vec<(&'static str, String)> {
    let mut out = vec![("Codec", codec_label(&i.codec(), &i.profile()))];
    if i.duration_us > 0 {
        out.push(("Duration", duration_text(i.duration_us)));
    }
    if let Some(f) = i.fps() {
        out.push(("Frame rate", format!("{} fps", fps_text(f))));
    }
    let pf = i.pix_fmt();
    let chroma = if pf.contains("444") || pf.starts_with("gbr") || pf.starts_with("rgb") || pf.starts_with("bgr") {
        Some("4:4:4")
    } else if pf.contains("422") || pf == "uyvy422" || pf == "yuyv422" {
        Some("4:2:2")
    } else if pf.contains("420") || pf.starts_with("nv12") || pf.starts_with("p010") || pf.starts_with("p016") {
        Some("4:2:0")
    } else {
        None
    };
    if i.bit_depth > 0 {
        let mut s = format!("{}-bit", i.bit_depth);
        if let Some(c) = chroma {
            s += &format!(" {c}");
        }
        if i.has_alpha != 0 {
            s += " with alpha";
        }
        out.push(("Format", s));
    }
    let gamut = match i.primaries {
        1 => Some("Rec. 709"),
        5 | 6 => Some("Rec. 601"),
        9 => Some("Rec. 2020"),
        11 => Some("DCI-P3"),
        12 => Some("Display P3"),
        _ => None,
    };
    let hdr = match i.transfer {
        16 => Some("HDR (PQ)"),
        18 => Some("HDR (HLG)"),
        _ => None,
    };
    match (gamut, hdr) {
        (Some(g), Some(h)) => out.push(("Colour", format!("{g}, {h}"))),
        (Some(g), None) => out.push(("Colour", g.to_string())),
        (None, Some(h)) => out.push(("Colour", h.to_string())),
        (None, None) => {}
    }
    if i.bit_rate > 0 {
        out.push(("Bit rate", format!("{:.1} Mbit/s", i.bit_rate as f64 / 1e6)));
    }
    if i.audio_channels > 0 {
        let ch = match i.audio_channels {
            1 => "mono".to_string(),
            2 => "stereo".to_string(),
            6 => "5.1".to_string(),
            8 => "7.1".to_string(),
            n => format!("{n} channels"),
        };
        let codec = match i.audio_codec().as_str() {
            "aac" => "AAC".to_string(),
            "mp3" | "mp3float" => "MP3".to_string(),
            "ac3" => "Dolby Digital".to_string(),
            "eac3" => "Dolby Digital Plus".to_string(),
            "opus" => "Opus".to_string(),
            "flac" => "FLAC".to_string(),
            "alac" => "Apple Lossless".to_string(),
            c if c.starts_with("pcm_") => "PCM".to_string(),
            c => c.to_uppercase(),
        };
        let mut s = format!("{codec}, {ch}");
        if i.audio_rate > 0 {
            s += &format!(", {} kHz", fps_text(i.audio_rate as f64 / 1000.0));
        }
        out.push(("Audio", s));
    } else {
        out.push(("Audio", "None".into()));
    }
    out
}

type ApiFn = unsafe extern "C" fn() -> c_int;
type ProbeFn = unsafe extern "C" fn(*const u16, *mut Info) -> c_int;
type StillFn = unsafe extern "C" fn(*const u16, c_int, c_int, *mut Info, *mut c_int, *mut c_int, *mut *mut u8) -> c_int;

pub(super) struct Lib {
    probe: ProbeFn,
    still: StillFn,
    free: unsafe extern "C" fn(*mut c_void),
    error: unsafe extern "C" fn(c_int, *mut c_char, c_int),
    next: unsafe extern "C" fn(*mut c_void, *mut LvFrame) -> c_int,
    next_audio: unsafe extern "C" fn(*mut c_void, *mut LvFrame) -> c_int,
    seek: unsafe extern "C" fn(*mut c_void, i64, c_int) -> c_int,
    interrupt: unsafe extern "C" fn(*mut c_void),
    resume: unsafe extern "C" fn(*mut c_void),
    hw: unsafe extern "C" fn(*const c_void) -> c_int,
    frame_times: unsafe extern "C" fn(*const c_void, *mut *const i64) -> c_int,
    audio_tracks: unsafe extern "C" fn(*const c_void, *mut *const Track) -> c_int,
    open_streams: unsafe extern "C" fn(*const u16, *mut c_void, c_int, c_int, c_int, *mut Info, *mut *mut c_void) -> c_int,
    video_tracks: unsafe extern "C" fn(*const c_void, *mut *const Track) -> c_int,
    close: unsafe extern "C" fn(*mut c_void),
    frame_free: unsafe extern "C" fn(*mut LvFrame),
    tempo_open: unsafe extern "C" fn(f64, *mut *mut c_void) -> c_int,
    tempo_process: unsafe extern "C" fn(*mut c_void, *const f32, c_int, *mut *const f32) -> c_int,
    tempo_close: unsafe extern "C" fn(*mut c_void),
    frame_rgba: unsafe extern "C" fn(*const LvFrame, c_int, c_int, c_int, *mut c_int, *mut c_int, *mut *mut u8) -> c_int,
    // The audio files' reader (la_* in looker_video.c, imaging/audio.rs).
    pub(super) la_open: unsafe extern "C" fn(*const u16, c_int, *mut super::audio::Facts, *mut *mut c_void) -> c_int,
    pub(super) la_close: unsafe extern "C" fn(*mut c_void),
    pub(super) la_read: unsafe extern "C" fn(*mut c_void, *mut *const f32, *mut c_int, *mut i64) -> c_int,
    pub(super) la_seek: unsafe extern "C" fn(*mut c_void, i64) -> c_int,
    pub(super) la_interrupt: unsafe extern "C" fn(*mut c_void),
    pub(super) la_cover: unsafe extern "C" fn(*const c_void, *mut *const u8, *mut c_int) -> c_int,
    pub(super) la_tags: unsafe extern "C" fn(*const c_void, *mut c_char, c_int) -> c_int,
    sub_tracks: unsafe extern "C" fn(*const c_void, *mut *const Track) -> c_int,
    start_us: unsafe extern "C" fn(*const c_void) -> i64,
    subs_open: unsafe extern "C" fn(*const u16, c_int, i64, f64, *mut *mut c_void, *mut SubInfo) -> c_int,
    subs_open_mem: unsafe extern "C" fn(*const u8, c_int, *const u16, f64, *mut *mut c_void, *mut SubInfo) -> c_int,
    subs_seek: unsafe extern "C" fn(*mut c_void, i64) -> c_int,
    subs_next: unsafe extern "C" fn(*mut c_void, *mut LvSubEvent) -> c_int,
    subs_render: unsafe extern "C" fn(*mut c_void, i64, c_int, c_int, c_int, c_int, f64, *mut c_int, *mut LvSubImage) -> c_int,
    subs_interrupt: unsafe extern "C" fn(*mut c_void),
    subs_close: unsafe extern "C" fn(*mut c_void),
    rip: unsafe extern "C" fn(*const RipJob) -> c_int,
    chapters: unsafe extern "C" fn(*const c_void, *mut *const LvChapter) -> c_int,
    passthrough: unsafe extern "C" fn(*const c_void, *mut c_int, *mut c_int) -> c_int,
}

static LIB: OnceLock<Result<Lib, String>> = OnceLock::new();

fn load() -> Result<Lib, String> {
    let dll = super::heif::codecs_dir().ok_or("no exe path")?.join("looker_video.dll");
    unsafe {
        // DLL_LOAD_DIR: its imports (the FFmpeg DLLs, dav1d) resolve from the same folder.
        let m = LoadLibraryExW(&HSTRING::from(dll.as_os_str()), None, LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR | LOAD_LIBRARY_SEARCH_DEFAULT_DIRS)
            .map_err(|e| format!("{}: {}", dll.display(), e.message()))?;
        macro_rules! sym {
            ($name:literal) => {
                std::mem::transmute(GetProcAddress(m, PCSTR(concat!($name, "\0").as_ptr())).ok_or(concat!("looker_video.dll lacks ", $name))?)
            };
        }
        let api: ApiFn = sym!("looker_video_api");
        let v = api();
        if v != API {
            return Err(format!("looker_video.dll speaks API {v}, Looker {API}"));
        }
        Ok(Lib {
            probe: sym!("looker_video_probe"),
            still: sym!("looker_video_still"),
            free: sym!("looker_video_free"),
            error: sym!("looker_video_error"),
            next: sym!("lv_next"),
            next_audio: sym!("lv_next_audio"),
            seek: sym!("lv_seek"),
            interrupt: sym!("lv_interrupt"),
            resume: sym!("lv_resume"),
            hw: sym!("lv_hw"),
            frame_times: sym!("lv_frame_times"),
            audio_tracks: sym!("lv_audio_tracks"),
            open_streams: sym!("lv_open_streams"),
            video_tracks: sym!("lv_video_tracks"),
            close: sym!("lv_close"),
            frame_free: sym!("lv_frame_free"),
            tempo_open: sym!("lv_tempo_open"),
            tempo_process: sym!("lv_tempo_process"),
            tempo_close: sym!("lv_tempo_close"),
            frame_rgba: sym!("lv_frame_rgba"),
            la_open: sym!("la_open"),
            la_close: sym!("la_close"),
            la_read: sym!("la_read"),
            la_seek: sym!("la_seek"),
            la_interrupt: sym!("la_interrupt"),
            la_cover: sym!("la_cover"),
            la_tags: sym!("la_tags"),
            sub_tracks: sym!("lv_sub_tracks"),
            start_us: sym!("lv_start_us"),
            subs_open: sym!("lv_subs_open"),
            subs_open_mem: sym!("lv_subs_open_mem"),
            subs_seek: sym!("lv_subs_seek"),
            subs_next: sym!("lv_subs_next"),
            subs_render: sym!("lv_subs_render"),
            subs_interrupt: sym!("lv_subs_interrupt"),
            subs_close: sym!("lv_subs_close"),
            rip: sym!("lv_rip"),
            chapters: sym!("lv_chapters"),
            passthrough: sym!("lv_passthrough"),
        })
    }
}

pub(super) fn lib() -> Result<&'static Lib, String> {
    LIB.get_or_init(|| {
        let r = load();
        if let Err(e) = &r {
            crate::trace::mark(format!("video unavailable: {e}"));
        }
        r
    })
    .as_ref()
    .map_err(|e| e.clone())
}

pub(super) fn error(l: &Lib, code: c_int) -> String {
    let mut buf = [0 as c_char; 256];
    unsafe { (l.error)(code, buf.as_mut_ptr(), buf.len() as c_int) };
    format!("FFmpeg {code}: {}", text(&buf))
}

pub(super) fn wide(path: &Path) -> Vec<u16> {
    use std::os::windows::ffi::OsStrExt;
    path.as_os_str().encode_wide().chain(Some(0)).collect()
}

// --- DVD ripping (lv_rip in looker_rip.c, src/dvd.rs) ---

/// `LvRipStream`.
#[repr(C)]
struct RipStream {
    id: c_int,
    lang: [c_char; 4],
}

/// `LvRipJob`.
#[repr(C)]
struct RipJob {
    read: unsafe extern "C" fn(*mut c_void, *mut u8, c_int) -> c_int,
    ctx: *mut c_void,
    out: *const u16,
    mode: c_int,
    audio_count: c_int,
    audio: *const RipStream,
    sub_count: c_int,
    subs: *const RipStream,
    sub_extradata: *const c_char,
    chapter_count: c_int,
    chapters_us: *const i64,
    duration_us: i64,
    dar_num: c_int,
    dar_den: c_int,
    video_kbps: c_int,
    cancel: *const i32,
}

/// What to rip: the streams by their program stream ids, with ISO 639-2 languages.
pub struct RipSpec<'a> {
    pub out: &'a Path,
    /// H.264 + AAC in MP4; else every stream copied into Matroska.
    pub mp4: bool,
    pub audio: Vec<(i32, String)>,
    pub subs: Vec<(i32, String)>,
    pub sub_header: String,
    pub chapters_us: Vec<i64>,
    pub duration_us: i64,
    pub dar: (i32, i32),
}

struct RipSource<'a> {
    reader: &'a mut dyn std::io::Read,
    error: Option<std::io::Error>,
}

unsafe extern "C" fn rip_read(ctx: *mut c_void, buf: *mut u8, size: c_int) -> c_int {
    // SAFETY: ctx is the RipSource `rip` lent lv_rip for the call; buf holds `size` bytes.
    let src = unsafe { &mut *(ctx as *mut RipSource) };
    let out = unsafe { std::slice::from_raw_parts_mut(buf, size.max(0) as usize) };
    match src.reader.read(out) {
        Ok(n) => n as c_int,
        Err(e) => {
            src.error = Some(e);
            -5 // AVERROR(EIO)
        }
    }
}

/// Rips a DVD title read through `reader` (a dvd::TitleReader) to `spec.out`. Blocks; `cancel` set non-zero stops
/// it. A partial file is left for the caller to remove.
pub fn rip(spec: &RipSpec, reader: &mut dyn std::io::Read, cancel: &std::sync::atomic::AtomicI32) -> Result<(), String> {
    let l = lib()?;
    let stream = |(id, lang): &(i32, String)| {
        let mut l = [0 as c_char; 4];
        for (d, s) in l.iter_mut().zip(lang.bytes().take(3)) {
            *d = s as c_char;
        }
        RipStream { id: *id, lang: l }
    };
    let audio: Vec<RipStream> = spec.audio.iter().map(stream).collect();
    let subs: Vec<RipStream> = spec.subs.iter().map(stream).collect();
    let header = std::ffi::CString::new(spec.sub_header.clone()).unwrap_or_default();
    let out = wide(spec.out);
    let mut src = RipSource { reader, error: None };
    let job = RipJob {
        read: rip_read,
        ctx: &mut src as *mut RipSource as *mut c_void,
        out: out.as_ptr(),
        mode: spec.mp4 as c_int,
        audio_count: audio.len() as c_int,
        audio: audio.as_ptr(),
        sub_count: subs.len() as c_int,
        subs: subs.as_ptr(),
        sub_extradata: if spec.sub_header.is_empty() { std::ptr::null() } else { header.as_ptr() },
        chapter_count: spec.chapters_us.len() as c_int,
        chapters_us: spec.chapters_us.as_ptr(),
        duration_us: spec.duration_us,
        dar_num: spec.dar.0,
        dar_den: spec.dar.1,
        video_kbps: 2500,
        cancel: cancel.as_ptr(),
    };
    // SAFETY: every pointer in job outlives the call.
    let r = unsafe { (l.rip)(&job) };
    if let Some(e) = src.error {
        return Err(format!("Couldn't read the disc: {e}"));
    }
    if cancel.load(std::sync::atomic::Ordering::Relaxed) != 0 {
        return Err("Cancelled".into());
    }
    if r < 0 {
        return Err(error(l, r));
    }
    Ok(())
}

// --- Playback (lv_* in looker_video.c) ---

pub const VIDEO: c_int = 1;
pub const AUDIO: c_int = 2;
/// `LvFrame::layout`: a slice of the decoder's NV12/P010 texture array on the GPU.
pub const TEXTURE: c_int = 1;
/// CPU planes: Y, U, V (+ A), or Y and interleaved UV when `nplanes == 2`.
pub const PLANAR: c_int = 2;
/// CPU, straight RGBA, 8 bits a channel.
pub const RGBA: c_int = 3;
/// Audio: IEC 61937 for a receiver (passthrough), 16-bit samples in `data[0]`.
pub const BITSTREAM: c_int = 4;
/// Audio always comes as interleaved float stereo at this rate (but passthrough's, which is the IEC stream's).
pub const AUDIO_RATE: u32 = 48_000;

/// `LvChapter` in looker_video.c.
#[repr(C)]
struct LvChapter {
    start_us: i64,
    end_us: i64,
    title: [c_char; 128],
}

/// A chapter of a video, on the player's clock.
#[derive(Clone, Debug, PartialEq)]
pub struct Chapter {
    pub start_us: i64,
    pub end_us: i64,
    pub title: String,
}

/// `LV_PASS_*` in looker_video.c: the codecs the output device takes as IEC 61937 (passthrough to a receiver).
pub const PASS_AC3: i32 = 8;
pub const PASS_EAC3: i32 = 16;
pub const PASS_DTS: i32 = 32;
pub const PASS_TRUEHD: i32 = 64;

/// Passthrough in use: which codec (`PASS_*`), and the IEC stream's rate and channel count (16-bit samples).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Pass {
    pub codec: i32,
    pub rate: u32,
    pub channels: u16,
}

/// `LvFrame` in looker_video.c: a picture or a block of audio.
#[repr(C)]
pub struct LvFrame {
    pub kind: c_int,
    pub layout: c_int,
    pub pts_us: i64,
    pub duration_us: i64,
    pub width: c_int,
    pub height: c_int,
    /// Samples wider than 8 bits sit in 16-bit words, shifted left by `shift`.
    pub bit_depth: c_int,
    pub shift: c_int,
    pub chroma_w_shift: c_int,
    pub chroma_h_shift: c_int,
    pub nplanes: c_int,
    pub matrix: c_int,
    pub full_range: c_int,
    pub transfer: c_int,
    pub primaries: c_int,
    pub texture: *mut c_void,
    pub texture_index: c_int,
    pub data: [*const u8; 4],
    pub linesize: [c_int; 4],
    pub samples: *const f32,
    pub nb_samples: c_int,
    opaque: *mut c_void,
}

/// One of a video's audio, video or subtitle streams (`LvTrack` in looker_video.c), for the track menus.
#[repr(C)]
#[derive(Clone)]
pub struct Track {
    /// Its stream index: what `Decoder::open_audio` takes.
    pub stream: c_int,
    pub channels: c_int,
    pub is_default: c_int,
    /// The one being played.
    pub playing: c_int,
    lang: [c_char; 16],
    title: [c_char; 64],
    codec: [c_char; 32],
    /// A video track's picture size (0 for audio).
    pub width: c_int,
    pub height: c_int,
    /// A subtitle track's `TRACK_*` flags.
    pub flags: c_int,
}

/// `LvTrack::flags`: the container marks it forced (shown even with subtitles off).
pub const TRACK_FORCED: c_int = 1;
/// Captions for the deaf and hard of hearing (SDH). (The shim also sets 4 for text, 8 for ASS, 16 for pictures.)
pub const TRACK_HEARING_IMPAIRED: c_int = 2;

impl Track {
    /// How the menu names it: its title, else its language, then what it is ("Commentary · English · AAC 2.0").
    pub fn label(&self, n: usize) -> String {
        let (title, lang, codec) = (text(&self.title), text(&self.lang), text(&self.codec));
        // Muxers' boilerplate handler names ("SoundHandle", "Core Media Audio", "GPAC ISO Audio Handler") aren't titles.
        let low = title.to_ascii_lowercase();
        let title = if ["handle", "core media", "iso media", "media handler"].iter().any(|b| low.contains(b)) { String::new() } else { title };
        let lang = language_name(&lang);
        let ch = match self.channels {
            1 => "mono".to_string(),
            2 => "stereo".to_string(),
            6 => "5.1".to_string(),
            8 => "7.1".to_string(),
            0 => String::new(),
            c => format!("{c} channels"),
        };
        let what = [codec_name(&codec).to_uppercase(), ch].into_iter().filter(|s| !s.is_empty()).collect::<Vec<_>>().join(" ");
        let head = match (title.is_empty(), lang.is_empty()) {
            (false, false) => format!("{title} · {lang}"),
            (false, true) => title,
            (true, false) => lang,
            (true, true) => format!("Track {n}"),
        };
        if what.is_empty() { head } else { format!("{head} ({what})") }
    }
}

impl Track {
    /// How the Video track menu names one: its title, else its language, then its size and codec
    /// ("Camera 2 (1920 × 1080 H264)").
    pub fn video_label(&self, n: usize) -> String {
        let (title, lang, codec) = (text(&self.title), text(&self.lang), text(&self.codec));
        let low = title.to_ascii_lowercase();
        let title = if ["handle", "core media", "iso media", "media handler"].iter().any(|b| low.contains(b)) { String::new() } else { title };
        let lang = language_name(&lang);
        let size = if self.width > 0 && self.height > 0 { format!("{} \u{d7} {}", self.width, self.height) } else { String::new() };
        let what = [size, codec_name(&codec).to_uppercase()].into_iter().filter(|s| !s.is_empty()).collect::<Vec<_>>().join(" ");
        let head = match (title.is_empty(), lang.is_empty()) {
            (false, false) => format!("{title} \u{b7} {lang}"),
            (false, true) => title,
            (true, false) => lang,
            (true, true) => format!("Track {n}"),
        };
        if what.is_empty() { head } else { format!("{head} ({what})") }
    }
}

impl Track {
    /// How the Subtitles menu names one: its title, else its language, then forced or SDH, then its format
    /// ("English (SRT)", "Signs & Songs · English (ASS)", "English, forced (PGS)").
    pub fn sub_label(&self, n: usize) -> String {
        let (title, lang, codec) = (text(&self.title), text(&self.lang), text(&self.codec));
        let low = title.to_ascii_lowercase();
        let title = if ["handle", "core media", "iso media", "media handler"].iter().any(|b| low.contains(b)) { String::new() } else { title };
        let lang = language_name(&lang);
        let mut head = match (title.is_empty(), lang.is_empty()) {
            (false, false) if !title.eq_ignore_ascii_case(&lang) => format!("{title} \u{b7} {lang}"),
            (false, _) => title,
            (true, false) => lang,
            (true, true) => format!("Track {n}"),
        };
        let low = head.to_ascii_lowercase();
        if self.flags & TRACK_FORCED != 0 && !low.contains("forced") {
            head.push_str(", forced");
        }
        if self.flags & TRACK_HEARING_IMPAIRED != 0 && !low.contains("sdh") && !low.contains("hearing") {
            head.push_str(", SDH");
        }
        format!("{head} ({})", sub_format_name(&codec))
    }

    /// Its language tag as the container wrote it ("eng"), or "".
    pub fn lang(&self) -> String {
        text(&self.lang)
    }
}

/// A subtitle codec as people know the format (FFmpeg's names are often the spec's).
pub fn sub_format_name(codec: &str) -> String {
    match codec {
        "subrip" | "srt" => "SRT",
        "ass" => "ASS",
        "ssa" => "SSA",
        "mov_text" => "Timed Text",
        "webvtt" => "WebVTT",
        "hdmv_pgs_subtitle" => "PGS",
        "dvd_subtitle" => "VobSub",
        "dvb_subtitle" => "DVB",
        "eia_608" => "CC",
        "microdvd" => "MicroDVD",
        "subviewer" | "subviewer1" => "SubViewer",
        "sami" => "SAMI",
        "text" => "Text",
        "realtext" => "RealText",
        "mpl2" => "MPL2",
        "vplayer" => "VPlayer",
        "pjs" => "PJS",
        other => return other.to_uppercase(),
    }
    .to_string()
}

/// A language tag (ISO 639-2, as containers write them, or 639-1 as file names do) as its English name, the
/// common ones; else the tag.
pub fn language_name(tag: &str) -> String {
    let n = match tag.to_ascii_lowercase().as_str() {
        "eng" | "en" => "English",
        "jpn" | "ja" => "Japanese",
        "spa" | "es" => "Spanish",
        "fre" | "fra" | "fr" => "French",
        "ger" | "deu" | "de" => "German",
        "ita" | "it" => "Italian",
        "por" | "pt" => "Portuguese",
        "rus" | "ru" => "Russian",
        "chi" | "zho" | "zh" => "Chinese",
        "kor" | "ko" => "Korean",
        "hin" | "hi" => "Hindi",
        "ara" | "ar" => "Arabic",
        "dut" | "nld" | "nl" => "Dutch",
        "swe" | "sv" => "Swedish",
        "pol" | "pl" => "Polish",
        "tur" | "tr" => "Turkish",
        "nor" | "nob" | "no" | "nb" => "Norwegian",
        "dan" | "da" => "Danish",
        "fin" | "fi" => "Finnish",
        "cze" | "ces" | "cs" => "Czech",
        "hun" | "hu" => "Hungarian",
        "gre" | "ell" | "el" => "Greek",
        "heb" | "he" => "Hebrew",
        "tha" | "th" => "Thai",
        "vie" | "vi" => "Vietnamese",
        "ukr" | "uk" => "Ukrainian",
        "rum" | "ron" | "ro" => "Romanian",
        "ind" | "id" => "Indonesian",
        "bul" | "bg" => "Bulgarian",
        "hrv" | "hr" => "Croatian",
        "srp" | "sr" => "Serbian",
        "slv" | "sl" => "Slovenian",
        "slo" | "slk" | "sk" => "Slovak",
        "per" | "fas" | "fa" => "Persian",
        _ => return tag.to_string(),
    };
    n.to_string()
}

/// A decoded picture, freed (its decoder surface given back) on drop.
pub struct Picture(LvFrame);
// SAFETY: an AVFrame is reference counted and freeing it is thread-safe; the planes are read-only.
unsafe impl Send for Picture {}

impl std::ops::Deref for Picture {
    type Target = LvFrame;
    fn deref(&self) -> &LvFrame {
        &self.0
    }
}

impl Picture {
    /// A picture that isn't the decoder's (a copy on the GPU, player.rs's scrub cache): freeing it frees nothing,
    /// and whatever `f` points at must outlive it.
    pub fn detached(mut f: LvFrame) -> Picture {
        f.opaque = std::ptr::null_mut();
        Picture(f)
    }

    /// Lets the CPU planes go (they're on the GPU now): only the picture's facts stay.
    pub fn release(&mut self) {
        if let Ok(l) = lib() {
            unsafe { (l.frame_free)(&mut self.0) };
        }
        self.0.data = [std::ptr::null(); 4];
    }

    /// A CPU picture (not a texture) fitted into `box_w × box_h`, square-pixelled and turned `rotation` degrees,
    /// as straight RGBA: (width, height, pixels).
    pub fn rgba(&self, rotation: i32, box_w: u32, box_h: u32) -> Result<(u32, u32, Vec<u8>), String> {
        let l = lib()?;
        let (mut w, mut h) = (0, 0);
        let mut px: *mut u8 = std::ptr::null_mut();
        let r = unsafe { (l.frame_rgba)(&self.0, rotation, box_w as c_int, box_h as c_int, &mut w, &mut h, &mut px) };
        if r < 0 || px.is_null() || w <= 0 || h <= 0 {
            if !px.is_null() {
                unsafe { (l.free)(px as _) };
            }
            return Err(if r < 0 { error(l, r) } else { "no picture".into() });
        }
        let v = unsafe { std::slice::from_raw_parts(px, w as usize * h as usize * 4) }.to_vec();
        unsafe { (l.free)(px as _) };
        Ok((w as u32, h as u32, v))
    }
}

impl Drop for Picture {
    fn drop(&mut self) {
        if let Ok(l) = lib() {
            unsafe { (l.frame_free)(&mut self.0) };
        }
    }
}

/// What `Decoder::next` hands back.
pub enum Item {
    Picture(Picture),
    /// Interleaved stereo samples and the time of the first.
    Audio(i64, Vec<f32>),
    End,
    /// `Interrupter::interrupt` stopped it: a seek is waiting.
    Interrupted,
}

/// AVERROR_EXIT, FFERRTAG('E','X','I','T'): what lv_next says after lv_interrupt.
const INTERRUPTED: c_int = -0x5449_5845;
const LV_NO_AUDIO: c_int = 1;
const LV_KEYFRAMES: c_int = 2;

/// Stops a decoder's `next` from another thread (it hands back `Item::Interrupted` until its next `seek`).
pub struct Interrupter(*mut c_void);
// SAFETY: lv_interrupt only sets a flag atomically; the owner keeps the decoder alive while this exists (player.rs
// drops it first).
unsafe impl Send for Interrupter {}
unsafe impl Sync for Interrupter {}

impl Interrupter {
    pub fn interrupt(&self) {
        if let Ok(l) = lib() {
            unsafe { (l.interrupt)(self.0) };
        }
    }
}

/// An open video, decoding in file order (looker_video.c's LvPlayer). One thread at a time.
pub struct Decoder(*mut c_void);
// SAFETY: used by one thread at a time (moved to the decode thread once).
unsafe impl Send for Decoder {}

impl Decoder {
    /// Opens `path`; `d3d11` is the window's ID3D11Device (made with video support) to decode on its GPU, or
    /// null for the CPU. `keyframes` (the seek bar's previews): no sound, and only keyframes, each handed out as
    /// soon as it's decoded.
    pub fn open(path: &Path, d3d11: *mut c_void, keyframes: bool) -> Result<(Decoder, Info), String> {
        Decoder::open_audio(path, d3d11, keyframes, None)
    }

    /// open, playing the audio stream udio (a Track::stream), else the best one.
    pub fn open_audio(path: &Path, d3d11: *mut c_void, keyframes: bool, audio: Option<i32>) -> Result<(Decoder, Info), String> {
        Decoder::open_streams(path, d3d11, keyframes, None, audio)
    }

    /// open, showing the video stream `video` and playing the audio stream `audio` (`Track::stream`s), else the
    /// file's best of each.
    pub fn open_streams(path: &Path, d3d11: *mut c_void, keyframes: bool, video: Option<i32>, audio: Option<i32>) -> Result<(Decoder, Info), String> {
        Decoder::open_with(path, d3d11, if keyframes { LV_NO_AUDIO | LV_KEYFRAMES } else { 0 }, video, audio)
    }

    /// The player's: `open_streams` passing the sound through to a receiver in the codecs `pass` allows
    /// (`PASS_*`, what the output device takes; 0 decodes it all).
    pub fn open_playing(path: &Path, d3d11: *mut c_void, pass: i32, video: Option<i32>, audio: Option<i32>) -> Result<(Decoder, Info), String> {
        Decoder::open_with(path, d3d11, pass & (PASS_AC3 | PASS_EAC3 | PASS_DTS | PASS_TRUEHD), video, audio)
    }

    fn open_with(path: &Path, d3d11: *mut c_void, flags: c_int, video: Option<i32>, audio: Option<i32>) -> Result<(Decoder, Info), String> {
        let l = lib()?;
        let mut info = Info::zeroed();
        let mut p = std::ptr::null_mut();
        let r = unsafe { (l.open_streams)(wide(path).as_ptr(), d3d11, flags, video.unwrap_or(-1), audio.unwrap_or(-1), &mut info, &mut p) };
        if r < 0 || p.is_null() {
            return Err(error(l, r));
        }
        Ok((Decoder(p), info))
    }

    /// Clears an interrupt that came too late to matter (no newer seek is waiting): `next` decodes on.
    pub fn resume(&mut self) {
        if let Ok(l) = lib() {
            unsafe { (l.resume)(self.0) };
        }
    }

    /// A handle that stops `next` from another thread.
    pub fn interrupter(&self) -> Interrupter {
        Interrupter(self.0)
    }

    /// Whether pictures come from the GPU's decoder.
    pub fn hardware(&self) -> bool {
        lib().is_ok_and(|l| unsafe { (l.hw)(self.0) } != 0)
    }

    /// Every picture's time from the container's index, sorted, 0 = the first (MP4/MOV, AVI); empty when the
    /// container doesn't list every picture. A picture's number is its place in this list.
    pub fn frame_times(&self) -> Vec<i64> {
        let Ok(l) = lib() else { return Vec::new() };
        let mut t: *const i64 = std::ptr::null();
        let n = unsafe { (l.frame_times)(self.0, &mut t) };
        if n <= 0 || t.is_null() {
            return Vec::new();
        }
        unsafe { std::slice::from_raw_parts(t, n as usize) }.to_vec()
    }

    /// The file's video streams (cover art left out), the one showing marked.
    pub fn video_tracks(&self) -> Vec<Track> {
        let Ok(l) = lib() else { return Vec::new() };
        let mut t: *const Track = std::ptr::null();
        let n = unsafe { (l.video_tracks)(self.0, &mut t) };
        if n <= 0 || t.is_null() {
            return Vec::new();
        }
        unsafe { std::slice::from_raw_parts(t, n as usize) }.to_vec()
    }

    /// The file's audio streams, the one playing marked.
    /// The file's subtitle streams.
    pub fn sub_tracks(&self) -> Vec<Track> {
        let Ok(l) = lib() else { return Vec::new() };
        let mut t: *const Track = std::ptr::null();
        let n = unsafe { (l.sub_tracks)(self.0, &mut t) };
        if n <= 0 || t.is_null() {
            return Vec::new();
        }
        unsafe { std::slice::from_raw_parts(t, n as usize) }.to_vec()
    }

    /// The zero of this decoder's clock in the file (the video stream's start), µs.
    pub fn start_us(&self) -> i64 {
        lib().map_or(0, |l| unsafe { (l.start_us)(self.0) })
    }

    /// The file's chapters, by start time (none when it has none, or one covering it all).
    pub fn chapters(&self) -> Vec<Chapter> {
        let Ok(l) = lib() else { return Vec::new() };
        let mut c: *const LvChapter = std::ptr::null();
        let n = unsafe { (l.chapters)(self.0, &mut c) };
        if n <= 0 || c.is_null() {
            return Vec::new();
        }
        unsafe { std::slice::from_raw_parts(c, n as usize) }
            .iter()
            .map(|c| Chapter { start_us: c.start_us, end_us: c.end_us, title: text(&c.title) })
            .collect()
    }

    /// Passthrough in use (the sound comes as IEC 61937 for a receiver), else None.
    pub fn passthrough(&self) -> Option<Pass> {
        let l = lib().ok()?;
        let (mut rate, mut channels) = (0, 0);
        let codec = unsafe { (l.passthrough)(self.0, &mut rate, &mut channels) };
        (codec != 0 && rate > 0 && channels > 0).then_some(Pass { codec, rate: rate as u32, channels: channels as u16 })
    }

    pub fn audio_tracks(&self) -> Vec<Track> {
        let Ok(l) = lib() else { return Vec::new() };
        let mut t: *const Track = std::ptr::null();
        let n = unsafe { (l.audio_tracks)(self.0, &mut t) };
        if n <= 0 || t.is_null() {
            return Vec::new();
        }
        unsafe { std::slice::from_raw_parts(t, n as usize) }.to_vec()
    }

    pub fn next(&mut self) -> Result<Item, String> {
        let l = lib()?;
        let mut f: LvFrame = unsafe { std::mem::zeroed() };
        let r = unsafe { (l.next)(self.0, &mut f) };
        const EOF: c_int = -541478725; // AVERROR_EOF, FFERRTAG('E','O','F',' ')
        if r == EOF {
            return Ok(Item::End);
        }
        if r == INTERRUPTED {
            return Ok(Item::Interrupted);
        }
        if r < 0 {
            return Err(error(l, r));
        }
        Ok(match f.kind {
            AUDIO => Item::Audio(f.pts_us, audio_samples(&f)),
            VIDEO => Item::Picture(Picture(f)),
            k => return Err(format!("unknown item kind {k}")),
        })
    }

    /// The next block of sound alone, reading past pictures (kept, undecoded, for `next`) to reach it; None when
    /// it may not read further ahead, or the sound is over.
    pub fn next_audio(&mut self) -> Result<Option<Item>, String> {
        let l = lib()?;
        let mut f: LvFrame = unsafe { std::mem::zeroed() };
        let r = unsafe { (l.next_audio)(self.0, &mut f) };
        const EOF: c_int = -541478725;
        const EAGAIN: c_int = -11;
        if r == EOF || r == EAGAIN {
            return Ok(None);
        }
        if r == INTERRUPTED {
            return Ok(Some(Item::Interrupted));
        }
        if r < 0 {
            return Err(error(l, r));
        }
        Ok(Some(Item::Audio(f.pts_us, audio_samples(&f))))
    }

    /// Jumps to `us`: the keyframe at or before it, or with `exact` the picture showing at `us`.
    pub fn seek(&mut self, us: i64, exact: bool) -> Result<(), String> {
        let l = lib()?;
        let r = unsafe { (l.seek)(self.0, us, exact as c_int) };
        if r < 0 { Err(error(l, r)) } else { Ok(()) }
    }
}

/// A block of audio as `Item::Audio` carries it: float stereo, or (passthrough) each 16-bit IEC 61937 sample as
/// an f32 holding its value exactly, interleaved over the IEC stream's channels.
fn audio_samples(f: &LvFrame) -> Vec<f32> {
    if f.layout == BITSTREAM {
        let n = f.linesize[0].max(0) as usize / 2;
        if f.data[0].is_null() || n == 0 {
            return Vec::new();
        }
        let b = unsafe { std::slice::from_raw_parts(f.data[0], n * 2) };
        return b.chunks_exact(2).map(|s| i16::from_le_bytes([s[0], s[1]]) as f32).collect();
    }
    let n = f.nb_samples.max(0) as usize * 2;
    if f.samples.is_null() { Vec::new() } else { unsafe { std::slice::from_raw_parts(f.samples, n) }.to_vec() }
}

impl Drop for Decoder {
    fn drop(&mut self) {
        if let Ok(l) = lib() {
            unsafe { (l.close)(self.0) };
        }
    }
}

/// Playback speed with the same pitch (FFmpeg's atempo over 48 kHz float stereo). Output frame n stands for
/// input frame n × tempo.
pub struct Tempo(*mut c_void);
// SAFETY: used by one thread at a time (the audio thread).
unsafe impl Send for Tempo {}

impl Tempo {
    /// `tempo`: 0.25 to 4.
    pub fn new(tempo: f64) -> Result<Tempo, String> {
        let l = lib()?;
        let mut p = std::ptr::null_mut();
        let r = unsafe { (l.tempo_open)(tempo, &mut p) };
        if r < 0 || p.is_null() {
            return Err(error(l, r));
        }
        Ok(Tempo(p))
    }

    /// Pushes interleaved stereo frames in and appends what comes out to `out`.
    pub fn process(&mut self, input: &[f32], out: &mut Vec<f32>) -> Result<(), String> {
        let l = lib()?;
        let mut o: *const f32 = std::ptr::null();
        let r = unsafe { (l.tempo_process)(self.0, input.as_ptr(), (input.len() / 2) as c_int, &mut o) };
        if r < 0 {
            return Err(error(l, r));
        }
        if r > 0 && !o.is_null() {
            out.extend_from_slice(unsafe { std::slice::from_raw_parts(o, r as usize * 2) });
        }
        Ok(())
    }
}

impl Drop for Tempo {
    fn drop(&mut self) {
        if let Ok(l) = lib() {
            unsafe { (l.tempo_close)(self.0) };
        }
    }
}

// --- Subtitles (lv_subs_* in looker_video.c) ---

/// `SubInfo::kind`: 1 is text Looker lays out (FFmpeg's decoders give every text format as ASS-tagged text);
/// ASS/SSA, which libass draws (`Subs::render`):
pub const SUB_ASS: c_int = 2;
/// Pictures placed on a canvas (PGS, VobSub, DVB).
pub const SUB_BITMAP: c_int = 3;

/// `LvSubInfo`: what was opened.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct SubInfo {
    pub kind: c_int,
    pub stream: c_int,
    /// Fonts the file carries, handed to libass.
    pub fonts: c_int,
    pub canvas_w: c_int,
    pub canvas_h: c_int,
    codec: [c_char; 32],
}

impl SubInfo {
    pub fn codec(&self) -> String {
        text(&self.codec)
    }
}

#[repr(C)]
struct LvSubRect {
    x: c_int,
    y: c_int,
    w: c_int,
    h: c_int,
    stride: c_int,
    bgra: *const u8,
}

#[repr(C)]
struct LvSubEvent {
    start_us: i64,
    end_us: i64,
    read_us: i64,
    text: *const c_char,
    n_rects: c_int,
    rects: *const LvSubRect,
    canvas_w: c_int,
    canvas_h: c_int,
    read_order: c_int,
}

#[repr(C)]
struct LvSubImage {
    x: c_int,
    y: c_int,
    w: c_int,
    h: c_int,
    stride: c_int,
    bgra: *const u8,
}

/// A bitmap subtitle's picture: premultiplied BGRA, placed at (x, y) on its event's canvas.
#[derive(Clone)]
pub struct SubRect {
    pub x: i32,
    pub y: i32,
    pub w: u32,
    pub h: u32,
    pub bgra: std::sync::Arc<Vec<u8>>,
}

/// One event read from a subtitle stream.
#[derive(Clone)]
pub struct SubEvent {
    pub start_us: i64,
    /// None: it lasts until the next event (PGS).
    pub end_us: Option<i64>,
    /// How far the reader had got: the time of the packet it came from.
    pub read_us: i64,
    /// Text: the ASS-tagged text; ASS: the packet (ReadOrder,Layer,...,Text).
    pub text: String,
    pub rects: Vec<SubRect>,
    /// What the rects are placed on (0 when the stream doesn't say: the video's size).
    pub canvas: (i32, i32),
    pub read_order: i32,
}

/// What `Subs::next` hands back.
pub enum SubRead {
    Event(SubEvent),
    End,
    /// `SubsInterrupter::interrupt` stopped it.
    Interrupted,
}

/// libass's picture of a moment: premultiplied BGRA `w × h` at (x, y) in the frame asked for.
pub struct SubImage {
    pub x: i32,
    pub y: i32,
    pub w: u32,
    pub h: u32,
    pub bgra: Vec<u8>,
}

/// An open subtitle stream (looker_video.c's LvSubs): `next` from one thread (the reader), `render` from another
/// (the UI); the shim keeps libass's track behind a lock between the two.
pub struct Subs(*mut c_void);
// SAFETY: the shim serialises what the two threads share (libass's track); reading is one thread's.
unsafe impl Send for Subs {}
unsafe impl Sync for Subs {}

/// Stops a `Subs::next` in progress from another thread.
pub struct SubsInterrupter(*mut c_void);
// SAFETY: lv_subs_interrupt only sets a flag; the owner keeps the stream alive while this exists.
unsafe impl Send for SubsInterrupter {}
unsafe impl Sync for SubsInterrupter {}

impl SubsInterrupter {
    pub fn interrupt(&self) {
        if let Ok(l) = lib() {
            unsafe { (l.subs_interrupt)(self.0) };
        }
    }
}

impl Subs {
    /// The video's embedded subtitle stream `stream` (a `Track::stream`), on the clock whose zero is `zero_us`
    /// in the file; or with `stream` None a binary sidecar file (VobSub's .idx). `fps`: for MicroDVD, or 0.
    pub fn open(path: &Path, stream: Option<i32>, zero_us: i64, fps: f64) -> Result<(Subs, SubInfo), String> {
        let l = lib()?;
        let mut p = std::ptr::null_mut();
        let mut info: SubInfo = unsafe { std::mem::zeroed() };
        let r = unsafe { (l.subs_open)(wide(path).as_ptr(), stream.unwrap_or(-1), zero_us, fps, &mut p, &mut info) };
        if r < 0 || p.is_null() {
            return Err(error(l, r));
        }
        Ok((Subs(p), info))
    }

    /// A text subtitle file already turned into UTF-8; `name` (the file's) helps pick the format.
    pub fn open_text(utf8: &[u8], name: &Path, fps: f64) -> Result<(Subs, SubInfo), String> {
        let l = lib()?;
        let mut p = std::ptr::null_mut();
        let mut info: SubInfo = unsafe { std::mem::zeroed() };
        let r = unsafe { (l.subs_open_mem)(utf8.as_ptr(), utf8.len() as c_int, wide(name).as_ptr(), fps, &mut p, &mut info) };
        if r < 0 || p.is_null() {
            return Err(error(l, r));
        }
        Ok((Subs(p), info))
    }

    pub fn interrupter(&self) -> SubsInterrupter {
        SubsInterrupter(self.0)
    }

    /// Reading goes on from the keyframe at or before `us`.
    pub fn seek(&self, us: i64) -> Result<(), String> {
        let l = lib()?;
        let r = unsafe { (l.subs_seek)(self.0, us) };
        if r < 0 { Err(error(l, r)) } else { Ok(()) }
    }

    pub fn next(&self) -> Result<SubRead, String> {
        let l = lib()?;
        let mut e: LvSubEvent = unsafe { std::mem::zeroed() };
        let r = unsafe { (l.subs_next)(self.0, &mut e) };
        const EOF: c_int = -541478725;
        if r == EOF {
            return Ok(SubRead::End);
        }
        if r == INTERRUPTED {
            return Ok(SubRead::Interrupted);
        }
        if r < 0 {
            return Err(error(l, r));
        }
        let text = if e.text.is_null() { String::new() } else { unsafe { std::ffi::CStr::from_ptr(e.text) }.to_string_lossy().into_owned() };
        let mut rects = Vec::new();
        if !e.rects.is_null() {
            for r in unsafe { std::slice::from_raw_parts(e.rects, e.n_rects.max(0) as usize) } {
                if r.w <= 0 || r.h <= 0 || r.bgra.is_null() {
                    continue;
                }
                let (w, h) = (r.w as usize, r.h as usize);
                let mut px = Vec::with_capacity(w * h * 4);
                for y in 0..h {
                    px.extend_from_slice(unsafe { std::slice::from_raw_parts(r.bgra.add(y * r.stride as usize), w * 4) });
                }
                rects.push(SubRect { x: r.x, y: r.y, w: w as u32, h: h as u32, bgra: std::sync::Arc::new(px) });
            }
        }
        Ok(SubRead::Event(SubEvent {
            start_us: e.start_us,
            end_us: (e.end_us >= 0).then_some(e.end_us),
            read_us: e.read_us,
            text,
            rects,
            canvas: (e.canvas_w, e.canvas_h),
            read_order: e.read_order,
        }))
    }

    /// libass's picture of moment `us` in a `w × h` frame showing a video `video_w × video_h`, its text scaled by
    /// `scale`. None when it's the same as the last call's; Some(None) when nothing shows.
    pub fn render(&self, us: i64, w: u32, h: u32, video_w: u32, video_h: u32, scale: f64) -> Result<Option<Option<SubImage>>, String> {
        let l = lib()?;
        let mut img: LvSubImage = unsafe { std::mem::zeroed() };
        let mut changed: c_int = 0;
        let r = unsafe { (l.subs_render)(self.0, us, w as c_int, h as c_int, video_w as c_int, video_h as c_int, scale, &mut changed, &mut img) };
        if r < 0 {
            return Err(error(l, r));
        }
        if changed == 0 {
            return Ok(None);
        }
        if img.bgra.is_null() || img.w <= 0 || img.h <= 0 {
            return Ok(Some(None));
        }
        let (iw, ih) = (img.w as usize, img.h as usize);
        let mut px = Vec::with_capacity(iw * ih * 4);
        for y in 0..ih {
            px.extend_from_slice(unsafe { std::slice::from_raw_parts(img.bgra.add(y * img.stride as usize), iw * 4) });
        }
        Ok(Some(Some(SubImage { x: img.x, y: img.y, w: iw as u32, h: ih as u32, bgra: px })))
    }
}

impl Drop for Subs {
    fn drop(&mut self) {
        if let Ok(l) = lib() {
            unsafe { (l.subs_close)(self.0) };
        }
    }
}

/// The stream's facts without decoding a picture.
pub fn probe(path: &Path) -> Result<Info, String> {
    let l = lib()?;
    let mut info = Info::zeroed();
    let r = unsafe { (l.probe)(wide(path).as_ptr(), &mut info) };
    if r < 0 {
        return Err(error(l, r));
    }
    Ok(info)
}

/// The first picture fitted to `box_w × box_h` (`(0, 0)` = full size), as a still.
pub fn decode(f: &IWICImagingFactory, path: &Path, box_w: u32, box_h: u32) -> Result<Decoded, String> {
    let l = lib()?;
    let mut info = Info::zeroed();
    let (mut w, mut h) = (0, 0);
    let mut px: *mut u8 = std::ptr::null_mut();
    let r = unsafe { (l.still)(wide(path).as_ptr(), box_w as c_int, box_h as c_int, &mut info, &mut w, &mut h, &mut px) };
    if r < 0 || px.is_null() || w <= 0 || h <= 0 {
        if !px.is_null() {
            unsafe { (l.free)(px as _) };
        }
        return Err(if r < 0 { error(l, r) } else { "no picture".into() });
    }
    let (w, h) = (w as u32, h as u32);
    let rgba = unsafe { std::slice::from_raw_parts(px, w as usize * h as usize * 4) };
    let converted = wic::from_rgba(f, rgba, w, h, w, h);
    unsafe { (l.free)(px as _) };
    let (dw, dh, bgra) = converted.map_err(|e| e.message().to_string())?;
    let taken = (info.created > 0).then(|| {
        let ticks = (info.created as u64 + 11_644_473_600) * 10_000_000;
        windows::Win32::Foundation::FILETIME { dwLowDateTime: ticks as u32, dwHighDateTime: (ticks >> 32) as u32 }
    });
    let native_w = (info.width.max(0) as u32).max(w);
    let native_h = (info.height.max(0) as u32).max(h);
    Ok(Decoded::still(dw, dh, bgra, native_w, native_h, taken))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wording() {
        assert_eq!(codec_label("prores", "HQ"), "ProRes 422 HQ");
        assert_eq!(codec_label("prores", "4444"), "ProRes 4444");
        assert_eq!(codec_label("dnxhd", "DNXHR HQX"), "DNxHR HQX");
        assert_eq!(codec_label("hevc", "Main 10"), "HEVC Main 10");
        assert_eq!(codec_label("vp9", ""), "VP9");
        assert_eq!(fps_text(30000.0 / 1001.0), "29.97");
        assert_eq!(fps_text(24000.0 / 1001.0), "23.976");
        assert_eq!(fps_text(25.0), "25");
        assert_eq!(duration_text(7_400_000), "0:07");
        assert_eq!(duration_text(754_000_000), "12:34");
        assert_eq!(duration_text(3_723_000_000), "1:02:03");
        let mut i = Info::zeroed();
        i.bit_depth = 10;
        i.primaries = 9;
        i.transfer = 18;
        i.audio_channels = 2;
        i.audio_rate = 48000;
        i.pix_fmt[..12].copy_from_slice(&b"yuv420p10le\0".map(|b| b as c_char));
        i.audio_codec[..4].copy_from_slice(&b"aac\0".map(|b| b as c_char));
        let f = facts(&i);
        assert!(f.contains(&("Format", "10-bit 4:2:0".into())), "{f:?}");
        assert!(f.contains(&("Colour", "Rec. 2020, HDR (HLG)".into())), "{f:?}");
        assert!(f.contains(&("Audio", "AAC, stereo, 48 kHz".into())), "{f:?}");
    }

    /// Prints the pictures `Decoder` hands out after an exact seek to `LOOKER_SEEK` µs in `LOOKER_DECODE`.
    #[test]
    #[ignore]
    fn seek_sequence() {
        let p = std::path::PathBuf::from(std::env::var("LOOKER_DECODE").expect("set LOOKER_DECODE"));
        let to: i64 = std::env::var("LOOKER_SEEK").ok().and_then(|v| v.parse().ok()).unwrap_or(25_000);
        let (mut d, _) = Decoder::open(&p, std::ptr::null_mut(), false).unwrap();
        let ahead: usize = std::env::var("LOOKER_AHEAD").ok().and_then(|v| v.parse().ok()).unwrap_or(3);
        for _ in 0..ahead {
            if let Ok(Item::Picture(f)) = d.next() {
                println!("start: picture {}", f.pts_us);
                if f.layout == PLANAR && f.nplanes == 4 {
                    let a = unsafe { std::slice::from_raw_parts(f.data[3] as *const u16, 8) };
                    let y = unsafe { std::slice::from_raw_parts(f.data[0] as *const u16, 8) };
                    println!("  alpha {a:?} luma {y:?} depth {}", f.bit_depth);
                }
            }
        }
        d.seek(to, true).unwrap();
        let mut n = 0;
        while n < 3 {
            match d.next().unwrap() {
                Item::Picture(f) => {
                    println!("after seek {to}: picture {}", f.pts_us);
                    n += 1;
                }
                Item::Audio(t, _) => println!("after seek {to}: audio {t}"),
                Item::End | Item::Interrupted => break,
            }
        }
    }

    #[test]
    fn frame_matches_the_c_layout() {
        // sizeof(LvFrame) on x64: 15 ints with the padding before each pointer, the pointers, 4 + 1 more ints.
        assert_eq!(std::mem::size_of::<LvFrame>(), 160);
        assert_eq!(std::mem::offset_of!(LvFrame, texture), 72);
        assert_eq!(std::mem::offset_of!(LvFrame, data), 88);
        assert_eq!(std::mem::offset_of!(LvFrame, samples), 136);
    }

    #[test]
    fn info_matches_the_c_layout() {
        // The fields in order with the 4 bytes of padding before duration_us, then the four strings:
        // sizeof(LvInfo) in looker_video.c.
        assert_eq!(std::mem::size_of::<Info>(), 4 * 4 + 4 + 4 + 8 + 4 * 2 + 8 + 4 * 8 + 8 + 32 + 48 + 32 + 32);
    }

    /// For each video in `LOOKER_VIDEOS`: its chapters, the spacing of its first pictures (an interlaced one
    /// deinterlaced plays a picture per field: 20 ms apart at 25i), and with passthrough allowed for every codec,
    /// whether the sound comes as IEC 61937 and how much of it.
    /// `LOOKER_VIDEOS=<folder> cargo test --release -- --ignored --nocapture chapters_fields_passthrough`
    #[test]
    #[ignore]
    fn chapters_fields_passthrough() {
        let Some(d) = std::env::var_os("LOOKER_VIDEOS") else { return };
        let mut v: Vec<_> = std::fs::read_dir(d).unwrap().flatten().map(|e| e.path()).filter(|p| crate::format::of_path(p) == crate::format::Format::Video).collect();
        v.sort();
        for p in &v {
            let name = p.file_name().unwrap().to_string_lossy().into_owned();
            let all = PASS_AC3 | PASS_EAC3 | PASS_DTS | PASS_TRUEHD;
            let Ok((mut dec, info)) = Decoder::open_playing(p, std::ptr::null_mut(), all, None, None) else {
                println!("FAIL {name}");
                continue;
            };
            let chapters: Vec<String> = dec.chapters().iter().map(|c| format!("{:.1}-{:.1} {:?}", c.start_us as f64 / 1e6, c.end_us as f64 / 1e6, c.title)).collect();
            let (mut pts, mut bits, mut blocks) = (Vec::new(), 0usize, 0);
            for _ in 0..400 {
                match dec.next() {
                    Ok(Item::Picture(f)) if pts.len() < 6 => pts.push(f.pts_us),
                    Ok(Item::Audio(_, s)) => {
                        bits += s.len();
                        blocks += 1;
                    }
                    Ok(Item::End) | Err(_) => break,
                    _ => {}
                }
            }
            let gaps: Vec<i64> = pts.windows(2).map(|w| (w[1] - w[0]) / 1000).collect();
            println!(
                "{name}: {} fps; chapters {chapters:?}; picture gaps {gaps:?} ms; passthrough {:?}, {blocks} audio blocks, {bits} samples",
                info.fps().unwrap_or(0.0),
                dec.passthrough()
            );
        }
    }

    /// The first picture of `LOOKER_DECODE` (or of every video in `LOOKER_VIDEOS`, a folder), written beside it
    /// as a PNG, with the stream's facts and timings.
    /// `LOOKER_CODECS=%LOCALAPPDATA%\Looker\codecs LOOKER_DECODE=x.mov cargo test --release -- --ignored --nocapture first_frames`
    #[test]
    #[ignore]
    fn first_frames() {
        use windows::Win32::Graphics::Imaging::CLSID_WICImagingFactory2;
        use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx};
        let f: IWICImagingFactory = unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
            CoCreateInstance(&CLSID_WICImagingFactory2, None, CLSCTX_INPROC_SERVER).unwrap()
        };
        let mut paths = Vec::new();
        if let Some(p) = std::env::var_os("LOOKER_DECODE") {
            paths.push(std::path::PathBuf::from(p));
        }
        if let Some(d) = std::env::var_os("LOOKER_VIDEOS") {
            let mut v: Vec<_> = std::fs::read_dir(d).unwrap().flatten().map(|e| e.path()).filter(|p| crate::format::of_path(p) == crate::format::Format::Video).collect();
            v.sort();
            paths.extend(v);
        }
        assert!(!paths.is_empty(), "set LOOKER_DECODE or LOOKER_VIDEOS");
        let t = std::time::Instant::now();
        lib().unwrap();
        println!("looker_video.dll loaded in {:.1} ms", t.elapsed().as_secs_f64() * 1000.0);
        for p in &paths {
            let name = p.file_name().unwrap().to_string_lossy().into_owned();
            let t = std::time::Instant::now();
            let info = match probe(p) {
                Ok(i) => i,
                Err(e) => {
                    println!("FAIL {name}: {e}");
                    continue;
                }
            };
            let probe_ms = t.elapsed().as_secs_f64() * 1000.0;
            let t = std::time::Instant::now();
            match decode(&f, p, 1280, 1280) {
                Ok(d) => {
                    let ms = t.elapsed().as_secs_f64() * 1000.0;
                    println!(
                        "ok   {name}: {} {} {}x{} rot {} {:.3} fps {}-bit {} trc {} audio {} {}ch, {:.1} s; still {}x{} in {ms:.0} ms (probe {probe_ms:.0} ms)",
                        codec_name(&info.codec()),
                        info.profile(),
                        info.width,
                        info.height,
                        info.rotation,
                        info.fps().unwrap_or(0.0),
                        info.bit_depth,
                        info.pix_fmt(),
                        info.transfer,
                        info.audio_codec(),
                        info.audio_channels,
                        info.duration_us as f64 / 1e6,
                        d.width,
                        d.height,
                    );
                    let out = p.with_extension(format!("{}.png", p.extension().unwrap().to_string_lossy()));
                    let out = std::env::temp_dir().join(out.file_name().unwrap());
                    wic::save_png(&f, &out, d.width, d.height, &d.frames[0].pixels).unwrap();
                }
                Err(e) => println!("FAIL {name}: {e}"),
            }
        }
    }

    /// atempo gives n / tempo frames for n in, at every speed the menu offers.
    /// `LOOKER_CODECS=%LOCALAPPDATA%\Looker\codecs cargo test --release -- --ignored tempo_lengths`
    #[test]
    #[ignore]
    fn tempo_lengths() {
        let input: Vec<f32> = (0..AUDIO_RATE as usize * 4).flat_map(|i| {
            let v = (i as f32 * 440.0 * std::f32::consts::TAU / AUDIO_RATE as f32).sin() * 0.5;
            [v, v]
        }).collect();
        for tempo in [0.25, 0.5, 0.75, 1.25, 1.5, 2.0] {
            let mut t = Tempo::new(tempo).unwrap();
            let mut out = Vec::new();
            for chunk in input.chunks(4096 * 2) {
                t.process(chunk, &mut out).unwrap();
            }
            let want = input.len() as f64 / 2.0 / tempo;
            let got = out.len() as f64 / 2.0;
            println!("{tempo}x: {got} frames out, {want} expected");
            // Less what the filter still holds (a window a stage: up to ~0.2 s of input).
            assert!(got <= want + 1.0 && got > want - AUDIO_RATE as f64 * 0.25 / tempo, "{tempo}x: {got} vs {want}");
        }
    }
}
