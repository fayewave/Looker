//! Pure-Rust decoders for what WIC can't read (or can't without a Store codec). Each produces full-size
//! straight RGBA and hands it to [`wic::from_rgba`] for scaling and premultiplying. Statically linked and
//! with no initialisation of their own, so they cost nothing until a file needs one.

use std::path::Path;

use windows::Win32::Graphics::Imaging::IWICImagingFactory;

use super::{Decoded, wic};

fn finish(f: &IWICImagingFactory, rgba: &[u8], w: u32, h: u32, box_w: u32, box_h: u32) -> Result<Decoded, String> {
    if w == 0 || h == 0 || rgba.len() < (w as usize * h as usize * 4) {
        return Err("empty image".into());
    }
    let (dw, dh, px) = wic::from_rgba(f, rgba, w, h, box_w, box_h).map_err(|e| e.message().to_string())?;
    Ok(Decoded::still(dw, dh, px, w, h, None))
}

/// Interleaved samples with `channels` per pixel (gray, gray+alpha, RGB, RGBA, CMYK, CMYK+alpha) → RGBA.
fn to_rgba(samples: &[u8], w: u32, h: u32, channels: usize, cmyk: bool) -> Vec<u8> {
    let n = w as usize * h as usize;
    let mut out = vec![0u8; n * 4];
    for (i, o) in out.chunks_exact_mut(4).enumerate() {
        let p = &samples[i * channels..i * channels + channels];
        let (rgb, a) = match (channels, cmyk) {
            (1, _) => ([p[0]; 3], 255),
            (2, _) => ([p[0]; 3], p[1]),
            (3, _) => ([p[0], p[1], p[2]], 255),
            (4, false) => ([p[0], p[1], p[2]], p[3]),
            (4 | 5, true) => {
                let k = 255 - p[3] as u32;
                let c = |v: u8| ((255 - v as u32) * k / 255) as u8;
                ([c(p[0]), c(p[1]), c(p[2])], if channels == 5 { p[4] } else { 255 })
            }
            _ => ([p[0], p[1.min(channels - 1)], p[2.min(channels - 1)]], 255),
        };
        o[..3].copy_from_slice(&rgb);
        o[3] = a;
    }
    out
}

/// TGA, QOI, Netpbm, DDS, and the PNG/GIF/WebP decoders when WIC fails.
pub fn image(f: &IWICImagingFactory, path: &Path, box_w: u32, box_h: u32) -> Result<Decoded, String> {
    let img = image::ImageReader::open(path)
        .map_err(|e| e.to_string())?
        .with_guessed_format()
        .map_err(|e| e.to_string())?
        .decode()
        .map_err(|e| e.to_string())?
        .to_rgba8();
    let (w, h) = img.dimensions();
    finish(f, img.as_raw(), w, h, box_w, box_h)
}

/// OpenEXR and Radiance HDR: linear light, 1.0 taken as SDR white. On an scRGB display an HDR decode (highlights
/// kept, tone-mapped to the panel by gfx.rs); elsewhere clipped to SDR and sRGB-encoded.
pub fn float_image(f: &IWICImagingFactory, path: &Path, box_w: u32, box_h: u32) -> Result<Decoded, String> {
    let img = image::ImageReader::open(path)
        .map_err(|e| e.to_string())?
        .with_guessed_format()
        .map_err(|e| e.to_string())?
        .decode()
        .map_err(|e| e.to_string())?
        .into_rgba32f();
    let (w, h) = img.dimensions();
    if w == 0 || h == 0 {
        return Err("empty image".into());
    }
    let px = img.into_raw();
    // NaN and negative values (out-of-gamut EXR) would poison the averages: dark is the honest reading.
    let clean = |v: f32| if v.is_finite() { v.max(0.0) } else { 0.0 };
    let rgb: Vec<[f32; 3]> = px.chunks_exact(4).map(|p| [clean(p[0]), clean(p[1]), clean(p[2])]).collect();
    let alpha: Vec<f32> = px.chunks_exact(4).map(|p| if p[3].is_finite() { p[3].clamp(0.0, 1.0) } else { 1.0 }).collect();
    drop(px);
    let opaque = alpha.iter().all(|&a| a >= 1.0);
    if super::target().is_scrgb() {
        let (dw, dh, rgb, alpha) = super::hdr::fit(rgb, (!opaque).then_some(alpha), w, h, box_w, box_h);
        let peak = super::hdr::peak(&rgb);
        crate::trace::mark(format!("float image: peak {peak:.2}x SDR white"));
        let mut d = Decoded::still(dw, dh, super::hdr::to_half(&rgb, alpha.as_deref()), w, h, None);
        d.histogram = Some(Box::new(crate::metadata::Histogram::of(&super::hdr::sdr_bgra(&rgb))));
        d.colour = super::Colour::Linear { absolute: false, peak };
        return Ok(d);
    }
    let enc = |v: f32| {
        let v = v.clamp(0.0, 1.0);
        let e = if v <= 0.003_130_8 { v * 12.92 } else { 1.055 * v.powf(1.0 / 2.4) - 0.055 };
        (e * 255.0).round() as u8
    };
    let mut rgba = Vec::with_capacity(rgb.len() * 4);
    for (c, a) in rgb.iter().zip(&alpha) {
        rgba.extend_from_slice(&[enc(c[0]), enc(c[1]), enc(c[2]), (a * 255.0).round() as u8]);
    }
    finish(f, &rgba, w, h, box_w, box_h)
}

/// HEIC/HEIF/AVIF through the bundled libheif (see `heif.rs`).
pub fn heif(f: &IWICImagingFactory, path: &Path, box_w: u32, box_h: u32) -> Result<Decoded, String> {
    let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
    let (w, h, rgba) = super::heif::decode(&bytes)?;
    finish(f, &rgba, w, h, box_w, box_h)
}

/// Camera RAW through the bundled LibRaw; the native size is the RAW's full size even when it decoded at half.
pub fn raw(f: &IWICImagingFactory, path: &Path, box_w: u32, box_h: u32) -> Result<Decoded, String> {
    let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
    let r = super::raw::decode(&bytes, box_w, box_h)?;
    if r.width == 0 || r.height == 0 {
        return Err("empty image".into());
    }
    let (dw, dh, px) = wic::from_rgba(f, &r.rgba, r.width, r.height, box_w, box_h).map_err(|e| e.message().to_string())?;
    // Unix seconds to a FILETIME (100 ns ticks since 1601), as WIC's DateTaken comes.
    let taken = r.taken.map(|t| {
        let ticks = (t as u64 + 11_644_473_600) * 10_000_000;
        windows::Win32::Foundation::FILETIME { dwLowDateTime: ticks as u32, dwHighDateTime: (ticks >> 32) as u32 }
    });
    Ok(Decoded::still(dw, dh, px, r.full_width.max(r.width), r.full_height.max(r.height), taken))
}

/// Flattened composite (the image Photoshop saves alongside the layers, "maximize compatibility"); PSD and PSB.
pub fn psd(f: &IWICImagingFactory, path: &Path, box_w: u32, box_h: u32) -> Result<Decoded, String> {
    let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
    let (w, h, rgba) = super::raster::psd(&bytes)?;
    finish(f, &rgba, w, h, box_w, box_h)
}

pub fn jpeg_xl(f: &IWICImagingFactory, path: &Path, box_w: u32, box_h: u32) -> Result<Decoded, String> {
    use jxl_oxide::{EnumColourEncoding, JxlImage, RenderingIntent};
    let mut img = JxlImage::builder().open(path).map_err(|e| e.to_string())?;
    img.set_cms(jxl_oxide::Moxcms);
    img.request_color_encoding(EnumColourEncoding::srgb(RenderingIntent::Relative));
    let render = img.render_frame(0).map_err(|e| e.to_string())?;
    let mut stream = render.stream();
    let (w, h, c) = (stream.width(), stream.height(), stream.channels() as usize);
    let mut samples = vec![0u8; w as usize * h as usize * c];
    stream.write_to_buffer(&mut samples);
    finish(f, &to_rgba(&samples, w, h, c, false), w, h, box_w, box_h)
}

pub fn jpeg_2000(f: &IWICImagingFactory, path: &Path, box_w: u32, box_h: u32) -> Result<Decoded, String> {
    use hayro_jpeg2000::{ColorSpace, DecodeSettings, DecoderContext, Image};
    let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
    let img = Image::new(&bytes, &DecodeSettings::default()).map_err(|e| format!("{e:?}"))?;
    let (w, h) = (img.width(), img.height());
    let cmyk = matches!(img.color_space(), ColorSpace::CMYK) || matches!(img.color_space(), ColorSpace::Icc { num_channels: 4, .. });
    let mut ctx = DecoderContext::default();
    let decoded = img.decode(&mut ctx).map_err(|e| format!("{e:?}"))?;
    let channels = decoded.components().len();
    let samples = decoded.data_u8();
    finish(f, &to_rgba(&samples, w, h, channels, cmyk), w, h, box_w, box_h)
}

pub fn pcx(f: &IWICImagingFactory, path: &Path, box_w: u32, box_h: u32) -> Result<Decoded, String> {
    let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
    let (w, h, rgba) = super::raster::pcx(&bytes)?;
    finish(f, &rgba, w, h, box_w, box_h)
}

/// GIMP's own format: the visible layers composited bottom to top ("normal" mode, layer opacity, offsets).
/// Other blend modes draw as normal; that's close enough for a viewer.
pub fn xcf(f: &IWICImagingFactory, path: &Path, box_w: u32, box_h: u32) -> Result<Decoded, String> {
    use xcf::{PropertyIdentifier, PropertyPayload};
    let doc = xcf::Xcf::open(path).map_err(|e| format!("{e:?}"))?;
    let (w, h) = doc.dimensions();
    let mut canvas = vec![0f32; w as usize * h as usize * 4]; // premultiplied, 0..1
    for layer in doc.layers.iter().rev() {
        let (mut visible, mut opacity, mut ox, mut oy) = (true, 1.0f32, 0i64, 0i64);
        for p in &layer.properties {
            let PropertyPayload::Unknown(b) = &p.payload else { continue };
            let be = |i: usize| b.get(i..i + 4).map(|s| u32::from_be_bytes([s[0], s[1], s[2], s[3]]));
            match p.kind {
                PropertyIdentifier::PropVisible => visible = be(0).is_some_and(|v| v != 0),
                PropertyIdentifier::PropOpacity => opacity = be(0).map_or(1.0, |v| v.min(255) as f32 / 255.0),
                PropertyIdentifier::PropOffsets => {
                    ox = be(0).map_or(0, |v| v as i32 as i64);
                    oy = be(4).map_or(0, |v| v as i32 as i64);
                }
                _ => {}
            }
        }
        if !visible {
            continue;
        }
        let px = layer.raw_rgba_buffer();
        for ly in 0..layer.height as i64 {
            let y = ly + oy;
            if y < 0 || y >= h as i64 {
                continue;
            }
            for lx in 0..layer.width as i64 {
                let x = lx + ox;
                if x < 0 || x >= w as i64 {
                    continue;
                }
                let s = &px[(ly * layer.width as i64 + lx) as usize];
                let a = s.a() as f32 / 255.0 * opacity;
                let d = &mut canvas[((y * w as i64 + x) * 4) as usize..][..4];
                for (c, v) in [s.r(), s.g(), s.b()].into_iter().enumerate() {
                    d[c] = v as f32 / 255.0 * a + d[c] * (1.0 - a);
                }
                d[3] = a + d[3] * (1.0 - a);
            }
        }
    }
    // Back to straight RGBA for the shared tail.
    let rgba: Vec<u8> = canvas
        .chunks_exact(4)
        .flat_map(|p| {
            let a = p[3];
            let un = |v: f32| if a > 0.0 { (v / a * 255.0).round().clamp(0.0, 255.0) as u8 } else { 0 };
            [un(p[0]), un(p[1]), un(p[2]), (a * 255.0).round() as u8]
        })
        .collect();
    finish(f, &rgba, w, h, box_w, box_h)
}

#[cfg(test)]
mod tests {
    use super::to_rgba;

    #[test]
    fn expands_channel_layouts() {
        assert_eq!(to_rgba(&[10], 1, 1, 1, false), [10, 10, 10, 255]);
        assert_eq!(to_rgba(&[10, 20], 1, 1, 2, false), [10, 10, 10, 20]);
        assert_eq!(to_rgba(&[1, 2, 3], 1, 1, 3, false), [1, 2, 3, 255]);
        assert_eq!(to_rgba(&[1, 2, 3, 4], 1, 1, 4, false), [1, 2, 3, 4]);
        // CMYK: no ink is white, full black is black.
        assert_eq!(to_rgba(&[0, 0, 0, 0], 1, 1, 4, true), [255, 255, 255, 255]);
        assert_eq!(to_rgba(&[0, 0, 0, 255], 1, 1, 4, true), [0, 0, 0, 255]);
    }

    /// EXR and Radiance HDR written by the image crate come back through the decoder chain: clipped and
    /// sRGB-encoded for an SDR screen, kept above 1.0 for an scRGB one.
    #[test]
    fn float_images_round_trip() {
        use crate::imaging::{Colour, decode, decode_for_screen};
        use windows::Win32::Graphics::Imaging::{CLSID_WICImagingFactory2, IWICImagingFactory};
        use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx};
        let f: IWICImagingFactory = unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
            CoCreateInstance(&CLSID_WICImagingFactory2, None, CLSCTX_INPROC_SERVER).unwrap()
        };
        // Black, 18 % grey, SDR white, four times SDR white.
        let px: Vec<f32> = [0.0f32, 0.18, 1.0, 4.0].iter().flat_map(|&v| [v, v, v]).collect();
        let img = image::Rgb32FImage::from_raw(4, 1, px).unwrap();
        let dir = std::env::temp_dir();
        for ext in ["exr", "hdr"] {
            let path = dir.join(format!("looker-float-test.{ext}"));
            image::DynamicImage::ImageRgb32F(img.clone()).save(&path).unwrap();
            let d = decode(&f, &path, 0, 0, true).unwrap();
            assert_eq!((d.width, d.height), (4, 1), "{ext}");
            // Premultiplied BGRA: blue of each pixel. 18 % linear is ~118 in sRGB.
            let b: Vec<u8> = d.frames[0].pixels.chunks_exact(4).map(|p| p[0]).collect();
            assert_eq!(b[0], 0, "{ext}");
            assert!((b[1] as i32 - 118).abs() <= 2, "{ext}: {b:?}");
            assert_eq!(&b[2..], &[255, 255], "{ext}");
            let out = crate::colour::Output { space: crate::colour::Space::Scrgb { white_nits: 200.0, hdr: true }, generation: 0 };
            let d = decode_for_screen(&f, &path, 0, 0, true, &out).unwrap();
            match d.colour {
                Colour::Linear { absolute: false, peak } => assert!((peak - 4.0).abs() < 0.1, "{ext}: {peak}"),
                c => panic!("{ext}: {c:?}"),
            }
            let _ = std::fs::remove_file(&path);
        }
    }
}
