//! Printing the text editor's document (File > Print, Ctrl+P) and File > Page setup, as Notepad does: Windows'
//! print and page setup dialogs, then GDI on the printer's device context. The text wraps at the page's width
//! in the editor's font (at its point size, not the screen's zoom); the file's name heads each page and its
//! number foots it.

use windows::Win32::Foundation::{HGLOBAL, HWND, RECT};
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::Storage::Xps::{DOCINFOW, EndDoc, EndPage, StartDocW, StartPage};
use windows::Win32::UI::Controls::Dialogs::*;
use windows::core::{HSTRING, PCWSTR};

use super::texttabs::FontSpec;

/// Margins in thousandths of an inch (left, top, right, bottom): Notepad's defaults.
#[cfg(test)]
pub(super) const DEFAULT_MARGINS: [i32; 4] = [750, 1000, 750, 1000];

/// File > Page setup: Windows' dialog, margins in thousandths of an inch. The printer and paper picked are kept
/// for Print (`devmode`).
pub(super) fn page_setup(hwnd: HWND, margins: &mut [i32; 4], devmode: &mut (HGLOBAL, HGLOBAL)) -> bool {
    unsafe {
        let mut p = PAGESETUPDLGW {
            lStructSize: std::mem::size_of::<PAGESETUPDLGW>() as u32,
            hwndOwner: hwnd,
            hDevMode: devmode.0,
            hDevNames: devmode.1,
            Flags: PSD_INTHOUSANDTHSOFINCHES | PSD_MARGINS,
            rtMargin: RECT { left: margins[0], top: margins[1], right: margins[2], bottom: margins[3] },
            ..Default::default()
        };
        if !PageSetupDlgW(&mut p).as_bool() {
            return false;
        }
        *margins = [p.rtMargin.left, p.rtMargin.top, p.rtMargin.right, p.rtMargin.bottom];
        *devmode = (p.hDevMode, p.hDevNames);
        true
    }
}

/// File > Print: Windows' print dialog, then the document onto the printer picked. Returns an error to show, or
/// None (printed, or cancelled).
pub(super) fn print(hwnd: HWND, title: &str, text: &str, font: &FontSpec, margins: [i32; 4], devmode: &mut (HGLOBAL, HGLOBAL)) -> Option<String> {
    unsafe {
        let mut range = PRINTPAGERANGE { nFromPage: 1, nToPage: 1 };
        let mut p = PRINTDLGEXW {
            lStructSize: std::mem::size_of::<PRINTDLGEXW>() as u32,
            hwndOwner: hwnd,
            hDevMode: devmode.0,
            hDevNames: devmode.1,
            Flags: PD_RETURNDC | PD_NOSELECTION | PD_NOCURRENTPAGE | PD_USEDEVMODECOPIESANDCOLLATE,
            nMaxPageRanges: 1,
            lpPageRanges: &mut range,
            nMinPage: 1,
            nMaxPage: 9999,
            nCopies: 1,
            nStartPage: START_PAGE_GENERAL,
            ..Default::default()
        };
        if let Err(e) = PrintDlgExW(&mut p) {
            return Some(e.message().to_string());
        }
        *devmode = (p.hDevMode, p.hDevNames);
        if p.dwResultAction != PD_RESULT_PRINT {
            if !p.hDC.is_invalid() {
                let _ = DeleteDC(p.hDC);
            }
            return None;
        }
        let pages = (p.Flags.0 & PD_PAGENUMS.0 != 0).then(|| (range.nFromPage, range.nToPage));
        let r = print_to(p.hDC, title, text, font, margins, pages, None);
        let _ = DeleteDC(p.hDC);
        r.err()
    }
}

/// Lays the text out on pages of the printer `dc` and prints them (only `pages`, first to last, when given).
/// `output`: a file to print into, for "Microsoft Print to PDF" without its dialog (the test).
pub(super) fn print_to(dc: HDC, title: &str, text: &str, font: &FontSpec, margins: [i32; 4], pages: Option<(u32, u32)>, output: Option<&str>) -> Result<u32, String> {
    unsafe {
        let dpi_x = GetDeviceCaps(Some(dc), LOGPIXELSX);
        let dpi_y = GetDeviceCaps(Some(dc), LOGPIXELSY);
        let (page_w, page_h) = (GetDeviceCaps(Some(dc), PHYSICALWIDTH), GetDeviceCaps(Some(dc), PHYSICALHEIGHT));
        let (off_x, off_y) = (GetDeviceCaps(Some(dc), PHYSICALOFFSETX), GetDeviceCaps(Some(dc), PHYSICALOFFSETY));
        if dpi_x == 0 || page_w == 0 {
            return Err("the printer gave no page size".into());
        }
        let to_x = |thou: i32| thou * dpi_x / 1000;
        let to_y = |thou: i32| thou * dpi_y / 1000;
        // The printable area, in the DC's coordinates (its origin is the printable corner, not the paper's).
        let area = RECT { left: to_x(margins[0]) - off_x, top: to_y(margins[1]) - off_y, right: page_w - to_x(margins[2]) - off_x, bottom: page_h - to_y(margins[3]) - off_y };
        let pt = font.px * 72.0 / 96.0;
        let face = HSTRING::from(font.family.as_str());
        let mut lf = LOGFONTW {
            lfHeight: -((pt * dpi_y as f32 / 72.0).round() as i32),
            lfWeight: font.weight,
            lfItalic: font.italic as u8,
            lfCharSet: DEFAULT_CHARSET,
            lfQuality: CLEARTYPE_QUALITY,
            ..Default::default()
        };
        for (d, s) in lf.lfFaceName.iter_mut().zip(face.as_ref().iter().take(31)) {
            *d = *s;
        }
        let hfont = CreateFontIndirectW(&lf);
        let small = CreateFontW(-((9.0 * dpi_y as f32 / 72.0).round() as i32), 0, 0, 0, 400, 0, 0, 0, DEFAULT_CHARSET, OUT_DEFAULT_PRECIS, CLIP_DEFAULT_PRECIS, CLEARTYPE_QUALITY, 0, &HSTRING::from("Segoe UI"));
        let old = SelectObject(dc, hfont.into());
        // The header and footer get a line each, a quarter inch off the text.
        let band = (dpi_y as f32 * 0.25) as i32;
        let mut tm = TEXTMETRICW::default();
        let _ = GetTextMetricsW(dc, &mut tm);
        let line_h = tm.tmHeight + tm.tmExternalLeading;
        // Tabs are expanded here (every 4 columns, as on screen): DT_TABSTOP can't go with DT_NOPREFIX or DT_CALCRECT.
        let fmt = DT_WORDBREAK | DT_NOPREFIX | DT_EDITCONTROL;

        // Pages: each line of the file wraps to as many rows as it needs; a page holds whole lines (a line
        // taller than a page is cut at the page).
        let mut layout: Vec<Vec<&str>> = vec![Vec::new()];
        let mut used = 0;
        let height = area.bottom - area.top;
        for line in text.split('\n') {
            let mut w = expand_tabs(line);
            let mut r = RECT { left: 0, top: 0, right: area.right - area.left, bottom: 0 };
            DrawTextW(dc, &mut w, &mut r, fmt | DT_CALCRECT);
            let h = (r.bottom - r.top).max(line_h);
            if used + h > height && used > 0 {
                layout.push(Vec::new());
                used = 0;
            }
            layout.last_mut().unwrap().push(line);
            used += h;
        }
        let count = layout.len() as u32;
        let docname = HSTRING::from(title);
        let out = output.map(HSTRING::from);
        let di = DOCINFOW {
            cbSize: std::mem::size_of::<DOCINFOW>() as i32,
            lpszDocName: PCWSTR(docname.as_ptr()),
            lpszOutput: out.as_ref().map_or(PCWSTR::null(), |o| PCWSTR(o.as_ptr())),
            ..Default::default()
        };
        if StartDocW(dc, &di) <= 0 {
            SelectObject(dc, old);
            let _ = DeleteObject(hfont.into());
            let _ = DeleteObject(small.into());
            return Err("the printer didn't start the document".into());
        }
        let (first, last) = pages.unwrap_or((1, count));
        let mut printed = 0;
        for (k, lines) in layout.iter().enumerate() {
            let n = k as u32 + 1;
            if n < first || n > last {
                continue;
            }
            StartPage(dc);
            SetBkMode(dc, TRANSPARENT);
            SetTextColor(dc, windows::Win32::Foundation::COLORREF(0));
            SelectObject(dc, small.into());
            let mut head: Vec<u16> = title.encode_utf16().collect();
            let mut hr = RECT { left: area.left, top: area.top - band - line_h, right: area.right, bottom: area.top - band };
            DrawTextW(dc, &mut head, &mut hr, DT_CENTER | DT_SINGLELINE | DT_NOPREFIX | DT_END_ELLIPSIS);
            let mut foot: Vec<u16> = format!("Page {n}").encode_utf16().collect();
            let mut fr = RECT { left: area.left, top: area.bottom + band, right: area.right, bottom: area.bottom + band + line_h };
            DrawTextW(dc, &mut foot, &mut fr, DT_CENTER | DT_SINGLELINE | DT_NOPREFIX);
            SelectObject(dc, hfont.into());
            let mut y = area.top;
            for line in lines {
                let mut w = expand_tabs(line);
                let mut r = RECT { left: area.left, top: y, right: area.right, bottom: area.bottom };
                let h = DrawTextW(dc, &mut w, &mut r, fmt);
                y += h.max(line_h);
            }
            EndPage(dc);
            printed += 1;
        }
        EndDoc(dc);
        SelectObject(dc, old);
        let _ = DeleteObject(hfont.into());
        let _ = DeleteObject(small.into());
        Ok(printed)
    }
}

/// A line with its tabs as spaces to the next multiple of 4 columns (never empty: an empty line still takes a row).
fn expand_tabs(line: &str) -> Vec<u16> {
    let mut out = Vec::with_capacity(line.len());
    let mut col = 0;
    for u in line.encode_utf16() {
        if u == 9 {
            let n = 4 - col % 4;
            out.extend(std::iter::repeat_n(b' ' as u16, n));
            col += n;
        } else {
            out.push(u);
            col += 1;
        }
    }
    if out.is_empty() {
        out.push(b' ' as u16);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Prints a long text through "Microsoft Print to PDF" into `%TEMP%\looker-print-test.pdf`, no dialog.
    #[test]
    #[ignore]
    fn prints_to_pdf() {
        unsafe {
            let dc = CreateDCW(&HSTRING::from("WINSPOOL"), &HSTRING::from("Microsoft Print to PDF"), PCWSTR::null(), None);
            assert!(!dc.is_invalid(), "no Microsoft Print to PDF");
            let text: String = (1..=150).map(|i| format!("Line {i}\tThe quick brown fox jumps over the lazy dog, again and again and again until it wraps.\n")).collect();
            let out = std::env::temp_dir().join("looker-print-test.pdf");
            let _ = std::fs::remove_file(&out);
            let font = FontSpec { family: "Consolas".into(), weight: 400, italic: false, px: 14.0 };
            let n = print_to(dc, "notes.txt", &text, &font, DEFAULT_MARGINS, None, Some(&out.to_string_lossy())).unwrap();
            let _ = DeleteDC(dc);
            println!("{n} pages to {}", out.display());
            assert!(n >= 4);
            // The PDF is written by the spooler a moment later.
            for _ in 0..50 {
                if out.metadata().is_ok_and(|m| m.len() > 1000) {
                    return;
                }
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
            panic!("no PDF written");
        }
    }
}
