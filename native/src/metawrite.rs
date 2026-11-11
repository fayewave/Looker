//! Writing a photo's words back into it: title, description, keywords, rating, author and copyright, through
//! Windows' own photo metadata policy (`System.Title`, `System.Keywords`...), which keeps the file's XMP, EXIF
//! and IPTC copies of each in step the way Explorer's Properties does.
//!
//! WinRT's `BitmapEncoder` does the work. In place first (`CreateForInPlacePropertyEncodingAsync`): only the
//! metadata block changes, when it has room. Otherwise the file is transcoded into a sibling temp file with the
//! new properties and swapped in (`ReplaceFileW`, which keeps its creation time). For JPEG the transcode copies
//! the compressed pixels as they are: nothing is re-encoded (the `round_trips` test checks the pixels match).

use std::path::Path;

use windows::Foundation::{PropertyType, PropertyValue};
use windows::Graphics::Imaging::{BitmapDecoder, BitmapEncoder, BitmapPropertySet, BitmapTypedValue};
use windows::Storage::FileAccessMode;
use windows::core::HSTRING;

/// A field the card can edit.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash, PartialOrd, Ord)]
pub enum Field {
    Title,
    Description,
    Keywords,
    Rating,
    Author,
    Copyright,
}

impl Field {
    pub const ALL: [Field; 6] = [Field::Title, Field::Description, Field::Keywords, Field::Rating, Field::Author, Field::Copyright];

    /// The card's name for it (its row label in the Description group).
    pub fn label(self) -> &'static str {
        match self {
            Field::Title => "Title",
            Field::Description => "Description",
            Field::Keywords => "Keywords",
            Field::Rating => "Rating",
            Field::Author => "Author",
            Field::Copyright => "Copyright",
        }
    }

    pub fn from_label(l: &str) -> Option<Field> {
        Field::ALL.into_iter().find(|f| f.label() == l)
    }
}

/// Whether the file's format can have these written (JPEG and TIFF: Windows' policy writes their XMP, EXIF and
/// IPTC).
pub fn can_write(path: &Path) -> bool {
    let ext = path.extension().map(|e| e.to_string_lossy().to_ascii_lowercase()).unwrap_or_default();
    matches!(ext.as_str(), "jpg" | "jpeg" | "jpe" | "jfif" | "tif" | "tiff")
}

/// Keywords as typed ("beach, sunset; dusk") into a list.
pub fn split_keywords(s: &str) -> Vec<String> {
    s.split([',', ';']).map(str::trim).filter(|k| !k.is_empty()).map(str::to_string).collect()
}

/// A rating as shown ("★★★☆☆", "Rejected") or typed ("3") into 0-5 stars (0: none).
pub fn parse_rating(s: &str) -> Option<u32> {
    let full = s.chars().filter(|&c| c == '\u{2605}').count();
    if full > 0 {
        return Some(full.min(5) as u32);
    }
    s.trim().parse::<u32>().ok().filter(|r| *r <= 5)
}

fn err(e: windows::core::Error) -> String {
    e.message().to_string()
}

/// The property set for `edits`: each field's System property, or Empty to remove it.
fn properties(edits: &[(Field, Option<String>)]) -> Result<BitmapPropertySet, String> {
    let set = BitmapPropertySet::new().map_err(err)?;
    let typed = |v: windows::core::IInspectable, t: PropertyType| BitmapTypedValue::Create(&v, t).map_err(err);
    // Removing: an empty string, an empty list, a zero rating (an Empty value is refused); the card reads blanks
    // as absent.
    let string = |s: &str| -> Result<BitmapTypedValue, String> { typed(PropertyValue::CreateString(&HSTRING::from(s)).map_err(err)?, PropertyType::String) };
    let strings = |v: &[String]| -> Result<BitmapTypedValue, String> {
        let hs: Vec<HSTRING> = v.iter().map(HSTRING::from).collect();
        typed(PropertyValue::CreateStringArray(&hs).map_err(err)?, PropertyType::StringArray)
    };
    for (f, new) in edits {
        let new = new.as_deref().map(str::trim).unwrap_or("");
        let (name, value) = match f {
            Field::Title => ("System.Title", string(new)?),
            Field::Description => ("System.Comment", string(new)?),
            Field::Copyright => ("System.Copyright", string(new)?),
            Field::Keywords => ("System.Keywords", strings(&split_keywords(new))?),
            Field::Author => ("System.Author", strings(&split_keywords(new))?),
            Field::Rating => {
                let stars = parse_rating(new).unwrap_or(0);
                ("System.SimpleRating", typed(PropertyValue::CreateUInt16(stars as u16).map_err(err)?, PropertyType::UInt16)?)
            }
        };
        set.Insert(&HSTRING::from(name), &value).map_err(err)?;
    }
    Ok(set)
}

/// Writes `edits` (None or blank removes the field) into the file at `path`. On a failure the file is as it was.
pub fn write(path: &Path, edits: &[(Field, Option<String>)]) -> Result<(), String> {
    if !can_write(path) {
        return Err("Looker can write these only into JPEG and TIFF files".into());
    }
    let mut edits = edits.to_vec();
    // Windows' policy writes the Title into XMP's dc:description too, where the card reads the description
    // first: a description edit writes the title again after, so the two stay apart.
    if edits.iter().any(|(f, _)| *f == Field::Description) && !edits.iter().any(|(f, _)| *f == Field::Title) {
        if let Some(title) = crate::metadata::read(path).and_then(|e| e.words.iter().find(|(l, _)| *l == "Title").map(|(_, v)| v.clone())) {
            edits.push((Field::Title, Some(title)));
        }
    }
    edits.sort_by_key(|(f, _)| *f);
    // In place, when the metadata block has room.
    let in_place = (|| -> Result<(), String> {
        let stream = crate::fileops::open_stream(path, FileAccessMode::ReadWrite)?;
        let decoder = BitmapDecoder::CreateAsync(&stream).and_then(|op| op.join()).map_err(err)?;
        let encoder = BitmapEncoder::CreateForInPlacePropertyEncodingAsync(&decoder).and_then(|op| op.join()).map_err(err)?;
        encoder.BitmapProperties().map_err(err)?.SetPropertiesAsync(&properties(&edits)?).and_then(|op| op.join()).map_err(err)?;
        encoder.FlushAsync().and_then(|op| op.join()).map_err(err)
    })();
    if in_place.is_ok() {
        return Ok(());
    }
    // Otherwise a transcode with the new properties, swapped in.
    let name = path.file_name().ok_or("no file name")?.to_string_lossy().into_owned();
    let temp = path.with_file_name(format!(".{name}.looker.tmp"));
    let r = (|| -> Result<(), String> {
        std::fs::File::create(&temp).map_err(|e| e.to_string())?;
        {
            let src = crate::fileops::open_stream(path, FileAccessMode::Read)?;
            let dst = crate::fileops::open_stream(&temp, FileAccessMode::ReadWrite)?;
            let decoder = BitmapDecoder::CreateAsync(&src).and_then(|op| op.join()).map_err(err)?;
            let encoder = BitmapEncoder::CreateForTranscodingAsync(&dst, &decoder).and_then(|op| op.join()).map_err(err)?;
            encoder.BitmapProperties().map_err(err)?.SetPropertiesAsync(&properties(&edits)?).and_then(|op| op.join()).map_err(err)?;
            encoder.FlushAsync().and_then(|op| op.join()).map_err(err)?;
        }
        crate::fileops::replace_file(&temp, path)
    })();
    let _ = std::fs::remove_file(&temp);
    r
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_what_the_card_shows() {
        assert_eq!(parse_rating("\u{2605}\u{2605}\u{2605}\u{2606}\u{2606}"), Some(3));
        assert_eq!(parse_rating("4"), Some(4));
        assert_eq!(parse_rating("Rejected"), None);
        assert_eq!(split_keywords("beach, sunset;dusk, "), ["beach", "sunset", "dusk"]);
    }

    /// Writes every field into a copy of `LOOKER_DECODE` (or the test photo), reads them back, removes them, and
    /// checks the pixels never changed. `cargo test --release -- --ignored --nocapture round_trips`
    #[test]
    #[ignore]
    fn round_trips() {
        use windows::Win32::Graphics::Imaging::{CLSID_WICImagingFactory2, IWICImagingFactory};
        use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx};
        let src = std::env::var_os("LOOKER_DECODE").map(std::path::PathBuf::from).unwrap_or_else(|| std::path::PathBuf::from(std::env::var("USERPROFILE").unwrap()).join("Pictures").join("Looker Test Photos").join("01 JPEG.jpg"));
        let ext = src.extension().unwrap().to_string_lossy().into_owned();
        let copy = std::env::temp_dir().join(format!("looker-metawrite-test.{ext}"));
        std::fs::copy(&src, &copy).unwrap();
        let f: IWICImagingFactory = unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
            CoCreateInstance(&CLSID_WICImagingFactory2, None, CLSCTX_INPROC_SERVER).unwrap()
        };
        let pixels = |p: &Path| crate::imaging::wic::decode(&f, crate::imaging::wic::Source::File(p), 0, 0).unwrap().frames[0].pixels.clone();
        let before = pixels(&copy);
        let words = |p: &Path| crate::metadata::read(p).map(|e| e.words).unwrap_or_default();
        let edits = vec![
            (Field::Title, Some("A title".to_string())),
            (Field::Description, Some("A longer description".to_string())),
            (Field::Keywords, Some("one, two".to_string())),
            (Field::Rating, Some("3".to_string())),
            (Field::Author, Some("Someone".to_string())),
            (Field::Copyright, Some("CC0".to_string())),
        ];
        let t = std::time::Instant::now();
        write(&copy, &edits).unwrap();
        println!("wrote in {:.0} ms: {:?}", t.elapsed().as_secs_f64() * 1000.0, words(&copy));
        let w = words(&copy);
        let get = |l: &str| w.iter().find(|(k, _)| *k == l).map(|(_, v)| v.clone());
        assert_eq!(get("Title").as_deref(), Some("A title"));
        assert_eq!(get("Description").as_deref(), Some("A longer description"));
        assert_eq!(get("Keywords").as_deref(), Some("one, two"));
        assert_eq!(get("Rating").as_deref(), Some("\u{2605}\u{2605}\u{2605}\u{2606}\u{2606}"));
        assert_eq!(get("Author").as_deref(), Some("Someone"));
        assert_eq!(get("Copyright").as_deref(), Some("CC0"));
        assert!(pixels(&copy) == before, "the pixels changed");
        // Changing one again (in place now, likely), then removing them all.
        write(&copy, &[(Field::Description, Some("Changed".into()))]).unwrap();
        let w = words(&copy);
        println!("after a second edit: {w:?}");
        assert!(w.iter().any(|(k, v)| *k == "Description" && v == "Changed"));
        assert!(w.iter().any(|(k, v)| *k == "Title" && v == "A title"));
        write(&copy, &Field::ALL.map(|f| (f, None))).unwrap();
        let w = words(&copy);
        println!("after removing: {w:?}");
        for f in Field::ALL {
            assert!(!w.iter().any(|(k, _)| *k == f.label()), "{} is still there", f.label());
        }
        assert!(pixels(&copy) == before, "the pixels changed");
        let _ = std::fs::remove_file(&copy);
    }
}
