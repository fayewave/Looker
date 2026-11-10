//! Plain text (.txt) for thumbnails: the start of the file on a Letter page in the theme's colours (its popup
//! grey, its text), wrapped in Consolas. On screen a text file is the editor (app/textview.rs), not this.

use std::path::Path;

use windows::Win32::Graphics::Direct2D::Common::*;
use windows::Win32::Graphics::Direct2D::*;
use windows::Win32::Graphics::DirectWrite::*;
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
use windows::Win32::Graphics::Imaging::*;
use windows::core::{Interface, w};

/// Letter at 96 dpi, as `PdfPage.Size` reports it.
pub const PAGE_W: f32 = 816.0;
pub const PAGE_H: f32 = 1056.0;
const MARGIN: f32 = 72.0;
/// 10 pt.
const FONT_SIZE: f32 = 13.333;
/// Past this many characters the rest is left off (a log of hundreds of megabytes would take minutes to lay
/// out); the last page says so.
const MAX_CHARS: usize = 4 << 20;

pub struct TextDoc {
    text: Vec<u16>,
    /// Each page's characters, as a range of `text`.
    pages: Vec<(usize, usize)>,
}

fn err(e: windows::core::Error) -> String {
    e.message().to_string()
}

/// The file's text: UTF-16 by its byte order mark, else UTF-8 (BOM or not), else Windows-1252 when it isn't
/// valid UTF-8 (old Notepad's "ANSI").
fn decode_text(b: &[u8]) -> String {
    if let Some(rest) = b.strip_prefix(&[0xFF, 0xFE]) {
        return String::from_utf16_lossy(&rest.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect::<Vec<_>>());
    }
    if let Some(rest) = b.strip_prefix(&[0xFE, 0xFF]) {
        return String::from_utf16_lossy(&rest.chunks_exact(2).map(|c| u16::from_be_bytes([c[0], c[1]])).collect::<Vec<_>>());
    }
    let b = b.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(b);
    match std::str::from_utf8(b) {
        Ok(s) => s.to_string(),
        Err(_) => ansi(b),
    }
}

fn cp1252(c: u8) -> char {
    const HIGH: [u16; 32] = [
        0x20AC, 0x81, 0x201A, 0x0192, 0x201E, 0x2026, 0x2020, 0x2021, 0x02C6, 0x2030, 0x0160, 0x2039, 0x0152, 0x8D, 0x017D, 0x8F,
        0x90, 0x2018, 0x2019, 0x201C, 0x201D, 0x2022, 0x2013, 0x2014, 0x02DC, 0x2122, 0x0161, 0x203A, 0x0153, 0x9D, 0x017E, 0x0178,
    ];
    match c {
        0x80..=0x9F => char::from_u32(HIGH[(c - 0x80) as usize] as u32).unwrap_or('?'),
        _ => c as char,
    }
}

fn dwrite() -> Result<IDWriteFactory, String> {
    unsafe { DWriteCreateFactory::<IDWriteFactory>(DWRITE_FACTORY_TYPE_SHARED).map_err(err) }
}

fn format(dw: &IDWriteFactory) -> Result<IDWriteTextFormat, String> {
    unsafe {
        let f = dw
            .CreateTextFormat(w!("Consolas"), None, DWRITE_FONT_WEIGHT_NORMAL, DWRITE_FONT_STYLE_NORMAL, DWRITE_FONT_STRETCH_NORMAL, FONT_SIZE, w!("en-us"))
            .map_err(err)?;
        f.SetWordWrapping(DWRITE_WORD_WRAPPING_WRAP).map_err(err)?;
        // Tabs every 4 characters (Consolas is 0.55 em wide).
        f.SetIncrementalTabStop(FONT_SIZE * 0.5498 * 4.0).map_err(err)?;
        Ok(f)
    }
}

fn layout(dw: &IDWriteFactory, fmt: &IDWriteTextFormat, text: &[u16]) -> Result<IDWriteTextLayout, String> {
    unsafe { dw.CreateTextLayout(text, fmt, PAGE_W - 2.0 * MARGIN, PAGE_H).map_err(err) }
}

pub fn open(path: &Path) -> Result<TextDoc, String> {
    let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
    let mut s = decode_text(&bytes).replace("\r\n", "\n").replace('\r', "\n");
    if s.len() > MAX_CHARS {
        let mut cut = MAX_CHARS;
        while !s.is_char_boundary(cut) {
            cut -= 1;
        }
        s.truncate(cut);
        s.push_str("\n\n[\u{2026} the rest of the file isn't shown]");
    }
    let text: Vec<u16> = s.encode_utf16().collect();
    if text.is_empty() {
        return Ok(TextDoc { text, pages: vec![(0, 0)] });
    }
    let dw = dwrite()?;
    let fmt = format(&dw)?;
    let all = layout(&dw, &fmt, &text)?;
    let mut lines = Vec::new();
    unsafe {
        let mut n = 0u32;
        let _ = all.GetLineMetrics(None, &mut n);
        lines.resize(n as usize, DWRITE_LINE_METRICS::default());
        all.GetLineMetrics(Some(&mut lines), &mut n).map_err(err)?;
    }
    let room = PAGE_H - 2.0 * MARGIN;
    let mut pages = Vec::new();
    let (mut start, mut at, mut used) = (0usize, 0usize, 0f32);
    for l in &lines {
        if used + l.height > room && at > start {
            pages.push((start, at));
            start = at;
            used = 0.0;
        }
        used += l.height;
        at += l.length as usize;
    }
    pages.push((start, at.min(text.len())));
    Ok(TextDoc { text, pages })
}

/// The first page, fitted to the box: the still the thumbnails and the strip show.
pub fn decode(f: &IWICImagingFactory, path: &Path, box_w: u32, box_h: u32) -> Result<super::Decoded, String> {
    let doc = open(path)?;
    let (box_w, box_h) = if box_w == 0 || box_h == 0 { (1024, 1024) } else { (box_w, box_h) };
    let k = (box_w as f32 / PAGE_W).min(box_h as f32 / PAGE_H).min(2.0);
    let (w, h) = (((PAGE_W * k).round() as u32).max(1), ((PAGE_H * k).round() as u32).max(1));
    let px = render(f, &doc, 0, w, h)?;
    Ok(super::Decoded::still(w, h, px, w, h, None))
}

/// Bytes that aren't UTF-8 read as Windows-1252 (old Notepad's "ANSI").
pub fn ansi(b: &[u8]) -> String {
    b.iter().map(|&c| cp1252(c)).collect()
}

/// Text back to Windows-1252; None if some character has no place in it.
pub fn to_ansi(s: &str) -> Option<Vec<u8>> {
    s.chars()
        .map(|ch| {
            let u = ch as u32;
            if u < 0x80 || (0xA0..=0xFF).contains(&u) {
                return Some(u as u8);
            }
            (0x80u8..=0x9F).find(|&b| cp1252(b) == ch)
        })
        .collect()
}

/// Page `index` at exactly `w × h`, premultiplied BGRA.
pub fn render(f: &IWICImagingFactory, doc: &TextDoc, index: usize, w: u32, h: u32) -> Result<Vec<u8>, String> {
    let &(a, b) = doc.pages.get(index).ok_or("no such page")?;
    let dw = dwrite()?;
    let fmt = format(&dw)?;
    let tl = layout(&dw, &fmt, &doc.text[a..b])?;
    unsafe {
        let bmp = f.CreateBitmap(w, h, &GUID_WICPixelFormat32bppPBGRA, WICBitmapCacheOnLoad).map_err(err)?;
        let props = D2D1_RENDER_TARGET_PROPERTIES {
            r#type: D2D1_RENDER_TARGET_TYPE_SOFTWARE,
            pixelFormat: D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED },
            dpiX: 96.0,
            dpiY: 96.0,
            ..Default::default()
        };
        let rt = super::svg::factory()?.CreateWicBitmapRenderTarget(&bmp, &props).map_err(err)?;
        let dc: ID2D1DeviceContext = rt.cast().map_err(err)?;
        dc.SetTextAntialiasMode(D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE);
        let c = |v: u32| D2D1_COLOR_F { r: (v >> 16 & 0xFF) as f32 / 255.0, g: (v >> 8 & 0xFF) as f32 / 255.0, b: (v & 0xFF) as f32 / 255.0, a: 1.0 };
        let t = crate::theme::t();
        let brush = dc.CreateSolidColorBrush(&c(t.text), None).map_err(err)?;
        dc.BeginDraw();
        dc.Clear(Some(&c(crate::theme::raised().popup)));
        let (sx, sy) = (w as f32 / PAGE_W, h as f32 / PAGE_H);
        dc.SetTransform(&windows_numerics::Matrix3x2 { M11: sx, M12: 0.0, M21: 0.0, M22: sy, M31: 0.0, M32: 0.0 });
        dc.DrawTextLayout(windows_numerics::Vector2 { X: MARGIN, Y: MARGIN }, &tl, &brush, D2D1_DRAW_TEXT_OPTIONS_NONE);
        dc.EndDraw(None, None).map_err(err)?;
        let mut px = vec![0u8; (w * h * 4) as usize];
        bmp.CopyPixels(std::ptr::null(), w * 4, &mut px).map_err(err)?;
        Ok(px)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_utf8_utf16_and_ansi() {
        assert_eq!(decode_text(b"\xEF\xBB\xBFh\xC3\xA9"), "h\u{e9}");
        assert_eq!(decode_text(&[0xFF, 0xFE, b'h', 0, 0xE9, 0]), "h\u{e9}");
        assert_eq!(decode_text(b"caf\xE9 \x93q\x94"), "caf\u{e9} \u{201c}q\u{201d}");
    }

    #[test]
    fn long_text_splits_into_pages() {
        let path = std::env::temp_dir().join(format!("looker-text-{}.txt", std::process::id()));
        let body: String = (0..500).map(|i| format!("line {i}\r\n")).collect();
        std::fs::write(&path, body).unwrap();
        let doc = open(&path).unwrap();
        let _ = std::fs::remove_file(&path);
        // 912 DIPs of room at ~15.6 DIP lines: about 58 lines a page.
        assert!((7..=10).contains(&doc.pages.len()), "{} pages", doc.pages.len());
        assert_eq!(doc.pages.first().unwrap().0, 0);
        assert_eq!(doc.pages.last().unwrap().1, doc.text.len());
        assert!(doc.pages.windows(2).all(|p| p[0].1 == p[1].0));
    }
}
