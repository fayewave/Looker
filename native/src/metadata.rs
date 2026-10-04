//! What the info card shows beyond the file basics: the curated EXIF groups of `MetadataService.Read`
//! (Camera, Exposure, Date, Location) and the luma/RGB histogram of the decoded pixels.
//!
//! EXIF comes from `kamadak-exif`, which reads the attribute block straight out of JPEG, TIFF (and the RAW
//! formats built on it), HEIF/AVIF, PNG and WebP without decoding pixels. Formatting is ours and pure, so
//! each row's wording is unit tested.

use std::path::Path;

use exif::{In, Tag, Value};

/// An EXIF date/time as written (local wall-clock time, no zone).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct ExifTime {
    pub year: u16,
    pub month: u16,
    pub day: u16,
    pub hour: u16,
    pub minute: u16,
    pub second: u16,
}

/// The fields the card shows, already turned into display text (dates excepted: the window formats those
/// in the user's locale).
#[derive(Default, Debug, PartialEq)]
pub struct Exif {
    pub make: Option<String>,
    pub model: Option<String>,
    pub lens: Option<String>,
    pub exposure: Option<String>,
    pub aperture: Option<String>,
    pub iso: Option<String>,
    pub focal: Option<String>,
    pub bias: Option<String>,
    pub metering: Option<String>,
    pub flash: Option<String>,
    pub white_balance: Option<String>,
    pub taken: Option<ExifTime>,
    pub digitized: Option<ExifTime>,
    pub latitude: Option<f64>,
    pub longitude: Option<f64>,
    pub altitude: Option<String>,
}

/// Reads the file's EXIF block; None when it has none or the container isn't one EXIF can live in.
pub fn read(path: &Path) -> Option<Exif> {
    let file = std::fs::File::open(path).ok()?;
    let mut r = std::io::BufReader::new(file);
    let exif = exif::Reader::new().read_from_container(&mut r).ok()?;
    let get = |t: Tag| exif.get_field(t, In::PRIMARY).map(|f| &f.value);
    let mut e = Exif {
        make: get(Tag::Make).and_then(ascii),
        model: get(Tag::Model).and_then(ascii),
        lens: get(Tag::LensModel).and_then(ascii),
        exposure: get(Tag::ExposureTime).and_then(rational).map(exposure_time),
        aperture: get(Tag::FNumber).and_then(rational).filter(|v| *v > 0.0).map(|v| format!("f/{}", trim(v, 1))),
        iso: get(Tag::PhotographicSensitivity).and_then(|v| v.get_uint(0)).filter(|v| *v > 0).map(|v| v.to_string()),
        focal: get(Tag::FocalLength).and_then(rational).filter(|v| *v > 0.0).map(|v| format!("{} mm", trim(v, 1))),
        bias: get(Tag::ExposureBiasValue).and_then(rational).map(exposure_bias),
        metering: get(Tag::MeteringMode).and_then(|v| v.get_uint(0)).and_then(metering),
        flash: get(Tag::Flash).and_then(|v| v.get_uint(0)).map(flash),
        white_balance: get(Tag::WhiteBalance).and_then(|v| v.get_uint(0)).and_then(|v| match v {
            0 => Some("Auto".to_string()),
            1 => Some("Manual".to_string()),
            _ => None,
        }),
        taken: get(Tag::DateTimeOriginal).and_then(ascii).and_then(|s| parse_time(&s)),
        digitized: get(Tag::DateTimeDigitized).and_then(ascii).and_then(|s| parse_time(&s)),
        ..Default::default()
    };
    let lat = get(Tag::GPSLatitude).and_then(dms);
    let lon = get(Tag::GPSLongitude).and_then(dms);
    let lat_ref = get(Tag::GPSLatitudeRef).and_then(ascii);
    let lon_ref = get(Tag::GPSLongitudeRef).and_then(ascii);
    if let (Some(lat), Some(lon)) = (lat, lon) {
        // A zero fix is what cameras write without a GPS lock.
        if lat != 0.0 || lon != 0.0 {
            e.latitude = Some(if lat_ref.as_deref() == Some("S") { -lat } else { lat });
            e.longitude = Some(if lon_ref.as_deref() == Some("W") { -lon } else { lon });
        }
    }
    if let Some(alt) = get(Tag::GPSAltitude).and_then(rational) {
        let below = get(Tag::GPSAltitudeRef).and_then(|v| v.get_uint(0)) == Some(1);
        e.altitude = Some(format!("{} metres", trim(if below { -alt } else { alt }, 1)));
    }
    Some(e)
}

fn ascii(v: &Value) -> Option<String> {
    let Value::Ascii(parts) = v else { return None };
    let s = String::from_utf8_lossy(parts.first()?).trim().trim_end_matches('\0').trim().to_string();
    (!s.is_empty()).then_some(s)
}

fn rational(v: &Value) -> Option<f64> {
    match v {
        Value::Rational(r) => r.first().filter(|r| r.denom != 0).map(|r| r.num as f64 / r.denom as f64),
        Value::SRational(r) => r.first().filter(|r| r.denom != 0).map(|r| r.num as f64 / r.denom as f64),
        _ => None,
    }
}

/// Degrees, minutes, seconds → decimal degrees.
fn dms(v: &Value) -> Option<f64> {
    let Value::Rational(r) = v else { return None };
    if r.len() < 3 || r.iter().any(|x| x.denom == 0) {
        return None;
    }
    Some(r[0].to_f64() + r[1].to_f64() / 60.0 + r[2].to_f64() / 3600.0)
}

/// A number with at most `decimals` places and no trailing zeros ("2.8", "8", "50").
fn trim(v: f64, decimals: usize) -> String {
    let s = format!("{v:.decimals$}");
    if s.contains('.') { s.trim_end_matches('0').trim_end_matches('.').to_string() } else { s }
}

fn exposure_time(t: f64) -> String {
    if t <= 0.0 {
        "0 sec".into()
    } else if t >= 1.0 {
        format!("{} sec", trim(t, 1))
    } else {
        // 1/250, and 1/2.5 for the in-between stops
        format!("1/{} sec", trim(1.0 / t, 1))
    }
}

fn exposure_bias(ev: f64) -> String {
    if ev.abs() < 0.005 { "0 EV".into() } else { format!("{}{} EV", if ev > 0.0 { "+" } else { "" }, trim(ev, 2)) }
}

fn metering(v: u32) -> Option<String> {
    Some(
        match v {
            1 => "Average",
            2 => "Center weighted average",
            3 => "Spot",
            4 => "Multi-spot",
            5 => "Multi-segment",
            6 => "Partial",
            255 => "Other",
            _ => return None,
        }
        .into(),
    )
}

fn flash(v: u32) -> String {
    if v & 0x20 != 0 {
        return "No flash".into();
    }
    let mut s = if v & 1 != 0 { "Fired".to_string() } else { "Did not fire".to_string() };
    match (v >> 3) & 3 {
        1 => s.push_str(", forced"),
        2 => s.push_str(", off"),
        3 => s.push_str(", auto"),
        _ => {}
    }
    if v & 0x40 != 0 {
        s.push_str(", red-eye reduction");
    }
    s
}

/// "2024:05:01 13:22:10" (EXIF's own layout).
fn parse_time(s: &str) -> Option<ExifTime> {
    let n: Vec<u16> = s.split([':', ' ', '-', 'T']).filter(|p| !p.is_empty()).take(6).map(|p| p.trim().parse().ok()).collect::<Option<_>>()?;
    if n.len() < 5 || n[0] == 0 || !(1..=12).contains(&n[1]) || !(1..=31).contains(&n[2]) {
        return None;
    }
    Some(ExifTime { year: n[0], month: n[1], day: n[2], hour: n[3], minute: n[4], second: *n.get(5).unwrap_or(&0) })
}

/// A titled group of label/value rows.
pub struct Group {
    pub title: &'static str,
    pub rows: Vec<(&'static str, String)>,
}

/// The Camera / Exposure / Date / Location groups, skipping empty ones (`MetadataService.Read`).
pub fn groups(e: &Exif, date: impl Fn(ExifTime) -> String) -> Vec<Group> {
    let mut out = Vec::new();
    let mut push = |title, rows: Vec<(&'static str, Option<String>)>| {
        let rows: Vec<_> = rows.into_iter().filter_map(|(l, v)| v.map(|v| (l, v))).collect();
        if !rows.is_empty() {
            out.push(Group { title, rows });
        }
    };
    push("Camera", vec![("Make", e.make.clone()), ("Model", e.model.clone()), ("Lens", e.lens.clone())]);
    push(
        "Exposure",
        vec![
            ("Exposure", e.exposure.clone()),
            ("Aperture", e.aperture.clone()),
            ("ISO", e.iso.clone()),
            ("Focal length", e.focal.clone()),
            ("Exposure bias", e.bias.clone()),
            ("Metering", e.metering.clone()),
            ("Flash", e.flash.clone()),
            ("White balance", e.white_balance.clone()),
        ],
    );
    push("Date", vec![("Taken", e.taken.map(&date)), ("Digitized", e.digitized.map(&date))]);
    push(
        "Location",
        vec![
            ("Latitude", e.latitude.map(|v| format!("{v:.6}"))),
            ("Longitude", e.longitude.map(|v| format!("{v:.6}"))),
            ("Altitude", e.altitude.clone()),
        ],
    );
    out
}

// --- Histogram ---------------------------------------------------------------------------------------

/// 256 bins per channel plus luma, and the tallest interior bin (0 and 255 are left out, so a big clipped or
/// flat area doesn't flatten the rest of the curve).
pub struct Histogram {
    pub red: [u32; 256],
    pub green: [u32; 256],
    pub blue: [u32; 256],
    pub luma: [u32; 256],
    pub max: u32,
}

impl Histogram {
    /// From BGRA pixels (the display decode).
    pub fn of(bgra: &[u8]) -> Histogram {
        let mut h = Histogram { red: [0; 256], green: [0; 256], blue: [0; 256], luma: [0; 256], max: 1 };
        for p in bgra.chunks_exact(4) {
            let (b, g, r) = (p[0] as usize, p[1] as usize, p[2] as usize);
            h.red[r] += 1;
            h.green[g] += 1;
            h.blue[b] += 1;
            // Rec. 709 luma, integer weights summing to 256.
            h.luma[(r * 54 + g * 183 + b * 19) >> 8] += 1;
        }
        for i in 1..255 {
            h.max = h.max.max(h.red[i]).max(h.green[i]).max(h.blue[i]).max(h.luma[i]);
        }
        h
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exposure_wording() {
        assert_eq!(exposure_time(1.0 / 250.0), "1/250 sec");
        assert_eq!(exposure_time(2.0), "2 sec");
        assert_eq!(exposure_time(0.5), "1/2 sec");
        assert_eq!(exposure_time(0.4), "1/2.5 sec");
        assert_eq!(exposure_bias(0.0), "0 EV");
        assert_eq!(exposure_bias(-1.0 / 3.0), "-0.33 EV");
        assert_eq!(exposure_bias(0.7), "+0.7 EV");
        assert_eq!(trim(2.8, 1), "2.8");
        assert_eq!(trim(8.0, 1), "8");
    }

    #[test]
    fn flash_wording() {
        assert_eq!(flash(0x10), "Did not fire, off");
        assert_eq!(flash(0x19), "Fired, auto");
        assert_eq!(flash(0x20), "No flash");
    }

    #[test]
    fn exif_dates_parse() {
        assert_eq!(parse_time("2024:05:01 13:22:10"), Some(ExifTime { year: 2024, month: 5, day: 1, hour: 13, minute: 22, second: 10 }));
        assert_eq!(parse_time("0000:00:00 00:00:00"), None);
        assert_eq!(parse_time("    :  :     :  :  "), None);
    }

    #[test]
    fn empty_groups_are_left_out() {
        let e = Exif { model: Some("X100V".into()), iso: Some("200".into()), ..Default::default() };
        let g = groups(&e, |_| String::new());
        assert_eq!(g.iter().map(|g| g.title).collect::<Vec<_>>(), ["Camera", "Exposure"]);
        assert_eq!(g[0].rows, [("Model", "X100V".to_string())]);
    }

    #[test]
    fn histogram_ignores_the_end_bins_for_its_scale() {
        let mut px = vec![0u8; 4 * 10]; // ten black pixels
        px.extend([128, 128, 128, 255]);
        let h = Histogram::of(&px);
        assert_eq!(h.red[0], 10);
        assert_eq!(h.red[128], 1);
        assert_eq!(h.max, 1);
    }
}
