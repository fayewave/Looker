//! PDF through the inbox `Windows.Data.Pdf` renderer: no codec pack, no Ghostscript. A page renders to an
//! in-memory BMP at exactly the target size, which WIC then reads. `PdfPage.Size` is in 96-dpi pixels
//! (Letter = 816 × 1056), which doubles as the native size for 100% zoom.
//!
//! For now this renders page 1 and reports the page count; the page strip and page bar arrive with the
//! viewer work (plan N3).

use std::path::Path;

use windows::Data::Pdf::{PdfDocument, PdfPageRenderOptions};
use windows::Graphics::Imaging::BitmapEncoder;
use windows::Storage::StorageFile;
use windows::Storage::Streams::InMemoryRandomAccessStream;
use windows::Win32::Graphics::Imaging::*;
use windows::Win32::System::Com::IStream;
use windows::Win32::System::WinRT::CreateStreamOverRandomAccessStream;
use windows::core::HSTRING;

use super::{Decoded, VECTOR_MAX_EDGE};

pub fn decode(f: &IWICImagingFactory, path: &Path, box_w: u32, box_h: u32) -> Result<Decoded, String> {
    let e = |e: windows::core::Error| e.message().to_string();
    let absolute = std::path::absolute(path).map_err(|e| e.to_string())?;
    let file = StorageFile::GetFileFromPathAsync(&HSTRING::from(absolute.as_os_str())).and_then(|op| op.join()).map_err(e)?;
    // Encrypted and broken files fail here and show as "Can't display".
    let doc = PdfDocument::LoadFromFileAsync(&file).and_then(|op| op.join()).map_err(e)?;
    let pages = doc.PageCount().map_err(e)?;
    let page = doc.GetPage(0).map_err(e)?;
    let size = page.Size().map_err(e)?;
    let (nw, nh) = (size.Width.round().max(1.0) as f64, size.Height.round().max(1.0) as f64);
    let (box_w, box_h) = if box_w == 0 || box_h == 0 { (VECTOR_MAX_EDGE, VECTOR_MAX_EDGE) } else { (box_w, box_h) };
    // A page is a vector: it renders sharp at any size, up to the per-bitmap limit.
    let scale = (box_w as f64 / nw).min(box_h as f64 / nh).min(VECTOR_MAX_EDGE as f64 / nw.max(nh));
    let w = ((nw * scale).round() as u32).max(1);
    let h = ((nh * scale).round() as u32).max(1);

    let opts = PdfPageRenderOptions::new().map_err(e)?;
    opts.SetDestinationWidth(w).map_err(e)?;
    opts.SetDestinationHeight(h).map_err(e)?;
    opts.SetBitmapEncoderId(BitmapEncoder::BmpEncoderId().map_err(e)?).map_err(e)?;
    let stream = InMemoryRandomAccessStream::new().map_err(e)?;
    page.RenderWithOptionsToStreamAsync(&stream, &opts).and_then(|op| op.join()).map_err(e)?;
    stream.Seek(0).map_err(e)?;
    unsafe {
        let istream: IStream = CreateStreamOverRandomAccessStream(&stream).map_err(e)?;
        let dec = f.CreateDecoderFromStream(&istream, std::ptr::null(), WICDecodeMetadataCacheOnDemand).map_err(e)?;
        let frame = dec.GetFrame(0).map_err(e)?;
        // The page is opaque (white paper). Going through BGR first drops whatever the BMP's fourth byte says:
        // 32-bit BMPs often carry alpha 0, which would draw the page transparent.
        let opaque = f.CreateFormatConverter().map_err(e)?;
        opaque.Initialize(&frame, &GUID_WICPixelFormat32bppBGR, WICBitmapDitherTypeNone, None, 0.0, WICBitmapPaletteTypeCustom).map_err(e)?;
        let pre = f.CreateFormatConverter().map_err(e)?;
        pre.Initialize(&opaque, &GUID_WICPixelFormat32bppPBGRA, WICBitmapDitherTypeNone, None, 0.0, WICBitmapPaletteTypeCustom).map_err(e)?;
        let (mut dw, mut dh) = (0u32, 0u32);
        pre.GetSize(&mut dw, &mut dh).map_err(e)?;
        let mut px = vec![0u8; (dw * dh * 4) as usize];
        pre.CopyPixels(std::ptr::null(), dw * 4, &mut px).map_err(e)?;
        let mut d = Decoded::still(dw, dh, px, nw as u32, nh as u32, None);
        d.vector = true;
        d.pages = pages;
        Ok(d)
    }
}
