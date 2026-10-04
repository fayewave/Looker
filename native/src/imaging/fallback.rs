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
}
