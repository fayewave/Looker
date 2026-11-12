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

use crate::colour::{self, Space};

/// Inter's 24 pt optical size, Medium and SemiBold, as Transient sets its UI (Faye, 2026-11-09: Medium for all
/// text, SemiBold where it stands out; Regular read thin on the dark themes). The variable font's default 14 pt
/// cut set the type looser and heavier at Looker's small sizes.
static INTER_MEDIUM: &[u8] = include_bytes!("../assets/Fonts/Inter_24pt-Medium.ttf");
static INTER_SEMIBOLD: &[u8] = include_bytes!("../assets/Fonts/Inter_24pt-SemiBold.ttf");

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
        let create = |flags: D3D11_CREATE_DEVICE_FLAG| {
            let mut d3d = None;
            D3D11CreateDevice(
                None,
                if warp { D3D_DRIVER_TYPE_WARP } else { D3D_DRIVER_TYPE_HARDWARE },
                HMODULE::default(),
                flags,
                Some(&[D3D_FEATURE_LEVEL_11_1, D3D_FEATURE_LEVEL_11_0, D3D_FEATURE_LEVEL_10_1, D3D_FEATURE_LEVEL_10_0]),
                D3D11_SDK_VERSION,
                Some(&mut d3d),
                None,
                None,
            )
            .map(|_| d3d.unwrap())
        };
        // The hardware device also decodes video (FFmpeg's D3D11VA, see imaging/video.rs), straight into
        // textures Direct2D draws.
        let d3d: ID3D11Device = if warp {
            create(D3D11_CREATE_DEVICE_BGRA_SUPPORT)?
        } else {
            create(D3D11_CREATE_DEVICE_BGRA_SUPPORT | D3D11_CREATE_DEVICE_VIDEO_SUPPORT).or_else(|_| create(D3D11_CREATE_DEVICE_BGRA_SUPPORT))?
        };
        // The video decode thread (FFmpeg) uses the immediate context too: every call on it takes the device's
        // lock. FFmpeg turns this on only for devices it makes itself; without it, decoding beside Direct2D
        // crashed in avcodec.
        if !warp {
            if let Ok(mt) = d3d.GetImmediateContext().and_then(|c| c.cast::<ID3D11Multithread>()) {
                let _ = mt.SetMultithreadProtected(true);
            }
        }
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
    /// NUL-terminated family names as DirectWrite reports them: Medium's, and SemiBold's (the two cuts can
    /// come out as separate families, as in Transient).
    inter_family: Vec<u16>,
    inter_strong: Vec<u16>,
}

pub fn init_text() -> Result<Text> {
    unsafe {
        let dwrite: IDWriteFactory5 = DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)?;
        let loader = dwrite.CreateInMemoryFontFileLoader()?;
        dwrite.RegisterFontFileLoader(&loader)?;
        let builder = dwrite.CreateFontSetBuilder()?;
        for bytes in [INTER_MEDIUM, INTER_SEMIBOLD] {
            let file = loader.CreateInMemoryFontFileReference(&dwrite, bytes.as_ptr() as _, bytes.len() as u32, None)?;
            builder.AddFontFile(&file)?;
        }
        let set = builder.CreateFontSet()?;
        let inter = dwrite.CreateFontCollectionFromFontSet(&set)?;
        let (mut inter_family, mut inter_strong) = (Vec::new(), Vec::new());
        for i in 0..inter.GetFontFamilyCount() {
            let names = inter.GetFontFamily(i)?.GetFamilyNames()?;
            let len = names.GetStringLength(0)? as usize;
            let mut name = vec![0u16; len + 1];
            names.GetString(0, &mut name)?;
            if String::from_utf16_lossy(&name[..len]).contains("SemiBold") {
                inter_strong = name;
            } else {
                inter_family = name;
            }
        }
        if inter_family.is_empty() {
            inter_family = inter_strong.clone();
        }
        if inter_strong.is_empty() {
            inter_strong = inter_family.clone();
        }
        crate::trace::mark("gfx: Inter loaded");
        Ok(Text { dwrite, inter, inter_family, inter_strong })
    }
}

impl Text {
    /// The family holding `weight`: SemiBold's from 600 up, else Medium's.
    pub fn family(&self, weight: DWRITE_FONT_WEIGHT) -> PCWSTR {
        PCWSTR(if weight.0 >= DWRITE_FONT_WEIGHT_SEMI_BOLD.0 { self.inter_strong.as_ptr() } else { self.inter_family.as_ptr() })
    }

    /// Subtitles' text: Inter Medium at `size` DIPs (bold is SemiBold, set per range by the caller), wrapping at
    /// words, centred; scripts Inter lacks fall back to Windows' fonts.
    pub fn subtitle_format(&self, size: f32) -> Result<IDWriteTextFormat> {
        unsafe {
            let weight = DWRITE_FONT_WEIGHT_MEDIUM;
            let f = self.dwrite.CreateTextFormat(self.family(weight), &self.inter, weight, DWRITE_FONT_STYLE_NORMAL, DWRITE_FONT_STRETCH_NORMAL, size, w!("en-us"))?;
            f.SetWordWrapping(DWRITE_WORD_WRAPPING_WRAP)?;
            f.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_CENTER)?;
            f.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_NEAR)?;
            Ok(f)
        }
    }

    /// A one-line Inter format, ellipsis-trimmed, vertically centred (for drawing outside the UI, e.g. mtl.rs).
    pub fn inter(&self, size: f32, weight: DWRITE_FONT_WEIGHT) -> Result<IDWriteTextFormat> {
        unsafe {
            let f = self.dwrite.CreateTextFormat(
                self.family(weight),
                &self.inter,
                weight,
                DWRITE_FONT_STYLE_NORMAL,
                DWRITE_FONT_STRETCH_NORMAL,
                size,
                w!("en-us"),
            )?;
            prep(&f)?;
            let sign = self.dwrite.CreateEllipsisTrimmingSign(&f)?;
            f.SetTrimming(&DWRITE_TRIMMING { granularity: DWRITE_TRIMMING_GRANULARITY_CHARACTER, delimiter: 0, delimiterCount: 0 }, &sign)?;
            Ok(f)
        }
    }
}

pub struct Fonts {
    /// 12 px: the status row, hints, menu shortcuts, tooltips (the same size as `body`, kept apart for its uses).
    pub caption: IDWriteTextFormat,
    /// 12 px: labels, buttons, menu rows, the title bar.
    pub body: IDWriteTextFormat,
    /// 12 px, wrapping, top-aligned, 18 px lines: dialog text.
    pub body_wrap: IDWriteTextFormat,
    /// 16 px semibold: dialog titles.
    pub title: IDWriteTextFormat,
    /// 12 px, wrapping, top-aligned, 17 px lines: info card rows, setting notes.
    pub caption_wrap: IDWriteTextFormat,
    /// 12 px semibold: UPPERCASE section headings.
    pub overline: IDWriteTextFormat,
    /// 14 px semibold: the Settings card's headings.
    pub page_title: IDWriteTextFormat,
    /// 14 px medium, one line: the video's toasts.
    pub body_large: IDWriteTextFormat,
    /// Segoe Fluent Icons, 16 px (toolbar) and 10 px (caption buttons, chevrons).
    pub icons: IDWriteTextFormat,
    pub caption_icons: IDWriteTextFormat,
    /// 12 px icons: a text box's clear button, the Settings card's section icons.
    pub small_icons: IDWriteTextFormat,
    /// 14 px icons: menu rows, glyphs on text buttons.
    pub body_icons: IDWriteTextFormat,
    /// 18 px icons: the video bar's speaker (its glyphs sit small in their box).
    pub large_icons: IDWriteTextFormat,
    /// 96 px icons: the file glyph standing in for a file Looker can't show.
    pub hero_icons: IDWriteTextFormat,
    /// 14 px, wrapping, centred, top-aligned: that file's name.
    pub subtitle_wrap: IDWriteTextFormat,
    /// 14 px semibold, wrapping, top-aligned, 19 px lines: the info card's file name.
    pub heading_wrap: IDWriteTextFormat,
    /// Inter's tabular figures (`tnum`): every digit the same width, so the status row's numbers don't
    /// shuffle as they change (`Gfx::text_tabular`).
    pub tabular: IDWriteTypography,
    _ellipsis: Vec<IDWriteInlineObject>,
}

/// What a cached text layout is for: drawn (plain or with tabular figures), or measured.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum LayoutKind {
    Plain,
    Tabular,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct LayoutKey {
    /// The text's hash; the text itself is kept in the entry and compared on every hit.
    hash: u64,
    format: usize,
    /// The box, as f32 bits.
    w: u32,
    h: u32,
    align: i32,
    kind: LayoutKind,
}

struct LayoutEntry {
    text: Box<[u16]>,
    layout: IDWriteTextLayout,
    /// Its ink reaches outside its box: drawn clipped. Clipping costs (D2D1_DRAW_TEXT_OPTIONS_CLIP was ~30 µs a
    /// call: the explorer's 32 cells took 1 ms a frame), so text that fits is drawn without.
    spills: bool,
    /// The frame it was last used in.
    used: u64,
}

/// Entries unused for this many frames are let go (checked every `LAYOUT_SWEEP` frames), and past `LAYOUT_CAP`
/// the cache starts over (scrolling through a huge folder).
const LAYOUT_KEEP: u64 = 600;
const LAYOUT_SWEEP: u64 = 120;
const LAYOUT_CAP: usize = 20_000;

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
    /// The checkerboard's brush, for the DPI and the squares (a key of the theme's colours) it was made for.
    checker: Option<(ID2D1BitmapBrush1, f32, u32)>,
    /// The space the swap chain is in (see colour.rs). Under scRGB the swap chain is 16-bit float and holds
    /// only the photo layer; everything else draws into `ui` (8-bit sRGB, the same pixels as an SDR frame),
    /// laid over it at the end of the frame.
    space: Space,
    ui: Option<Overlay>,
    /// One per `push_fade`: the transform to put back, if it pushed a layer.
    fade_stack: std::cell::RefCell<Vec<Option<windows_numerics::Matrix3x2>>>,
    /// Where a format's first baseline sits in a box of a given height (by format and height): text is
    /// placed so it lands on a whole device pixel (`snap_text`).
    baselines: std::cell::RefCell<std::collections::HashMap<(usize, u32), f32>>,
    /// Text layouts kept from frame to frame (`cached_layout`): laying text out was most of what drawing the UI
    /// cost, and nearly all of it is the same every frame (the explorer's rows took ~2 ms a frame laid out
    /// afresh, which at 360 Hz is most of a vsync).
    layouts: std::cell::RefCell<std::collections::HashMap<LayoutKey, LayoutEntry>>,
    /// Frames begun, for the layouts' ages.
    frame: std::cell::Cell<u64>,
    /// The copy of the frame `blur_backdrop` blurs, kept while the size, format and DPI stay.
    backdrop: Option<(ID3D11Texture2D, ID2D1Bitmap1, (u32, u32, i32, u32))>,
    blur_failed: bool,
    /// Shell icons on the GPU, by icon (its address; the Arc keeps it alive) and tint.
    icons: std::cell::RefCell<std::collections::HashMap<(usize, Option<u32>), (std::sync::Arc<crate::shell_icons::Icon>, ID2D1Bitmap1)>>,
    /// The soft shadow's blurred rounded square (`soft_shadow`), by corner radius (quarter DIPs) and DPI.
    shadows: std::cell::RefCell<std::collections::HashMap<(u32, u32), (ID2D1Bitmap1, u32)>>,
}

/// The UI layer under scRGB, a copy of the photo layer to blend it with and the effects that lay it over the
/// photo layer (`ui_over`).
#[derive(Clone)]
struct Overlay {
    bitmap: ID2D1Bitmap1,
    photo: (ID3D11Texture2D, ID2D1Bitmap1),
    out: ID2D1Effect,
}

#[derive(Clone, Copy, PartialEq)]
pub enum Align {
    Left,
    Right,
    Center,
}

/// The checkerboard's darker square for the theme in use (`Theme::checker`: neutral greys a hair apart, so they
/// read as texture, not a pattern): also the plain viewport fill when the checkerboard is off.
pub fn checker_plain() -> u32 {
    crate::theme::t().checker[0]
}

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

pub fn rect(x: f32, y: f32, w: f32, h: f32) -> D2D_RECT_F {
    D2D_RECT_F { left: x, top: y, right: x + w, bottom: y + h }
}

impl Gfx {
    pub fn attach(dev: Device, text: Text, hwnd: HWND, width: u32, height: u32, dpi: f32, space: Space) -> Result<Gfx> {
        unsafe {
            let swap = swap_chain(&dev, width, height, space.is_scrgb())?;
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
                space,
                ui: None,
                fade_stack: Default::default(),
                baselines: Default::default(),
                layouts: Default::default(),
                frame: Default::default(),
                backdrop: None,
                blur_failed: false,
                icons: Default::default(),
                shadows: Default::default(),
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
            let swap = swap_chain(&dev, self.size.0, self.size.1, self.space.is_scrgb())?;
            let brush = dev.dc.CreateSolidColorBrush(&rgb(0xFFFFFF), None)?;
            self.dev.dc.SetTarget(None);
            self.target = None;
            self.checker = None;
            self.ui = None;
            self.backdrop = None;
            self.icons.borrow_mut().clear();
            self.shadows.borrow_mut().clear();
            self.swap = swap;
            self.brush = brush;
            self.dev = dev;
            self.make_target()
        }
    }

    /// Moves to another colour space (see colour.rs). Photo bitmaps made for the old one are wrong
    /// afterwards; the caller re-makes its own. When the swap chain's format changes, draw a frame and then
    /// `commit_swap`, as after `switch_device`.
    pub fn set_space(&mut self, space: Space) -> Result<bool> {
        let new_chain = space.is_scrgb() != self.space.is_scrgb();
        self.space = space;
        self.checker = None;
        self.ui = None;
        self.backdrop = None;
        if new_chain {
            unsafe {
                let swap = swap_chain(&self.dev, self.size.0, self.size.1, self.space.is_scrgb())?;
                self.dev.dc.SetTarget(None);
                self.target = None;
                self.swap = swap;
            }
        }
        self.make_target()?;
        Ok(new_chain)
    }

    pub fn space(&self) -> &Space {
        &self.space
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

    /// The QPC time of the vblank the swap chain's last frame was shown at (on the monitor the window is on), when
    /// DXGI keeps statistics for it.
    pub fn last_vblank_qpc(&self) -> Option<i64> {
        let mut st = DXGI_FRAME_STATISTICS::default();
        unsafe { self.swap.GetFrameStatistics(&mut st) }.ok()?;
        (st.SyncQPCTime > 0).then_some(st.SyncQPCTime)
    }

    /// The D3D11 device under Direct2D (the 3D model view draws on it, see model/render.rs).
    pub fn d3d(&self) -> &ID3D11Device {
        &self.dev.d3d
    }

    /// A texture of this device (premultiplied, in the photo layer's format) as a bitmap to draw.
    pub fn texture_bitmap(&self, texture: &ID3D11Texture2D, format: DXGI_FORMAT) -> Result<ID2D1Bitmap1> {
        unsafe {
            let surface: IDXGISurface = texture.cast()?;
            let props = D2D1_BITMAP_PROPERTIES1 {
                pixelFormat: D2D1_PIXEL_FORMAT { format, alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED },
                dpiX: 96.0,
                dpiY: 96.0,
                bitmapOptions: D2D1_BITMAP_OPTIONS_NONE,
                colorContext: std::mem::ManuallyDrop::new(None),
            };
            self.dev.dc.CreateBitmapFromDxgiSurface(&surface, Some(&props))
        }
    }

    fn make_target(&mut self) -> Result<()> {
        unsafe {
            let surface: IDXGISurface = self.swap.GetBuffer(0)?;
            let format = if self.space.is_scrgb() { DXGI_FORMAT_R16G16B16A16_FLOAT } else { DXGI_FORMAT_B8G8R8A8_UNORM };
            let props = D2D1_BITMAP_PROPERTIES1 {
                pixelFormat: D2D1_PIXEL_FORMAT { format, alphaMode: D2D1_ALPHA_MODE_IGNORE },
                dpiX: self.dpi,
                dpiY: self.dpi,
                bitmapOptions: D2D1_BITMAP_OPTIONS_TARGET | D2D1_BITMAP_OPTIONS_CANNOT_DRAW,
                colorContext: std::mem::ManuallyDrop::new(None),
            };
            let bmp = self.dev.dc.CreateBitmapFromDxgiSurface(&surface, Some(&props))?;
            self.dev.dc.SetTarget(&bmp);
            self.dev.dc.SetDpi(self.dpi, self.dpi);
            // Effects and layers keep values outside 0..1 (wide-gamut colours are negative in scRGB).
            let precision = if self.space.is_scrgb() { D2D1_BUFFER_PRECISION_16BPC_FLOAT } else { D2D1_BUFFER_PRECISION_UNKNOWN };
            self.dev.dc.SetRenderingControls(&D2D1_RENDERING_CONTROLS { bufferPrecision: precision, tileSize: D2D_SIZE_U { width: 0, height: 0 } });
            self.target = Some(bmp);
            Ok(())
        }
    }

    pub fn resize(&mut self, width: u32, height: u32, dpi: f32) -> Result<()> {
        self.size = (width, height);
        unsafe {
            self.dev.dc.SetTarget(None);
            self.target = None;
            self.ui = None;
            self.backdrop = None;
            if dpi != self.dpi {
                self.checker = None;
            }
            self.dpi = dpi;
            self.swap.ResizeBuffers(0, width.max(1), height.max(1), DXGI_FORMAT_UNKNOWN, DXGI_SWAP_CHAIN_FLAG(0))?;
            self.make_target()
        }
    }

    /// Starts a frame with the photo layer: `bg` fills the window. Under scRGB, `begin_ui` must follow once
    /// the photo layer is drawn.
    pub fn begin(&self, bg: u32) {
        let frame = self.frame.get() + 1;
        self.frame.set(frame);
        if frame % LAYOUT_SWEEP == 0 {
            let mut l = self.layouts.borrow_mut();
            if l.len() > LAYOUT_CAP {
                l.clear();
            } else {
                l.retain(|_, e| frame - e.used <= LAYOUT_KEEP);
            }
        }
        unsafe {
            self.dev.dc.BeginDraw();
            self.dev.dc.Clear(Some(&self.photo_rgb(bg)));
        }
    }

    /// From here on the frame draws the UI. Under scRGB that's the 8-bit layer laid over the photos in `end`;
    /// `backdrop` makes it opaque in that colour (no photo shows: the landing page), so it blends exactly as on
    /// SDR.
    pub fn begin_ui(&mut self, backdrop: Option<u32>) {
        if !self.space.is_scrgb() {
            return;
        }
        if self.ui.is_none() {
            self.ui = self.make_overlay().map_err(|e| crate::trace::mark(format!("UI layer failed: {e}"))).ok();
        }
        if let Some(o) = &self.ui {
            unsafe {
                self.dev.dc.SetTarget(&o.bitmap);
                self.dev.dc.Clear(Some(&backdrop.map_or(D2D1_COLOR_F { r: 0.0, g: 0.0, b: 0.0, a: 0.0 }, rgb)));
            }
        }
    }

    fn make_overlay(&self) -> Result<Overlay> {
        unsafe {
            let props = D2D1_BITMAP_PROPERTIES1 {
                pixelFormat: D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED },
                dpiX: self.dpi,
                dpiY: self.dpi,
                bitmapOptions: D2D1_BITMAP_OPTIONS_TARGET,
                colorContext: std::mem::ManuallyDrop::new(None),
            };
            let (w, h) = (self.size.0.max(1), self.size.1.max(1));
            let bitmap = self.dev.dc.CreateBitmap(D2D_SIZE_U { width: w, height: h }, None, 0, &props)?;
            let desc = D3D11_TEXTURE2D_DESC {
                Width: w,
                Height: h,
                MipLevels: 1,
                ArraySize: 1,
                Format: DXGI_FORMAT_R16G16B16A16_FLOAT,
                SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
                Usage: D3D11_USAGE_DEFAULT,
                BindFlags: D3D11_BIND_SHADER_RESOURCE.0 as u32,
                CPUAccessFlags: 0,
                MiscFlags: 0,
            };
            let mut tex = None;
            self.dev.d3d.CreateTexture2D(&desc, None, Some(&mut tex))?;
            let tex = tex.ok_or(windows::core::Error::from(windows::Win32::Foundation::E_FAIL))?;
            let copy_props = D2D1_BITMAP_PROPERTIES1 {
                pixelFormat: D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_R16G16B16A16_FLOAT, alphaMode: D2D1_ALPHA_MODE_IGNORE },
                dpiX: self.dpi,
                dpiY: self.dpi,
                bitmapOptions: D2D1_BITMAP_OPTIONS_NONE,
                colorContext: std::mem::ManuallyDrop::new(None),
            };
            let copy = self.dev.dc.CreateBitmapFromDxgiSurface(&tex.cast::<IDXGISurface>()?, Some(&copy_props))?;
            let out = ui_over(&self.dev.dc, &copy, &bitmap, self.space.white_scale())?;
            Ok(Overlay { bitmap, photo: (tex, copy), out })
        }
    }

    /// Lays the UI layer over the photo layer in the swap chain (scRGB only).
    unsafe fn flatten_ui(&self, o: &Overlay, target: &ID2D1Bitmap1) -> Result<()> {
        unsafe {
            let dc = &self.dev.dc;
            dc.SetTarget(target);
            dc.SetTransform(&windows_numerics::Matrix3x2::identity());
            // `ui_over` reads the photo layer as drawn so far.
            dc.Flush(None, None)?;
            let back: ID3D11Texture2D = self.swap.GetBuffer(0)?;
            self.dev.d3d.GetImmediateContext()?.CopyResource(&o.photo.0, &back);
            dc.DrawImage(&o.out.GetOutput()?, None, None, D2D1_INTERPOLATION_MODE_NEAREST_NEIGHBOR, D2D1_COMPOSITE_MODE_SOURCE_OVER);
            Ok(())
        }
    }

    pub fn end(&self) -> Result<()> {
        unsafe {
            if let (true, Some(o), Some(t)) = (self.space.is_scrgb(), &self.ui, &self.target)
                && let Err(e) = self.flatten_ui(o, t)
            {
                crate::trace::mark(format!("UI layer not laid over: {e}"));
            }
            self.dev.dc.EndDraw(None, None)?;
            self.swap.Present(1, DXGI_PRESENT(0)).ok()
        }
    }

    /// A subtitle's text as a picture, made outside a frame (Direct2D isn't mid-draw): `layout` (DIPs; white where
    /// it has no colour of its own) over a dark outline `outline` DIPs wide and a soft shadow below it, so it reads
    /// on any picture. Premultiplied BGRA at the display's DPI; returns it with its size in DIPs and where the
    /// layout's origin sits in it (the margin the outline and shadow take).
    pub fn outlined_text(&self, layout: &IDWriteTextLayout, outline: f32) -> Result<(ID2D1Bitmap1, f32, f32, f32)> {
        unsafe {
            let dc = &self.dev.dc;
            let mut m = DWRITE_TEXT_METRICS::default();
            layout.GetMetrics(&mut m)?;
            let blur = outline.max(1.0) * 1.2;
            let pad = (outline + blur * 3.0 + 2.0).ceil();
            // The layout's whole width (its text is aligned within it), and its lines' height.
            let (w, h) = (m.layoutWidth + 2.0 * pad, m.height + 2.0 * pad);
            let origin_x = pad;
            let k = self.dpi / 96.0;
            let (pw, ph) = (((w * k).ceil() as u32).clamp(1, self.max_bitmap_size()), ((h * k).ceil() as u32).clamp(1, self.max_bitmap_size()));
            let props = D2D1_BITMAP_PROPERTIES1 {
                pixelFormat: D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED },
                dpiX: self.dpi,
                dpiY: self.dpi,
                bitmapOptions: D2D1_BITMAP_OPTIONS_TARGET,
                colorContext: std::mem::ManuallyDrop::new(None),
            };
            let out = dc.CreateBitmap(D2D_SIZE_U { width: pw, height: ph }, None, 0, &props)?;
            let prev = dc.GetTarget().ok();
            let (mut dx, mut dy) = (0.0f32, 0.0f32);
            dc.GetDpi(&mut dx, &mut dy);
            let mut world = windows_numerics::Matrix3x2::identity();
            dc.GetTransform(&mut world);
            let white = dc.CreateSolidColorBrush(&rgb(0xFFFFFF), None)?;
            let at = windows_numerics::Vector2 { X: origin_x, Y: pad };
            // The text's shape alone, for the outline and shadow to grow from.
            let shape = dc.CreateCommandList()?;
            dc.SetTarget(&shape);
            dc.SetDpi(self.dpi, self.dpi);
            dc.BeginDraw();
            dc.SetTransform(&windows_numerics::Matrix3x2::identity());
            dc.DrawTextLayout(at, layout, &white, D2D1_DRAW_TEXT_OPTIONS_NONE);
            let drew = dc.EndDraw(None, None);
            let closed = shape.Close();
            let r = drew.and(closed).and_then(|_| {
                dc.SetTarget(&out);
                dc.SetDpi(self.dpi, self.dpi);
                dc.BeginDraw();
                dc.SetTransform(&windows_numerics::Matrix3x2::identity());
                dc.Clear(Some(&D2D1_COLOR_F { r: 0.0, g: 0.0, b: 0.0, a: 0.0 }));
                let grown = dc.CreateEffect(&CLSID_D2D1Morphology)?;
                grown.SetInput(0, &shape, true);
                set_u32(&grown, D2D1_MORPHOLOGY_PROP_MODE.0 as u32, D2D1_MORPHOLOGY_MODE_DILATE.0 as u32)?;
                let kernel = ((2.0 * outline * k).round() as u32 + 1).clamp(1, 99);
                e_u32(&grown, D2D1_MORPHOLOGY_PROP_WIDTH.0 as u32, kernel)?;
                e_u32(&grown, D2D1_MORPHOLOGY_PROP_HEIGHT.0 as u32, kernel)?;
                // The outline: the grown shape, its edge softened a little; the shadow: blurred further, lower.
                let edge = shadow(dc, &grown.GetOutput()?, (0.45 * k).max(0.5), 0.92)?;
                let drop = shadow(dc, &grown.GetOutput()?, blur, 0.55)?;
                let below = windows_numerics::Vector2 { X: outline * 0.3, Y: outline * 0.9 };
                dc.DrawImage(&drop.GetOutput()?, Some(&below), None, D2D1_INTERPOLATION_MODE_LINEAR, D2D1_COMPOSITE_MODE_SOURCE_OVER);
                dc.DrawImage(&edge.GetOutput()?, None, None, D2D1_INTERPOLATION_MODE_LINEAR, D2D1_COMPOSITE_MODE_SOURCE_OVER);
                dc.DrawTextLayout(at, layout, &white, D2D1_DRAW_TEXT_OPTIONS_ENABLE_COLOR_FONT);
                dc.EndDraw(None, None).map(|_| ())
            });
            dc.SetTarget(prev.as_ref());
            dc.SetDpi(dx, dy);
            dc.SetTransform(&world);
            r.map(|_| (out, pw as f32 / k, ph as f32 / k, pad))
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

    /// A bitmap for the photo layer: as `bitmap` when the pixels are already in the display's space (SDR, where
    /// the decoder converted them), else converted from `colour` into scRGB once, here, on the GPU.
    pub fn photo_bitmap(&self, width: u32, height: u32, pixels: &[u8], colour: &crate::imaging::Colour) -> Result<ID2D1Bitmap1> {
        if let crate::imaging::Colour::Linear { absolute, peak } = *colour {
            return self.hdr_bitmap(width, height, pixels, absolute, peak);
        }
        let src = self.bitmap(width, height, pixels)?;
        if !self.space.is_scrgb() || matches!(colour, crate::imaging::Colour::Display) {
            return Ok(src);
        }
        unsafe {
            let dc = &self.dev.dc;
            let fx = to_scrgb(dc, &src.cast()?, &colour_context(dc, colour)?, self.space.white_scale(), true)?;
            let out = bake(dc, &fx.GetOutput()?, width, height);
            dc.SetDpi(self.dpi, self.dpi);
            out
        }
    }

    /// An HDR decode (half floats, see imaging/hdr.rs) on the photo layer: SDR white lifted to the display's
    /// (unless the pixels are `absolute` scRGB), and tone-mapped down to the panel's peak when they go past it.
    fn hdr_bitmap(&self, width: u32, height: u32, pixels: &[u8], absolute: bool, peak: f32) -> Result<ID2D1Bitmap1> {
        unsafe {
            let dc = &self.dev.dc;
            let props = D2D1_BITMAP_PROPERTIES1 {
                pixelFormat: D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_R16G16B16A16_FLOAT, alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED },
                dpiX: 96.0,
                dpiY: 96.0,
                bitmapOptions: D2D1_BITMAP_OPTIONS_NONE,
                colorContext: std::mem::ManuallyDrop::new(None),
            };
            let src = dc.CreateBitmap(D2D_SIZE_U { width, height }, Some(pixels.as_ptr() as _), width * 8, &props)?;
            if !self.space.is_scrgb() {
                return Ok(src);
            }
            let k = if absolute { 1.0 } else { self.space.white_scale() };
            let mut out: ID2D1Image = scale_rgb(dc, &src.cast()?, k)?.GetOutput()?;
            let content_nits = peak * k * 80.0;
            let panel_nits = colour::peak_nits();
            if content_nits > panel_nits * 1.02 {
                let tm = dc.CreateEffect(&CLSID_D2D1HdrToneMap)?;
                tm.SetInput(0, &out, true);
                set_f32(&tm, D2D1_HDRTONEMAP_PROP_INPUT_MAX_LUMINANCE.0 as u32, content_nits)?;
                set_f32(&tm, D2D1_HDRTONEMAP_PROP_OUTPUT_MAX_LUMINANCE.0 as u32, panel_nits)?;
                set_u32(&tm, D2D1_HDRTONEMAP_PROP_DISPLAY_MODE.0 as u32, D2D1_HDRTONEMAP_DISPLAY_MODE_HDR.0 as u32)?;
                out = tm.GetOutput()?;
            }
            let baked = bake(dc, &out, width, height);
            dc.SetDpi(self.dpi, self.dpi);
            baked
        }
    }

    /// A colour of the photo layer (the viewport's fill, a PDF's paper) from sRGB: as is under SDR, linear and
    /// at SDR white's level under scRGB.
    pub fn photo_rgb(&self, hex: u32) -> D2D1_COLOR_F {
        let c = rgb(hex);
        if !self.space.is_scrgb() {
            return c;
        }
        let k = self.space.white_scale();
        D2D1_COLOR_F { r: colour::srgb_to_linear(c.r) * k, g: colour::srgb_to_linear(c.g) * k, b: colour::srgb_to_linear(c.b) * k, a: c.a }
    }
    /// A Windows shell icon `dip` DIPs across, centred in `r` and pixel-snapped so it stays crisp: at its own
    /// size when that's no bigger than needed (drawn 1:1), else scaled down to `dip`. In its own colours, or
    /// recoloured in `tint`. Made a GPU bitmap on first use.
    pub fn draw_icon(&self, icon: &std::sync::Arc<crate::shell_icons::Icon>, r: D2D_RECT_F, dip: f32, opacity: f32, tint: Option<u32>) {
        let key = (std::sync::Arc::as_ptr(icon) as usize, tint);
        let mut cache = self.icons.borrow_mut();
        if !cache.contains_key(&key) {
            let px = match tint {
                Some(t) => crate::shell_icons::tinted(icon, t),
                None => icon.bgra.clone(),
            };
            let Ok(bmp) = self.bitmap(icon.w, icon.h, &px) else { return };
            cache.insert(key, (icon.clone(), bmp));
        }
        let Some((_, bmp)) = cache.get(&key) else { return };
        let scale = self.dpi / 96.0;
        let need = dip * scale;
        let (w, h) = if icon.w as f32 <= need + 0.5 { (icon.w as f32, icon.h as f32) } else { (need, need * icon.h as f32 / icon.w as f32) };
        let (w, h) = (w / scale, h / scale);
        let x = self.snap(r.left + (r.right - r.left - w) / 2.0);
        let y = self.snap(r.top + (r.bottom - r.top - h) / 2.0);
        self.draw_bitmap(bmp, rect(x, y, w, h), opacity);
    }

    /// The soft drop shadow under a floating surface `r` with corner `radius` (Faye, 2026-11-11): a real Gaussian
    /// blur of its shape, a touch below it, so it fades out evenly all round and lighter round the corners, as a
    /// blurred shape does (stacked rounded rings were as dark into the corners as along the edges, which read as
    /// heavy corners on the light themes). The blurred corner is worked out once per radius and DPI on the CPU and
    /// drawn in nine pieces, the edges and middle stretched.
    /// False when the surface is too small for it (the caller draws the plain stepped shadow instead).
    pub fn soft_shadow(&self, r: D2D_RECT_F, radius: f32, opacity: f32) -> bool {
        // Blur (standard deviation), drop and darkness, in DIPs and alpha.
        const SIGMA: f32 = 2.6;
        const DROP: f32 = 2.0;
        const DARK: f32 = 0.30;
        if opacity <= 0.0 {
            return true;
        }
        let s = self.dpi / 96.0;
        // The radius as drawn (never more than half the surface's shorter side, as Direct2D draws it).
        let radius = radius.min((r.right - r.left).min(r.bottom - r.top) / 2.0).max(0.0);
        let key = ((radius * 4.0).round() as u32, self.dpi.round() as u32);
        let mut cache = self.shadows.borrow_mut();
        if !cache.contains_key(&key) {
            let Some(made) = shadow_square(radius * s, SIGMA * s, DARK).and_then(|(px, n, pad)| self.bitmap(n, n, &px).ok().map(|b| (b, pad))) else { return false };
            cache.insert(key, made);
        }
        let Some((bmp, pad)) = cache.get(&key) else { return false };
        let pad = *pad as f32;
        let n = unsafe { bmp.GetPixelSize() }.width as f32;
        // The square: the shape `pad` in from each side; a corner piece reaches `pad` past the end of the curve,
        // so the 2 pixels in the middle are pure edge, stretched along the sides.
        let c = (n - 2.0) / 2.0;
        let grow = pad / s;
        let out = D2D_RECT_F { left: self.snap(r.left - grow), top: self.snap(r.top - grow + DROP), right: self.snap(r.right + grow), bottom: self.snap(r.bottom + grow + DROP) };
        let cd = c / s;
        let (xs, ys) = ([out.left, out.left + cd, out.right - cd, out.right], [out.top, out.top + cd, out.bottom - cd, out.bottom]);
        // A surface smaller than two corners' reach.
        if xs[1] > xs[2] || ys[1] > ys[2] {
            return false;
        }
        let src = [0.0, c, c + 2.0, n];
        unsafe {
            for row in 0..3 {
                for col in 0..3 {
                    let d = D2D_RECT_F { left: xs[col], top: ys[row], right: xs[col + 1], bottom: ys[row + 1] };
                    if d.right - d.left <= 0.0 || d.bottom - d.top <= 0.0 {
                        continue;
                    }
                    let sr = D2D_RECT_F { left: src[col], top: src[row], right: src[col + 1], bottom: src[row + 1] };
                    self.dev.dc.DrawBitmap(bmp, Some(&d), opacity, D2D1_INTERPOLATION_MODE_LINEAR, Some(&sr), None);
                }
            }
        }
        true
    }

    pub fn draw_bitmap(&self, bmp: &ID2D1Bitmap1, dest: D2D_RECT_F, opacity: f32) {
        unsafe {
            self.dev.dc.DrawBitmap(bmp, Some(&dest), opacity, D2D1_INTERPOLATION_MODE_HIGH_QUALITY_CUBIC, None, None);
        }
    }

    /// A photo-layer bitmap at `dest`. `sdr`: its colours lie between black and SDR white (an sRGB or
    /// monitor-profile decode, a PDF page). Under scRGB those are clamped there after scaling, as an 8-bit target
    /// does: the cubic filter rings at hard edges, and unclamped every letter of a PDF had a halo brighter than
    /// the paper (up to 115 % of SDR white) and a fringe below black. HDR and wide-gamut pixels go past both
    /// legitimately and are drawn as they are.
    pub fn draw_photo(&self, bmp: &ID2D1Bitmap1, dest: D2D_RECT_F, opacity: f32, sdr: bool) {
        self.draw_photo_px(bmp, dest, opacity, sdr, false);
    }

    /// [`draw_photo`](Self::draw_photo), and `pixels`: the bitmap is the image's own pixels (a full-size decode),
    /// so at 100 % and above each one is drawn as a hard square (nearest neighbour), pin sharp for pixel
    /// peeping, instead of the cubic filter's blur (Faye, 2026-11-11). Nearest neighbour never rings, so it
    /// needs no clamp under scRGB either. Below 100 % it stays the high-quality cubic.
    /// Where an image `w` x `h` pixels drawn at `dest` (DIPs, no transform) goes when its pixels are drawn as hard
    /// squares: its corner on a whole device pixel and, at a whole zoom (100 %, 200 %, …, give or take a hair of
    /// float), exactly that many device pixels per image pixel. Nearest neighbour from half a pixel off, or at a
    /// scale of 0.9999, drops and doubles rows and columns: lines through the picture that came and went with the
    /// window's size (Faye, 2026-11-11). None below 100 % (drawn smooth, nothing to line up).
    pub fn pixel_rect(&self, dest: D2D_RECT_F, w: u32, h: u32) -> Option<D2D_RECT_F> {
        pixel_rect_at(dest, w, h, self.dpi / 96.0)
    }

    pub fn draw_photo_px(&self, bmp: &ID2D1Bitmap1, dest: D2D_RECT_F, opacity: f32, sdr: bool, pixels: bool) {
        if pixels && self.device_scale(bmp, dest) >= PIXELS_FROM {
            unsafe {
                self.dev.dc.DrawBitmap(bmp, Some(&dest), opacity, D2D1_INTERPOLATION_MODE_NEAREST_NEIGHBOR, None, None);
            }
            return;
        }
        if !sdr || !self.space.is_scrgb() || self.draw_clamped(bmp, dest, opacity).is_err() {
            self.draw_bitmap(bmp, dest, opacity);
        }
    }
}

/// `Gfx::pixel_rect` at `s` device pixels per DIP.
fn pixel_rect_at(dest: D2D_RECT_F, w: u32, h: u32, s: f32) -> Option<D2D_RECT_F> {
    if w == 0 || h == 0 {
        return None;
    }
    let k = (dest.right - dest.left) * s / w as f32;
    if k < PIXELS_FROM {
        return None;
    }
    // A whole zoom only when the float hair moves the far edge less than half a device pixel. A wider tolerance
    // (it was 1 %) snapped a 6000-pixel photo mid-animation from 99.1 % to 100 %, 57 pixels bigger out of its
    // corner, so zooming to and from 100 % jolted for a few frames (Faye, 2026-11-11).
    let near = k.round().max(1.0);
    let k = if (k - near).abs() * (w.max(h) as f32) < 0.5 { near } else { k };
    let left = (dest.left * s).round() / s;
    let top = (dest.top * s).round() / s;
    Some(D2D_RECT_F { left, top, right: left + w as f32 * k / s, bottom: top + h as f32 * k / s })
}

impl Gfx {
    /// Device pixels per bitmap pixel when `bmp` is drawn into `dest` under the current transform.
    fn device_scale(&self, bmp: &ID2D1Bitmap1, dest: D2D_RECT_F) -> f32 {
        unsafe {
            let size = bmp.GetSize();
            if size.width <= 0.0 {
                return 0.0;
            }
            let mut world = windows_numerics::Matrix3x2::identity();
            self.dev.dc.GetTransform(&mut world);
            let world_scale = (world.M11 * world.M11 + world.M12 * world.M12).sqrt();
            (dest.right - dest.left) / size.width * world_scale * self.dpi / 96.0
        }
    }

    fn draw_clamped(&self, bmp: &ID2D1Bitmap1, dest: D2D_RECT_F, opacity: f32) -> Result<()> {
        unsafe {
            let dc = &self.dev.dc;
            let size = bmp.GetSize();
            if size.width <= 0.0 || size.height <= 0.0 {
                return Ok(());
            }
            // D2D runs an effect graph at its inputs' resolution and resamples the result, so a clamp on the
            // bitmap would come before the overshoot. The resampling is done inside the graph instead (bitmap to
            // device pixels, high-quality cubic), then the clamp: down to SDR white = 1, clamped, back up (with
            // the opacity), drawn 1:1 in pixels.
            let mut world = windows_numerics::Matrix3x2::identity();
            dc.GetTransform(&mut world);
            let place = windows_numerics::Matrix3x2 {
                M11: (dest.right - dest.left) / size.width,
                M12: 0.0,
                M21: 0.0,
                M22: (dest.bottom - dest.top) / size.height,
                M31: dest.left,
                M32: dest.top,
            };
            let px = self.dpi / 96.0;
            let to_device = place * world * windows_numerics::Matrix3x2 { M11: px, M12: 0.0, M21: 0.0, M22: px, M31: 0.0, M32: 0.0 };
            let affine = dc.CreateEffect(&CLSID_D2D12DAffineTransform)?;
            affine.SetInput(0, bmp, true);
            let m = [to_device.M11, to_device.M12, to_device.M21, to_device.M22, to_device.M31, to_device.M32];
            let bytes: Vec<u8> = m.iter().flat_map(|v| v.to_le_bytes()).collect();
            affine.SetValue(D2D1_2DAFFINETRANSFORM_PROP_TRANSFORM_MATRIX.0 as u32, D2D1_PROPERTY_TYPE_MATRIX_3X2, &bytes)?;
            set_u32(&affine, D2D1_2DAFFINETRANSFORM_PROP_INTERPOLATION_MODE.0 as u32, D2D1_2DAFFINETRANSFORM_INTERPOLATION_MODE_HIGH_QUALITY_CUBIC.0 as u32)?;
            let k = self.space.white_scale();
            let down = color_matrix(dc, &affine.GetOutput()?, 1.0 / k, 1.0, true)?;
            let up = color_matrix(dc, &down.GetOutput()?, k, opacity, false)?;
            dc.SetTransform(&windows_numerics::Matrix3x2::identity());
            dc.SetUnitMode(D2D1_UNIT_MODE_PIXELS);
            dc.DrawImage(&up.GetOutput()?, None, None, D2D1_INTERPOLATION_MODE_NEAREST_NEIGHBOR, D2D1_COMPOSITE_MODE_SOURCE_OVER);
            dc.SetUnitMode(D2D1_UNIT_MODE_DIPS);
            dc.SetTransform(&world);
            Ok(())
        }
    }

    /// The preview-area checkerboard: two greys a hair apart (near-black, or near-white in the light theme) in
    /// 8-DIP squares, rendered at the display's DPI so the squares stay crisp and whole-pixel, anchored at the
    /// viewport's top-left.
    pub fn checkerboard(&mut self, r: D2D_RECT_F) {
        let squares = crate::theme::t().checker;
        let key = squares[0] ^ (squares[1] << 1);
        #[allow(non_snake_case)]
        let (DARK, LIGHT) = (squares[0], squares[1]);
        let scale = self.dpi / 96.0;
        if self.checker.as_ref().is_none_or(|(_, d, k)| *d != self.dpi || *k != key) {
            let cell = (8.0 * scale).round().max(1.0) as u32;
            let n = cell * 2;
            // Under scRGB the tile is half floats at the photo layer's level (`photo_rgb`).
            let scrgb = self.space.is_scrgb();
            let bpp = if scrgb { 8 } else { 4 };
            let mut px = vec![0u8; (n * n * bpp) as usize];
            for y in 0..n {
                for x in 0..n {
                    let c = if (x < cell) == (y < cell) { LIGHT } else { DARK };
                    let i = ((y * n + x) * bpp) as usize;
                    if scrgb {
                        let l = self.photo_rgb(c);
                        for (k, v) in [l.r, l.g, l.b, 1.0].into_iter().enumerate() {
                            px[i + k * 2..i + k * 2 + 2].copy_from_slice(&colour::f16(v).to_le_bytes());
                        }
                    } else {
                        px[i] = (c & 0xFF) as u8;
                        px[i + 1] = ((c >> 8) & 0xFF) as u8;
                        px[i + 2] = ((c >> 16) & 0xFF) as u8;
                        px[i + 3] = 0xFF;
                    }
                }
            }
            unsafe {
                let format = if scrgb { DXGI_FORMAT_R16G16B16A16_FLOAT } else { DXGI_FORMAT_B8G8R8A8_UNORM };
                let props = D2D1_BITMAP_PROPERTIES1 {
                    pixelFormat: D2D1_PIXEL_FORMAT { format, alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED },
                    dpiX: self.dpi,
                    dpiY: self.dpi,
                    bitmapOptions: D2D1_BITMAP_OPTIONS_NONE,
                    colorContext: std::mem::ManuallyDrop::new(None),
                };
                let Ok(tile) = self.dev.dc.CreateBitmap(D2D_SIZE_U { width: n, height: n }, Some(px.as_ptr() as _), n * bpp, &props) else {
                    return;
                };
                let bp = D2D1_BITMAP_BRUSH_PROPERTIES1 {
                    extendModeX: D2D1_EXTEND_MODE_WRAP,
                    extendModeY: D2D1_EXTEND_MODE_WRAP,
                    interpolationMode: D2D1_INTERPOLATION_MODE_NEAREST_NEIGHBOR,
                };
                let Ok(brush) = self.dev.dc.CreateBitmapBrush(&tile, Some(&bp), None) else { return };
                self.checker = Some((brush, self.dpi, key));
            }
        }
        if let Some((brush, _, _)) = &self.checker {
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

    /// The colour to draw an antialiased shape in `color` a second time over itself, so its edges look as if they
    /// were blended in linear light (Faye, 2026-11-12: corners and curves looked slightly jagged everywhere).
    /// Direct2D blends a partly covered edge pixel in sRGB values: a light line half over a dark ground comes out
    /// at a quarter of its light, so on a curve, where the line is split across pixels, it thins and brightens
    /// in steps, while straight edges on whole pixels stay crisp. Drawn twice, coverage k becomes 1 - (1 - k)^2,
    /// close to linear light's k^(1/2.2) through the middle of the ramp (0.75 for 0.73 at a half, 0.44 for 0.53
    /// at a quarter); whole pixels don't change. Only for opaque colours lighter than a dark theme's page (a second
    /// pass of a translucent colour would thicken its inside; a dark shape on a light ground needs the opposite,
    /// which drawing again can't give), fading in over the last few per cent of opacity so a hover's fade doesn't
    /// pop at the end.
    fn edge_pass(&self, color: D2D1_COLOR_F) -> Option<D2D1_COLOR_F> {
        if color.a <= 0.95 {
            return None;
        }
        let t = crate::theme::t();
        let luma = |c: D2D1_COLOR_F| 0.2126 * c.r + 0.7152 * c.g + 0.0722 * c.b;
        if !t.dark || luma(color) < luma(rgb(t.content)) + 0.04 {
            return None;
        }
        Some(D2D1_COLOR_F { a: color.a * ((color.a - 0.95) / 0.05).min(1.0), ..color })
    }

    /// Paints with the shared brush in `color`, then again for [`Gfx::edge_pass`] when it applies.
    fn paint(&self, color: D2D1_COLOR_F, draw: impl Fn(&ID2D1SolidColorBrush)) {
        unsafe { self.brush.SetColor(&color) };
        draw(&self.brush);
        if let Some(c) = self.edge_pass(color) {
            unsafe { self.brush.SetColor(&c) };
            draw(&self.brush);
        }
    }

    pub fn fill_round(&self, r: D2D_RECT_F, radius: f32, color: D2D1_COLOR_F) {
        let rr = D2D1_ROUNDED_RECT { rect: r, radiusX: radius, radiusY: radius };
        self.paint(color, |b| unsafe { self.dev.dc.FillRoundedRectangle(&rr, b) });
    }

    /// A line `width` wide just inside `r`'s rounded edge. Its corners are concentric with the fill's
    /// (`radius` less half the width along the inset centre line): at the full radius the line's corners came out
    /// squarer than the fill's, which showed past them in a faint rim on each diagonal.
    pub fn outline_round(&self, r: D2D_RECT_F, radius: f32, color: D2D1_COLOR_F, width: f32) {
        let h = width / 2.0;
        let r = D2D_RECT_F { left: r.left + h, top: r.top + h, right: r.right - h, bottom: r.bottom - h };
        let rad = (radius - h).max(0.0);
        let rr = D2D1_ROUNDED_RECT { rect: r, radiusX: rad, radiusY: rad };
        self.paint(color, |b| unsafe { self.dev.dc.DrawRoundedRectangle(&rr, b, width, None) });
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

    /// `r` nudged (by under a pixel) so the text's baseline lands on a whole device pixel. Unscaled, DirectWrite
    /// snaps the baseline there itself; under a fade's scale it doesn't, so a menu's icons and labels used to
    /// sit up to a pixel off while it grew in and then jump into place. Placed here already, they don't move.
    fn snap_text(&self, fmt: &IDWriteTextFormat, r: D2D_RECT_F) -> D2D_RECT_F {
        let h = r.bottom - r.top;
        if !(h > 0.0 && h < 10_000.0) {
            return r;
        }
        let key = (fmt.as_raw() as usize, h.to_bits());
        let cached = self.baselines.borrow().get(&key).copied();
        let base = match cached {
            Some(b) => b,
            None => {
                let b = unsafe {
                    self.text.dwrite.CreateTextLayout(&[b'X' as u16], fmt, 1000.0, h).ok().and_then(|l| {
                        let mut m = DWRITE_TEXT_METRICS::default();
                        l.GetMetrics(&mut m).ok()?;
                        let mut lines = [DWRITE_LINE_METRICS::default(); 1];
                        let mut n = 0u32;
                        let _ = l.GetLineMetrics(Some(&mut lines), &mut n);
                        Some(m.top + lines[0].baseline)
                    })
                }
                .unwrap_or(0.0);
                self.baselines.borrow_mut().insert(key, b);
                b
            }
        };
        let k = self.dpi / 96.0;
        let y = (r.top + base) * k;
        let dy = (y.round() - y) / k;
        D2D_RECT_F { top: r.top + dy, bottom: r.bottom + dy, ..r }
    }

    /// A layout of `s` in `fmt`, `w` x `h`, aligned `align` (with tabular figures for `LayoutKind::Tabular`):
    /// the one made for the same text, format, box and alignment in an earlier frame, else a new one kept for
    /// the next. Formats are never changed after they're made (but for their alignment, which `text` no longer
    /// touches), so a format's address names its settings.
    fn cached_layout(&self, s: &[u16], fmt: &IDWriteTextFormat, w: f32, h: f32, align: DWRITE_TEXT_ALIGNMENT, kind: LayoutKind) -> Option<(IDWriteTextLayout, bool)> {
        use std::hash::{Hash, Hasher};
        let (w, h) = (w.max(1.0), h.max(1.0));
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        s.hash(&mut hasher);
        let key = LayoutKey { hash: hasher.finish(), format: fmt.as_raw() as usize, w: w.to_bits(), h: h.to_bits(), align: align.0, kind };
        let frame = self.frame.get();
        if let Some(e) = self.layouts.borrow_mut().get_mut(&key) {
            if *e.text == *s {
                e.used = frame;
                return Some((e.layout.clone(), e.spills));
            }
        }
        let layout = unsafe {
            let l = self.text.dwrite.CreateTextLayout(s, fmt, w, h).ok()?;
            l.SetTextAlignment(align).ok()?;
            if kind == LayoutKind::Tabular {
                l.SetTypography(&self.fonts.tabular, DWRITE_TEXT_RANGE { startPosition: 0, length: s.len() as u32 }).ok()?;
            }
            l
        };
        // Overhangs are how far the ink reaches past each edge of the box (negative inside it); a quarter DIP
        // is antialiasing.
        let spills = unsafe { layout.GetOverhangMetrics() }.map_or(true, |o| o.left.max(o.top).max(o.right).max(o.bottom) > 0.25);
        self.layouts.borrow_mut().insert(key, LayoutEntry { text: s.into(), layout: layout.clone(), spills, used: frame });
        Some((layout, spills))
    }

    fn draw_cached(&self, s: &[u16], fmt: &IDWriteTextFormat, r: D2D_RECT_F, color: D2D1_COLOR_F, align: Align, kind: LayoutKind) {
        let r = self.snap_text(fmt, r);
        if !(r.right > r.left && r.bottom > r.top) {
            return;
        }
        let align = match align {
            Align::Left => DWRITE_TEXT_ALIGNMENT_LEADING,
            Align::Right => DWRITE_TEXT_ALIGNMENT_TRAILING,
            Align::Center => DWRITE_TEXT_ALIGNMENT_CENTER,
        };
        let Some((l, spills)) = self.cached_layout(s, fmt, r.right - r.left, r.bottom - r.top, align, kind) else { return };
        let opt = if spills { D2D1_DRAW_TEXT_OPTIONS_CLIP | D2D1_DRAW_TEXT_OPTIONS_ENABLE_COLOR_FONT } else { D2D1_DRAW_TEXT_OPTIONS_ENABLE_COLOR_FONT };
        unsafe {
            self.brush.SetColor(&color);
            self.dev.dc.DrawTextLayout(windows_numerics::Vector2 { X: r.left, Y: r.top }, &l, &self.brush, opt);
        }
    }

    pub fn text(&self, s: &[u16], fmt: &IDWriteTextFormat, r: D2D_RECT_F, color: D2D1_COLOR_F, align: Align) {
        self.draw_cached(s, fmt, r, color, align, LayoutKind::Plain);
    }

    /// `text` with tabular figures (the status row).
    pub fn text_tabular(&self, s: &[u16], fmt: &IDWriteTextFormat, r: D2D_RECT_F, color: D2D1_COLOR_F, align: Align) {
        self.draw_cached(s, fmt, r, color, align, LayoutKind::Tabular);
    }

    /// The metrics of `s` laid out left-aligned in a `w` x `h` box (measured left-aligned: right-aligned in a
    /// 10,000 DIP box a width came out a hair short and the text drawn into it was trimmed).
    fn metrics(&self, s: &[u16], fmt: &IDWriteTextFormat, w: f32, h: f32, kind: LayoutKind) -> Option<DWRITE_TEXT_METRICS> {
        let (l, _) = self.cached_layout(s, fmt, w, h, DWRITE_TEXT_ALIGNMENT_LEADING, kind)?;
        let mut m = DWRITE_TEXT_METRICS::default();
        unsafe { l.GetMetrics(&mut m) }.ok()?;
        Some(m)
    }

    /// `measure` with tabular figures.
    pub fn measure_tabular(&self, s: &[u16], fmt: &IDWriteTextFormat) -> f32 {
        self.metrics(s, fmt, 10_000.0, 100.0, LayoutKind::Tabular).map_or(0.0, |m| m.widthIncludingTrailingWhitespace)
    }

    /// `measure_height` with tabular figures.
    pub fn measure_height_tabular(&self, s: &[u16], fmt: &IDWriteTextFormat, width: f32) -> f32 {
        self.metrics(s, fmt, width, 10_000.0, LayoutKind::Tabular).map_or(0.0, |m| m.height)
    }

    /// Width of a single line of text, in DIPs.
    pub fn measure(&self, s: &[u16], fmt: &IDWriteTextFormat) -> f32 {
        self.metrics(s, fmt, 10_000.0, 100.0, LayoutKind::Plain).map_or(0.0, |m| m.widthIncludingTrailingWhitespace)
    }

    /// Height of text wrapped at `width` (with a wrapping format), in DIPs.
    pub fn measure_height(&self, s: &[u16], fmt: &IDWriteTextFormat, width: f32) -> f32 {
        self.metrics(s, fmt, width, 10_000.0, LayoutKind::Plain).map_or(0.0, |m| m.height)
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

    /// Transient's (File Pilot's) tab: rounded top corners, the bottom corners flaring out in concave curves
    /// into the line it sits on (`r`'s bottom), so the tab flows into the panel below.
    /// `inset` is the outline's half width: its top corners are then concentric with the fill's.
    fn tab_path(&self, r: D2D_RECT_F, top: f32, flare: f32, closed: bool, inset: f32) -> Option<ID2D1PathGeometry> {
        let top = (top - inset).max(0.0);
        unsafe {
            let geo = self.dev.dc.GetFactory().ok()?.CreatePathGeometry().ok()?;
            let sink = geo.Open().ok()?;
            let p = |x: f32, y: f32| windows_numerics::Vector2 { X: x, Y: y };
            let arc = |to, radius: f32, dir| D2D1_ARC_SEGMENT {
                point: to,
                size: D2D_SIZE_F { width: radius, height: radius },
                rotationAngle: 0.0,
                sweepDirection: dir,
                arcSize: D2D1_ARC_SIZE_SMALL,
            };
            let (l, t, rt, b) = (r.left, r.top, r.right, r.bottom);
            sink.BeginFigure(p(l - flare, b), if closed { D2D1_FIGURE_BEGIN_FILLED } else { D2D1_FIGURE_BEGIN_HOLLOW });
            if flare > 0.0 {
                sink.AddArc(&arc(p(l, b - flare), flare, D2D1_SWEEP_DIRECTION_COUNTER_CLOCKWISE));
            }
            sink.AddLine(p(l, t + top));
            sink.AddArc(&arc(p(l + top, t), top, D2D1_SWEEP_DIRECTION_CLOCKWISE));
            sink.AddLine(p(rt - top, t));
            sink.AddArc(&arc(p(rt, t + top), top, D2D1_SWEEP_DIRECTION_CLOCKWISE));
            sink.AddLine(p(rt, b - flare));
            if flare > 0.0 {
                sink.AddArc(&arc(p(rt + flare, b), flare, D2D1_SWEEP_DIRECTION_COUNTER_CLOCKWISE));
            }
            sink.EndFigure(if closed { D2D1_FIGURE_END_CLOSED } else { D2D1_FIGURE_END_OPEN });
            sink.Close().ok()?;
            Some(geo)
        }
    }

    /// `r` with its edges on whole device pixels.
    fn snap_rect(&self, r: D2D_RECT_F) -> D2D_RECT_F {
        D2D_RECT_F { left: self.snap(r.left), top: self.snap(r.top), right: self.snap(r.right), bottom: self.snap(r.bottom) }
    }

    /// A tab's shape (see `tab_path`), its edges snapped to device pixels as `stroke_tab`'s are.
    pub fn fill_tab(&self, r: D2D_RECT_F, top: f32, flare: f32, color: D2D1_COLOR_F) {
        let Some(geo) = self.tab_path(self.snap_rect(r), top, flare, true, 0.0) else { return };
        self.paint(color, |b| unsafe { self.dev.dc.FillGeometry(&geo, b, None) });
    }

    /// The tab's outline (sides, top and flares; open along the bottom), a hairline. The tab's edges are snapped
    /// to device pixels first: tabs sit at half pixels and slide by fractions, and a hairline on a pixel edge
    /// spreads over two columns at half strength, so the sides came out a dim two pixels under a crisp one-pixel
    /// top, and the shoulder between them looked jagged.
    pub fn stroke_tab(&self, r: D2D_RECT_F, top: f32, flare: f32, color: D2D1_COLOR_F) {
        let h = self.px() / 2.0;
        let r = self.snap_rect(r);
        let r = D2D_RECT_F { left: r.left + h, top: r.top + h, right: r.right - h, bottom: r.bottom - h };
        let Some(geo) = self.tab_path(r, top, flare, false, h) else { return };
        let w = self.px();
        self.paint(color, |b| unsafe { self.dev.dc.DrawGeometry(&geo, b, w, None) });
    }

    /// `stroke_tab` fading down the tab: `color` from the top to `solid` of the way down, gone by `gone` (0..1 of
    /// the tab's height), so the sides fade out before the flares.
    pub fn stroke_tab_fade(&self, r: D2D_RECT_F, top: f32, flare: f32, color: D2D1_COLOR_F, solid: f32, gone: f32) {
        let h = self.px() / 2.0;
        let r = self.snap_rect(r);
        let r = D2D_RECT_F { left: r.left + h, top: r.top + h, right: r.right - h, bottom: r.bottom - h };
        let Some(geo) = self.tab_path(r, top, flare, false, h) else { return };
        // Eased (smoothstep), not straight: a straight ramp on a hairline looked like it stopped suddenly.
        let stops: Vec<D2D1_GRADIENT_STOP> = (0..=12)
            .map(|i| {
                let u = i as f32 / 12.0;
                let k = 1.0 - u * u * (3.0 - 2.0 * u);
                D2D1_GRADIENT_STOP { position: solid + (gone - solid) * u, color: D2D1_COLOR_F { a: color.a * k, ..color } }
            })
            .collect();
        unsafe {
            let rt: &ID2D1RenderTarget = &self.dev.dc;
            let Ok(coll) = rt.CreateGradientStopCollection(&stops, D2D1_GAMMA_2_2, D2D1_EXTEND_MODE_CLAMP) else { return };
            let props = D2D1_LINEAR_GRADIENT_BRUSH_PROPERTIES {
                startPoint: windows_numerics::Vector2 { X: r.left, Y: r.top },
                endPoint: windows_numerics::Vector2 { X: r.left, Y: r.bottom },
            };
            if let Ok(b) = self.dev.dc.CreateLinearGradientBrush(&props, None, &coll) {
                self.dev.dc.DrawGeometry(&geo, &b, self.px(), None);
            }
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

    /// `r` less a disc at (cx, cy) of radius `rad`.
    fn rect_with_hole(&self, r: D2D_RECT_F, cx: f32, cy: f32, rad: f32) -> Option<ID2D1Geometry> {
        unsafe {
            let f = self.dev.dc.GetFactory().ok()?;
            let outer = f.CreateRectangleGeometry(&r).ok()?;
            let disc = f.CreateEllipseGeometry(&D2D1_ELLIPSE { point: windows_numerics::Vector2 { X: cx, Y: cy }, radiusX: rad, radiusY: rad }).ok()?;
            let path = f.CreatePathGeometry().ok()?;
            let sink = path.Open().ok()?;
            outer.CombineWithGeometry(&disc, D2D1_COMBINE_MODE_EXCLUDE, None, D2D1_DEFAULT_FLATTENING_TOLERANCE, &sink).ok()?;
            sink.Close().ok()?;
            path.cast().ok()
        }
    }

    /// What `draw` paints, everywhere in `r` but a disc at (cx, cy) of radius `rad`: an icon with a notch cut out
    /// for a badge, whatever is behind it (the explorer's folder and gear).
    pub fn with_hole(&self, r: D2D_RECT_F, cx: f32, cy: f32, rad: f32, draw: impl FnOnce()) {
        let Some(mask) = self.rect_with_hole(r, cx, cy, rad) else {
            draw();
            return;
        };
        unsafe {
            let params = D2D1_LAYER_PARAMETERS1 {
                contentBounds: r,
                geometricMask: std::mem::ManuallyDrop::new(Some(mask)),
                maskAntialiasMode: D2D1_ANTIALIAS_MODE_PER_PRIMITIVE,
                maskTransform: windows_numerics::Matrix3x2::identity(),
                opacity: 1.0,
                opacityBrush: std::mem::ManuallyDrop::new(None),
                layerOptions: D2D1_LAYER_OPTIONS1_NONE,
            };
            self.dev.dc.PushLayer(&params, None);
            draw();
            self.dev.dc.PopLayer();
            drop(std::mem::ManuallyDrop::into_inner(params.geometricMask));
        }
    }

    /// A filled polygon with its corners rounded: each corner starts and ends `radius` along its edges, curved
    /// through the corner (a quadratic Bézier): the play triangle as Segoe draws it, filled.
    pub fn fill_rounded_polygon(&self, points: &[(f32, f32)], radius: f32, color: D2D1_COLOR_F) {
        let n = points.len();
        if n < 3 {
            return;
        }
        let pt = |(x, y): (f32, f32)| windows_numerics::Vector2 { X: x, Y: y };
        // The point `d` along from a towards b.
        let along = |a: (f32, f32), b: (f32, f32), d: f32| {
            let (dx, dy) = (b.0 - a.0, b.1 - a.1);
            let len = (dx * dx + dy * dy).sqrt().max(1e-3);
            let d = d.min(len / 2.0);
            (a.0 + dx / len * d, a.1 + dy / len * d)
        };
        let geo = unsafe {
            (|| -> Option<ID2D1PathGeometry> {
                let geo = self.dev.dc.GetFactory().ok()?.CreatePathGeometry().ok()?;
                let sink = geo.Open().ok()?;
                for i in 0..n {
                    let (prev, cur, next) = (points[(i + n - 1) % n], points[i], points[(i + 1) % n]);
                    let (a, b) = (along(cur, prev, radius), along(cur, next, radius));
                    if i == 0 {
                        sink.BeginFigure(pt(a), D2D1_FIGURE_BEGIN_FILLED);
                    } else {
                        sink.AddLine(pt(a));
                    }
                    sink.AddQuadraticBezier(&D2D1_QUADRATIC_BEZIER_SEGMENT { point1: pt(cur), point2: pt(b) });
                }
                sink.EndFigure(D2D1_FIGURE_END_CLOSED);
                sink.Close().ok()?;
                Some(geo)
            })()
        };
        if let Some(geo) = geo {
            self.paint(color, |b| unsafe { self.dev.dc.FillGeometry(&geo, b, None) });
        }
    }

    /// One pass only (no [`Gfx::edge_pass`]): the waveform's envelopes are thousands of points, tessellated per
    /// draw, and their look was tuned as they are.
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
            self.paint(color, |b| unsafe { self.dev.dc.DrawGeometry(&geo, b, width, None) });
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

    /// `r` rounded by `radius`, filled with a gradient across it (`horizontal`) or down it: the colour picker's
    /// square and hue strip.
    pub fn fill_gradient_round(&self, r: D2D_RECT_F, radius: f32, horizontal: bool, stops: &[(f32, D2D1_COLOR_F)]) {
        unsafe {
            let stops: Vec<D2D1_GRADIENT_STOP> = stops.iter().map(|&(position, color)| D2D1_GRADIENT_STOP { position, color }).collect();
            let rt: &ID2D1RenderTarget = &self.dev.dc;
            let Ok(coll) = rt.CreateGradientStopCollection(&stops, D2D1_GAMMA_2_2, D2D1_EXTEND_MODE_CLAMP) else { return };
            let end = if horizontal { windows_numerics::Vector2 { X: r.right, Y: r.top } } else { windows_numerics::Vector2 { X: r.left, Y: r.bottom } };
            let props = D2D1_LINEAR_GRADIENT_BRUSH_PROPERTIES { startPoint: windows_numerics::Vector2 { X: r.left, Y: r.top }, endPoint: end };
            if let Ok(b) = self.dev.dc.CreateLinearGradientBrush(&props, None, &coll) {
                self.dev.dc.FillRoundedRectangle(&D2D1_ROUNDED_RECT { rect: r, radiusX: radius, radiusY: radius }, &b);
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

    /// Draws what follows at `opacity`, scaled by `scale` about (ox, oy) in DIPs: a menu, dialog or card
    /// opening (Snapper's fade-and-grow). Pair with `pop_fade`. Once fully shown no layer is pushed.
    pub fn push_fade(&self, opacity: f32, scale: f32, ox: f32, oy: f32) {
        let mut stack = self.fade_stack.borrow_mut();
        if opacity >= 1.0 && scale == 1.0 {
            stack.push(None);
            return;
        }
        unsafe {
            let dc = &self.dev.dc;
            let mut prev = windows_numerics::Matrix3x2::identity();
            dc.GetTransform(&mut prev);
            stack.push(Some(prev));
            let params = D2D1_LAYER_PARAMETERS1 {
                contentBounds: D2D_RECT_F { left: -1e6, top: -1e6, right: 1e6, bottom: 1e6 },
                geometricMask: std::mem::ManuallyDrop::new(None),
                maskAntialiasMode: D2D1_ANTIALIAS_MODE_PER_PRIMITIVE,
                maskTransform: windows_numerics::Matrix3x2::identity(),
                opacity: opacity.clamp(0.0, 1.0),
                opacityBrush: std::mem::ManuallyDrop::new(None),
                layerOptions: D2D1_LAYER_OPTIONS1_NONE,
            };
            dc.PushLayer(&params, None);
            // Scaled about (ox, oy), then by whatever an outer fade is doing.
            let (tx, ty) = (ox * (1.0 - scale), oy * (1.0 - scale));
            let p = prev;
            dc.SetTransform(&windows_numerics::Matrix3x2 {
                M11: scale * p.M11,
                M12: scale * p.M12,
                M21: scale * p.M21,
                M22: scale * p.M22,
                M31: tx * p.M11 + ty * p.M21 + p.M31,
                M32: tx * p.M12 + ty * p.M22 + p.M32,
            });
        }
    }

    pub fn pop_fade(&self) {
        let Some(Some(prev)) = self.fade_stack.borrow_mut().pop() else { return };
        unsafe {
            self.dev.dc.SetTransform(&prev);
            self.dev.dc.PopLayer();
        }
    }

    /// Blurs everything drawn so far this frame by `radius` DIPs: the Settings card's backdrop, as Snapper
    /// blurs its captures behind its card. The frame so far is copied off the back buffer on the GPU and drawn
    /// back blurred. Under scRGB the UI drawn so far is laid on the photo layer first and the UI layer starts
    /// again empty, so what follows draws over the blur. False (nothing changed) when it can't be done.
    pub fn blur_backdrop(&mut self, radius: f32) -> bool {
        if radius < 0.05 {
            return true;
        }
        match unsafe { self.try_blur(radius) } {
            Ok(()) => true,
            Err(e) => {
                if !self.blur_failed {
                    self.blur_failed = true;
                    crate::trace::mark(format!("backdrop blur failed: {e}"));
                }
                false
            }
        }
    }

    unsafe fn try_blur(&mut self, radius: f32) -> Result<()> {
        unsafe {
            let dc = self.dev.dc.clone();
            let Some(target) = self.target.clone() else { return Err(windows::Win32::Foundation::E_FAIL.into()) };
            let mut prev = windows_numerics::Matrix3x2::identity();
            dc.GetTransform(&mut prev);
            dc.SetTransform(&windows_numerics::Matrix3x2::identity());
            let overlay = if self.space.is_scrgb() { self.ui.clone() } else { None };
            if let Some(o) = &overlay {
                self.flatten_ui(o, &target)?;
            }
            let overlay = overlay.map(|o| o.bitmap);
            dc.Flush(None, None)?;
            let back: ID3D11Texture2D = self.swap.GetBuffer(0)?;
            let mut desc = D3D11_TEXTURE2D_DESC::default();
            back.GetDesc(&mut desc);
            let key = (desc.Width, desc.Height, desc.Format.0, self.dpi.to_bits());
            if self.backdrop.as_ref().is_none_or(|b| b.2 != key) {
                let copy = D3D11_TEXTURE2D_DESC {
                    Width: desc.Width,
                    Height: desc.Height,
                    MipLevels: 1,
                    ArraySize: 1,
                    Format: desc.Format,
                    SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
                    Usage: D3D11_USAGE_DEFAULT,
                    BindFlags: D3D11_BIND_SHADER_RESOURCE.0 as u32,
                    CPUAccessFlags: 0,
                    MiscFlags: 0,
                };
                let mut tex = None;
                self.dev.d3d.CreateTexture2D(&copy, None, Some(&mut tex))?;
                let tex = tex.ok_or(windows::core::Error::from(windows::Win32::Foundation::E_FAIL))?;
                let surface: IDXGISurface = tex.cast()?;
                let props = D2D1_BITMAP_PROPERTIES1 {
                    pixelFormat: D2D1_PIXEL_FORMAT { format: desc.Format, alphaMode: D2D1_ALPHA_MODE_IGNORE },
                    dpiX: self.dpi,
                    dpiY: self.dpi,
                    bitmapOptions: D2D1_BITMAP_OPTIONS_NONE,
                    colorContext: std::mem::ManuallyDrop::new(None),
                };
                let bmp = dc.CreateBitmapFromDxgiSurface(&surface, Some(&props))?;
                self.backdrop = Some((tex, bmp, key));
            }
            let (tex, bmp, _) = self.backdrop.as_ref().unwrap();
            self.dev.d3d.GetImmediateContext()?.CopyResource(tex, &back);
            let blur = dc.CreateEffect(&CLSID_D2D1GaussianBlur)?;
            blur.SetInput(0, bmp, true);
            set_f32(&blur, D2D1_GAUSSIANBLUR_PROP_STANDARD_DEVIATION.0 as u32, radius)?;
            // Edges keep their colour rather than fading to nothing.
            set_u32(&blur, D2D1_GAUSSIANBLUR_PROP_BORDER_MODE.0 as u32, D2D1_BORDER_MODE_HARD.0 as u32)?;
            dc.DrawImage(&blur.GetOutput()?, None, None, D2D1_INTERPOLATION_MODE_LINEAR, D2D1_COMPOSITE_MODE_SOURCE_OVER);
            if let Some(bitmap) = &overlay {
                dc.SetTarget(bitmap);
                dc.Clear(Some(&D2D1_COLOR_F { r: 0.0, g: 0.0, b: 0.0, a: 0.0 }));
            }
            dc.SetTransform(&prev);
            Ok(())
        }
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
        let mut ellipsis = Vec::new();
        let mut inter = |size: f32, weight: DWRITE_FONT_WEIGHT| -> Result<IDWriteTextFormat> {
            let f = dw.CreateTextFormat(core.family(weight), &core.inter, weight, DWRITE_FONT_STYLE_NORMAL, DWRITE_FONT_STRETCH_NORMAL, size, w!("en-us"))?;
            prep(&f)?;
            let sign = dw.CreateEllipsisTrimmingSign(&f)?;
            f.SetTrimming(&DWRITE_TRIMMING { granularity: DWRITE_TRIMMING_GRANULARITY_CHARACTER, delimiter: 0, delimiterCount: 0 }, &sign)?;
            ellipsis.push(sign);
            Ok(f)
        };
        // Snapper's ramp (VEIL Transient's), but nothing under 12 (Faye, 2026-11-09): its 11 px small text and
        // section headings are 12 here, beside 12 body, 14 titles and 16 headers.
        let caption = inter(12.0, DWRITE_FONT_WEIGHT_MEDIUM)?;
        let body = inter(12.0, DWRITE_FONT_WEIGHT_MEDIUM)?;
        let title = inter(16.0, DWRITE_FONT_WEIGHT_SEMI_BOLD)?;
        let body_wrap = inter(12.0, DWRITE_FONT_WEIGHT_MEDIUM)?;
        body_wrap.SetWordWrapping(DWRITE_WORD_WRAPPING_EMERGENCY_BREAK)?;
        body_wrap.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_NEAR)?;
        body_wrap.SetLineSpacing(DWRITE_LINE_SPACING_METHOD_UNIFORM, 18.0, 13.5)?;
        let caption_wrap = inter(12.0, DWRITE_FONT_WEIGHT_MEDIUM)?;
        caption_wrap.SetWordWrapping(DWRITE_WORD_WRAPPING_EMERGENCY_BREAK)?;
        caption_wrap.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_NEAR)?;
        caption_wrap.SetLineSpacing(DWRITE_LINE_SPACING_METHOD_UNIFORM, 17.0, 13.0)?;
        let overline = inter(12.0, DWRITE_FONT_WEIGHT_SEMI_BOLD)?;
        let page_title = inter(14.0, DWRITE_FONT_WEIGHT_SEMI_BOLD)?;
        let body_large = inter(14.0, DWRITE_FONT_WEIGHT_MEDIUM)?;
        let subtitle_wrap = inter(14.0, DWRITE_FONT_WEIGHT_MEDIUM)?;
        subtitle_wrap.SetWordWrapping(DWRITE_WORD_WRAPPING_EMERGENCY_BREAK)?;
        subtitle_wrap.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_NEAR)?;
        let heading_wrap = inter(14.0, DWRITE_FONT_WEIGHT_SEMI_BOLD)?;
        heading_wrap.SetWordWrapping(DWRITE_WORD_WRAPPING_EMERGENCY_BREAK)?;
        heading_wrap.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_NEAR)?;
        heading_wrap.SetLineSpacing(DWRITE_LINE_SPACING_METHOD_UNIFORM, 19.0, 14.0)?;
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
        Ok(Fonts { caption, body, body_wrap, title, caption_wrap, overline, page_title, body_large, icons: icon(16.0)?, caption_icons: icon(10.0)?, small_icons: icon(12.0)?, body_icons: icon(14.0)?, large_icons: icon(18.0)?, hero_icons: icon(96.0)?, subtitle_wrap, heading_wrap, tabular, _ellipsis: ellipsis })
    }
}

/// The Direct2D colour context of a decode's [`crate::imaging::Colour`].
fn colour_context(dc: &ID2D1DeviceContext, colour: &crate::imaging::Colour) -> Result<ID2D1ColorContext> {
    use crate::imaging::Colour;
    unsafe {
        match colour {
            Colour::Icc(p) => dc.CreateColorContext(D2D1_COLOR_SPACE_CUSTOM, Some(p)).or_else(|_| dc.CreateColorContext(D2D1_COLOR_SPACE_SRGB, None)),
            Colour::AdobeRgb => {
                let wic: windows::Win32::Graphics::Imaging::IWICImagingFactory = windows::Win32::System::Com::CoCreateInstance(
                    &windows::Win32::Graphics::Imaging::CLSID_WICImagingFactory2,
                    None,
                    windows::Win32::System::Com::CLSCTX_INPROC_SERVER,
                )?;
                let c = wic.CreateColorContext()?;
                c.InitializeFromExifColorSpace(2)?;
                dc.CreateColorContextFromWicColorContext(&c)
            }
            _ => dc.CreateColorContext(D2D1_COLOR_SPACE_SRGB, None),
        }
    }
}

/// `src` (premultiplied, in `ctx`) into scRGB, SDR white at `white_scale` x scRGB 1.0. `premultiplied`: convert the
/// colour under the alpha (photos); otherwise the values as they are (opaque images).
fn to_scrgb(dc: &ID2D1DeviceContext, src: &ID2D1Image, ctx: &ID2D1ColorContext, white_scale: f32, premultiplied: bool) -> Result<ID2D1Effect> {
    unsafe {
        let cm = dc.CreateEffect(&CLSID_D2D1ColorManagement)?;
        cm.SetInput(0, src, true);
        set_interface(&cm, D2D1_COLORMANAGEMENT_PROP_SOURCE_COLOR_CONTEXT.0 as u32, ctx)?;
        set_interface(&cm, D2D1_COLORMANAGEMENT_PROP_DESTINATION_COLOR_CONTEXT.0 as u32, &dc.CreateColorContext(D2D1_COLOR_SPACE_SCRGB, None)?)?;
        let alpha = if premultiplied { D2D1_COLORMANAGEMENT_ALPHA_MODE_PREMULTIPLIED } else { D2D1_COLORMANAGEMENT_ALPHA_MODE_STRAIGHT };
        set_u32(&cm, D2D1_COLORMANAGEMENT_PROP_ALPHA_MODE.0 as u32, alpha.0 as u32)?;
        let best = dc.IsBufferPrecisionSupported(D2D1_BUFFER_PRECISION_32BPC_FLOAT).as_bool();
        let quality = if best { D2D1_COLORMANAGEMENT_QUALITY_BEST } else { D2D1_COLORMANAGEMENT_QUALITY_NORMAL };
        set_u32(&cm, D2D1_COLORMANAGEMENT_PROP_QUALITY.0 as u32, quality.0 as u32)?;
        // SDR white up to the display's level. (The White Level Adjustment effect came out dividing where it
        // should multiply: everything sat at a quarter of its brightness at 160 nits.)
        scale_rgb(dc, &cm.GetOutput()?, white_scale)
    }
}

/// The UI layer `ui` (8-bit, premultiplied sRGB) laid over `photo` (a copy of the photo layer, scRGB with SDR white
/// at `white_scale`) as an SDR frame would blend it: in sRGB values, the photo layer brought back to them first.
/// Converting the layer's premultiplied values alone (as until 2026-11-12) is that blend only over black: over
/// the light themes' page every partly covered pixel came out darker than a whole one (a card's hairline darker
/// at its rounded corners than along its sides). The result covers only what the UI draws on; elsewhere the
/// photo layer stays as it is (the round trip through sRGB would clip wide-gamut and HDR colours).
fn ui_over(dc: &ID2D1DeviceContext, photo: &ID2D1Bitmap1, ui: &ID2D1Bitmap1, white_scale: f32) -> Result<ID2D1Effect> {
    unsafe {
        let srgb = dc.CreateColorContext(D2D1_COLOR_SPACE_SRGB, None)?;
        let best = dc.IsBufferPrecisionSupported(D2D1_BUFFER_PRECISION_32BPC_FLOAT).as_bool();
        let quality = if best { D2D1_COLORMANAGEMENT_QUALITY_BEST } else { D2D1_COLORMANAGEMENT_QUALITY_NORMAL };
        let down = scale_rgb(dc, &photo.cast()?, 1.0 / white_scale)?;
        let enc = dc.CreateEffect(&CLSID_D2D1ColorManagement)?;
        enc.SetInput(0, &down.GetOutput()?, true);
        set_interface(&enc, D2D1_COLORMANAGEMENT_PROP_SOURCE_COLOR_CONTEXT.0 as u32, &dc.CreateColorContext(D2D1_COLOR_SPACE_SCRGB, None)?)?;
        set_interface(&enc, D2D1_COLORMANAGEMENT_PROP_DESTINATION_COLOR_CONTEXT.0 as u32, &srgb)?;
        set_u32(&enc, D2D1_COLORMANAGEMENT_PROP_QUALITY.0 as u32, quality.0 as u32)?;
        let over = dc.CreateEffect(&CLSID_D2D1Composite)?;
        over.SetInput(0, &enc.GetOutput()?, true);
        over.SetInput(1, ui, true);
        // Opaque now: straight and premultiplied are the same.
        let lin = to_scrgb(dc, &over.GetOutput()?, &srgb, white_scale, false)?;
        // Where the UI draws anything at all.
        let mask = color_matrix(dc, &ui.cast()?, 0.0, 255.0, true)?;
        let cut = dc.CreateEffect(&CLSID_D2D1Composite)?;
        cut.SetInput(0, &lin.GetOutput()?, true);
        cut.SetInput(1, &mask.GetOutput()?, true);
        set_u32(&cut, D2D1_COMPOSITE_PROP_MODE.0 as u32, D2D1_COMPOSITE_MODE_DESTINATION_IN.0 as u32)?;
        Ok(cut)
    }
}

/// `src` (premultiplied) with its colour times `k`.
fn scale_rgb(dc: &ID2D1DeviceContext, src: &ID2D1Image, k: f32) -> Result<ID2D1Effect> {
    color_matrix(dc, src, k, 1.0, false)
}

/// `src` (premultiplied) with its colour times `k` and its alpha times `a`; `clamp` then limits the colour to 0..1.
fn color_matrix(dc: &ID2D1DeviceContext, src: &ID2D1Image, k: f32, a: f32, clamp: bool) -> Result<ID2D1Effect> {
    unsafe {
        let fx = dc.CreateEffect(&CLSID_D2D1ColorMatrix)?;
        fx.SetInput(0, src, true);
        #[rustfmt::skip]
        let m: [f32; 20] = [
            k, 0.0, 0.0, 0.0,
            0.0, k, 0.0, 0.0,
            0.0, 0.0, k, 0.0,
            0.0, 0.0, 0.0, a,
            0.0, 0.0, 0.0, 0.0,
        ];
        let bytes: Vec<u8> = m.iter().flat_map(|v| v.to_le_bytes()).collect();
        fx.SetValue(D2D1_COLORMATRIX_PROP_COLOR_MATRIX.0 as u32, D2D1_PROPERTY_TYPE_MATRIX_5X4, &bytes)?;
        set_u32(&fx, D2D1_COLORMATRIX_PROP_ALPHA_MODE.0 as u32, 1)?; // premultiplied
        if clamp {
            fx.SetValue(D2D1_COLORMATRIX_PROP_CLAMP_OUTPUT.0 as u32, D2D1_PROPERTY_TYPE_BOOL, &1u32.to_le_bytes())?;
        }
        Ok(fx)
    }
}

/// Renders `image` (pixel units, at the origin) into a new 16-bit float bitmap of `width x height`. Leaves the
/// context at 96 dpi: the caller puts its own back.
/// Device pixels per image pixel from which a full-size photo is drawn as hard pixels (`Gfx::pixel_rect` makes
/// a scale this close to 1 exactly 1).
const PIXELS_FROM: f32 = 0.99;

/// The soft shadow's source: a rounded square (corner `radius` px) `pad` px in from each side of an `n`-pixel
/// image, Gaussian-blurred (`sigma` px) to at most `dark` alpha, premultiplied black. A corner piece is `2 pad +
/// radius` across, so the two middle pixels are a straight edge's profile. Returns (pixels, n, pad).
fn shadow_square(radius: f32, sigma: f32, dark: f32) -> Option<(Vec<u8>, u32, u32)> {
    let pad = (3.0 * sigma).ceil().max(1.0) as usize;
    let r = radius.max(0.0);
    let c = 2 * pad + r.ceil() as usize;
    let n = 2 * c + 2;
    if n > 1024 {
        return None;
    }
    // Coverage of the shape, 4 x 4 samples a pixel.
    let (lo, hi) = (pad as f32, (n - pad) as f32);
    let inside = |x: f32, y: f32| -> bool {
        if x < lo || x >= hi || y < lo || y >= hi {
            return false;
        }
        let cx = x.clamp(lo + r, hi - r);
        let cy = y.clamp(lo + r, hi - r);
        let (dx, dy) = (x - cx, y - cy);
        dx * dx + dy * dy <= r * r
    };
    let mut a = vec![0f32; n * n];
    for y in 0..n {
        for x in 0..n {
            let mut k = 0;
            for sy in 0..4 {
                for sx in 0..4 {
                    if inside(x as f32 + (sx as f32 + 0.5) / 4.0, y as f32 + (sy as f32 + 0.5) / 4.0) {
                        k += 1;
                    }
                }
            }
            a[y * n + x] = k as f32 / 16.0;
        }
    }
    // Separable Gaussian, out to `pad` either way.
    let kernel: Vec<f32> = (0..=2 * pad).map(|i| (-((i as f32 - pad as f32).powi(2)) / (2.0 * sigma * sigma)).exp()).collect();
    let sum: f32 = kernel.iter().sum();
    let kernel: Vec<f32> = kernel.iter().map(|k| k / sum).collect();
    let blur = |src: &[f32], horizontal: bool| -> Vec<f32> {
        let mut out = vec![0f32; n * n];
        for y in 0..n {
            for x in 0..n {
                let mut v = 0.0;
                for (i, k) in kernel.iter().enumerate() {
                    let o = i as isize - pad as isize;
                    let (sx, sy) = if horizontal { (x as isize + o, y as isize) } else { (x as isize, y as isize + o) };
                    if sx >= 0 && sy >= 0 && (sx as usize) < n && (sy as usize) < n {
                        v += k * src[sy as usize * n + sx as usize];
                    }
                }
                out[y * n + x] = v;
            }
        }
        out
    };
    let a = blur(&blur(&a, true), false);
    let px = a.iter().flat_map(|&v| [0, 0, 0, (v * dark * 255.0).round().clamp(0.0, 255.0) as u8]).collect();
    Some((px, n as u32, pad as u32))
}

fn bake(dc: &ID2D1DeviceContext, image: &ID2D1Image, width: u32, height: u32) -> Result<ID2D1Bitmap1> {
    unsafe {
        let props = D2D1_BITMAP_PROPERTIES1 {
            pixelFormat: D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_R16G16B16A16_FLOAT, alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED },
            dpiX: 96.0,
            dpiY: 96.0,
            bitmapOptions: D2D1_BITMAP_OPTIONS_TARGET,
            colorContext: std::mem::ManuallyDrop::new(None),
        };
        let out = dc.CreateBitmap(D2D_SIZE_U { width, height }, None, 0, &props)?;
        let prev = dc.GetTarget().ok();
        dc.SetTarget(&out);
        dc.SetDpi(96.0, 96.0);
        dc.BeginDraw();
        dc.SetTransform(&windows_numerics::Matrix3x2::identity());
        dc.Clear(Some(&D2D1_COLOR_F { r: 0.0, g: 0.0, b: 0.0, a: 0.0 }));
        dc.DrawImage(image, None, None, D2D1_INTERPOLATION_MODE_NEAREST_NEIGHBOR, D2D1_COMPOSITE_MODE_SOURCE_COPY);
        let r = dc.EndDraw(None, None);
        dc.SetTarget(prev.as_ref());
        r.map(|_| out)
    }
}

/// A shadow of `src`'s alpha: black at `alpha`, blurred by `deviation` DIPs.
fn shadow(dc: &ID2D1DeviceContext, src: &ID2D1Image, deviation: f32, alpha: f32) -> Result<ID2D1Effect> {
    unsafe {
        let fx = dc.CreateEffect(&CLSID_D2D1Shadow)?;
        fx.SetInput(0, src, true);
        set_f32(&fx, D2D1_SHADOW_PROP_BLUR_STANDARD_DEVIATION.0 as u32, deviation)?;
        let c: [f32; 4] = [0.0, 0.0, 0.0, alpha];
        let bytes: Vec<u8> = c.iter().flat_map(|v| v.to_le_bytes()).collect();
        fx.SetValue(D2D1_SHADOW_PROP_COLOR.0 as u32, D2D1_PROPERTY_TYPE_VECTOR4, &bytes)?;
        Ok(fx)
    }
}

/// An effect's UINT32 property (`set_u32` is for enums).
fn e_u32(e: &ID2D1Effect, index: u32, v: u32) -> Result<()> {
    unsafe { e.SetValue(index, D2D1_PROPERTY_TYPE_UINT32, &v.to_le_bytes()) }
}

fn set_u32(e: &ID2D1Effect, index: u32, v: u32) -> Result<()> {
    unsafe { e.SetValue(index, D2D1_PROPERTY_TYPE_ENUM, &v.to_le_bytes()) }
}

fn set_f32(e: &ID2D1Effect, index: u32, v: f32) -> Result<()> {
    unsafe { e.SetValue(index, D2D1_PROPERTY_TYPE_FLOAT, &v.to_le_bytes()) }
}

fn set_interface<T: Interface>(e: &ID2D1Effect, index: u32, v: &T) -> Result<()> {
    let raw = v.as_raw() as usize;
    unsafe { e.SetValue(index, D2D1_PROPERTY_TYPE_COLOR_CONTEXT, &raw.to_le_bytes()) }
}

/// An 8-bit swap chain for SDR, or a 16-bit float one in scRGB (linear, 1.0 = 80 nits).
fn swap_chain(dev: &Device, width: u32, height: u32, scrgb: bool) -> Result<IDXGISwapChain1> {
    unsafe {
        let dxgi: IDXGIDevice = dev.d3d.cast()?;
        let adapter = dxgi.GetAdapter()?;
        let factory: IDXGIFactory2 = adapter.GetParent()?;
        let desc = DXGI_SWAP_CHAIN_DESC1 {
            Width: width.max(1),
            Height: height.max(1),
            Format: if scrgb { DXGI_FORMAT_R16G16B16A16_FLOAT } else { DXGI_FORMAT_B8G8R8A8_UNORM },
            SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
            BufferUsage: DXGI_USAGE_RENDER_TARGET_OUTPUT,
            BufferCount: 2,
            Scaling: DXGI_SCALING_STRETCH,
            SwapEffect: DXGI_SWAP_EFFECT_FLIP_SEQUENTIAL,
            AlphaMode: DXGI_ALPHA_MODE_IGNORE,
            ..Default::default()
        };
        let swap = factory.CreateSwapChainForComposition(&dev.d3d, &desc, None)?;
        if scrgb {
            swap.cast::<IDXGISwapChain3>()?.SetColorSpace1(DXGI_COLOR_SPACE_RGB_FULL_G10_NONE_P709)?;
        }
        Ok(swap)
    }
}

fn prep(f: &IDWriteTextFormat) -> Result<()> {
    unsafe {
        f.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP)?;
        f.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn half(h: u16) -> f32 {
        let s = if h & 0x8000 != 0 { -1.0 } else { 1.0 };
        let e = ((h >> 10) & 0x1F) as i32;
        let m = (h & 0x3FF) as f32;
        s * if e == 0 { m / 1024.0 * 2f32.powi(-14) } else { (1.0 + m / 1024.0) * 2f32.powi(e - 15) }
    }

    /// Reads a 16-bit float bitmap back as RGBA floats.
    fn read_f16(dc: &ID2D1DeviceContext, bmp: &ID2D1Bitmap1, w: u32, h: u32) -> Vec<[f32; 4]> {
        unsafe {
            let props = D2D1_BITMAP_PROPERTIES1 {
                pixelFormat: D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_R16G16B16A16_FLOAT, alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED },
                dpiX: 96.0,
                dpiY: 96.0,
                bitmapOptions: D2D1_BITMAP_OPTIONS_CPU_READ | D2D1_BITMAP_OPTIONS_CANNOT_DRAW,
                colorContext: std::mem::ManuallyDrop::new(None),
            };
            let cpu = dc.CreateBitmap(D2D_SIZE_U { width: w, height: h }, None, 0, &props).unwrap();
            cpu.CopyFromBitmap(None, bmp, None).unwrap();
            let m = cpu.Map(D2D1_MAP_OPTIONS_READ).unwrap();
            let mut out = Vec::new();
            for y in 0..h as usize {
                let row = std::slice::from_raw_parts(m.bits.add(y * m.pitch as usize) as *const u16, w as usize * 4);
                for p in row.chunks_exact(4) {
                    out.push([half(p[0]), half(p[1]), half(p[2]), half(p[3])]);
                }
            }
            cpu.Unmap().unwrap();
            out
        }
    }

    fn to_half(v: f32) -> u16 {
        let s = if v < 0.0 { 0x8000 } else { 0 };
        let v = v.abs();
        if v < 2f32.powi(-14) {
            return s | (v / 2f32.powi(-24)).round() as u16;
        }
        let e = v.log2().floor() as i32;
        let m = ((v / 2f32.powi(e) - 1.0) * 1024.0).round() as u16;
        s | (((e + 15) as u16) << 10) + m
    }

    /// Under scRGB the UI layer lands on the photo layer as an SDR frame blends it: a hairline's half-covered
    /// pixel on a light page comes out between the two (not darker than the hairline, as at the light themes'
    /// rounded corners until 2026-11-12), an opaque one as the hairline, and where the UI draws nothing the photo
    /// layer is left alone (a wide-gamut, HDR colour too).
    #[test]
    fn ui_blends_as_sdr() {
        let dev = create_device(true).unwrap();
        let dc = &dev.dc;
        let white = 2.25;
        let lin = |c: f32| if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) };
        unsafe {
            dc.SetRenderingControls(&D2D1_RENDERING_CONTROLS { bufferPrecision: D2D1_BUFFER_PRECISION_16BPC_FLOAT, tileSize: D2D_SIZE_U { width: 0, height: 0 } });
            let page = lin(0.9) * white;
            let photo_px: Vec<u16> = [[page, page, page], [page, page, page], [-0.1, 3.0, 0.5]]
                .iter()
                .flat_map(|c| [to_half(c[0]), to_half(c[1]), to_half(c[2]), to_half(1.0)])
                .collect();
            let fprops = D2D1_BITMAP_PROPERTIES1 {
                pixelFormat: D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_R16G16B16A16_FLOAT, alphaMode: D2D1_ALPHA_MODE_IGNORE },
                dpiX: 96.0,
                dpiY: 96.0,
                bitmapOptions: D2D1_BITMAP_OPTIONS_NONE,
                colorContext: std::mem::ManuallyDrop::new(None),
            };
            let photo = dc.CreateBitmap(D2D_SIZE_U { width: 3, height: 1 }, Some(photo_px.as_ptr() as _), 24, &fprops).unwrap();
            let props = D2D1_BITMAP_PROPERTIES1 {
                pixelFormat: D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED },
                ..fprops
            };
            // BGRA premultiplied: 0.8 grey at half coverage, the same grey whole, nothing.
            let ui_px = [102u8, 102, 102, 128, 204, 204, 204, 255, 0, 0, 0, 0];
            let ui = dc.CreateBitmap(D2D_SIZE_U { width: 3, height: 1 }, Some(ui_px.as_ptr() as _), 12, &props).unwrap();
            let fx = ui_over(dc, &photo, &ui, white).unwrap();
            let out = read_f16(dc, &bake(dc, &fx.GetOutput().unwrap(), 3, 1).unwrap(), 3, 1);
            println!("{out:?}");
            let half = lin(102.0 / 255.0 + (1.0 - 128.0 / 255.0) * 0.9) * white;
            assert!((out[0][1] - half).abs() < 0.01 && (out[0][3] - 1.0).abs() < 0.01, "half covered {:?}, want {half}", out[0]);
            // Never darker than the hairline itself.
            assert!(out[0][1] > lin(0.8) * white);
            assert!((out[1][1] - lin(0.8) * white).abs() < 0.01, "whole {:?}", out[1]);
            assert!(out[2][3].abs() < 0.001, "untouched {:?}", out[2]);
        }
    }

    /// What the colour-management effect's alpha modes do with a translucent pixel (6 % white, premultiplied).
    #[test]
    #[ignore]
    fn alpha_modes() {
        let dev = create_device(true).unwrap();
        let dc = &dev.dc;
        unsafe {
            dc.SetRenderingControls(&D2D1_RENDERING_CONTROLS { bufferPrecision: D2D1_BUFFER_PRECISION_16BPC_FLOAT, tileSize: D2D_SIZE_U { width: 0, height: 0 } });
            let props = D2D1_BITMAP_PROPERTIES1 {
                pixelFormat: D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED },
                dpiX: 96.0,
                dpiY: 96.0,
                bitmapOptions: D2D1_BITMAP_OPTIONS_NONE,
                colorContext: std::mem::ManuallyDrop::new(None),
            };
            let px = [15u8, 15, 15, 15, 128, 128, 128, 255];
            let src = dc.CreateBitmap(D2D_SIZE_U { width: 2, height: 1 }, Some(px.as_ptr() as _), 8, &props).unwrap();
            for mode in [D2D1_COLORMANAGEMENT_ALPHA_MODE_PREMULTIPLIED, D2D1_COLORMANAGEMENT_ALPHA_MODE_STRAIGHT] {
                let cm = dc.CreateEffect(&CLSID_D2D1ColorManagement).unwrap();
                cm.SetInput(0, &src, true);
                set_interface(&cm, D2D1_COLORMANAGEMENT_PROP_SOURCE_COLOR_CONTEXT.0 as u32, &dc.CreateColorContext(D2D1_COLOR_SPACE_SRGB, None).unwrap()).unwrap();
                set_interface(&cm, D2D1_COLORMANAGEMENT_PROP_DESTINATION_COLOR_CONTEXT.0 as u32, &dc.CreateColorContext(D2D1_COLOR_SPACE_SCRGB, None).unwrap()).unwrap();
                set_u32(&cm, D2D1_COLORMANAGEMENT_PROP_ALPHA_MODE.0 as u32, mode.0 as u32).unwrap();
                let out = bake(dc, &cm.GetOutput().unwrap(), 2, 1).unwrap();
                println!("mode {}: {:?}", mode.0, read_f16(dc, &out, 2, 1));
            }
        }
    }

    /// 100 % from half a pixel off lands on whole pixels, exactly one per image pixel; a scale a hair under 1 is 1.
    #[test]
    fn hard_pixels_line_up() {
        let r = pixel_rect_at(D2D_RECT_F { left: 194.5, top: 192.25, right: 1194.5, bottom: 754.25 }, 1000, 562, 1.0).unwrap();
        assert_eq!((r.left, r.top, r.right - r.left, r.bottom - r.top), (195.0, 192.0, 1000.0, 562.0));
        let r = pixel_rect_at(D2D_RECT_F { left: 10.3, top: 0.0, right: 10.3 + 666.6, bottom: 374.0 }, 1000, 562, 1.5).unwrap();
        assert!((r.left * 1.5 - (r.left * 1.5).round()).abs() < 1e-3);
        assert!(((r.right - r.left) * 1.5 - 1000.0).abs() < 1e-2);
        assert!(pixel_rect_at(D2D_RECT_F { left: 0.0, top: 0.0, right: 500.0, bottom: 281.0 }, 1000, 562, 1.0).is_none());
    }

    /// Mid-way through a zoom to 100 % (99.05 % of a 6000-pixel photo) the drawn box is the view's own, not
    /// snapped to 100 % out of its corner (which jolted the picture for a few frames).
    #[test]
    fn hard_pixels_follow_a_zoom_near_100() {
        let dest = D2D_RECT_F { left: -2105.0, top: -1599.0, right: -2105.0 + 5942.76, bottom: -1599.0 + 3961.84 };
        let r = pixel_rect_at(dest, 6000, 4000, 1.0).unwrap();
        assert!((r.right - dest.right).abs() < 0.6 && (r.bottom - dest.bottom).abs() < 0.6, "{r:?}");
        // A hair under 100 % still lands on exactly 6000 pixels.
        let r = pixel_rect_at(D2D_RECT_F { right: -2105.0 + 5999.95, ..dest }, 6000, 4000, 1.0).unwrap();
        assert_eq!(r.right - r.left, 6000.0);
    }

    /// A card's shadow, fill and hairline on a light page, at 2x, written to `%TEMP%\looker-card-corner.png` to
    /// look at the corners. `cargo test --release card_corner -- --ignored`
    #[test]
    #[ignore]
    fn card_corner() {
        let dev = create_device(true).unwrap();
        let dc = &dev.dc;
        let (w, h) = (240u32, 160u32);
        unsafe {
            let props = D2D1_BITMAP_PROPERTIES1 {
                pixelFormat: D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED },
                dpiX: 192.0,
                dpiY: 192.0,
                bitmapOptions: D2D1_BITMAP_OPTIONS_TARGET,
                colorContext: std::mem::ManuallyDrop::new(None),
            };
            let target = dc.CreateBitmap(D2D_SIZE_U { width: w, height: h }, None, 0, &props).unwrap();
            dc.SetTarget(&target);
            dc.SetDpi(192.0, 192.0);
            dc.BeginDraw();
            dc.Clear(Some(&rgb(0xEDE7DC)));
            let brush = dc.CreateSolidColorBrush(&rgb(0), None).unwrap();
            let r = D2D_RECT_F { left: 20.0, top: 20.0, right: 100.0, bottom: 60.0 };
            // `soft_shadow`'s nine pieces, at 2x.
            let (px, n, pad) = shadow_square(20.0, 2.6 * 2.0, 0.30).unwrap();
            let bprops = D2D1_BITMAP_PROPERTIES1 {
                pixelFormat: props.pixelFormat,
                dpiX: 96.0,
                dpiY: 96.0,
                bitmapOptions: D2D1_BITMAP_OPTIONS_NONE,
                colorContext: std::mem::ManuallyDrop::new(None),
            };
            let bmp = dc.CreateBitmap(D2D_SIZE_U { width: n, height: n }, Some(px.as_ptr() as _), n * 4, &bprops).unwrap();
            let (nf, c, grow) = (n as f32, (n as f32 - 2.0) / 2.0, pad as f32 / 2.0);
            let out = D2D_RECT_F { left: r.left - grow, top: r.top - grow + 2.0, right: r.right + grow, bottom: r.bottom + grow + 2.0 };
            let cd = c / 2.0;
            let (xs, ys) = ([out.left, out.left + cd, out.right - cd, out.right], [out.top, out.top + cd, out.bottom - cd, out.bottom]);
            let src = [0.0, c, c + 2.0, nf];
            for row in 0..3 {
                for col in 0..3 {
                    let d = D2D_RECT_F { left: xs[col], top: ys[row], right: xs[col + 1], bottom: ys[row + 1] };
                    let sr = D2D_RECT_F { left: src[col], top: src[row], right: src[col + 1], bottom: src[row + 1] };
                    dc.DrawBitmap(&bmp, Some(&d), 1.0, D2D1_INTERPOLATION_MODE_LINEAR, Some(&sr), None);
                }
            }
            brush.SetColor(&rgb(0xF6F1E8));
            dc.FillRoundedRectangle(&D2D1_ROUNDED_RECT { rect: r, radiusX: 10.0, radiusY: 10.0 }, &brush);
            brush.SetColor(&rgb(0xD9D1C3));
            let hp = 0.25;
            let s = D2D_RECT_F { left: r.left + hp, top: r.top + hp, right: r.right - hp, bottom: r.bottom - hp };
            dc.DrawRoundedRectangle(&D2D1_ROUNDED_RECT { rect: s, radiusX: 10.0, radiusY: 10.0 }, &brush, 0.5, None);
            dc.EndDraw(None, None).unwrap();
            let cpu_props = D2D1_BITMAP_PROPERTIES1 { bitmapOptions: D2D1_BITMAP_OPTIONS_CPU_READ | D2D1_BITMAP_OPTIONS_CANNOT_DRAW, ..props };
            let cpu = dc.CreateBitmap(D2D_SIZE_U { width: w, height: h }, None, 0, &cpu_props).unwrap();
            cpu.CopyFromBitmap(None, &target, None).unwrap();
            let m = cpu.Map(D2D1_MAP_OPTIONS_READ).unwrap();
            let mut px = Vec::new();
            for y in 0..h as usize {
                let row = std::slice::from_raw_parts(m.bits.add(y * m.pitch as usize), w as usize * 4);
                px.extend(row.chunks_exact(4).flat_map(|c| [c[2], c[1], c[0], 255]));
            }
            cpu.Unmap().unwrap();
            let out = std::env::temp_dir().join("looker-card-corner.png");
            image::save_buffer(&out, &px, w, h, image::ExtendedColorType::Rgba8).unwrap();
            println!("{}", out.display());
        }
    }

    /// sRGB white lands at the white scale; a Display P3 red lands outside sRGB (red above white, negative
    /// green and blue), and two P3 reds that sRGB would clip to one stay apart.
    /// `LOOKER_P3_ICC=<a Display P3 profile> cargo test --release photos_reach_scrgb -- --nocapture`
    #[test]
    fn photos_reach_scrgb() {
        let dev = create_device(true).unwrap();
        let dc = &dev.dc;
        unsafe {
            dc.SetRenderingControls(&D2D1_RENDERING_CONTROLS { bufferPrecision: D2D1_BUFFER_PRECISION_16BPC_FLOAT, tileSize: D2D_SIZE_U { width: 0, height: 0 } });
        }
        let convert = |px: &[u8], colour: &crate::imaging::Colour| unsafe {
            let props = D2D1_BITMAP_PROPERTIES1 {
                pixelFormat: D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED },
                dpiX: 96.0,
                dpiY: 96.0,
                bitmapOptions: D2D1_BITMAP_OPTIONS_NONE,
                colorContext: std::mem::ManuallyDrop::new(None),
            };
            let src = dc.CreateBitmap(D2D_SIZE_U { width: 2, height: 1 }, Some(px.as_ptr() as _), 8, &props).unwrap();
            let fx = to_scrgb(dc, &src.cast().unwrap(), &colour_context(dc, colour).unwrap(), 2.0, true).unwrap();
            let out = bake(dc, &fx.GetOutput().unwrap(), 2, 1).unwrap();
            read_f16(dc, &out, 2, 1)
        };
        // BGRA: white, then mid grey.
        let s = convert(&[255, 255, 255, 255, 128, 128, 128, 255], &crate::imaging::Colour::Srgb);
        println!("sRGB white {:?}, grey {:?}", s[0], s[1]);
        assert!((s[0][0] - 2.0).abs() < 0.01 && (s[0][3] - 1.0).abs() < 0.01);
        assert!((s[1][0] - 2.0 * 0.2158).abs() < 0.01);
        let Some(p3) = std::env::var("LOOKER_P3_ICC").ok().and_then(|p| std::fs::read(p).ok()) else {
            println!("LOOKER_P3_ICC not set: skipped the wide-gamut half");
            return;
        };
        let p = convert(&[0, 0, 255, 255, 0, 0, 240, 255], &crate::imaging::Colour::Icc(std::sync::Arc::new(p3)));
        println!("P3 red {:?}, darker P3 red {:?}", p[0], p[1]);
        assert!(p[0][0] > 2.0 && p[0][1] < 0.0 && p[0][2] < 0.0);
        assert!(p[0][0] - p[1][0] > 0.1);
    }
}
