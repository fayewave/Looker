//! Which image format a file is, from its leading bytes (port of `Imaging/FormatSniffer.cs`,
//! `ImageFormat.cs` and `SupportedFormats.cs`). Content beats extension because extensions lie; the extension
//! decides only for formats with no reliable signature (TGA) and for the TIFF-based camera RAWs.

use std::path::Path;

/// Bytes to read for a confident sniff (covers ISO-BMFF ftyp brands).
pub const HEADER_SIZE: usize = 64;

#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub enum Format {
    Unknown,
    Jpeg,
    Png,
    Bmp,
    Gif,
    Tiff,
    Webp,
    Ico,
    Heif,
    Avif,
    Svg,
    Psd,
    JpegXl,
    Tga,
    Dds,
    Raw,
    JpegXr,
    Jpeg2000,
    Cur,
    Pcx,
    Pnm,
    Xcf,
    Qoi,
    /// OpenEXR: linear float, HDR (`fallback::float_image`).
    Exr,
    /// Radiance RGBE (.hdr): linear, HDR.
    Radiance,
    Pdf,
    /// Wavefront material library (text; drawn as a sheet of material cards).
    Mtl,
    /// Plain text (and Source materials): the editor on screen (`app/textview.rs`).
    Text,
    /// Valve Texture Format, Source engine textures (`imaging/vtf.rs`).
    Vtf,
    /// A 3D model: OBJ, STL, PLY, 3MF, glTF/GLB (rendered with Direct3D, see model/).
    Model,
    /// A video (FFmpeg through looker_video.dll, see imaging/video.rs).
    Video,
    /// An audio file (FFmpeg too: imaging/audio.rs; played with its waveform, app/audio.rs).
    Audio,
}

#[cfg(test)]
pub const ALL: &[Format] = &[
    Format::Jpeg,
    Format::Png,
    Format::Bmp,
    Format::Gif,
    Format::Tiff,
    Format::Webp,
    Format::Ico,
    Format::Heif,
    Format::Avif,
    Format::Svg,
    Format::Psd,
    Format::JpegXl,
    Format::Tga,
    Format::Dds,
    Format::Raw,
    Format::JpegXr,
    Format::Jpeg2000,
    Format::Cur,
    Format::Pcx,
    Format::Pnm,
    Format::Xcf,
    Format::Qoi,
    Format::Exr,
    Format::Radiance,
    Format::Pdf,
    Format::Mtl,
    Format::Text,
    Format::Vtf,
    Format::Model,
    Format::Video,
    Format::Audio,
];

/// Every extension Looker opens (lower case, no dot). Must equal the manifest FTA list.
pub const EXTENSIONS: &[&str] = &[
    "jpg", "jpeg", "jpe", "jfif", "png", "apng", "bmp", "dib", "gif", "tif", "tiff", "webp", "ico", "cur", "jxr", "wdp",
    "hdp", "heic", "heif", "hif", "heics", "avif", "avifs", "jxl", "jp2", "j2k", "jpf", "jpx", "j2c", "svg", "pdf", "psd",
    "psb", "tga", "icb", "vda", "vst", "dds", "pcx", "pnm", "ppm", "pgm", "pbm", "xcf", "qoi", "svgz", "exr", "hdr",
    // Camera RAW. No Sigma .x3f: libraw dropped Foveon and no WIC codec reads it.
    "cr2", "cr3", "crw", "nef", "nrw", "arw", "sr2", "srf", "dng", "orf", "raf", "rw2", "pef", "srw", "3fr", "fff", "rwl",
    "iiq", "mrw", "dcr", "kdc", "erf", "mef",
    // 3D models and material libraries.
    "obj", "stl", "ply", "3mf", "gltf", "glb", "mtl",
    // Source engine: Hammer maps and materials.
    "vmf", "vmt", "vtf",
    // Text: plain, Markdown, logs and settings, Source's configs and model scripts, data, subtitles, NFOs.
    "txt", "md", "markdown", "log", "ini", "cfg", "vdf", "qc", "json", "xml", "csv", "tsv", "yaml", "yml", "toml", "srt", "vtt",
    "nfo", "ass", "ssa",
    // Video.
    "mp4", "m4v", "mov", "mkv", "webm", "avi", "wmv", "mxf", "mts", "m2ts", "mpg", "mpeg", "3gp", "ts",
    "flv", "ogv", "m2v", "vob",
    // More video, for VLC parity (2026-11-11): the containers above under other names, then RealMedia, DV, NSV,
    // NuppelVideo, Windows Media Center, Y4M, GXF, IVF/OBU, raw H.264/HEVC streams, Bink, Smacker.
    "asf", "f4v", "3g2", "divx", "xvid", "ogm", "m1v", "mpv", "mpe", "m2p", "mod", "tod", "m2t", "trp", "tp", "dvr-ms",
    "rm", "rmvb", "dv", "nsv", "nuv", "wtv", "y4m", "gxf", "ivf", "obu", "264", "h264", "265", "hevc", "bik", "smk",
    // Audio.
    "mp3", "m4a", "m4b", "aac", "flac", "wav", "aif", "aiff", "ogg", "oga", "opus", "wma", "mka", "wv", "ape", "caf",
    // More audio (VLC parity): MPEG layer 1/2, Wave64, WebM audio, raw Dolby/DTS streams, Musepack, TTA, TAK, Shorten,
    // AMR, Sun/NeXT, Creative Voice, GSM, xWMA, DSD, RealAudio.
    "mp1", "mp2", "mpa", "w64", "weba", "ac3", "eac3", "dts", "thd", "mlp", "mpc", "tta", "tak", "shn", "amr", "awb",
    "au", "snd", "voc", "gsm", "xwma", "dsf", "dff", "ra",
];

fn ext_of(path: &Path) -> Option<String> {
    path.extension().and_then(|e| e.to_str()).map(|e| e.to_ascii_lowercase())
}

/// What the file is by name, as `from_extension`, except that a `.ts` is looked into once: TypeScript is text, a
/// transport stream video. An online-only cloud file is never read (it stays a video by name).
pub fn of_path(path: &Path) -> Format {
    let ext = ext_of(path);
    let f = from_extension(ext.as_deref());
    if ext.as_deref() == Some("ts") && ts_is_text(path) { Format::Text } else { f }
}

fn ts_is_text(path: &Path) -> bool {
    use std::collections::HashMap;
    use std::os::windows::fs::MetadataExt;
    use std::time::{Duration, Instant};
    type Seen = HashMap<std::path::PathBuf, (Instant, u64, bool)>;
    thread_local! {
        static SEEN: std::cell::RefCell<Seen> = std::cell::RefCell::new(HashMap::new());
    }
    // Asked every frame for each row on screen: answer from memory, looking at the file again after a second.
    let now = Instant::now();
    let cached = SEEN.with(|m| m.borrow().get(path).copied());
    if let Some((_, _, t)) = cached.filter(|(at, _, _)| now - *at < Duration::from_secs(1)) {
        return t;
    }
    let Ok(meta) = std::fs::metadata(path) else { return false };
    // FILE_ATTRIBUTE_OFFLINE, RECALL_ON_OPEN, RECALL_ON_DATA_ACCESS.
    let t = if meta.file_attributes() & (0x1000 | 0x40000 | 0x400000) != 0 {
        false
    } else {
        let stamp = meta.last_write_time() ^ meta.len();
        match cached {
            Some((_, s, t)) if s == stamp => t,
            _ => sniff_file(path) == Format::Text,
        }
    };
    let stamp = meta.last_write_time() ^ meta.len();
    SEEN.with(|m| {
        let mut m = m.borrow_mut();
        if m.len() > 4096 {
            m.clear();
        }
        m.insert(path.to_path_buf(), (now, stamp, t));
    });
    t
}

pub fn is_supported(path: &Path) -> bool {
    ext_of(path).is_some_and(|e| EXTENSIONS.contains(&e.as_str()))
}

pub fn from_extension(ext: Option<&str>) -> Format {
    let Some(ext) = ext else { return Format::Unknown };
    match ext.trim_start_matches('.').to_ascii_lowercase().as_str() {
        "jpg" | "jpeg" | "jpe" | "jfif" => Format::Jpeg,
        "png" | "apng" => Format::Png,
        "gif" => Format::Gif,
        "bmp" | "dib" => Format::Bmp,
        "tif" | "tiff" => Format::Tiff,
        "webp" => Format::Webp,
        "ico" => Format::Ico,
        "cur" => Format::Cur,
        "jxr" | "wdp" | "hdp" => Format::JpegXr,
        "jp2" | "j2k" | "jpf" | "jpx" | "j2c" => Format::Jpeg2000,
        "heic" | "heif" | "hif" | "heics" => Format::Heif,
        "avif" | "avifs" => Format::Avif,
        "svg" | "svgz" => Format::Svg,
        "exr" => Format::Exr,
        "hdr" => Format::Radiance,
        "pdf" => Format::Pdf,
        "psd" | "psb" => Format::Psd,
        "jxl" => Format::JpegXl,
        "dds" => Format::Dds,
        "tga" | "icb" | "vda" | "vst" => Format::Tga,
        "pcx" => Format::Pcx,
        "pnm" | "ppm" | "pgm" | "pbm" => Format::Pnm,
        "xcf" => Format::Xcf,
        "qoi" => Format::Qoi,
        "mtl" => Format::Mtl,
        // Source materials are KeyValues text: read and edited as text (Faye, 2026-11-11).
        "txt" | "vmt" | "md" | "markdown" | "log" | "ini" | "cfg" | "vdf" | "qc" | "json" | "xml" | "csv" | "tsv" | "yaml" | "yml"
        | "toml" | "srt" | "vtt" | "nfo" | "ass" | "ssa" => Format::Text,
        "vtf" => Format::Vtf,
        "obj" | "stl" | "ply" | "3mf" | "gltf" | "glb" | "vmf" => Format::Model,
        "mp4" | "m4v" | "mov" | "mkv" | "webm" | "avi" | "wmv" | "mxf" | "mts" | "m2ts" | "mpg" | "mpeg" | "3gp" | "ts" | "flv" | "ogv"
        | "m2v" | "vob" | "asf" | "f4v" | "3g2" | "divx" | "xvid" | "ogm" | "m1v" | "mpv" | "mpe" | "m2p" | "mod" | "tod"
        | "m2t" | "trp" | "tp" | "dvr-ms" | "rm" | "rmvb" | "dv" | "nsv" | "nuv" | "wtv" | "y4m" | "gxf" | "ivf" | "obu"
        | "264" | "h264" | "265" | "hevc" | "bik" | "smk" => Format::Video,
        "mp3" | "m4a" | "m4b" | "aac" | "flac" | "wav" | "aif" | "aiff" | "ogg" | "oga" | "opus" | "wma" | "mka" | "wv" | "ape"
        | "caf" | "mp1" | "mp2" | "mpa" | "w64" | "weba" | "ac3" | "eac3" | "dts" | "thd" | "mlp" | "mpc" | "tta" | "tak"
        | "shn" | "amr" | "awb" | "au" | "snd" | "voc" | "gsm" | "xwma" | "dsf" | "dff" | "ra" => Format::Audio,
        "cr2" | "cr3" | "crw" | "nef" | "nrw" | "arw" | "sr2" | "srf" | "dng" | "orf" | "raf" | "rw2" | "pef" | "srw"
        | "3fr" | "fff" | "rwl" | "iiq" | "mrw" | "dcr" | "kdc" | "erf" | "mef" => Format::Raw,
        _ => Format::Unknown,
    }
}

fn starts(h: &[u8], prefix: &[u8]) -> bool {
    h.len() >= prefix.len() && &h[..prefix.len()] == prefix
}

fn ascii(h: &[u8], offset: usize, text: &str) -> bool {
    h.len() >= offset + text.len() && &h[offset..offset + text.len()] == text.as_bytes()
}

/// At least one image, and the first directory entry's reserved byte is 0. With only a short header to go
/// on (tests), the type word alone decides.
fn icon_directory_plausible(h: &[u8]) -> bool {
    if h.len() < 6 {
        return true;
    }
    let count = u16::from_le_bytes([h[4], h[5]]);
    count >= 1 && (h.len() < 10 || h[9] == 0)
}

pub fn sniff(h: &[u8], ext: Option<&str>) -> Format {
    if starts(h, &[0xFF, 0xD8, 0xFF]) {
        return Format::Jpeg;
    }
    if starts(h, &[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]) {
        return Format::Png;
    }
    if ascii(h, 0, "GIF87a") || ascii(h, 0, "GIF89a") {
        return Format::Gif;
    }
    if ascii(h, 0, "BM") {
        return Format::Bmp;
    }
    // Camera RAW with its own signature: Olympus ORF, Fujifilm RAF, Panasonic/Leica RW2/RWL ("IIU\0"),
    // Minolta MRW ("\0MRM"), Canon CRW (CIFF: "II\x1a\0\0\0HEAPCCDR").
    if ascii(h, 0, "IIRO")
        || ascii(h, 0, "IIRS")
        || ascii(h, 0, "MMOR")
        || ascii(h, 0, "FUJIFILMCCD-RAW")
        || starts(h, &[0x49, 0x49, 0x55, 0x00])
        || starts(h, &[0x00, 0x4D, 0x52, 0x4D])
        || (starts(h, &[0x49, 0x49, 0x1A, 0x00]) && ascii(h, 6, "HEAPCCDR"))
    {
        return Format::Raw;
    }
    if starts(h, &[0x49, 0x49, 0xBC]) {
        return Format::JpegXr;
    }
    // TIFF. Most RAW containers (DNG, NEF, ARW, CR2...) are TIFF underneath and a TIFF decoder would hand
    // back IFD0, usually a small preview. CR2 says so at offset 8; the rest only by extension.
    if starts(h, &[0x49, 0x49, 0x2A, 0x00]) || starts(h, &[0x4D, 0x4D, 0x00, 0x2A]) {
        return if ascii(h, 8, "CR") || from_extension(ext) == Format::Raw { Format::Raw } else { Format::Tiff };
    }
    if ascii(h, 0, "RIFF") && ascii(h, 8, "WEBP") {
        return Format::Webp;
    }
    if ascii(h, 4, "ftyp") {
        if ascii(h, 8, "crx ") {
            return Format::Raw;
        }
        if ascii(h, 8, "avif") || ascii(h, 8, "avis") {
            return Format::Avif;
        }
        if ["heic", "heix", "heif", "hevc", "mif1", "msf1"].iter().any(|b| ascii(h, 8, b)) {
            return Format::Heif;
        }
        // Audio-only MP4: iTunes' brands, or a general one (isom, mp42) named .m4a.
        if ["M4A ", "M4B ", "M4P ", "F4A "].iter().any(|b| ascii(h, 8, b)) || from_extension(ext) == Format::Audio {
            return Format::Audio;
        }
        // Every other ISO-BMFF brand is a movie: isom, mp41/mp42, M4V, qt, 3gp4...
        return Format::Video;
    }
    // Audio files: WAV (and RF64), AIFF, FLAC, Ogg, MP3 with an ID3 tag, WavPack, Monkey's Audio, CAF; Matroska and
    // ASF only by name (.mka, .wma: the same containers as video).
    if (ascii(h, 0, "RIFF") || ascii(h, 0, "RF64")) && ascii(h, 8, "WAVE")
        || (ascii(h, 0, "FORM") && (ascii(h, 8, "AIFF") || ascii(h, 8, "AIFC")))
        || ascii(h, 0, "fLaC")
        || (ascii(h, 0, "OggS") && from_extension(ext) != Format::Video)
        || ascii(h, 0, "ID3")
        || ascii(h, 0, "wvpk")
        || ascii(h, 0, "MAC ")
        || ascii(h, 0, "caff")
        || ((starts(h, &[0x1A, 0x45, 0xDF, 0xA3]) || starts(h, &[0x30, 0x26, 0xB2, 0x75, 0x8E, 0x66, 0xCF, 0x11]))
            && from_extension(ext) == Format::Audio)
    {
        return Format::Audio;
    }
    // Video containers: Matroska/WebM (EBML), AVI, ASF/WMV, MXF (partition pack key), MPEG program stream, and a
    // QuickTime movie old enough to start with an atom other than ftyp.
    if starts(h, &[0x1A, 0x45, 0xDF, 0xA3])
        || (ascii(h, 0, "RIFF") && ascii(h, 8, "AVI "))
        || starts(h, &[0x30, 0x26, 0xB2, 0x75, 0x8E, 0x66, 0xCF, 0x11])
        || starts(h, &[0x06, 0x0E, 0x2B, 0x34, 0x02, 0x05, 0x01, 0x01])
        || starts(h, &[0, 0, 1, 0xBA])
        || starts(h, &[0, 0, 1, 0xB3])
        || ascii(h, 0, "FLV")
        || ascii(h, 0, "OggS")
        || (["moov", "mdat", "wide", "pnot"].iter().any(|a| ascii(h, 4, a)) && from_extension(ext) == Format::Video)
    {
        return Format::Video;
    }
    if ascii(h, 0, "8BPS") {
        return Format::Psd;
    }
    if starts(h, &[0xFF, 0x0A]) || starts(h, &[0, 0, 0, 0x0C, b'J', b'X', b'L', b' ', 0x0D, 0x0A, 0x87, 0x0A]) {
        return Format::JpegXl;
    }
    if ascii(h, 0, "DDS ") {
        return Format::Dds;
    }
    // ICO / CUR: reserved 0, type 1 or 2, then an image count. An uncompressed true-colour TGA starts
    // 00 00 02 00 too (no ID, no palette, type 2), so also require a plausible count and first entry.
    if (starts(h, &[0, 0, 1, 0]) || starts(h, &[0, 0, 2, 0])) && icon_directory_plausible(h) {
        return if h[2] == 1 { Format::Ico } else { Format::Cur };
    }
    if starts(h, &[0, 0, 0, 0x0C, b'j', b'P', b' ', b' ', 0x0D, 0x0A, 0x87, 0x0A]) || starts(h, &[0xFF, 0x4F, 0xFF, 0x51]) {
        return Format::Jpeg2000;
    }
    if ascii(h, 0, "gimp xcf ") {
        return Format::Xcf;
    }
    if ascii(h, 0, "qoif") {
        return Format::Qoi;
    }
    if h.len() >= 3 && h[0] == 0x0A && h[1] <= 5 && h[2] == 1 {
        return Format::Pcx;
    }
    if h.len() >= 3 && h[0] == b'P' && (b'1'..=b'6').contains(&h[1]) && matches!(h[2], b' ' | b'\n' | b'\r' | b'\t') {
        return Format::Pnm;
    }
    if starts(h, b"VTF ") {
        return Format::Vtf;
    }
    if ascii(h, 0, "%PDF-") {
        return Format::Pdf;
    }
    if starts(h, &[0x76, 0x2F, 0x31, 0x01]) {
        return Format::Exr;
    }
    if ascii(h, 0, "#?RADIANCE") || ascii(h, 0, "#?RGBE") {
        return Format::Radiance;
    }
    // Gzip: an SVGZ by name (nothing else Looker opens is a bare gzip).
    if starts(h, &[0x1F, 0x8B]) && ext.is_some_and(|e| e.trim_start_matches('.').eq_ignore_ascii_case("svgz")) {
        return Format::Svg;
    }
    // A .ts without an MPEG transport stream's sync byte (0x47, at 0 or after a 4-byte timecode) is TypeScript.
    if !h.is_empty()
        && h[0] != 0x47
        && h.get(4) != Some(&0x47)
        && ext.is_some_and(|e| e.trim_start_matches('.').eq_ignore_ascii_case("ts"))
    {
        return Format::Text;
    }
    let s = if starts(h, &[0xEF, 0xBB, 0xBF]) { &h[3..] } else { h };
    // XML that is named as text (.xml, .nfo...) stays text; SVG's own markup is XML too.
    if (ascii(s, 0, "<?xml") || ascii(s, 0, "<svg") || ascii(s, 0, "<!--")) && from_extension(ext) != Format::Text {
        return Format::Svg;
    }
    // A bare MPEG audio or ADTS frame (an MP3 or AAC without a tag): 11 sync bits.
    if h.len() >= 2 && h[0] == 0xFF && h[1] & 0xE0 == 0xE0 {
        return Format::Audio;
    }
    // .mod is a camcorder's MPEG program stream (caught above) or a tracker module, which Looker doesn't play.
    if !h.is_empty() && ext.is_some_and(|e| e.trim_start_matches('.').eq_ignore_ascii_case("mod")) {
        return Format::Unknown;
    }
    from_extension(ext)
}

/// Reads the header and sniffs. Never touches an online-only cloud file (the caller checks that first).
pub fn sniff_file(path: &Path) -> Format {
    use std::io::Read;
    let mut buf = [0u8; HEADER_SIZE];
    let n = std::fs::File::open(path).and_then(|mut f| f.read(&mut buf)).unwrap_or(0);
    sniff(&buf[..n], ext_of(path).as_deref())
}

/// User-facing name for the status row and info card, e.g. "JPEG", "HEIC", "RAW (NEF)".
pub fn display_name(format: Format, path: Option<&Path>) -> Option<String> {
    let ext = path.and_then(ext_of).unwrap_or_default().to_ascii_uppercase();
    let s = match format {
        Format::Unknown => return None,
        Format::Jpeg => "JPEG",
        Format::Png => if ext == "APNG" { "APNG" } else { "PNG" },
        Format::Bmp => "BMP",
        Format::Gif => "GIF",
        Format::Tiff => "TIFF",
        Format::Webp => "WebP",
        Format::Ico => "ICO",
        Format::Cur => "CUR",
        Format::JpegXr => "JPEG XR",
        Format::Jpeg2000 => "JPEG 2000",
        Format::Heif => match ext.as_str() {
            "HEIC" | "HEICS" => "HEIC",
            "HIF" => "HIF",
            _ => "HEIF",
        },
        Format::Avif => "AVIF",
        Format::Svg => if ext == "SVGZ" { "SVGZ" } else { "SVG" },
        Format::Psd => if ext == "PSB" { "PSB" } else { "PSD" },
        Format::JpegXl => "JPEG XL",
        Format::Tga => "TGA",
        Format::Dds => "DDS",
        Format::Pcx => "PCX",
        Format::Pnm => match ext.as_str() {
            "PPM" | "PGM" | "PBM" => return Some(ext),
            _ => "PNM",
        },
        Format::Xcf => "XCF",
        Format::Qoi => "QOI",
        Format::Exr => "OpenEXR",
        Format::Radiance => "Radiance HDR",
        Format::Raw => return Some(if ext.is_empty() { "RAW".into() } else { format!("RAW ({ext})") }),
        Format::Pdf => "PDF",
        Format::Text => match ext.as_str() {
            "TXT" | "" => "Text",
            "MD" | "MARKDOWN" => "Markdown",
            e => return Some(e.to_string()),
        },
        Format::Vtf => "VTF",
        Format::Mtl => "MTL",
        Format::Model => match ext.as_str() {
            "GLTF" => "glTF",
            "" => "3D model",
            e => return Some(e.to_string()),
        },
        Format::Video => return Some(if ext.is_empty() { "Video".into() } else { ext }),
        Format::Audio => return Some(if ext.is_empty() { "Audio".into() } else { ext }),
    };
    Some(s.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cat(parts: &[&[u8]]) -> Vec<u8> {
        parts.concat()
    }

    #[test]
    fn detects_by_magic() {
        let cases: &[(&[u8], Format)] = &[
            (&[0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x10], Format::Jpeg),
            (&[0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A], Format::Png),
            (b"GIF89a....", Format::Gif),
            (b"BM....", Format::Bmp),
            (&[0x49, 0x49, 0x2A, 0x00], Format::Tiff),
            (&[0x4D, 0x4D, 0x00, 0x2A], Format::Tiff),
            (&[0x49, 0x49, 0xBC, 0x01, 0x20], Format::JpegXr),
            (&[0x00, 0x00, 0x02, 0x00, 0x01], Format::Cur),
            (&[0x00, 0x00, 0x01, 0x00, 0x01], Format::Ico),
            (&[0, 0, 0, 0x0C, 0x6A, 0x50, 0x20, 0x20, 0x0D, 0x0A, 0x87, 0x0A], Format::Jpeg2000),
            (&[0xFF, 0x4F, 0xFF, 0x51, 0x00], Format::Jpeg2000),
            (b"gimp xcf file\0", Format::Xcf),
            (b"qoif....", Format::Qoi),
            (&[0x0A, 0x05, 0x01, 0x08], Format::Pcx),
            (b"P6\n1920 1280\n255\n", Format::Pnm),
            (b"P1 8 8\n", Format::Pnm),
            (b"Photo notes", Format::Unknown),
            (b"8BPS....", Format::Psd),
            (&[0xFF, 0x0A], Format::JpegXl),
            (b"<svg xmlns=\"http://www.w3.org/2000/svg\">", Format::Svg),
            (b"<?xml version=\"1.0\"?><svg>", Format::Svg),
            (b"%PDF-1.7\n", Format::Pdf),
            (&[0xAA, 0xBB, 0xCC, 0xDD], Format::Unknown),
        ];
        for (h, f) in cases {
            assert_eq!(sniff(h, None), *f, "{h:?}");
        }
        assert_eq!(sniff(&cat(&[b"RIFF", &[0, 0, 0, 0], b"WEBP"]), None), Format::Webp);
        assert_eq!(sniff(&cat(&[&[0, 0, 0, 0x18], b"ftyp", b"heic"]), None), Format::Heif);
        assert_eq!(sniff(&cat(&[&[0, 0, 0, 0x18], b"ftyp", b"avif"]), None), Format::Avif);
        assert_eq!(sniff(&cat(&[&[0xEF, 0xBB, 0xBF], b"<svg>"]), None), Format::Svg);
    }

    #[test]
    fn video_containers() {
        for brand in [&b"isom"[..], b"mp42", b"qt  ", b"M4V ", b"3gp4"] {
            assert_eq!(sniff(&cat(&[&[0, 0, 0, 0x20], b"ftyp", brand]), None), Format::Video, "{brand:?}");
        }
        assert_eq!(sniff(&[0x1A, 0x45, 0xDF, 0xA3, 0x9F], None), Format::Video);
        assert_eq!(sniff(&cat(&[b"RIFF", &[0, 0, 0, 0], b"AVI LIST"]), None), Format::Video);
        assert_eq!(sniff(&[0x30, 0x26, 0xB2, 0x75, 0x8E, 0x66, 0xCF, 0x11, 0xA6], None), Format::Video);
        assert_eq!(sniff(&[0x06, 0x0E, 0x2B, 0x34, 0x02, 0x05, 0x01, 0x01, 0x0D], None), Format::Video);
        assert_eq!(sniff(&[0, 0, 1, 0xBA, 0x44], None), Format::Video);
        assert_eq!(sniff(&cat(&[&[0, 0, 0, 8], b"wide"]), Some("mov")), Format::Video);
        // An image keeps its brand, and a stray atom name means nothing without a video extension.
        assert_eq!(sniff(&cat(&[&[0, 0, 0, 0x18], b"ftyp", b"heic"]), Some("mp4")), Format::Heif);
        assert_eq!(sniff(&cat(&[&[0, 0, 0, 8], b"mdat"]), Some("dat")), Format::Unknown);
        assert_eq!(display_name(Format::Video, Some(Path::new("clip.mov"))).as_deref(), Some("MOV"));
    }

    #[test]
    fn audio_files() {
        for brand in [&b"M4A "[..], b"M4B "] {
            assert_eq!(sniff(&cat(&[&[0, 0, 0, 0x20], b"ftyp", brand]), None), Format::Audio, "{brand:?}");
        }
        assert_eq!(sniff(&cat(&[&[0, 0, 0, 0x20], b"ftyp", b"mp42"]), Some("m4a")), Format::Audio);
        assert_eq!(sniff(&cat(&[b"RIFF", &[0, 0, 0, 0], b"WAVEfmt "]), None), Format::Audio);
        assert_eq!(sniff(&cat(&[b"FORM", &[0, 0, 0, 0], b"AIFF"]), None), Format::Audio);
        for m in [&b"fLaC"[..], b"OggS\0\x02", b"ID3\x04\0", b"wvpk", b"MAC \x96\x0f", b"caff", &[0xFF, 0xFB, 0x90], &[0xFF, 0xF1, 0x50]] {
            assert_eq!(sniff(m, None), Format::Audio, "{m:?}");
        }
        // Matroska and ASF by name only.
        assert_eq!(sniff(&[0x1A, 0x45, 0xDF, 0xA3, 0x9F], Some("mka")), Format::Audio);
        assert_eq!(sniff(&[0x30, 0x26, 0xB2, 0x75, 0x8E, 0x66, 0xCF, 0x11, 0xA6], Some("wma")), Format::Audio);
        assert_eq!(sniff(&[0x1A, 0x45, 0xDF, 0xA3, 0x9F], Some("mkv")), Format::Video);
        assert_eq!(display_name(Format::Audio, Some(Path::new("song.flac"))).as_deref(), Some("FLAC"));
    }

    #[test]
    fn vlc_parity_formats() {
        assert_eq!(sniff(b".RMF\0\0\0\x12", Some("rmvb")), Format::Video);
        assert_eq!(sniff(b".ra\xfd\0\x04", Some("ra")), Format::Audio);
        assert_eq!(sniff(b"RIFF\0\0\0\0XWMAfmt ", Some("xwma")), Format::Audio);
        assert_eq!(sniff(b"RIFF\0\0\0\0AVI LIST", Some("divx")), Format::Video);
        assert_eq!(sniff(b"OggS ", Some("ogm")), Format::Video);
        assert_eq!(sniff(&[0x1A, 0x45, 0xDF, 0xA3, 0x9F], Some("weba")), Format::Audio);
        assert_eq!(sniff(&[0, 0, 0, 1, 0x67, 0x64], Some("264")), Format::Video);
        assert_eq!(sniff(b"YUV4MPEG2 W640", Some("y4m")), Format::Video);
        assert_eq!(sniff(b"DSD \x1c\0\0\0", Some("dsf")), Format::Audio);
        assert_eq!(sniff(&[0x0B, 0x77, 0x01, 0x02], Some("ac3")), Format::Audio);
        assert_eq!(sniff(b"[Script Info]", Some("ass")), Format::Text);
        // .mod: a camcorder's program stream is video, a tracker module isn't opened.
        assert_eq!(sniff(&[0, 0, 1, 0xBA, 0x44], Some("mod")), Format::Video);
        assert_eq!(sniff(b"Song title here\0\0\0\0\0", Some("mod")), Format::Unknown);
    }

    #[test]
    fn batch_one_formats() {
        assert_eq!(sniff(&[0x76, 0x2F, 0x31, 0x01, 2, 0, 0, 0], None), Format::Exr);
        assert_eq!(sniff(b"#?RADIANCE
FORMAT=32-bit_rle_rgbe", None), Format::Radiance);
        assert_eq!(sniff(b"#?RGBE
", Some("hdr")), Format::Radiance);
        assert_eq!(sniff(&[0x1F, 0x8B, 0x08, 0x00], Some("svgz")), Format::Svg);
        assert_eq!(sniff(&[0x1F, 0x8B, 0x08, 0x00], Some("gz")), Format::Unknown);
        // Ogg: audio unless named as video.
        assert_eq!(sniff(b"OggS ", Some("ogv")), Format::Video);
        assert_eq!(sniff(b"OggS ", Some("ogg")), Format::Audio);
        assert_eq!(sniff(b"FLV", None), Format::Video);
        assert_eq!(sniff(&[0, 0, 1, 0xB3, 0x2D], None), Format::Video);
        // A transport stream (plain, or with M2TS's 4-byte timecode) is video; TypeScript is text.
        assert_eq!(sniff(&[0x47, 0x40, 0x00, 0x10], Some("ts")), Format::Video);
        assert_eq!(sniff(&[0, 0, 0, 0, 0x47, 0x40], Some("ts")), Format::Video);
        assert_eq!(sniff(b"import { x } from './y';", Some("ts")), Format::Text);
        // XML named as text stays text; SVG stays SVG.
        assert_eq!(sniff(b"<?xml version=\"1.0\"?><root/>", Some("xml")), Format::Text);
        assert_eq!(sniff(b"<?xml version=\"1.0\"?><svg/>", Some("svg")), Format::Svg);
        assert_eq!(sniff(b"{\"a\": 1}", Some("json")), Format::Text);
        for (e, n) in [("exr", "OpenEXR"), ("hdr", "Radiance HDR"), ("svgz", "SVGZ"), ("json", "JSON"), ("ts", "TS")] {
            let f = sniff(&[], Some(e));
            assert_eq!(display_name(f, Some(Path::new(&format!("a.{e}")))).as_deref(), Some(n), "{e}");
        }
    }

    #[test]
    fn camera_raw() {
        let tiff: &[u8] = &[0x49, 0x49, 0x2A, 0x00, 0x08, 0, 0, 0];
        for e in [".dng", ".nef", ".arw", ".DNG", ".rw2", ".pef", ".srw", ".3fr"] {
            assert_eq!(sniff(tiff, Some(e)), Format::Raw, "{e}");
        }
        for e in [Some(".tif"), Some(".tiff"), None] {
            assert_eq!(sniff(tiff, e), Format::Tiff);
        }
        let cr2 = cat(&[&[0x49, 0x49, 0x2A, 0x00, 0x10, 0, 0, 0], b"CR", &[2, 0]]);
        assert_eq!(sniff(&cr2, Some(".tif")), Format::Raw);
        for m in [&b"IIRO"[..], b"IIRS", b"MMOR", b"FUJIFILMCCD-RAW 0201", b"IIU\0\x18\0\0\0", b"\0MRM\0\x01W\xF8", b"II\x1a\0\0\0HEAPCCDR\x02"] {
            assert_eq!(sniff(m, None), Format::Raw, "{m:?}");
        }
        assert_eq!(sniff(&cat(&[&[0, 0, 0, 0x18], b"ftyp", b"crx "]), None), Format::Raw);
    }

    #[test]
    fn content_beats_extension_and_extension_breaks_ties() {
        assert_eq!(sniff(&[0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A], Some(".jpg")), Format::Png);
        assert_eq!(sniff(&[0, 0, 0, 0x0C, 0x4A, 0x58, 0x4C, 0x20, 0x0D, 0x0A, 0x87, 0x0A], Some(".jp2")), Format::JpegXl);
        assert_eq!(sniff(&[], Some(".pdf")), Format::Pdf);
        // An uncompressed TGA header starts 00 00 02 00 like a cursor; an empty directory gives it away.
        assert_eq!(sniff(&[0, 0, 2, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0], Some(".tga")), Format::Tga);
        assert_eq!(sniff(&[0, 0, 2, 0, 1, 0, 32, 32, 0, 0, 1, 0, 32, 0, 0, 0], Some(".cur")), Format::Cur);
        let unknown: &[u8] = &[0xAA, 0xBB, 0xCC, 0xDD];
        for (e, f) in [
            (".tga", Format::Tga),
            (".cr2", Format::Raw),
            (".heic", Format::Heif),
            (".hif", Format::Heif),
            (".heics", Format::Heif),
            (".avifs", Format::Avif),
            (".apng", Format::Png),
            (".psb", Format::Psd),
            (".wdp", Format::JpegXr),
            (".jpf", Format::Jpeg2000),
            (".ppm", Format::Pnm),
            (".mrw", Format::Raw),
            (".mtl", Format::Mtl),
            (".obj", Format::Model),
            (".glb", Format::Model),
            (".3mf", Format::Model),
        ] {
            assert_eq!(sniff(unknown, Some(e)), f, "{e}");
        }
    }

    #[test]
    fn every_supported_extension_sniffs_to_a_known_format() {
        for e in EXTENSIONS {
            assert_ne!(sniff(&[], Some(e)), Format::Unknown, "{e}");
        }
    }

    #[test]
    fn every_format_has_a_display_name() {
        for f in ALL {
            assert!(display_name(*f, None).is_some_and(|n| !n.is_empty()), "{f:?}");
        }
    }

    #[test]
    fn manifest_associations_match_supported_extensions() {
        let manifest = include_str!("../Package.appxmanifest");
        let mut listed: Vec<String> = manifest
            .split("<uap:FileType>")
            .skip(1)
            .filter_map(|s| s.split('<').next())
            .map(|s| s.trim().trim_start_matches('.').to_ascii_lowercase())
            .collect();
        listed.sort();
        let mut ours: Vec<String> = EXTENSIONS.iter().map(|s| s.to_string()).collect();
        ours.sort();
        assert_eq!(listed, ours);
    }
}
