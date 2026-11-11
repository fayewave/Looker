//! What a photo says about itself in words: its title, caption, keywords, rating, author, copyright and where it
//! was taken by name. Three places hold these, read in this order of trust:
//!
//! - **XMP** (Lightroom, Bridge, Windows' own Properties, phones): an XML packet anywhere in the file (JPEG APP1,
//!   PNG iTXt, TIFF tag 700, WebP's XMP chunk), found by scanning the bytes for it.
//! - **IPTC IIM** (older Photoshop and newsroom tools): the 8BIM resource 0x0404 in a JPEG's APP13 segment.
//! - **EXIF** text tags: ImageDescription, Artist, Copyright, Software, Rating and Windows' XP tags (XPTitle...).
//!
//! The parsing is deliberately small and pure (no XML crate): only the handful of fields shown is looked for, and
//! each piece is unit tested.

use exif::{Context, In, Tag, Value};

/// The words, merged (XMP over IPTC over EXIF). Empty fields are None.
#[derive(Default, Debug, PartialEq, Clone)]
pub struct Description {
    pub title: Option<String>,
    pub caption: Option<String>,
    pub keywords: Vec<String>,
    /// 1-5 stars; -1 rejected (Lightroom).
    pub rating: Option<i32>,
    /// A colour label (Lightroom/Bridge: "Red", "Green"...).
    pub label: Option<String>,
    pub author: Option<String>,
    pub copyright: Option<String>,
    pub software: Option<String>,
    pub place: Option<String>,
    pub city: Option<String>,
    pub state: Option<String>,
    pub country: Option<String>,
}

impl Description {
    /// Fills what this one lacks from `other` (the less trusted source).
    fn or(mut self, other: Description) -> Description {
        fn pick(a: &mut Option<String>, b: Option<String>) {
            if a.is_none() {
                *a = b;
            }
        }
        pick(&mut self.title, other.title);
        pick(&mut self.caption, other.caption);
        if self.keywords.is_empty() {
            self.keywords = other.keywords;
        }
        if self.rating.is_none() {
            self.rating = other.rating;
        }
        pick(&mut self.label, other.label);
        pick(&mut self.author, other.author);
        pick(&mut self.copyright, other.copyright);
        pick(&mut self.software, other.software);
        pick(&mut self.place, other.place);
        pick(&mut self.city, other.city);
        pick(&mut self.state, other.state);
        pick(&mut self.country, other.country);
        self
    }

    /// The Description group's rows, worded.
    pub fn rows(&self) -> Vec<(&'static str, String)> {
        let mut out = Vec::new();
        let mut push = |l: &'static str, v: &Option<String>| {
            if let Some(v) = v {
                out.push((l, v.clone()));
            }
        };
        push("Title", &self.title);
        push("Description", &self.caption);
        if !self.keywords.is_empty() {
            out.push(("Keywords", self.keywords.join(", ")));
        }
        match self.rating {
            Some(-1) => out.push(("Rating", "Rejected".to_string())),
            Some(r @ 1..=5) => out.push(("Rating", format!("{}{}", "\u{2605}".repeat(r as usize), "\u{2606}".repeat(5 - r as usize)))),
            _ => {}
        }
        let mut push = |l: &'static str, v: &Option<String>| {
            if let Some(v) = v {
                out.push((l, v.clone()));
            }
        };
        push("Label", &self.label);
        push("Author", &self.author);
        push("Copyright", &self.copyright);
        push("Software", &self.software);
        out
    }

    /// The place by name, for the Location group (before the coordinates).
    pub fn place_rows(&self) -> Vec<(&'static str, String)> {
        [("Place", &self.place), ("City", &self.city), ("State", &self.state), ("Country", &self.country)].into_iter().filter_map(|(l, v)| v.clone().map(|v| (l, v))).collect()
    }
}

/// Reads the words from a file's bytes (its first few MB and its last, see `metadata::read`) and its EXIF.
pub fn read(bytes: &[u8], exif: Option<&exif::Exif>) -> Description {
    let x = xmp_packet(bytes).map(|p| from_xmp(&p)).unwrap_or_default();
    let i = iptc_block(bytes).map(from_iptc).unwrap_or_default();
    let e = exif.map(from_exif).unwrap_or_default();
    // Windows' Properties writes its Title into XMP's description and EXIF's ImageDescription as well: a caption
    // that only repeats the title gives way to one of the other sources' (its Comments go to XPComment).
    let comment = exif.and_then(|x| match x.get_field(Tag(Context::Tiff, 0x9C9C), In::PRIMARY).map(|f| &f.value) {
        Some(Value::Byte(b)) => xp_text(b),
        _ => None,
    });
    let others = [i.caption.clone(), e.caption.clone(), comment];
    let mut d = x.or(i).or(e);
    if d.caption.is_some() && d.caption == d.title {
        d.caption = others.into_iter().flatten().find(|c| Some(c) != d.title.as_ref());
    }
    d
}

fn clean(s: &str) -> Option<String> {
    let s = s.trim().trim_matches('\0').trim();
    (!s.is_empty()).then(|| s.to_string())
}

// --- XMP ---------------------------------------------------------------------------------------------

/// The XMP packet's text, if the bytes hold one.
pub fn xmp_packet(bytes: &[u8]) -> Option<String> {
    let start = find(bytes, b"<x:xmpmeta").or_else(|| find(bytes, b"<rdf:RDF"))?;
    let rest = &bytes[start..];
    let end = find(rest, b"</x:xmpmeta>").map(|e| e + 12).or_else(|| find(rest, b"</rdf:RDF>").map(|e| e + 10))?;
    Some(String::from_utf8_lossy(&rest[..end]).into_owned())
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

fn from_xmp(x: &str) -> Description {
    let one = |name: &str| xmp_values(x, name).into_iter().find_map(|v| clean(&v));
    let all = |name: &str| xmp_values(x, name).into_iter().filter_map(|v| clean(&v)).collect::<Vec<_>>();
    let creators = all("dc:creator");
    Description {
        title: one("dc:title"),
        caption: one("dc:description"),
        keywords: all("dc:subject"),
        rating: one("xmp:Rating").and_then(|r| r.parse::<f32>().ok()).map(|r| r.round() as i32).filter(|r| *r != 0),
        label: one("xmp:Label"),
        author: (!creators.is_empty()).then(|| creators.join(", ")),
        copyright: one("dc:rights"),
        software: one("xmp:CreatorTool"),
        place: one("Iptc4xmpCore:Location"),
        city: one("photoshop:City"),
        state: one("photoshop:State"),
        country: one("photoshop:Country"),
    }
}

/// Every value of property `name` (`dc:title`, `xmp:Rating`...) in the packet: written as an attribute
/// (`xmp:Rating="4"`), as an element's text (`<photoshop:City>Adelaide</photoshop:City>`), or as an element
/// holding an rdf:Alt/Bag/Seq of `<rdf:li>` items.
pub fn xmp_values(x: &str, name: &str) -> Vec<String> {
    let mut out = Vec::new();
    // Attributes: preceded by whitespace, followed by =" or ='.
    let mut at = 0;
    while let Some(i) = x[at..].find(name) {
        let p = at + i;
        at = p + name.len();
        let before = x[..p].chars().next_back();
        let after = &x[at..];
        if before.is_some_and(char::is_whitespace) && (after.starts_with("=\"") || after.starts_with("='")) {
            let q = &after[1..2];
            if let Some(end) = after[2..].find(q) {
                out.push(unescape(&after[2..2 + end]));
            }
        }
    }
    // Elements.
    let open = format!("<{name}");
    let close = format!("</{name}>");
    let mut at = 0;
    while let Some(i) = x[at..].find(&open) {
        let p = at + i + open.len();
        at = p;
        // `<dc:title>` or `<dc:title xml:lang=...>`, not `<dc:titleX`; a self-closed one has no text.
        let Some(c) = x[p..].chars().next() else { break };
        if !(c == '>' || c.is_whitespace()) {
            continue;
        }
        let Some(gt) = x[p..].find('>') else { break };
        if x[..p + gt].ends_with('/') {
            continue;
        }
        let body_start = p + gt + 1;
        let Some(end) = x[body_start..].find(&close) else { break };
        let body = &x[body_start..body_start + end];
        at = body_start + end + close.len();
        if body.contains("<rdf:li") {
            let mut b = 0;
            while let Some(j) = body[b..].find("<rdf:li") {
                let s = b + j;
                let Some(gt) = body[s..].find('>') else { break };
                if body[..s + gt].ends_with('/') {
                    b = s + gt + 1;
                    continue;
                }
                let t = s + gt + 1;
                let Some(e) = body[t..].find("</rdf:li>") else { break };
                out.push(unescape(&body[t..t + e]));
                b = t + e;
            }
        } else if !body.trim_start().starts_with('<') {
            out.push(unescape(body));
        }
    }
    out
}

fn unescape(s: &str) -> String {
    if !s.contains('&') {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find('&') {
        out.push_str(&rest[..i]);
        rest = &rest[i..];
        let Some(semi) = rest.find(';').filter(|&k| k <= 10) else {
            out.push('&');
            rest = &rest[1..];
            continue;
        };
        let ent = &rest[1..semi];
        let ch = match ent {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            _ if ent.starts_with("#x") || ent.starts_with("#X") => u32::from_str_radix(&ent[2..], 16).ok().and_then(char::from_u32),
            _ if ent.starts_with('#') => ent[1..].parse().ok().and_then(char::from_u32),
            _ => None,
        };
        match ch {
            Some(c) => out.push(c),
            None => out.push_str(&rest[..=semi]),
        }
        rest = &rest[semi + 1..];
    }
    out.push_str(rest);
    out
}

// --- IPTC IIM ----------------------------------------------------------------------------------------

/// The IPTC records of a JPEG (APP13, Photoshop's 8BIM resource 0x0404).
pub fn iptc_block(bytes: &[u8]) -> Option<&[u8]> {
    if bytes.get(..2) != Some(&[0xFF, 0xD8]) {
        return None;
    }
    let mut i = 2;
    while i + 4 <= bytes.len() && bytes[i] == 0xFF {
        let marker = bytes[i + 1];
        if marker == 0xDA || marker == 0xD9 {
            break; // the scan: no more metadata segments
        }
        if marker == 0xFF || (0xD0..=0xD7).contains(&marker) || marker == 0x01 {
            i += if marker == 0xFF { 1 } else { 2 };
            continue;
        }
        let len = u16::from_be_bytes([bytes[i + 2], bytes[i + 3]]) as usize;
        let seg = bytes.get(i + 4..i + 2 + len)?;
        if marker == 0xED && seg.starts_with(b"Photoshop 3.0\0") {
            if let Some(b) = photoshop_iptc(&seg[14..]) {
                return Some(b);
            }
        }
        i += 2 + len;
    }
    None
}

/// Resource 0x0404 among Photoshop's image resources ("8BIM", id, padded Pascal name, size, padded data).
fn photoshop_iptc(mut r: &[u8]) -> Option<&[u8]> {
    while r.len() >= 12 && &r[..4] == b"8BIM" {
        let id = u16::from_be_bytes([r[4], r[5]]);
        let name_len = r[6] as usize;
        let mut p = 7 + name_len;
        if p % 2 == 1 {
            p += 1;
        }
        let size = u32::from_be_bytes(r.get(p..p + 4)?.try_into().ok()?) as usize;
        let data = r.get(p + 4..p + 4 + size)?;
        if id == 0x0404 {
            return Some(data);
        }
        let mut next = p + 4 + size;
        if next % 2 == 1 {
            next += 1;
        }
        r = r.get(next..)?;
    }
    None
}

fn from_iptc(b: &[u8]) -> Description {
    let mut d = Description::default();
    let mut i = 0;
    while i + 5 <= b.len() && b[i] == 0x1C {
        let (record, set) = (b[i + 1], b[i + 2]);
        let len = u16::from_be_bytes([b[i + 3], b[i + 4]]) as usize;
        if len & 0x8000 != 0 {
            break; // extended length: not used by these fields
        }
        let Some(v) = b.get(i + 5..i + 5 + len) else { break };
        i += 5 + len;
        if record != 2 {
            continue;
        }
        let s = match std::str::from_utf8(v) {
            Ok(s) => s.to_string(),
            Err(_) => v.iter().map(|&c| c as char).collect(), // Latin-1
        };
        let Some(s) = clean(&s) else { continue };
        match set {
            5 => d.title = d.title.or(Some(s)),
            25 => d.keywords.push(s),
            80 => d.author = Some(d.author.map_or(s.clone(), |a| format!("{a}, {s}"))),
            92 => d.place = d.place.or(Some(s)),
            90 => d.city = d.city.or(Some(s)),
            95 => d.state = d.state.or(Some(s)),
            101 => d.country = d.country.or(Some(s)),
            116 => d.copyright = d.copyright.or(Some(s)),
            120 => d.caption = d.caption.or(Some(s)),
            _ => {}
        }
    }
    d
}

// --- EXIF --------------------------------------------------------------------------------------------

/// Windows' XP tags (UCS-2 little-endian bytes), Rating, and the ASCII text tags.
fn from_exif(e: &exif::Exif) -> Description {
    let get = |t: Tag| e.get_field(t, In::PRIMARY).map(|f| &f.value);
    let ascii = |t: Tag| match get(t) {
        Some(Value::Ascii(parts)) => parts.first().and_then(|p| clean(&String::from_utf8_lossy(p))),
        _ => None,
    };
    let xp = |id: u16| match get(Tag(Context::Tiff, id)) {
        Some(Value::Byte(b)) => xp_text(b),
        _ => None,
    };
    // Cameras fill ImageDescription with their own name or spaces: not a caption.
    let caption = ascii(Tag::ImageDescription).filter(|s| !boilerplate(s)).or_else(|| xp(0x9C9C));
    let rating = match get(Tag(Context::Tiff, 0x4746)) {
        Some(v) => v.get_uint(0).map(|r| r as i32).filter(|r| (1..=5).contains(r)),
        None => None,
    };
    Description {
        title: xp(0x9C9B),
        caption,
        keywords: xp(0x9C9E).map(|k| k.split(';').filter_map(clean).collect()).unwrap_or_default(),
        rating,
        author: ascii(Tag::Artist).or_else(|| xp(0x9C9D)),
        copyright: ascii(Tag::Copyright),
        software: ascii(Tag::Software),
        ..Default::default()
    }
}

fn xp_text(b: &[u8]) -> Option<String> {
    let units: Vec<u16> = b.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).take_while(|&u| u != 0).collect();
    clean(&String::from_utf16_lossy(&units))
}

fn boilerplate(s: &str) -> bool {
    let u = s.to_uppercase();
    u.contains("DIGITAL CAMERA") || u == "SONY DSC" || u == "DCIM" || u.starts_with("DCF ") || u == "DEFAULT" || u == "SAMSUNG" || u == "IMAGE"
}

#[cfg(test)]
mod tests {
    use super::*;

    const XMP: &str = r#"<?xpacket begin=""?><x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF>
 <rdf:Description rdf:about="" xmp:Rating="4" xmp:Label="Red" photoshop:City="Adelaide" xmp:CreatorTool="Adobe Lightroom">
  <dc:title><rdf:Alt><rdf:li xml:lang="x-default">Sunset &amp; sea</rdf:li></rdf:Alt></dc:title>
  <dc:subject><rdf:Bag><rdf:li>beach</rdf:li><rdf:li>sunset</rdf:li></rdf:Bag></dc:subject>
  <dc:creator><rdf:Seq><rdf:li>Faye</rdf:li></rdf:Seq></dc:creator>
  <photoshop:Country>Australia</photoshop:Country>
  <dc:titleExtra>no</dc:titleExtra>
 </rdf:Description></rdf:RDF></x:xmpmeta>"#;

    #[test]
    fn reads_xmp_attributes_elements_and_lists() {
        let d = from_xmp(&xmp_packet(XMP.as_bytes()).unwrap());
        assert_eq!(d.title.as_deref(), Some("Sunset & sea"));
        assert_eq!(d.keywords, ["beach", "sunset"]);
        assert_eq!(d.rating, Some(4));
        assert_eq!(d.label.as_deref(), Some("Red"));
        assert_eq!(d.author.as_deref(), Some("Faye"));
        assert_eq!(d.city.as_deref(), Some("Adelaide"));
        assert_eq!(d.country.as_deref(), Some("Australia"));
        assert_eq!(d.software.as_deref(), Some("Adobe Lightroom"));
    }

    #[test]
    fn rating_rows() {
        let d = Description { rating: Some(3), ..Default::default() };
        assert_eq!(d.rows(), [("Rating", "\u{2605}\u{2605}\u{2605}\u{2606}\u{2606}".to_string())]);
        assert_eq!(Description { rating: Some(-1), ..Default::default() }.rows(), [("Rating", "Rejected".to_string())]);
    }

    #[test]
    fn unescapes_entities() {
        assert_eq!(unescape("a &lt;b&gt; &#169; &#x41; &bogus; &"), "a <b> \u{a9} A &bogus; &");
    }

    #[test]
    fn reads_iptc_from_a_jpeg() {
        let mut iim = Vec::new();
        for (set, v) in [(5u8, "Harbour"), (25, "boats"), (25, "dusk"), (120, "Boats at dusk"), (116, "(c) Faye")] {
            iim.extend([0x1C, 2, set]);
            iim.extend((v.len() as u16).to_be_bytes());
            iim.extend(v.as_bytes());
        }
        let mut res = b"8BIM\x04\x04\x00\x00".to_vec();
        res.extend((iim.len() as u32).to_be_bytes());
        res.extend(&iim);
        let mut app13 = b"Photoshop 3.0\0".to_vec();
        app13.extend(&res);
        let mut jpeg = vec![0xFF, 0xD8, 0xFF, 0xED];
        jpeg.extend(((app13.len() + 2) as u16).to_be_bytes());
        jpeg.extend(&app13);
        jpeg.extend([0xFF, 0xDA, 0, 2]);
        let d = from_iptc(iptc_block(&jpeg).unwrap());
        assert_eq!(d.title.as_deref(), Some("Harbour"));
        assert_eq!(d.keywords, ["boats", "dusk"]);
        assert_eq!(d.caption.as_deref(), Some("Boats at dusk"));
        assert_eq!(d.copyright.as_deref(), Some("(c) Faye"));
    }

    #[test]
    fn xmp_wins_over_the_rest() {
        let a = Description { title: Some("XMP".into()), ..Default::default() };
        let b = Description { title: Some("IPTC".into()), caption: Some("from IPTC".into()), ..Default::default() };
        let d = a.or(b);
        assert_eq!(d.title.as_deref(), Some("XMP"));
        assert_eq!(d.caption.as_deref(), Some("from IPTC"));
    }

    /// `LOOKER_DECODE=<photo> cargo test --release -- --ignored --nocapture describes_a_file`
    #[test]
    #[ignore]
    fn describes_a_file() {
        let path = std::path::PathBuf::from(std::env::var_os("LOOKER_DECODE").expect("LOOKER_DECODE"));
        let bytes = std::fs::read(&path).unwrap();
        let exif = exif::Reader::new().read_from_container(&mut std::io::Cursor::new(&bytes)).ok();
        if let Some(e) = &exif {
            for f in e.fields().filter(|f| f.ifd_num == In::PRIMARY) {
                println!("{:?} {:?}", f.tag, f.value);
            }
        }
        println!("{:#?}", read(&bytes, exif.as_ref()));
    }

    #[test]
    fn camera_boilerplate_is_not_a_caption() {
        assert!(boilerplate("OLYMPUS DIGITAL CAMERA"));
        assert!(!boilerplate("Our wedding"));
        assert_eq!(xp_text(&[0x48, 0, 0x69, 0, 0, 0]).as_deref(), Some("Hi"));
    }
}
