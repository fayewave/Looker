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
    Pdf,
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
    Format::Pdf,
];

/// Every extension Looker opens (lower case, no dot). Must equal the manifest FTA list.
pub const EXTENSIONS: &[&str] = &[
    "jpg", "jpeg", "jpe", "jfif", "png", "apng", "bmp", "dib", "gif", "tif", "tiff", "webp", "ico", "cur", "jxr", "wdp",
    "hdp", "heic", "heif", "hif", "heics", "avif", "avifs", "jxl", "jp2", "j2k", "jpf", "jpx", "j2c", "svg", "pdf", "psd",
    "psb", "tga", "icb", "vda", "vst", "dds", "pcx", "pnm", "ppm", "pgm", "pbm", "xcf", "qoi",
    // Camera RAW. No Sigma .x3f: libraw dropped Foveon and no WIC codec reads it.
    "cr2", "cr3", "crw", "nef", "nrw", "arw", "sr2", "srf", "dng", "orf", "raf", "rw2", "pef", "srw", "3fr", "fff", "rwl",
    "iiq", "mrw", "dcr", "kdc", "erf", "mef",
];

fn ext_of(path: &Path) -> Option<String> {
    path.extension().and_then(|e| e.to_str()).map(|e| e.to_ascii_lowercase())
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
        "svg" => Format::Svg,
        "pdf" => Format::Pdf,
        "psd" | "psb" => Format::Psd,
        "jxl" => Format::JpegXl,
        "dds" => Format::Dds,
        "tga" | "icb" | "vda" | "vst" => Format::Tga,
        "pcx" => Format::Pcx,
        "pnm" | "ppm" | "pgm" | "pbm" => Format::Pnm,
        "xcf" => Format::Xcf,
        "qoi" => Format::Qoi,
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
    if ascii(h, 0, "%PDF-") {
        return Format::Pdf;
    }
    let s = if starts(h, &[0xEF, 0xBB, 0xBF]) { &h[3..] } else { h };
    if ascii(s, 0, "<?xml") || ascii(s, 0, "<svg") || ascii(s, 0, "<!--") {
        return Format::Svg;
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
        Format::Svg => "SVG",
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
        Format::Raw => return Some(if ext.is_empty() { "RAW".into() } else { format!("RAW ({ext})") }),
        Format::Pdf => "PDF",
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
        let manifest = include_str!("../../src/Looker/Package.appxmanifest");
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
