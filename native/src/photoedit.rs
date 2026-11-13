//! Basic editing's pixels (the edit mode is app/edit.rs): a photo turned by quarter turns, straightened by a
//! small angle about its middle, cropped to a rectangle upright on screen, and its exposure moved, at full size,
//! then written as a copy beside it or over it (the original to the Recycle Bin first).
//!
//! Exposure is a gain in linear light under a 2.2 gamma: 8-bit values are multiplied by 2^(ev / 2.2) and clipped,
//! exactly what the edit mode's preview does with Direct2D (a colour matrix on the photo's sRGB values), so the
//! saved file looks as the preview did.

use std::path::{Path, PathBuf};

use windows::Foundation::{PropertyType, PropertyValue};
use windows::Graphics::Imaging::{BitmapAlphaMode, BitmapDecoder, BitmapEncoder, BitmapPixelFormat, BitmapPropertySet, BitmapTypedValue};
use windows::Storage::FileAccessMode;
use windows::core::HSTRING;

/// What an edit does, in the space it's seen in: the image turned by `turns` quarter turns (clockwise), `w` x `h`
/// pixels after them; turned `angle` degrees clockwise about its middle; cropped to `crop` (left, top, right,
/// bottom in pixels from the image's middle, upright on screen); exposure `ev` stops.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Params {
    pub turns: u8,
    pub angle: f32,
    pub crop: [f32; 4],
    pub ev: f32,
}

/// Exposure's gain on gamma-encoded values.
pub fn gain(ev: f32) -> f32 {
    2f32.powf(ev / 2.2)
}

/// Whether the point `(x, y)` (pixels from the middle, upright on screen) is on the image turned by `angle`.
pub fn on_image(w: f32, h: f32, angle: f32, x: f32, y: f32) -> bool {
    let (s, c) = angle.to_radians().sin_cos();
    let (qx, qy) = (c * x + s * y, -s * x + c * y);
    qx.abs() <= w / 2.0 + 0.01 && qy.abs() <= h / 2.0 + 0.01
}

/// Whether all of `crop` lies on the image.
pub fn crop_fits(w: f32, h: f32, angle: f32, crop: [f32; 4]) -> bool {
    let [l, t, r, b] = crop;
    r > l && b > t && [(l, t), (r, t), (r, b), (l, b)].iter().all(|&(x, y)| on_image(w, h, angle, x, y))
}

/// The biggest crop of aspect `a` (width over height) centred on the image turned by `angle`.
pub fn largest(w: f32, h: f32, angle: f32, a: f32) -> [f32; 4] {
    let (s, c) = angle.to_radians().sin_cos();
    let (s, c) = (s.abs(), c.abs());
    let ch = (w / (a * c + s)).min(h / (a * s + c));
    let cw = a * ch;
    [-cw / 2.0, -ch / 2.0, cw / 2.0, ch / 2.0]
}

/// `p` changes nothing.
pub fn is_identity(p: &Params, w: f32, h: f32) -> bool {
    let full = [-w / 2.0, -h / 2.0, w / 2.0, h / 2.0];
    p.turns == 0 && p.angle.abs() < 0.005 && p.ev.abs() < 0.005 && p.crop.iter().zip(full).all(|(a, b)| (a - b).abs() < 0.5)
}

/// Applies `p` to premultiplied BGRA `src` (`sw` x `sh`, upright: before `p.turns`): the result and its size.
pub fn apply(src: &[u8], sw: u32, sh: u32, p: &Params) -> (Vec<u8>, u32, u32) {
    let turned;
    let (src, w, h) = if p.turns % 4 != 0 {
        turned = crate::fileops::rotate_bgra(src, sw, sh, (p.turns % 4) as i32 * 90);
        (&turned.0[..], turned.1, turned.2)
    } else {
        (src, sw, sh)
    };
    let [l, t, r, b] = p.crop;
    let ow = ((r - l).round() as u32).max(1);
    let oh = ((b - t).round() as u32).max(1);
    let (sn, cs) = p.angle.to_radians().sin_cos();
    let (hw, hh) = (w as f32 / 2.0, h as f32 / 2.0);
    let g = gain(p.ev);
    // Exposure as a table per alpha would be exact for premultiplied values; per value with the alpha clip is
    // enough (photos are opaque).
    let mut out = vec![0u8; ow as usize * oh as usize * 4];
    let threads = std::thread::available_parallelism().map_or(4, |n| n.get()).clamp(1, 16);
    let rows_per = (oh as usize).div_ceil(threads);
    std::thread::scope(|scope| {
        for (ci, chunk) in out.chunks_mut(rows_per * ow as usize * 4).enumerate() {
            let src = &src;
            scope.spawn(move || {
                let y0 = ci * rows_per;
                for (ry, row) in chunk.chunks_mut(ow as usize * 4).enumerate() {
                    let sy = t + (y0 + ry) as f32 + 0.5;
                    for ox in 0..ow as usize {
                        let sx = l + ox as f32 + 0.5;
                        // Upright on screen -> the image's own pixels (turned back by the angle).
                        let qx = cs * sx + sn * sy + hw - 0.5;
                        let qy = -sn * sx + cs * sy + hh - 0.5;
                        let px = sample(src, w, h, qx, qy);
                        let d = &mut row[ox * 4..ox * 4 + 4];
                        let a = px[3];
                        for k in 0..3 {
                            d[k] = (px[k] * g).min(a).round().clamp(0.0, 255.0) as u8;
                        }
                        d[3] = a.round().clamp(0.0, 255.0) as u8;
                    }
                }
            });
        }
    });
    (out, ow, oh)
}

/// Bilinear sample of premultiplied BGRA at pixel centre coordinates `(x, y)`; outside is transparent.
fn sample(src: &[u8], w: u32, h: u32, x: f32, y: f32) -> [f32; 4] {
    let (w, h) = (w as i32, h as i32);
    let (x0, y0) = (x.floor() as i32, y.floor() as i32);
    let (fx, fy) = (x - x0 as f32, y - y0 as f32);
    let at = |xx: i32, yy: i32| -> [f32; 4] {
        // Edges are held (a crop that touches the image's border reads its last pixel, not black).
        let (xx, yy) = (xx.clamp(0, w - 1), yy.clamp(0, h - 1));
        let i = (yy as usize * w as usize + xx as usize) * 4;
        [src[i] as f32, src[i + 1] as f32, src[i + 2] as f32, src[i + 3] as f32]
    };
    if x < -1.0 || y < -1.0 || x > w as f32 || y > h as f32 {
        return [0.0; 4];
    }
    let (a, b, c, d) = (at(x0, y0), at(x0 + 1, y0), at(x0, y0 + 1), at(x0 + 1, y0 + 1));
    let mut o = [0f32; 4];
    for k in 0..4 {
        let top = a[k] + (b[k] - a[k]) * fx;
        let bot = c[k] + (d[k] - c[k]) * fx;
        o[k] = top + (bot - top) * fy;
    }
    o
}

/// The extensions an edit can be written back into (WIC has their encoders).
pub fn writable(path: &Path) -> bool {
    encoder_for(path).is_some()
}

fn encoder_for(path: &Path) -> Option<windows::core::GUID> {
    let ext = path.extension()?.to_string_lossy().to_ascii_lowercase();
    match ext.as_str() {
        "jpg" | "jpeg" | "jpe" | "jfif" => BitmapEncoder::JpegEncoderId().ok(),
        "png" => BitmapEncoder::PngEncoderId().ok(),
        "tif" | "tiff" => BitmapEncoder::TiffEncoderId().ok(),
        "bmp" | "dib" => BitmapEncoder::BmpEncoderId().ok(),
        _ => None,
    }
}

/// Where a copy of `path` goes: "name (edited).ext" beside it, numbered when taken; a format that can't be
/// written goes to JPEG (PNG with `alpha`).
pub fn copy_path(path: &Path, alpha: bool) -> PathBuf {
    let stem = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "photo".into());
    let ext = if writable(path) {
        path.extension().map(|e| e.to_string_lossy().into_owned()).unwrap_or_else(|| "jpg".into())
    } else if alpha {
        "png".into()
    } else {
        "jpg".into()
    };
    let dir = path.parent().map(Path::to_path_buf).unwrap_or_default();
    let mut n = 1;
    loop {
        let name = if n == 1 { format!("{stem} (edited).{ext}") } else { format!("{stem} (edited {n}).{ext}") };
        let p = dir.join(name);
        if !p.exists() {
            return p;
        }
        n += 1;
    }
}

fn err(e: windows::core::Error) -> String {
    let m = e.message();
    if m.is_empty() { format!("{:#010x}", e.code().0) } else { m.to_string() }
}

/// What's kept from the original's metadata (its orientation isn't: the pixels are upright now).
const KEPT: [&str; 20] = [
    "System.Photo.DateTaken",
    "System.Photo.CameraManufacturer",
    "System.Photo.CameraModel",
    "System.Photo.LensModel",
    "System.Photo.LensManufacturer",
    "System.Photo.FNumber",
    "System.Photo.ExposureTime",
    "System.Photo.ISOSpeed",
    "System.Photo.FocalLength",
    "System.Photo.FocalLengthInFilm",
    "System.Photo.Flash",
    "System.Photo.ExposureBias",
    "System.Photo.WhiteBalance",
    "System.Title",
    "System.Comment",
    "System.Keywords",
    "System.SimpleRating",
    "System.Author",
    "System.Copyright",
    "System.ApplicationName",
];

/// Decodes `path` at full size, applies `p`, and writes the result: to a copy beside it (`copy`), or over it,
/// the original moved to the Recycle Bin first (`owner` owns the shell's question for a drive without one).
/// Returns the file written.
pub fn save(path: &Path, p: &Params, copy: bool, owner: isize) -> Result<PathBuf, String> {
    let img = crate::fileops::full_image(path, 0).ok_or("couldn't read the photo")?;
    let (pixels, w, h) = apply(&img.pixels, img.width, img.height, p);
    let alpha = pixels.chunks_exact(4).any(|px| px[3] < 255);
    let out = if copy { copy_path(path, alpha) } else { path.to_path_buf() };
    if !copy && !writable(path) {
        return Err("this format can't be written: save a copy".into());
    }
    let encoder_id = match encoder_for(&out) {
        Some(e) => e,
        None => return Err("no encoder".into()),
    };
    // Written to a sibling temp first: the original is only touched once the new file is whole.
    let name = out.file_name().ok_or("no file name")?.to_string_lossy().into_owned();
    let temp = out.with_file_name(format!(".{name}.looker.tmp"));
    let r = (|| {
        std::fs::File::create(&temp).map_err(|e| e.to_string())?;
        encode(path, &temp, encoder_id, &pixels, w, h)?;
        if copy {
            std::fs::rename(&temp, &out).map_err(|e| e.to_string())
        } else {
            // The shell's file operation may ask (a drive without a Recycle Bin): an STA of its own for it.
            let p = path.to_path_buf();
            let recycled = std::thread::spawn(move || unsafe {
                let _ = windows::Win32::System::Com::CoInitializeEx(None, windows::Win32::System::Com::COINIT_APARTMENTTHREADED);
                crate::fileops::recycle(&p, owner)
            })
            .join()
            .unwrap_or(false);
            if !recycled {
                return Err("couldn't move the original to the Recycle Bin".into());
            }
            std::fs::rename(&temp, &out).map_err(|e| e.to_string())
        }
    })();
    let _ = std::fs::remove_file(&temp);
    r.map(|()| out)
}

fn encode(original: &Path, dst_path: &Path, encoder_id: windows::core::GUID, pixels: &[u8], w: u32, h: u32) -> Result<(), String> {
    let dst = crate::fileops::open_stream(dst_path, FileAccessMode::ReadWrite)?;
    let encoder = if encoder_id == BitmapEncoder::JpegEncoderId().map_err(err)? {
        let options = BitmapPropertySet::new().map_err(err)?;
        let quality = BitmapTypedValue::Create(&PropertyValue::CreateSingle(0.95).map_err(err)?, PropertyType::Single).map_err(err)?;
        options.Insert(&HSTRING::from("ImageQuality"), &quality).map_err(err)?;
        BitmapEncoder::CreateWithEncodingOptionsAsync(encoder_id, &dst, &options).and_then(|op| op.join()).map_err(err)?
    } else {
        BitmapEncoder::CreateAsync(encoder_id, &dst).and_then(|op| op.join()).map_err(err)?
    };
    let alpha = if pixels.chunks_exact(4).any(|px| px[3] < 255) { BitmapAlphaMode::Premultiplied } else { BitmapAlphaMode::Ignore };
    encoder.SetPixelData(BitmapPixelFormat::Bgra8, alpha, w, h, 96.0, 96.0, pixels).map_err(err)?;
    copy_metadata(original, &encoder);
    encoder.FlushAsync().and_then(|op| op.join()).map_err(err)?;
    Ok(())
}

/// Carries the original's date taken, camera, exposure and words over, each as far as the new format takes it.
fn copy_metadata(original: &Path, encoder: &BitmapEncoder) {
    let r = (|| -> Result<(), String> {
        let src = crate::fileops::open_stream(original, FileAccessMode::Read)?;
        let decoder = BitmapDecoder::CreateAsync(&src).and_then(|op| op.join()).map_err(err)?;
        let props = decoder.BitmapProperties().map_err(err)?;
        let out = encoder.BitmapProperties().map_err(err)?;
        for name in KEPT {
            let list = windows_collections::IIterable::<HSTRING>::from(vec![HSTRING::from(name)]);
            let Ok(found) = props.GetPropertiesAsync(&list).and_then(|op| op.join()) else { continue };
            if found.Size().unwrap_or(0) == 0 {
                continue;
            }
            // One at a time: a property the format can't hold fails alone.
            let _ = out.SetPropertiesAsync(&found).and_then(|op| op.join());
        }
        Ok(())
    })();
    if let Err(e) = r {
        crate::trace::mark(format!("edit: metadata not copied: {e}"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn largest_crops_fit_and_touch() {
        for angle in [0.0f32, 3.0, -12.5, 45.0] {
            for a in [1.0f32, 1.5, 0.75, 16.0 / 9.0] {
                let c = largest(4000.0, 3000.0, angle, a);
                assert!(crop_fits(4000.0, 3000.0, angle, c), "{angle} {a}");
                let bigger = [c[0] * 1.01, c[1] * 1.01, c[2] * 1.01, c[3] * 1.01];
                assert!(!crop_fits(4000.0, 3000.0, angle, bigger), "{angle} {a} not the largest");
                assert!(((c[2] - c[0]) / (c[3] - c[1]) - a).abs() < 1e-3);
            }
        }
        assert_eq!(largest(400.0, 300.0, 0.0, 4.0 / 3.0), [-200.0, -150.0, 200.0, 150.0]);
    }

    #[test]
    fn applies_crop_turn_and_exposure() {
        // 4 x 2: left half red, right half blue (BGRA, opaque).
        let mut src = Vec::new();
        for _y in 0..2 {
            for x in 0..4 {
                src.extend_from_slice(if x < 2 { &[0, 0, 200, 255] } else { &[200, 0, 0, 255] });
            }
        }
        // The right half only.
        let p = Params { turns: 0, angle: 0.0, crop: [0.0, -1.0, 2.0, 1.0], ev: 0.0 };
        let (out, w, h) = apply(&src, 4, 2, &p);
        assert_eq!((w, h), (2, 2));
        assert!(out.chunks_exact(4).all(|px| px == [200, 0, 0, 255]));
        // A quarter turn clockwise: 2 x 4, red on top.
        let p = Params { turns: 1, angle: 0.0, crop: [-1.0, -2.0, 1.0, 2.0], ev: 0.0 };
        let (out, w, h) = apply(&src, 4, 2, &p);
        assert_eq!((w, h), (2, 4));
        assert_eq!(&out[0..4], &[0, 0, 200, 255]);
        assert_eq!(&out[out.len() - 4..], &[200, 0, 0, 255]);
        // One stop up: x 2^(1/2.2).
        let p = Params { turns: 0, angle: 0.0, crop: [-2.0, -1.0, 2.0, 1.0], ev: 1.0 };
        let (out, _, _) = apply(&src, 4, 2, &p);
        let want = (100.0 * gain(1.0)).round() as u8;
        let mut half = src.clone();
        half.iter_mut().enumerate().for_each(|(i, v)| if i % 4 != 3 && *v > 0 { *v = 100 });
        let (out2, _, _) = apply(&half, 4, 2, &p);
        assert_eq!(out2[2], want);
        assert_eq!(out[2], 255); // 200 x 1.37 clips
    }

    #[test]
    fn copies_are_numbered() {
        let dir = std::env::temp_dir().join("looker-edit-copy-test");
        let _ = std::fs::create_dir_all(&dir);
        let orig = dir.join("a.jpg");
        let first = copy_path(&orig, false);
        assert_eq!(first.file_name().unwrap(), "a (edited).jpg");
        std::fs::write(&first, b"x").unwrap();
        assert_eq!(copy_path(&orig, false).file_name().unwrap(), "a (edited 2).jpg");
        assert_eq!(copy_path(&dir.join("b.heic"), false).file_name().unwrap(), "b (edited).jpg");
        assert_eq!(copy_path(&dir.join("b.webp"), true).file_name().unwrap(), "b (edited).png");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
