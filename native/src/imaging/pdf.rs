//! PDF through the inbox `Windows.Data.Pdf` renderer: no codec pack, no Ghostscript. A page renders to an
//! in-memory BMP at exactly the target size, which WIC then reads. `PdfPage.Size` is in 96-dpi pixels
//! (Letter = 816 × 1056), which doubles as the native size for 100% zoom.
//!
//! A document is shown as one strip of pages (`crate::pages`). The decode reads every page's size and renders
//! page 1 only; the others render on demand on the page thread through [`render`]. Every page of an entry
//! renders at one scale, so a bucket means the longest page edge, the way it means an image's long edge.

use std::path::Path;

use windows::Data::Pdf::{PdfDocument, PdfPageRenderOptions};
use windows::Graphics::Imaging::BitmapEncoder;
use windows::Storage::Streams::{DataWriter, InMemoryRandomAccessStream};
use windows::Win32::Graphics::Imaging::*;
use windows::Win32::System::Com::IStream;
use windows::Win32::System::WinRT::CreateStreamOverRandomAccessStream;

use super::{Decoded, VECTOR_MAX_EDGE};

fn err(e: windows::core::Error) -> String {
    e.message().to_string()
}

/// Opens a document from a copy of the file in memory: the renderer reads pages lazily for as long as the
/// document lives, and a document over the file itself would keep it open (no rename or delete meanwhile).
/// Encrypted and broken files fail here and show as "Can't display".
pub fn open(path: &Path) -> Result<PdfDocument, String> {
    let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
    let stream = InMemoryRandomAccessStream::new().map_err(err)?;
    let w = DataWriter::CreateDataWriter(&stream).map_err(err)?;
    w.WriteBytes(&bytes).map_err(err)?;
    w.StoreAsync().and_then(|op| op.join()).map_err(err)?;
    w.DetachStream().map_err(err)?;
    stream.Seek(0).map_err(err)?;
    PdfDocument::LoadFromStreamAsync(&stream).and_then(|op| op.join()).map_err(err)
}

/// Every page's size in 96-dpi pixels; a page without a usable size (corrupt MediaBox) still gets a slot.
pub fn page_sizes(doc: &PdfDocument) -> Result<Vec<(f32, f32)>, String> {
    let n = doc.PageCount().map_err(err)?;
    let sane = |v: f32| if v.is_finite() && v >= 1.0 { v.round() } else { 1.0 };
    (0..n)
        .map(|i| {
            let s = doc.GetPage(i).and_then(|p| p.Size()).map_err(err)?;
            Ok((sane(s.Width), sane(s.Height)))
        })
        .collect()
}

/// Rasterizes page `index` at exactly `w × h` into premultiplied BGRA.
pub fn render(f: &IWICImagingFactory, doc: &PdfDocument, index: u32, w: u32, h: u32) -> Result<Vec<u8>, String> {
    let page = doc.GetPage(index).map_err(err)?;
    let opts = PdfPageRenderOptions::new().map_err(err)?;
    opts.SetDestinationWidth(w).map_err(err)?;
    opts.SetDestinationHeight(h).map_err(err)?;
    opts.SetBitmapEncoderId(BitmapEncoder::BmpEncoderId().map_err(err)?).map_err(err)?;
    let stream = InMemoryRandomAccessStream::new().map_err(err)?;
    page.RenderWithOptionsToStreamAsync(&stream, &opts).and_then(|op| op.join()).map_err(err)?;
    stream.Seek(0).map_err(err)?;
    unsafe {
        let istream: IStream = CreateStreamOverRandomAccessStream(&stream).map_err(err)?;
        let dec = f.CreateDecoderFromStream(&istream, std::ptr::null(), WICDecodeMetadataCacheOnDemand).map_err(err)?;
        let frame = dec.GetFrame(0).map_err(err)?;
        // The page is opaque (white paper). Going through BGR first drops whatever the BMP's fourth byte says:
        // 32-bit BMPs often carry alpha 0, which would draw the page transparent.
        let opaque = f.CreateFormatConverter().map_err(err)?;
        opaque.Initialize(&frame, &GUID_WICPixelFormat32bppBGR, WICBitmapDitherTypeNone, None, 0.0, WICBitmapPaletteTypeCustom).map_err(err)?;
        let pre = f.CreateFormatConverter().map_err(err)?;
        pre.Initialize(&opaque, &GUID_WICPixelFormat32bppPBGRA, WICBitmapDitherTypeNone, None, 0.0, WICBitmapPaletteTypeCustom).map_err(err)?;
        let (mut dw, mut dh) = (0u32, 0u32);
        pre.GetSize(&mut dw, &mut dh).map_err(err)?;
        if (dw, dh) != (w, h) {
            return Err(format!("page rendered at {dw}x{dh}, asked for {w}x{h}"));
        }
        let mut px = vec![0u8; (dw * dh * 4) as usize];
        pre.CopyPixels(std::ptr::null(), dw * 4, &mut px).map_err(err)?;
        Ok(px)
    }
}

/// Render pixels per page pixel for a decode box: the longest page fits the box, up to the per-bitmap limit
/// (`(0, 0)` = as sharp as a page may get). A page is a vector, so this may exceed 1.
pub fn scale_for(sizes: &[(f32, f32)], box_w: u32, box_h: u32) -> f64 {
    let (mw, mh) = sizes.iter().fold((1f64, 1f64), |(w, h), &(pw, ph)| (w.max(pw as f64), h.max(ph as f64)));
    let (box_w, box_h) = if box_w == 0 || box_h == 0 { (VECTOR_MAX_EDGE, VECTOR_MAX_EDGE) } else { (box_w, box_h) };
    (box_w as f64 / mw).min(box_h as f64 / mh).min(VECTOR_MAX_EDGE as f64 / mw.max(mh))
}

/// A page's bitmap size at a render scale.
pub fn page_pixels(size: (f32, f32), scale: f64) -> (u32, u32) {
    let px = |v: f32| ((v as f64 * scale).round() as u32).clamp(1, VECTOR_MAX_EDGE);
    (px(size.0), px(size.1))
}

pub fn decode(f: &IWICImagingFactory, path: &Path, box_w: u32, box_h: u32) -> Result<Decoded, String> {
    let doc = open(path)?;
    let sizes = page_sizes(&doc)?;
    let Some(&first) = sizes.first() else { return Err("the document has no pages".into()) };
    let scale = scale_for(&sizes, box_w, box_h);
    let (w, h) = page_pixels(first, scale);
    let px = render(f, &doc, 0, w, h)?;
    let mut d = Decoded::still(w, h, px, first.0 as u32, first.1 as u32, None);
    d.vector = true;
    d.pages = sizes.len() as u32;
    d.page_sizes = sizes;
    d.page_scale = scale;
    Ok(d)
}

/// One page as large as a page renders (the clipboard and the wallpaper take the page on the bar).
pub fn render_page_of(f: &IWICImagingFactory, path: &Path, index: usize) -> Result<(u32, u32, Vec<u8>), String> {
    let doc = open(path)?;
    let sizes = page_sizes(&doc)?;
    let size = *sizes.get(index).ok_or("no such page")?;
    let (w, h) = page_pixels(size, VECTOR_MAX_EDGE as f64 / size.0.max(size.1) as f64);
    Ok((w, h, render(f, &doc, index as u32, w, h)?))
}
