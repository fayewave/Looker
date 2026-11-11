//! Motion photos: a still with a short video of the moments around it. Three kinds:
//!
//! - Google's (Pixel, and the Motion Photo format others write): a JPEG or HEIC whose XMP says
//!   `GCamera:MotionPhoto="1"` and lists its parts in a `Container:Directory`; the video is the last part, appended
//!   to the file, `Item:Length` bytes from its end. The older MicroVideo form gives `GCamera:MicroVideoOffset`, the
//!   same count from the end.
//! - Samsung's: the MP4 follows a `MotionPhoto_Data` marker in the JPEG's trailer (newer phones add Google's XMP too).
//! - Apple's Live Photos: the photo and a `.mov` of the same name side by side (IMG_1234.HEIC + IMG_1234.MOV).
//!
//! An embedded video is copied once into a cache folder (`%TEMP%\looker-motion`, keyed by the photo's path, size
//! and time) so the player opens it as a file of its own. Online-only cloud files are never read.

use std::path::{Path, PathBuf};

/// Where a motion photo's video is.
#[derive(Clone, Debug, PartialEq)]
pub enum Source {
    /// Inside the photo: `len` bytes from `offset`.
    Embedded { offset: u64, len: u64 },
    /// A file beside it (a Live Photo's `.mov`).
    Pair(PathBuf),
}

/// Photos that can carry a motion part, by extension.
fn may_have_motion(path: &Path) -> bool {
    path.extension().and_then(|e| e.to_str()).is_some_and(|e| matches!(e.to_ascii_lowercase().as_str(), "jpg" | "jpeg" | "jpe" | "heic" | "heif" | "hif"))
}

fn online_only(path: &Path) -> bool {
    use std::os::windows::fs::MetadataExt;
    // FILE_ATTRIBUTE_OFFLINE, RECALL_ON_OPEN, RECALL_ON_DATA_ACCESS.
    std::fs::metadata(path).is_ok_and(|m| m.file_attributes() & (0x1000 | 0x40000 | 0x400000) != 0)
}

/// The motion part of `path`, if it has one.
pub fn find(path: &Path) -> Option<Source> {
    if !may_have_motion(path) || online_only(path) {
        return None;
    }
    if let Some(s) = embedded(path) {
        return Some(s);
    }
    live_pair(path).map(Source::Pair)
}

/// A Live Photo's video: a `.mov` with the photo's name in the same folder.
fn live_pair(path: &Path) -> Option<PathBuf> {
    let stem = path.file_stem()?.to_str()?.to_string();
    let dir = path.parent()?;
    for ext in ["MOV", "mov"] {
        let p = dir.join(format!("{stem}.{ext}"));
        if p.is_file() {
            return Some(p);
        }
    }
    // Case-insensitive file systems answer the above; this catches "Mov" and the like on others.
    std::fs::read_dir(dir).ok()?.flatten().map(|e| e.path()).find(|p| {
        p.file_stem().and_then(|s| s.to_str()) == Some(stem.as_str()) && p.extension().and_then(|e| e.to_str()).is_some_and(|e| e.eq_ignore_ascii_case("mov"))
    })
}

fn embedded(path: &Path) -> Option<Source> {
    use std::io::{Read, Seek, SeekFrom};
    let mut f = std::fs::File::open(path).ok()?;
    let size = f.metadata().ok()?.len();
    // The XMP is near the start (a JPEG's APP1, an HEIC's metadata item before the picture data).
    let mut head = vec![0u8; size.min(512 * 1024) as usize];
    f.read_exact(&mut head).ok()?;
    let from_end = |n: u64| (n > 0 && n < size).then(|| size - n);
    let found = parse_xmp(&head).and_then(|x| match x {
        Xmp::FromEnd(n) => from_end(n).map(|o| (o, size - o)),
    });
    let mp4_at = |f: &mut std::fs::File, o: u64| -> bool {
        let mut b = [0u8; 8];
        f.seek(SeekFrom::Start(o)).is_ok() && f.read_exact(&mut b).is_ok() && &b[4..8] == b"ftyp"
    };
    if let Some((o, l)) = found {
        if mp4_at(&mut f, o) {
            return Some(Source::Embedded { offset: o, len: l });
        }
    }
    // Samsung: the marker, then the MP4, somewhere in the trailer (videos are a few MB; look in the last 32 MB).
    let tail_from = size.saturating_sub(32 << 20);
    f.seek(SeekFrom::Start(tail_from)).ok()?;
    let mut tail = Vec::with_capacity((size - tail_from) as usize);
    f.read_to_end(&mut tail).ok()?;
    const MARK: &[u8] = b"MotionPhoto_Data";
    let at = tail.windows(MARK.len()).position(|w| w == MARK)?;
    let o = tail_from + (at + MARK.len()) as u64;
    if !mp4_at(&mut f, o) {
        return None;
    }
    // It ends where Samsung's SEF trailer starts (when there is one), else at the end of the file.
    let len = mp4_len(&mut f, o, size).unwrap_or(size - o);
    Some(Source::Embedded { offset: o, len })
}

/// The MP4 at `o`: the sum of its top-level boxes (ftyp, moov, mdat...) up to the first that doesn't look like one.
fn mp4_len(f: &mut std::fs::File, o: u64, size: u64) -> Option<u64> {
    use std::io::{Read, Seek, SeekFrom};
    let mut at = o;
    while at + 8 <= size {
        let mut b = [0u8; 16];
        f.seek(SeekFrom::Start(at)).ok()?;
        f.read_exact(&mut b[..8]).ok()?;
        let kind = &b[4..8];
        if !kind.iter().all(|c| c.is_ascii_alphanumeric() || *c == b' ') {
            break;
        }
        let mut n = u32::from_be_bytes([b[0], b[1], b[2], b[3]]) as u64;
        if n == 1 {
            f.read_exact(&mut b[8..16]).ok()?;
            n = u64::from_be_bytes(b[8..16].try_into().ok()?);
        } else if n == 0 {
            n = size - at;
        }
        if n < 8 || at + n > size {
            break;
        }
        at += n;
    }
    (at > o).then_some(at - o)
}

enum Xmp {
    /// The video is this many bytes at the end of the file.
    FromEnd(u64),
}

/// The Motion Photo fields of the XMP in `head`: the video's length from the Container directory's MotionPhoto item,
/// else MicroVideo's offset.
fn parse_xmp(head: &[u8]) -> Option<Xmp> {
    let start = find_bytes(head, b"<x:xmpmeta").or_else(|| find_bytes(head, b"<rdf:RDF"))?;
    let end = find_bytes(&head[start..], b"</x:xmpmeta>").map_or(head.len(), |e| start + e);
    let xmp = String::from_utf8_lossy(&head[start..end]);
    let motion = field(&xmp, "MotionPhoto").is_some_and(|v| v == "1") || field(&xmp, "MicroVideo").is_some_and(|v| v == "1");
    if !motion {
        return None;
    }
    // Each Container:Item: its Semantic and Length (attributes or elements).
    let mut rest = &*xmp;
    while let Some(i) = rest.find("Container:Item") {
        rest = &rest[i + "Container:Item".len()..];
        let item_end = rest.find("Container:Item").unwrap_or(rest.len());
        let item = &rest[..item_end];
        if field(item, "Semantic").as_deref() == Some("MotionPhoto") {
            if let Some(n) = field(item, "Length").and_then(|l| l.parse().ok()) {
                return Some(Xmp::FromEnd(n));
            }
        }
    }
    field(&xmp, "MicroVideoOffset").and_then(|v| v.parse().ok()).map(Xmp::FromEnd)
}

/// An XMP property's value by its local name (any prefix), as an attribute (`p:Name="v"`) or an element
/// (`<p:Name>v</p:Name>`).
fn field(xmp: &str, name: &str) -> Option<String> {
    let mut at = 0;
    while let Some(i) = xmp[at..].find(name) {
        let s = at + i;
        at = s + name.len();
        // Preceded by a prefix's colon; followed by =" or >.
        if !xmp[..s].ends_with(':') {
            continue;
        }
        let after = &xmp[at..];
        if let Some(v) = after.strip_prefix("=\"") {
            return v.split('"').next().map(str::to_string);
        }
        if let Some(v) = after.strip_prefix('>') {
            return v.split('<').next().map(|s| s.trim().to_string());
        }
    }
    None
}

fn find_bytes(h: &[u8], needle: &[u8]) -> Option<usize> {
    h.windows(needle.len()).position(|w| w == needle)
}

/// The video as a file the player can open: a pair's own file, or the embedded one copied out (once) into the
/// cache folder.
pub fn playable(photo: &Path, src: &Source) -> Option<PathBuf> {
    match src {
        Source::Pair(p) => Some(p.clone()),
        Source::Embedded { offset, len } => {
            use std::hash::{Hash, Hasher};
            use std::io::{Read, Seek, SeekFrom};
            let meta = std::fs::metadata(photo).ok()?;
            let mut h = std::collections::hash_map::DefaultHasher::new();
            photo.hash(&mut h);
            meta.len().hash(&mut h);
            meta.modified().ok().hash(&mut h);
            let dir = std::env::temp_dir().join("looker-motion");
            std::fs::create_dir_all(&dir).ok()?;
            let out = dir.join(format!("{:016x}.mp4", h.finish()));
            if out.metadata().is_ok_and(|m| m.len() == *len) {
                return Some(out);
            }
            let mut f = std::fs::File::open(photo).ok()?;
            f.seek(SeekFrom::Start(*offset)).ok()?;
            let mut buf = Vec::with_capacity(*len as usize);
            f.take(*len).read_to_end(&mut buf).ok()?;
            // Written beside it then renamed: a half-written file is never taken for the video.
            let tmp = out.with_extension("part");
            std::fs::write(&tmp, &buf).ok()?;
            std::fs::rename(&tmp, &out).ok()?;
            prune(&dir, &out);
            Some(out)
        }
    }
}

/// Keeps the cache folder small: the newest 50 videos.
fn prune(dir: &Path, keep: &Path) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    let mut files: Vec<_> = rd.flatten().filter_map(|e| Some((e.metadata().ok()?.modified().ok()?, e.path()))).collect();
    if files.len() <= 50 {
        return;
    }
    files.sort();
    for (_, p) in files.iter().take(files.len() - 50) {
        if p != keep {
            let _ = std::fs::remove_file(p);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_google_xmp() {
        let x = br#"<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF><rdf:Description GCamera:MotionPhoto="1" GCamera:MotionPhotoVersion="1">
            <Container:Directory><rdf:Seq>
            <rdf:li rdf:parseType="Resource"><Container:Item Item:Mime="image/jpeg" Item:Semantic="Primary" Item:Length="0" Item:Padding="0"/></rdf:li>
            <rdf:li rdf:parseType="Resource"><Container:Item Item:Mime="video/mp4" Item:Semantic="MotionPhoto" Item:Length="123456" Item:Padding="0"/></rdf:li>
            </rdf:Seq></Container:Directory></rdf:Description></rdf:RDF></x:xmpmeta>"#;
        assert!(matches!(parse_xmp(x), Some(Xmp::FromEnd(123456))));
        let old = br#"<x:xmpmeta><rdf:Description GCamera:MicroVideo="1" GCamera:MicroVideoOffset="4242"/></x:xmpmeta>"#;
        assert!(matches!(parse_xmp(old), Some(Xmp::FromEnd(4242))));
        let elements = b"<x:xmpmeta><GCamera:MotionPhoto>1</GCamera:MotionPhoto><Container:Item><Item:Semantic>MotionPhoto</Item:Semantic><Item:Length>77</Item:Length></Container:Item></x:xmpmeta>";
        assert!(matches!(parse_xmp(elements), Some(Xmp::FromEnd(77))));
        assert!(parse_xmp(b"<x:xmpmeta><rdf:Description GCamera:MotionPhoto=\"0\"/></x:xmpmeta>").is_none());
        assert!(parse_xmp(b"no xmp here").is_none());
    }

    /// A JPEG with Google's XMP and an MP4 after it, and a Samsung-style trailer, found and copied out exactly.
    #[test]
    fn finds_and_copies_embedded_video() {
        let dir = std::env::temp_dir().join(format!("looker-motion-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mp4: Vec<u8> = [&[0, 0, 0, 16][..], b"ftypisom", &[0, 0, 0, 0], &[0, 0, 0, 12], b"mdat", b"abcd"].concat();
        let xmp = format!(
            r#"<x:xmpmeta><rdf:Description GCamera:MotionPhoto="1"><Container:Directory><Container:Item Item:Semantic="Primary" Item:Length="0"/><Container:Item Item:Semantic="MotionPhoto" Item:Length="{}"/></Container:Directory></rdf:Description></x:xmpmeta>"#,
            mp4.len()
        );
        let google = dir.join("PXL_1.MP.jpg");
        std::fs::write(&google, [&[0xFF, 0xD8, 0xFF, 0xE1][..], xmp.as_bytes(), &[0xFF, 0xD9], &mp4].concat()).unwrap();
        let s = find(&google).expect("google motion photo");
        assert_eq!(std::fs::read(playable(&google, &s).unwrap()).unwrap(), mp4);

        let samsung = dir.join("20250101_120000.jpg");
        std::fs::write(&samsung, [&[0xFF, 0xD8, 0xFF, 0xD9][..], b"MotionPhoto_Data", &mp4, b"\0\0SEFHjunkSEFT"].concat()).unwrap();
        let s = find(&samsung).expect("samsung motion photo");
        assert_eq!(s, Source::Embedded { offset: 20, len: mp4.len() as u64 });
        assert_eq!(std::fs::read(playable(&samsung, &s).unwrap()).unwrap(), mp4);

        let live = dir.join("IMG_0001.HEIC");
        std::fs::write(&live, b"not really").unwrap();
        std::fs::write(dir.join("IMG_0001.MOV"), b"mov").unwrap();
        assert!(matches!(find(&live), Some(Source::Pair(_))));

        let plain = dir.join("plain.jpg");
        std::fs::write(&plain, [0xFF, 0xD8, 0xFF, 0xD9]).unwrap();
        assert!(find(&plain).is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
