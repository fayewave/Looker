//! SVG through Direct2D's own SVG renderer (the one Win2D's CanvasSvgDocument wraps, so the same coverage as
//! the C# app), drawn on the worker thread into a WIC bitmap: no GPU device needed. The intrinsic size comes
//! from the root element's width/height, else its viewBox; zooming re-rasterizes up to `VECTOR_MAX_EDGE`.

use std::cell::RefCell;
use std::path::Path;

use windows::Win32::Graphics::Direct2D::Common::*;
use windows::Win32::Graphics::Direct2D::*;
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
use windows::Win32::Graphics::Imaging::*;
use windows::Win32::System::Com::IStream;
use windows::core::Interface;

use super::{Decoded, VECTOR_MAX_EDGE};

thread_local! {
    static FACTORY: RefCell<Option<ID2D1Factory1>> = const { RefCell::new(None) };
}

fn factory() -> Result<ID2D1Factory1, String> {
    FACTORY.with(|cell| {
        if let Some(f) = &*cell.borrow() {
            return Ok(f.clone());
        }
        let f: ID2D1Factory1 = unsafe { D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None) }.map_err(|e| e.message().to_string())?;
        *cell.borrow_mut() = Some(f.clone());
        Ok(f)
    })
}

/// A CSS length in px (absolute units converted at 96 dpi); `None` for percentages and junk.
fn length(v: &str) -> Option<f64> {
    let v = v.trim();
    let split = v.find(|c: char| !(c.is_ascii_digit() || c == '.' || c == '-' || c == '+' || c == 'e' || c == 'E')).unwrap_or(v.len());
    let (num, unit) = v.split_at(split);
    let n: f64 = num.parse().ok()?;
    let k = match unit.trim() {
        "" | "px" => 1.0,
        "pt" => 96.0 / 72.0,
        "pc" => 16.0,
        "in" => 96.0,
        "cm" => 96.0 / 2.54,
        "mm" => 96.0 / 25.4,
        "em" => 16.0,
        _ => return None,
    };
    (n > 0.0).then_some(n * k)
}

fn attr<'a>(tag: &'a str, name: &str) -> Option<&'a str> {
    let mut rest = tag;
    while let Some(i) = rest.find(name) {
        let before = rest[..i].chars().last();
        let after = &rest[i + name.len()..];
        let after_trim = after.trim_start();
        if before.is_some_and(|c| c.is_whitespace()) && after_trim.starts_with('=') {
            let v = after_trim[1..].trim_start();
            let q = v.chars().next()?;
            if q == '"' || q == '\'' {
                let end = v[1..].find(q)?;
                return Some(&v[1..1 + end]);
            }
        }
        rest = &rest[i + name.len()..];
    }
    None
}

/// Root `<svg>` width/height, else the viewBox size.
pub fn intrinsic_size(text: &str) -> Option<(f64, f64)> {
    let start = text.find("<svg")?;
    let tag = &text[start..start + text[start..].find('>')?];
    if let (Some(w), Some(h)) = (attr(tag, "width").and_then(length), attr(tag, "height").and_then(length)) {
        return Some((w, h));
    }
    let vb: Vec<f64> = attr(tag, "viewBox")?
        .split(|c: char| c.is_whitespace() || c == ',')
        .filter(|s| !s.is_empty())
        .filter_map(|s| s.parse().ok())
        .collect();
    (vb.len() == 4 && vb[2] > 0.0 && vb[3] > 0.0).then(|| (vb[2], vb[3]))
}

pub fn decode(f: &IWICImagingFactory, path: &Path, box_w: u32, box_h: u32) -> Result<Decoded, String> {
    let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
    let (box_w, box_h) = if box_w == 0 || box_h == 0 { (VECTOR_MAX_EDGE, VECTOR_MAX_EDGE) } else { (box_w, box_h) };
    let (bw, bh) = intrinsic_size(&String::from_utf8_lossy(&bytes[..bytes.len().min(64 * 1024)]))
        .unwrap_or((box_w.max(box_h) as f64, box_w.max(box_h) as f64));
    // A vector scales up as well as down: fit the box, capped at the per-bitmap limit.
    let mut scale = (box_w as f64 / bw).min(box_h as f64 / bh);
    scale = scale.min(VECTOR_MAX_EDGE as f64 / bw.max(bh));
    let w = ((bw * scale).round() as u32).max(1);
    let h = ((bh * scale).round() as u32).max(1);
    let e = |e: windows::core::Error| e.message().to_string();
    unsafe {
        let bmp = f.CreateBitmap(w, h, &GUID_WICPixelFormat32bppPBGRA, WICBitmapCacheOnLoad).map_err(e)?;
        let props = D2D1_RENDER_TARGET_PROPERTIES {
            r#type: D2D1_RENDER_TARGET_TYPE_SOFTWARE,
            pixelFormat: D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED },
            dpiX: 96.0,
            dpiY: 96.0,
            ..Default::default()
        };
        let rt = factory()?.CreateWicBitmapRenderTarget(&bmp, &props).map_err(e)?;
        let dc: ID2D1DeviceContext5 = rt.cast().map_err(|_| "this Windows has no Direct2D SVG renderer".to_string())?;
        let stream = f.CreateStream().map_err(e)?;
        stream.InitializeFromMemory(&bytes).map_err(e)?;
        let stream: IStream = stream.cast().map_err(e)?;
        let doc = dc.CreateSvgDocument(&stream, D2D_SIZE_F { width: bw as f32, height: bh as f32 }).map_err(e)?;
        dc.BeginDraw();
        dc.Clear(Some(&D2D1_COLOR_F { r: 0.0, g: 0.0, b: 0.0, a: 0.0 }));
        dc.SetTransform(&windows_numerics::Matrix3x2 { M11: scale as f32, M12: 0.0, M21: 0.0, M22: scale as f32, M31: 0.0, M32: 0.0 });
        dc.DrawSvgDocument(&doc);
        dc.EndDraw(None, None).map_err(e)?;
        let mut px = vec![0u8; (w * h * 4) as usize];
        bmp.CopyPixels(std::ptr::null(), w * 4, &mut px).map_err(e)?;
        let mut d = Decoded::still(w, h, px, bw.round().max(1.0) as u32, bh.round().max(1.0) as u32, None);
        d.vector = true;
        Ok(d)
    }
}

#[cfg(test)]
mod tests {
    use super::intrinsic_size;

    #[test]
    fn reads_width_height_then_viewbox() {
        assert_eq!(intrinsic_size(r#"<svg xmlns="x" width="200" height="100">"#), Some((200.0, 100.0)));
        assert_eq!(intrinsic_size(r#"<svg width="1in" height='2in'>"#), Some((96.0, 192.0)));
        assert_eq!(intrinsic_size(r#"<svg width="100%" height="100%" viewBox="0 0 640 480">"#), Some((640.0, 480.0)));
        assert_eq!(intrinsic_size(r#"<?xml version="1.0"?><svg viewBox="0,0,24,24"/>"#), Some((24.0, 24.0)));
        assert_eq!(intrinsic_size(r#"<svg stroke-width="3" viewBox="0 0 10 20">"#), Some((10.0, 20.0)));
        assert_eq!(intrinsic_size("<svg>"), None);
    }
}
