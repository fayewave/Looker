//! GPU drawing: a D3D11 device, a flip-model swap chain hosted in DirectComposition, a Direct2D device
//! context and DirectWrite with Inter loaded from memory.
//!
//! Two devices, on purpose. Creating the hardware D3D11 device loads the GPU vendor's user-mode driver
//! (NVIDIA: ~120 ms on every launch, holding the loader lock the whole time). So the first frames are drawn
//! on a WARP (CPU) device, which is ready in a few ms, and the hardware device is built once the window is up
//! and swapped in under the same DirectComposition visual. Text formats are device-independent and survive
//! the switch; bitmaps and brushes are re-created.

use windows::Win32::Foundation::{HMODULE, HWND};
use windows::Win32::Graphics::Direct2D::Common::*;
use windows::Win32::Graphics::Direct2D::*;
use windows::Win32::Graphics::Direct3D::*;
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::DirectComposition::*;
use windows::Win32::Graphics::DirectWrite::*;
use windows::Win32::Graphics::Dxgi::Common::*;
use windows::Win32::Graphics::Dxgi::*;
use windows::core::{Interface, PCWSTR, Result, w};

static INTER: &[u8] = include_bytes!("../assets/Fonts/InterVariable.ttf");

pub struct Device {
    d3d: ID3D11Device,
    pub dc: ID2D1DeviceContext,
    pub warp: bool,
}

/// COM pointers made on an init thread, handed to the UI thread (used by one thread at a time).
pub struct Sendable<T>(pub T);
unsafe impl<T> Send for Sendable<T> {}

pub fn create_device(warp: bool) -> Result<Device> {
    unsafe {
        let mut d3d = None;
        D3D11CreateDevice(
            None,
            if warp { D3D_DRIVER_TYPE_WARP } else { D3D_DRIVER_TYPE_HARDWARE },
            HMODULE::default(),
            D3D11_CREATE_DEVICE_BGRA_SUPPORT,
            Some(&[D3D_FEATURE_LEVEL_11_1, D3D_FEATURE_LEVEL_11_0, D3D_FEATURE_LEVEL_10_1, D3D_FEATURE_LEVEL_10_0]),
            D3D11_SDK_VERSION,
            Some(&mut d3d),
            None,
            None,
        )?;
        let d3d: ID3D11Device = d3d.unwrap();
        crate::trace::mark(if warp { "gfx: D3D11 device (WARP)" } else { "gfx: D3D11 device (hardware)" });
        let dxgi: IDXGIDevice1 = d3d.cast()?;
        let _ = dxgi.SetMaximumFrameLatency(1);
        let factory: ID2D1Factory1 = D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None)?;
        let dev = factory.CreateDevice(&dxgi)?;
        let dc = dev.CreateDeviceContext(D2D1_DEVICE_CONTEXT_OPTIONS_NONE)?;
        dc.SetTextAntialiasMode(D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE);
        crate::trace::mark("gfx: D2D device context");
        Ok(Device { d3d, dc, warp })
    }
}

/// DirectWrite with Inter registered from memory: device-independent.
pub struct Text {
    pub dwrite: IDWriteFactory5,
    inter: IDWriteFontCollection1,
    /// NUL-terminated family name as DirectWrite reports it.
    inter_family: Vec<u16>,
}

pub fn init_text() -> Result<Text> {
    unsafe {
        let dwrite: IDWriteFactory5 = DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)?;
        let loader = dwrite.CreateInMemoryFontFileLoader()?;
        dwrite.RegisterFontFileLoader(&loader)?;
        let file = loader.CreateInMemoryFontFileReference(&dwrite, INTER.as_ptr() as _, INTER.len() as u32, None)?;
        let builder = dwrite.CreateFontSetBuilder()?;
        builder.AddFontFile(&file)?;
        let set = builder.CreateFontSet()?;
        let inter = dwrite.CreateFontCollectionFromFontSet(&set)?;
        let family = inter.GetFontFamily(0)?;
        let names = family.GetFamilyNames()?;
        let len = names.GetStringLength(0)? as usize;
        let mut inter_family = vec![0u16; len + 1];
        names.GetString(0, &mut inter_family)?;
        crate::trace::mark("gfx: Inter loaded");
        Ok(Text { dwrite, inter, inter_family })
    }
}

pub struct Fonts {
    /// 12 px: title bar caption and status row.
    pub caption: IDWriteTextFormat,
    pub body: IDWriteTextFormat,
    pub body_strong: IDWriteTextFormat,
    /// 14 px, wrapping, top-aligned, 20 px lines (WinUI's BodyTextBlockStyle): dialog text.
    pub body_wrap: IDWriteTextFormat,
    /// 20 px semibold: dialog titles.
    pub title: IDWriteTextFormat,
    /// 12 px, wrapping, top-aligned, 16 px lines (CaptionTextBlockStyle): info card rows.
    pub caption_wrap: IDWriteTextFormat,
    /// 11 px semibold: info card group titles.
    pub overline: IDWriteTextFormat,
    /// 28 px semibold (TitleTextBlockStyle): a page's title.
    pub page_title: IDWriteTextFormat,
    /// Segoe Fluent Icons, 16 px (toolbar) and 10 px (caption buttons).
    pub icons: IDWriteTextFormat,
    pub caption_icons: IDWriteTextFormat,
    /// 12 px icons: a text box's clear button.
    pub small_icons: IDWriteTextFormat,
    /// 14 px icons: glyphs on text buttons.
    pub body_icons: IDWriteTextFormat,
    /// 96 px icons: the file glyph standing in for a file Looker can't show.
    pub hero_icons: IDWriteTextFormat,
    /// 16 px, wrapping, centred, top-aligned: that file's name.
    pub subtitle_wrap: IDWriteTextFormat,
    /// Inter's tabular figures (`tnum`): every digit the same width, so the status row's numbers don't
    /// shuffle as they change (`Gfx::text_tabular`).
    pub tabular: IDWriteTypography,
    _ellipsis: Vec<IDWriteInlineObject>,
}

pub struct Gfx {
    pub dev: Device,
    pub text: Text,
    swap: IDXGISwapChain1,
    comp: IDCompositionDevice,
    _target: IDCompositionTarget,
    visual: IDCompositionVisual,
    size: (u32, u32),
    target: Option<ID2D1Bitmap1>,
    pub brush: ID2D1SolidColorBrush,
    pub fonts: Fonts,
    pub dpi: f32,
    checker: Option<(ID2D1BitmapBrush1, f32)>,
}

#[derive(Clone, Copy, PartialEq)]
pub enum Align {
    Left,
    Right,
    Center,
}

/// The checkerboard's darker square: also the plain viewport fill when the checkerboard is off.
pub const CHECKER_DARK: u32 = 0x0F0F0F;

pub fn rgb(hex: u32) -> D2D1_COLOR_F {
    rgba(hex, 1.0)
}

pub fn rgba(hex: u32, a: f32) -> D2D1_COLOR_F {
    D2D1_COLOR_F {
        r: ((hex >> 16) & 0xFF) as f32 / 255.0,
        g: ((hex >> 8) & 0xFF) as f32 / 255.0,
        b: (hex & 0xFF) as f32 / 255.0,
        a,
    }
}

/// White at an alpha byte, as the WinUI theme resources are written (`#24FFFFFF` = `white(0x24)`).
pub fn white(alpha: u8) -> D2D1_COLOR_F {
    rgba(0xFFFFFF, alpha as f32 / 255.0)
}

pub fn rect(x: f32, y: f32, w: f32, h: f32) -> D2D_RECT_F {
    D2D_RECT_F { left: x, top: y, right: x + w, bottom: y + h }
}

impl Gfx {
    pub fn attach(dev: Device, text: Text, hwnd: HWND, width: u32, height: u32, dpi: f32) -> Result<Gfx> {
        unsafe {
            let swap = swap_chain(&dev, width, height)?;
            let dxgi: IDXGIDevice = dev.d3d.cast()?;
            let comp: IDCompositionDevice = DCompositionCreateDevice(&dxgi)?;
            let target = comp.CreateTargetForHwnd(hwnd, true)?;
            let visual = comp.CreateVisual()?;
            visual.SetContent(&swap)?;
            target.SetRoot(&visual)?;
            comp.Commit()?;
            crate::trace::mark("gfx: swap chain + DirectComposition");
            let brush = dev.dc.CreateSolidColorBrush(&rgb(0xFFFFFF), None)?;
            let fonts = make_fonts(&text)?;
            let mut g = Gfx {
                dev,
                text,
                swap,
                comp,
                _target: target,
                visual,
                size: (width, height),
                target: None,
                brush,
                fonts,
                dpi,
                checker: None,
            };
            g.make_target()?;
            Ok(g)
        }
    }

    /// Moves drawing to another device (WARP to hardware). Every bitmap made on the old device is invalid
    /// afterwards; the caller re-uploads its own. Call `end()` after drawing so the new swap chain has a frame
    /// before the visual shows it.
    pub fn switch_device(&mut self, dev: Device) -> Result<()> {
        unsafe {
            let swap = swap_chain(&dev, self.size.0, self.size.1)?;
            let brush = dev.dc.CreateSolidColorBrush(&rgb(0xFFFFFF), None)?;
            self.dev.dc.SetTarget(None);
            self.target = None;
            self.checker = None;
            self.swap = swap;
            self.brush = brush;
            self.dev = dev;
            self.make_target()
        }
    }

    /// Points the visual at the current swap chain (after `switch_device` and a first frame on it).
    pub fn commit_swap(&self) -> Result<()> {
        unsafe {
            self.visual.SetContent(&self.swap)?;
            self.comp.Commit()
        }
    }

    pub fn is_warp(&self) -> bool {
        self.dev.warp
    }

    fn make_target(&mut self) -> Result<()> {
        unsafe {
            let surface: IDXGISurface = self.swap.GetBuffer(0)?;
            let props = D2D1_BITMAP_PROPERTIES1 {
                pixelFormat: D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_IGNORE },
                dpiX: self.dpi,
                dpiY: self.dpi,
                bitmapOptions: D2D1_BITMAP_OPTIONS_TARGET | D2D1_BITMAP_OPTIONS_CANNOT_DRAW,
                colorContext: std::mem::ManuallyDrop::new(None),
            };
            let bmp = self.dev.dc.CreateBitmapFromDxgiSurface(&surface, Some(&props))?;
            self.dev.dc.SetTarget(&bmp);
            self.dev.dc.SetDpi(self.dpi, self.dpi);
            self.target = Some(bmp);
            Ok(())
        }
    }

    pub fn resize(&mut self, width: u32, height: u32, dpi: f32) -> Result<()> {
        self.size = (width, height);
        unsafe {
            self.dev.dc.SetTarget(None);
            self.target = None;
            if dpi != self.dpi {
                self.checker = None;
            }
            self.dpi = dpi;
            self.swap.ResizeBuffers(0, width.max(1), height.max(1), DXGI_FORMAT_UNKNOWN, DXGI_SWAP_CHAIN_FLAG(0))?;
            self.make_target()
        }
    }

    pub fn begin(&self, bg: u32) {
        unsafe {
            self.dev.dc.BeginDraw();
            self.dev.dc.Clear(Some(&rgb(bg)));
        }
    }

    pub fn end(&self) -> Result<()> {
        unsafe {
            self.dev.dc.EndDraw(None, None)?;
            self.swap.Present(1, DXGI_PRESENT(0)).ok()
        }
    }

    pub fn max_bitmap_size(&self) -> u32 {
        unsafe { self.dev.dc.GetMaximumBitmapSize() }
    }

    /// Premultiplied BGRA pixels → GPU bitmap (96 dpi; always drawn into an explicit rect).
    pub fn bitmap(&self, width: u32, height: u32, pixels: &[u8]) -> Result<ID2D1Bitmap1> {
        unsafe {
            let props = D2D1_BITMAP_PROPERTIES1 {
                pixelFormat: D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED },
                dpiX: 96.0,
                dpiY: 96.0,
                bitmapOptions: D2D1_BITMAP_OPTIONS_NONE,
                colorContext: std::mem::ManuallyDrop::new(None),
            };
            self.dev.dc.CreateBitmap(
                D2D_SIZE_U { width, height },
                Some(pixels.as_ptr() as _),
                width * 4,
                &props,
            )
        }
    }

    pub fn draw_bitmap(&self, bmp: &ID2D1Bitmap1, dest: D2D_RECT_F, opacity: f32) {
        unsafe {
            self.dev.dc.DrawBitmap(bmp, Some(&dest), opacity, D2D1_INTERPOLATION_MODE_HIGH_QUALITY_CUBIC, None, None);
        }
    }

    /// The preview-area checkerboard: two near-blacks a hair apart in 8-DIP squares, rendered at the
    /// display's DPI so the squares stay crisp and whole-pixel, anchored at the viewport's top-left.
    pub fn checkerboard(&mut self, r: D2D_RECT_F) {
        const DARK: u32 = CHECKER_DARK;
        const LIGHT: u32 = 0x161616;
        let scale = self.dpi / 96.0;
        if self.checker.as_ref().is_none_or(|(_, d)| *d != self.dpi) {
            let cell = (8.0 * scale).round().max(1.0) as u32;
            let n = cell * 2;
            let mut px = vec![0u8; (n * n * 4) as usize];
            for y in 0..n {
                for x in 0..n {
                    let c = if (x < cell) == (y < cell) { LIGHT } else { DARK };
                    let i = ((y * n + x) * 4) as usize;
                    px[i] = (c & 0xFF) as u8;
                    px[i + 1] = ((c >> 8) & 0xFF) as u8;
                    px[i + 2] = ((c >> 16) & 0xFF) as u8;
                    px[i + 3] = 0xFF;
                }
            }
            unsafe {
                let props = D2D1_BITMAP_PROPERTIES1 {
                    pixelFormat: D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED },
                    dpiX: self.dpi,
                    dpiY: self.dpi,
                    bitmapOptions: D2D1_BITMAP_OPTIONS_NONE,
                    colorContext: std::mem::ManuallyDrop::new(None),
                };
                let Ok(tile) = self.dev.dc.CreateBitmap(D2D_SIZE_U { width: n, height: n }, Some(px.as_ptr() as _), n * 4, &props) else {
                    return;
                };
                let bp = D2D1_BITMAP_BRUSH_PROPERTIES1 {
                    extendModeX: D2D1_EXTEND_MODE_WRAP,
                    extendModeY: D2D1_EXTEND_MODE_WRAP,
                    interpolationMode: D2D1_INTERPOLATION_MODE_NEAREST_NEIGHBOR,
                };
                let Ok(brush) = self.dev.dc.CreateBitmapBrush(&tile, Some(&bp), None) else { return };
                self.checker = Some((brush, self.dpi));
            }
        }
        if let Some((brush, _)) = &self.checker {
            unsafe {
                brush.SetTransform(&windows_numerics::Matrix3x2::translation(r.left, r.top));
                self.dev.dc.FillRectangle(&r, brush);
            }
        }
    }

    pub fn fill(&self, r: D2D_RECT_F, color: D2D1_COLOR_F) {
        unsafe {
            self.brush.SetColor(&color);
            self.dev.dc.FillRectangle(&r, &self.brush);
        }
    }

    pub fn fill_round(&self, r: D2D_RECT_F, radius: f32, color: D2D1_COLOR_F) {
        unsafe {
            self.brush.SetColor(&color);
            self.dev.dc.FillRoundedRectangle(&D2D1_ROUNDED_RECT { rect: r, radiusX: radius, radiusY: radius }, &self.brush);
        }
    }

    pub fn outline_round(&self, r: D2D_RECT_F, radius: f32, color: D2D1_COLOR_F, width: f32) {
        unsafe {
            self.brush.SetColor(&color);
            let h = width / 2.0;
            let r = D2D_RECT_F { left: r.left + h, top: r.top + h, right: r.right - h, bottom: r.bottom - h };
            self.dev.dc.DrawRoundedRectangle(&D2D1_ROUNDED_RECT { rect: r, radiusX: radius, radiusY: radius }, &self.brush, width, None);
        }
    }

    /// One device pixel, in DIPs: for hairlines that stay crisp at any scale.
    pub fn px(&self) -> f32 {
        96.0 / self.dpi
    }

    /// A one-device-pixel horizontal hairline.
    pub fn hline(&self, x: f32, y: f32, w: f32, color: D2D1_COLOR_F) {
        self.fill(rect(x, y, w, self.px()), color);
    }

    /// Rounds a DIP coordinate to the nearest device pixel.
    pub fn snap(&self, v: f32) -> f32 {
        let s = self.dpi / 96.0;
        (v * s).round() / s
    }

    pub fn text(&self, s: &[u16], fmt: &IDWriteTextFormat, r: D2D_RECT_F, color: D2D1_COLOR_F, align: Align) {
        unsafe {
            let _ = fmt.SetTextAlignment(match align {
                Align::Left => DWRITE_TEXT_ALIGNMENT_LEADING,
                Align::Right => DWRITE_TEXT_ALIGNMENT_TRAILING,
                Align::Center => DWRITE_TEXT_ALIGNMENT_CENTER,
            });
            self.brush.SetColor(&color);
            self.dev.dc.DrawText(s, fmt, &r, &self.brush, D2D1_DRAW_TEXT_OPTIONS_CLIP | D2D1_DRAW_TEXT_OPTIONS_ENABLE_COLOR_FONT, DWRITE_MEASURING_MODE_NATURAL);
        }
    }

    /// A layout of `s` in `fmt` with tabular figures, `w` x `h`.
    fn tabular_layout(&self, s: &[u16], fmt: &IDWriteTextFormat, w: f32, h: f32) -> Option<IDWriteTextLayout> {
        unsafe {
            let l = self.text.dwrite.CreateTextLayout(s, fmt, w.max(1.0), h.max(1.0)).ok()?;
            l.SetTypography(&self.fonts.tabular, DWRITE_TEXT_RANGE { startPosition: 0, length: s.len() as u32 }).ok()?;
            Some(l)
        }
    }

    /// `text` with tabular figures (the status row).
    pub fn text_tabular(&self, s: &[u16], fmt: &IDWriteTextFormat, r: D2D_RECT_F, color: D2D1_COLOR_F, align: Align) {
        let Some(l) = self.tabular_layout(s, fmt, r.right - r.left, r.bottom - r.top) else { return };
        unsafe {
            let _ = l.SetTextAlignment(match align {
                Align::Left => DWRITE_TEXT_ALIGNMENT_LEADING,
                Align::Right => DWRITE_TEXT_ALIGNMENT_TRAILING,
                Align::Center => DWRITE_TEXT_ALIGNMENT_CENTER,
            });
            self.brush.SetColor(&color);
            self.dev.dc.DrawTextLayout(
                windows_numerics::Vector2 { X: r.left, Y: r.top },
                &l,
                &self.brush,
                D2D1_DRAW_TEXT_OPTIONS_CLIP | D2D1_DRAW_TEXT_OPTIONS_ENABLE_COLOR_FONT,
            );
        }
    }

    /// `measure` with tabular figures.
    pub fn measure_tabular(&self, s: &[u16], fmt: &IDWriteTextFormat) -> f32 {
        let Some(l) = self.tabular_layout(s, fmt, 10_000.0, 100.0) else { return 0.0 };
        let mut m = DWRITE_TEXT_METRICS::default();
        unsafe { if l.GetMetrics(&mut m).is_ok() { m.widthIncludingTrailingWhitespace } else { 0.0 } }
    }

    /// `measure_height` with tabular figures.
    pub fn measure_height_tabular(&self, s: &[u16], fmt: &IDWriteTextFormat, width: f32) -> f32 {
        let Some(l) = self.tabular_layout(s, fmt, width, 10_000.0) else { return 0.0 };
        let mut m = DWRITE_TEXT_METRICS::default();
        unsafe { if l.GetMetrics(&mut m).is_ok() { m.height } else { 0.0 } }
    }

    /// Width of a single line of text, in DIPs.
    pub fn measure(&self, s: &[u16], fmt: &IDWriteTextFormat) -> f32 {
        unsafe {
            let Ok(layout) = self.text.dwrite.CreateTextLayout(s, fmt, 10_000.0, 100.0) else { return 0.0 };
            let mut m = DWRITE_TEXT_METRICS::default();
            if layout.GetMetrics(&mut m).is_ok() { m.widthIncludingTrailingWhitespace } else { 0.0 }
        }
    }

    /// Height of text wrapped at `width` (with a wrapping format), in DIPs.
    pub fn measure_height(&self, s: &[u16], fmt: &IDWriteTextFormat, width: f32) -> f32 {
        unsafe {
            let Ok(layout) = self.text.dwrite.CreateTextLayout(s, fmt, width.max(1.0), 10_000.0) else { return 0.0 };
            let mut m = DWRITE_TEXT_METRICS::default();
            if layout.GetMetrics(&mut m).is_ok() { m.height } else { 0.0 }
        }
    }

    /// A single-line, left-aligned layout of `s`, `height` tall (text vertically centred): for text boxes,
    /// which need caret positions and hit-testing.
    pub fn layout(&self, s: &[u16], fmt: &IDWriteTextFormat, height: f32) -> Option<IDWriteTextLayout> {
        unsafe {
            let l = self.text.dwrite.CreateTextLayout(s, fmt, 100_000.0, height).ok()?;
            l.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_LEADING).ok()?;
            l.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER).ok()?;
            Some(l)
        }
    }

    pub fn draw_layout(&self, l: &IDWriteTextLayout, x: f32, y: f32, color: D2D1_COLOR_F) {
        unsafe {
            self.brush.SetColor(&color);
            self.dev.dc.DrawTextLayout(windows_numerics::Vector2 { X: x, Y: y }, l, &self.brush, D2D1_DRAW_TEXT_OPTIONS_ENABLE_COLOR_FONT);
        }
    }

    /// Where the caret sits before UTF-16 position `pos`: (x, line top, line height), layout-relative.
    pub fn caret_at(&self, l: &IDWriteTextLayout, pos: usize) -> (f32, f32, f32) {
        unsafe {
            let (mut x, mut y) = (0.0f32, 0.0f32);
            let mut m = DWRITE_HIT_TEST_METRICS::default();
            if l.HitTestTextPosition(pos as u32, false, &mut x, &mut y, &mut m).is_ok() { (x, m.top, m.height) } else { (0.0, 0.0, 0.0) }
        }
    }

    /// The UTF-16 position nearest to layout-relative `x` (the side of a character the point is on).
    pub fn position_at(&self, l: &IDWriteTextLayout, x: f32) -> usize {
        unsafe {
            let mut trailing = windows::core::BOOL(0);
            let mut inside = windows::core::BOOL(0);
            let mut m = DWRITE_HIT_TEST_METRICS::default();
            if l.HitTestPoint(x, 1.0, &mut trailing, &mut inside, &mut m).is_err() {
                return 0;
            }
            (m.textPosition + if trailing.as_bool() { m.length } else { 0 }) as usize
        }
    }

    pub fn layout_width(&self, l: &IDWriteTextLayout) -> f32 {
        unsafe {
            let mut m = DWRITE_TEXT_METRICS::default();
            if l.GetMetrics(&mut m).is_ok() { m.widthIncludingTrailingWhitespace } else { 0.0 }
        }
    }

    fn path(&self, points: &[(f32, f32)], closed: bool) -> Option<ID2D1PathGeometry> {
        let (&first, rest) = points.split_first()?;
        unsafe {
            let geo = self.dev.dc.GetFactory().ok()?.CreatePathGeometry().ok()?;
            let sink = geo.Open().ok()?;
            let pt = |(x, y): (f32, f32)| windows_numerics::Vector2 { X: x, Y: y };
            sink.BeginFigure(pt(first), if closed { D2D1_FIGURE_BEGIN_FILLED } else { D2D1_FIGURE_BEGIN_HOLLOW });
            let rest: Vec<_> = rest.iter().map(|&p| pt(p)).collect();
            sink.AddLines(&rest);
            sink.EndFigure(if closed { D2D1_FIGURE_END_CLOSED } else { D2D1_FIGURE_END_OPEN });
            sink.Close().ok()?;
            Some(geo)
        }
    }

    pub fn fill_polygon(&self, points: &[(f32, f32)], color: D2D1_COLOR_F) {
        if let Some(geo) = self.path(points, true) {
            unsafe {
                self.brush.SetColor(&color);
                self.dev.dc.FillGeometry(&geo, &self.brush, None);
            }
        }
    }

    pub fn polyline(&self, points: &[(f32, f32)], color: D2D1_COLOR_F, width: f32) {
        if let Some(geo) = self.path(points, false) {
            unsafe {
                self.brush.SetColor(&color);
                self.dev.dc.DrawGeometry(&geo, &self.brush, width, None);
            }
        }
    }

    /// A top-to-bottom gradient across `r` through `stops` (position 0..1, colour): the scrims behind the
    /// window's top and bottom bars.
    pub fn fill_vgradient(&self, r: D2D_RECT_F, stops: &[(f32, D2D1_COLOR_F)]) {
        unsafe {
            let stops: Vec<D2D1_GRADIENT_STOP> = stops.iter().map(|&(position, color)| D2D1_GRADIENT_STOP { position, color }).collect();
            let rt: &ID2D1RenderTarget = &self.dev.dc;
            let Ok(coll) = rt.CreateGradientStopCollection(&stops, D2D1_GAMMA_2_2, D2D1_EXTEND_MODE_CLAMP) else { return };
            let props = D2D1_LINEAR_GRADIENT_BRUSH_PROPERTIES {
                startPoint: windows_numerics::Vector2 { X: r.left, Y: r.top },
                endPoint: windows_numerics::Vector2 { X: r.left, Y: r.bottom },
            };
            if let Ok(b) = self.dev.dc.CreateLinearGradientBrush(&props, None, &coll) {
                self.dev.dc.FillRectangle(&r, &b);
            }
        }
    }

    /// Starts a layer over `r` whose content fades out over `w` at each end, by `left` and `right` (0..1),
    /// so it fades into whatever is beneath (the strip's edge fades). Ends with `pop_layer`.
    pub fn push_hfade_layer(&self, r: D2D_RECT_F, w: f32, left: f32, right: f32) {
        unsafe {
            let span = (r.right - r.left).max(1.0);
            let edge = (w / span).min(0.5);
            let stop = |position: f32, a: f32| D2D1_GRADIENT_STOP { position, color: D2D1_COLOR_F { r: 0.0, g: 0.0, b: 0.0, a } };
            let stops = [stop(0.0, 1.0 - left), stop(edge, 1.0), stop(1.0 - edge, 1.0), stop(1.0, 1.0 - right)];
            let rt: &ID2D1RenderTarget = &self.dev.dc;
            let brush = rt.CreateGradientStopCollection(&stops, D2D1_GAMMA_2_2, D2D1_EXTEND_MODE_CLAMP).ok().and_then(|coll| {
                let props = D2D1_LINEAR_GRADIENT_BRUSH_PROPERTIES {
                    startPoint: windows_numerics::Vector2 { X: r.left, Y: r.top },
                    endPoint: windows_numerics::Vector2 { X: r.right, Y: r.top },
                };
                self.dev.dc.CreateLinearGradientBrush(&props, None, &coll).ok()
            });
            let mut params = D2D1_LAYER_PARAMETERS1 {
                contentBounds: r,
                geometricMask: std::mem::ManuallyDrop::new(None),
                maskAntialiasMode: D2D1_ANTIALIAS_MODE_PER_PRIMITIVE,
                maskTransform: windows_numerics::Matrix3x2::identity(),
                opacity: 1.0,
                opacityBrush: std::mem::ManuallyDrop::new(brush.map(|b| b.cast().unwrap())),
                layerOptions: D2D1_LAYER_OPTIONS1_NONE,
            };
            self.dev.dc.PushLayer(&params, None);
            std::mem::ManuallyDrop::drop(&mut params.opacityBrush);
        }
    }

    pub fn pop_layer(&self) {
        unsafe { self.dev.dc.PopLayer() }
    }

    pub fn push_clip(&self, r: D2D_RECT_F) {
        unsafe { self.dev.dc.PushAxisAlignedClip(&r, D2D1_ANTIALIAS_MODE_ALIASED) }
    }

    pub fn pop_clip(&self) {
        unsafe { self.dev.dc.PopAxisAlignedClip() }
    }

    /// Draws with a transform (DIPs) applied, restoring identity afterwards.
    pub fn with_transform(&self, m: &windows_numerics::Matrix3x2, draw: impl FnOnce()) {
        unsafe {
            self.dev.dc.SetTransform(m);
            draw();
            self.dev.dc.SetTransform(&windows_numerics::Matrix3x2::identity());
        }
    }
}

fn make_fonts(core: &Text) -> Result<Fonts> {
    unsafe {
        let dw = &core.dwrite;
        let fam = PCWSTR(core.inter_family.as_ptr());
        let mut ellipsis = Vec::new();
        let mut inter = |size: f32, weight: DWRITE_FONT_WEIGHT| -> Result<IDWriteTextFormat> {
            let f = dw.CreateTextFormat(fam, &core.inter, weight, DWRITE_FONT_STYLE_NORMAL, DWRITE_FONT_STRETCH_NORMAL, size, w!("en-us"))?;
            prep(&f)?;
            let sign = dw.CreateEllipsisTrimmingSign(&f)?;
            f.SetTrimming(&DWRITE_TRIMMING { granularity: DWRITE_TRIMMING_GRANULARITY_CHARACTER, delimiter: 0, delimiterCount: 0 }, &sign)?;
            ellipsis.push(sign);
            Ok(f)
        };
        let caption = inter(12.0, DWRITE_FONT_WEIGHT_NORMAL)?;
        let body = inter(14.0, DWRITE_FONT_WEIGHT_NORMAL)?;
        let body_strong = inter(14.0, DWRITE_FONT_WEIGHT_SEMI_BOLD)?;
        let title = inter(20.0, DWRITE_FONT_WEIGHT_SEMI_BOLD)?;
        let body_wrap = inter(14.0, DWRITE_FONT_WEIGHT_NORMAL)?;
        body_wrap.SetWordWrapping(DWRITE_WORD_WRAPPING_EMERGENCY_BREAK)?;
        body_wrap.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_NEAR)?;
        body_wrap.SetLineSpacing(DWRITE_LINE_SPACING_METHOD_UNIFORM, 20.0, 15.0)?;
        let caption_wrap = inter(12.0, DWRITE_FONT_WEIGHT_NORMAL)?;
        caption_wrap.SetWordWrapping(DWRITE_WORD_WRAPPING_EMERGENCY_BREAK)?;
        caption_wrap.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_NEAR)?;
        caption_wrap.SetLineSpacing(DWRITE_LINE_SPACING_METHOD_UNIFORM, 16.0, 12.5)?;
        let overline = inter(11.0, DWRITE_FONT_WEIGHT_SEMI_BOLD)?;
        let page_title = inter(28.0, DWRITE_FONT_WEIGHT_SEMI_BOLD)?;
        let subtitle_wrap = inter(16.0, DWRITE_FONT_WEIGHT_NORMAL)?;
        subtitle_wrap.SetWordWrapping(DWRITE_WORD_WRAPPING_EMERGENCY_BREAK)?;
        subtitle_wrap.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_NEAR)?;
        let icon = |size: f32| -> Result<IDWriteTextFormat> {
            let f = dw.CreateTextFormat(
                w!("Segoe Fluent Icons"),
                None,
                DWRITE_FONT_WEIGHT_NORMAL,
                DWRITE_FONT_STYLE_NORMAL,
                DWRITE_FONT_STRETCH_NORMAL,
                size,
                w!("en-us"),
            )?;
            prep(&f)?;
            f.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_CENTER)?;
            Ok(f)
        };
        let tabular = dw.CreateTypography()?;
        tabular.AddFontFeature(DWRITE_FONT_FEATURE { nameTag: DWRITE_FONT_FEATURE_TAG_TABULAR_FIGURES, parameter: 1 })?;
        Ok(Fonts { caption, body, body_strong, body_wrap, title, caption_wrap, overline, page_title, icons: icon(16.0)?, caption_icons: icon(10.0)?, small_icons: icon(12.0)?, body_icons: icon(14.0)?, hero_icons: icon(96.0)?, subtitle_wrap, tabular, _ellipsis: ellipsis })
    }
}

fn swap_chain(dev: &Device, width: u32, height: u32) -> Result<IDXGISwapChain1> {
    unsafe {
        let dxgi: IDXGIDevice = dev.d3d.cast()?;
        let adapter = dxgi.GetAdapter()?;
        let factory: IDXGIFactory2 = adapter.GetParent()?;
        let desc = DXGI_SWAP_CHAIN_DESC1 {
            Width: width.max(1),
            Height: height.max(1),
            Format: DXGI_FORMAT_B8G8R8A8_UNORM,
            SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
            BufferUsage: DXGI_USAGE_RENDER_TARGET_OUTPUT,
            BufferCount: 2,
            Scaling: DXGI_SCALING_STRETCH,
            SwapEffect: DXGI_SWAP_EFFECT_FLIP_SEQUENTIAL,
            AlphaMode: DXGI_ALPHA_MODE_IGNORE,
            ..Default::default()
        };
        factory.CreateSwapChainForComposition(&dev.d3d, &desc, None)
    }
}

fn prep(f: &IDWriteTextFormat) -> Result<()> {
    unsafe {
        f.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP)?;
        f.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER)?;
        Ok(())
    }
}
