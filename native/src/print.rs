//! Printing through Windows 11's modern print dialog (the one Photos and Notepad open: printer, copies, paper,
//! orientation, colour, page range and a live preview): the window's `PrintManager`, given a Direct2D document
//! source of Looker's own, as Microsoft's Direct2D printing sample does. The dialog asks the source for preview
//! pages (drawn into DXGI surfaces) and, on Print, for the document (drawn into command lists that a
//! `ID2D1PrintControl` turns into the print job).
//!
//! Photos print one to a page, fitted to the printable area or filling it; PDF pages print as vectors through
//! the inbox renderer (`IPdfRendererNative`), fitted or at actual size. Either turns a quarter when its shape
//! and the paper's disagree (as the old Print Pictures wizard and PDF readers do), so a landscape photo fills
//! portrait paper.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use windows::Data::Pdf::PdfDocument;
use windows::Foundation::{IReference, TypedEventHandler};
use windows_future::IAsyncOperation;
use windows::Graphics::Printing::OptionDetails::{PrintTaskOptionChangedEventArgs, PrintTaskOptionDetails};
use windows::Graphics::Printing::{
    IPrintDocumentSource, IPrintDocumentSource_Impl, PrintManager, PrintPageDescription, PrintTaskOptions, PrintTaskRequestedEventArgs,
    PrintTaskSourceRequestedArgs, PrintTaskSourceRequestedHandler, StandardPrintTaskOptions,
};
use windows::Win32::Foundation::{HMODULE, HWND};
use windows::Win32::Graphics::Direct2D::Common::{D2D_COLOR_F, D2D_RECT_F, D2D_SIZE_F, D2D_SIZE_U, D2D1_ALPHA_MODE_PREMULTIPLIED, D2D1_COLOR_F, D2D1_PIXEL_FORMAT};
use windows::Win32::Graphics::Direct2D::{
    D2D1_ANTIALIAS_MODE_ALIASED, D2D1_BITMAP_OPTIONS_CANNOT_DRAW, D2D1_BITMAP_OPTIONS_NONE, D2D1_BITMAP_OPTIONS_TARGET, D2D1_BITMAP_PROPERTIES1,
    D2D1_COLOR_SPACE_SRGB, D2D1_DEVICE_CONTEXT_OPTIONS_NONE, D2D1_FACTORY_TYPE_MULTI_THREADED, D2D1_INTERPOLATION_MODE_HIGH_QUALITY_CUBIC,
    D2D1_PRINT_CONTROL_PROPERTIES, D2D1_PRINT_FONT_SUBSET_MODE_DEFAULT, D2D1CreateFactory, ID2D1Bitmap1, ID2D1DeviceContext, ID2D1Factory1,
};
use windows::Win32::Graphics::Direct3D::{D3D_DRIVER_TYPE, D3D_DRIVER_TYPE_HARDWARE, D3D_DRIVER_TYPE_WARP};
use windows::Win32::Graphics::Direct3D11::{
    D3D11_BIND_RENDER_TARGET, D3D11_BIND_SHADER_RESOURCE, D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_SDK_VERSION, D3D11_TEXTURE2D_DESC,
    D3D11_USAGE_DEFAULT, D3D11CreateDevice, ID3D11Device, ID3D11Multithread,
};
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC};
use windows::Win32::Graphics::Dxgi::{IDXGIDevice, IDXGISurface};
use windows::Win32::Graphics::Imaging::{CLSID_WICImagingFactory2, IWICImagingFactory};
use windows::Win32::Graphics::Printing::{FinalPageCount, IPrintPreviewDxgiPackageTarget};
use windows::Win32::Storage::Xps::Printing::IPrintDocumentPackageTarget;
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance};
use windows::Win32::System::WinRT::Pdf::{IPdfRendererNative, PDF_RENDER_PARAMS, PdfCreateRenderer};
use windows::Win32::System::WinRT::Printing::{
    IPrintDocumentPageSource, IPrintDocumentPageSource_Impl, IPrintManagerInterop, IPrintPreviewPageCollection, IPrintPreviewPageCollection_Impl,
};
use windows::core::{GUID, HSTRING, IInspectable, IUnknown, IUnknownImpl, Interface, Ref, Result, implement};
use windows_numerics::Matrix3x2;

use crate::format::{self, Format};

/// The preview target's type (documenttarget.h `ID_PREVIEWPACKAGETARGET_DXGI`).
const ID_PREVIEWPACKAGETARGET_DXGI: GUID = GUID::from_u128(0x1a6dd0ad_1e2a_4e99_a5ba_91f17818290e);
/// `JOB_PAGE_APPLICATION_DEFINED`: MakePage's "whichever page you like".
const JOB_PAGE_APPLICATION_DEFINED: u32 = u32::MAX;
/// The dialog's own option for how a page fills the paper.
const SCALE_OPTION: &str = "LookerScale";

/// What Print takes: photos (anything Looker shows as a picture) and PDFs.
pub fn printable(path: &std::path::Path) -> bool {
    let f = format::of_path(path);
    f == Format::Pdf || format::is_photo(f)
}

/// What's being printed now: set before the dialog opens, read when it asks for a task (`PrintTaskRequested`).
static JOB: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());
/// The window whose `PrintTaskRequested` is hooked (once per window).
static HOOKED: Mutex<isize> = Mutex::new(0);

/// Windows' print dialog for these files (photos and PDFs, a page a photo).
pub fn print(hwnd: HWND, paths: Vec<PathBuf>) -> std::result::Result<(), String> {
    if paths.is_empty() {
        return Ok(());
    }
    *JOB.lock().unwrap() = paths;
    let err = |e: windows::core::Error| format!("Couldn't open Print: {}", e.message());
    let interop = windows::core::factory::<PrintManager, IPrintManagerInterop>().map_err(err)?;
    let mut hooked = HOOKED.lock().unwrap();
    if *hooked != hwnd.0 as isize {
        let manager: PrintManager = unsafe { interop.GetForWindow(hwnd) }.map_err(err)?;
        manager
            .PrintTaskRequested(&TypedEventHandler::<PrintManager, PrintTaskRequestedEventArgs>::new(|_, args| {
                if let Some(args) = args.as_ref() {
                    if let Err(e) = task_requested(args) {
                        crate::trace::mark(format!("print: task failed: {}", e.message()));
                    }
                }
                Ok(())
            }))
            .map_err(err)?;
        *hooked = hwnd.0 as isize;
    }
    drop(hooked);
    let _shown: IAsyncOperation<bool> = unsafe { interop.ShowPrintUIForWindowAsync(hwnd) }.map_err(err)?;
    Ok(())
}

/// COM objects handed to a delegate that may run on another thread; each is free-threaded or agile.
struct Sendable<T>(T);
unsafe impl<T> Send for Sendable<T> {}
unsafe impl<T> Sync for Sendable<T> {}

impl<T> Sendable<T> {
    /// The object (a method, so a closure takes the whole `Sendable`, not just its field).
    fn get(&self) -> &T {
        &self.0
    }
}

/// The dialog asked for a task: the document source, the options it shows, and Looker's Scale option.
fn task_requested(args: &PrintTaskRequestedEventArgs) -> Result<()> {
    let paths = JOB.lock().unwrap().clone();
    let pdfs = paths.iter().filter(|p| format::of_path(p) == Format::Pdf).count();
    let photos = paths.len() - pdfs;
    let title = match paths.as_slice() {
        [one] => one.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
        many => format!("{} files", many.len()),
    };
    let state = Arc::new(State { inner: Mutex::new(Inner::new(paths)), preview: Mutex::new(None) });
    let source: IPrintDocumentSource = Source { state: state.clone() }.into();
    let source = Sendable(source);
    let request = args.Request()?;
    let task = request.CreatePrintTask(
        &HSTRING::from(title),
        &PrintTaskSourceRequestedHandler::new(move |a: Ref<PrintTaskSourceRequestedArgs>| {
            if let Some(a) = a.as_ref() {
                a.SetSource(source.get())?;
            }
            Ok(())
        }),
    )?;
    let options = task.Options()?;
    let shown = options.DisplayedOptions()?;
    shown.Clear()?;
    for o in [StandardPrintTaskOptions::Copies()?, StandardPrintTaskOptions::Orientation()?, StandardPrintTaskOptions::MediaSize()?, StandardPrintTaskOptions::ColorMode()?] {
        shown.Append(&o)?;
    }
    if pdfs > 0 {
        shown.Append(&StandardPrintTaskOptions::Duplex()?)?;
    }
    // Pages: all, or a set typed in ("1-3, 5"), for a document or several photos.
    if pdfs > 0 || photos > 1 {
        let ranges = options.PageRangeOptions()?;
        ranges.SetAllowAllPages(true)?;
        ranges.SetAllowCustomSetOfPages(true)?;
        shown.Append(&StandardPrintTaskOptions::CustomPageRanges()?)?;
    }
    let details = PrintTaskOptionDetails::GetFromPrintTaskOptions(&options)?;
    let scale = details.CreateItemListOption(&HSTRING::from(SCALE_OPTION), &HSTRING::from(if pdfs > 0 && photos == 0 { "Scale" } else { "Photo size" }))?;
    scale.AddItem(&HSTRING::from("fit"), &HSTRING::from("Fit to page"))?;
    if photos > 0 {
        scale.AddItem(&HSTRING::from("fill"), &HSTRING::from("Fill page"))?;
    }
    if pdfs > 0 {
        scale.AddItem(&HSTRING::from("actual"), &HSTRING::from("Actual size"))?;
    }
    shown.Append(&HSTRING::from(SCALE_OPTION))?;
    // Any change (Scale included, which the dialog doesn't know touches the pages): draw the preview again.
    let preview = state.clone();
    details.OptionChanged(&TypedEventHandler::<PrintTaskOptionDetails, PrintTaskOptionChangedEventArgs>::new(move |_, _| {
        if let Some(p) = preview.preview.lock().unwrap().as_ref() {
            unsafe {
                let _ = p.get().InvalidatePreview();
            }
        }
        Ok(())
    }))?;
    Ok(())
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Scale {
    Fit,
    Fill,
    Actual,
}

fn read_scale(options: &PrintTaskOptions) -> Scale {
    let value = PrintTaskOptionDetails::GetFromPrintTaskOptions(options)
        .and_then(|d| d.Options())
        .and_then(|m| m.Lookup(&HSTRING::from(SCALE_OPTION)))
        .and_then(|o| o.Value())
        .and_then(|v| v.cast::<IReference<HSTRING>>())
        .and_then(|r| r.Value());
    match value.map(|s| s.to_string()).as_deref() {
        Ok("fill") => Scale::Fill,
        Ok("actual") => Scale::Actual,
        _ => Scale::Fit,
    }
}

/// A page: which file, and which page of it (0 for a photo).
type Page = (usize, u32);

struct State {
    inner: Mutex<Inner>,
    /// Kept apart from `inner` so an option change can redraw the preview while a page is being drawn.
    preview: Mutex<Option<Sendable<IPrintPreviewDxgiPackageTarget>>>,
}

struct Inner {
    paths: Vec<PathBuf>,
    gpu: Option<Gpu>,
    /// Every page, once counted (PDFs opened).
    pages: Option<Vec<Page>>,
    pdfs: Vec<Option<PdfDocument>>,
    /// The last few photos as bitmaps (file, bitmap, size), newest last: a 24 MP photo is 96 MB.
    photos: Vec<(usize, ID2D1Bitmap1, (f32, f32))>,
    desc: PrintPageDescription,
    scale: Scale,
}

// Used under the mutex by one thread at a time; Direct2D's factory is multithreaded, the PDF documents agile.
unsafe impl Send for Inner {}

struct Gpu {
    d3d: ID3D11Device,
    dc: ID2D1DeviceContext,
    d2d: windows::Win32::Graphics::Direct2D::ID2D1Device,
    wic: IWICImagingFactory,
    pdf: Option<IPdfRendererNative>,
}

impl Inner {
    fn new(paths: Vec<PathBuf>) -> Inner {
        let n = paths.len();
        Inner { paths, gpu: None, pages: None, pdfs: (0..n).map(|_| None).collect(), photos: Vec::new(), desc: PrintPageDescription::default(), scale: Scale::Fit }
    }

    fn gpu(&mut self) -> Result<&Gpu> {
        if self.gpu.is_none() {
            self.gpu = Some(make_gpu()?);
        }
        Ok(self.gpu.as_ref().unwrap())
    }

    fn pages(&mut self) -> &[Page] {
        if self.pages.is_none() {
            let mut pages = Vec::new();
            for i in 0..self.paths.len() {
                if format::sniff_file(&self.paths[i]) == Format::Pdf {
                    match crate::imaging::pdf::open(&self.paths[i]) {
                        Ok(doc) => {
                            let n = doc.PageCount().unwrap_or(0);
                            pages.extend((0..n).map(|p| (i, p)));
                            self.pdfs[i] = Some(doc);
                        }
                        Err(e) => crate::trace::mark(format!("print: pdf failed: {e}")),
                    }
                } else {
                    pages.push((i, 0));
                }
            }
            self.pages = Some(pages);
        }
        self.pages.as_deref().unwrap()
    }

    /// The photo as a bitmap on the print device, decoded full size the first time.
    fn photo(&mut self, file: usize) -> Option<(ID2D1Bitmap1, (f32, f32))> {
        if let Some((_, b, s)) = self.photos.iter().find(|(f, _, _)| *f == file) {
            return Some((b.clone(), *s));
        }
        let img = crate::fileops::full_image(&self.paths[file], 0)?;
        let dc = self.gpu().ok()?.dc.clone();
        let props = D2D1_BITMAP_PROPERTIES1 {
            pixelFormat: D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED },
            dpiX: 96.0,
            dpiY: 96.0,
            bitmapOptions: D2D1_BITMAP_OPTIONS_NONE,
            colorContext: std::mem::ManuallyDrop::new(None),
        };
        let bmp = unsafe { dc.CreateBitmap(D2D_SIZE_U { width: img.width, height: img.height }, Some(img.pixels.as_ptr() as _), img.width * 4, &props) }
            .map_err(|e| crate::trace::mark(format!("print: bitmap failed: {}", e.message())))
            .ok()?;
        let size = (img.width as f32, img.height as f32);
        if self.photos.len() >= 3 {
            self.photos.remove(0);
        }
        self.photos.push((file, bmp.clone(), size));
        Some((bmp, size))
    }

    /// Draws page `n` (0-based) in page coordinates (DIPs from the paper's top left) under `base`.
    fn draw(&mut self, n: usize, base: Matrix3x2) {
        let Some(&(file, page)) = self.pages().get(n) else { return };
        let Ok(gpu) = self.gpu() else { return };
        let dc = gpu.dc.clone();
        let r = self.desc.ImageableRect;
        let area = if r.Width > 0.0 && r.Height > 0.0 { (r.X, r.Y, r.Width, r.Height) } else { (0.0, 0.0, self.desc.PageSize.Width, self.desc.PageSize.Height) };
        enum Content {
            Photo(ID2D1Bitmap1),
            Pdf(windows::Data::Pdf::PdfPage),
        }
        let (content, (cw, ch)) = match self.pdfs.get(file).cloned().flatten() {
            Some(doc) => {
                let Ok(p) = doc.GetPage(page) else { return };
                let s = p.Size().unwrap_or_default();
                (Content::Pdf(p), (s.Width, s.Height))
            }
            None => match self.photo(file) {
                Some((b, s)) => (Content::Photo(b), s),
                None => return,
            },
        };
        if cw <= 0.0 || ch <= 0.0 {
            return;
        }
        let photo = matches!(content, Content::Photo(_));
        let scale = match self.scale {
            Scale::Fill if !photo => Scale::Fit,
            Scale::Actual if photo => Scale::Fit,
            s => s,
        };
        let content_transform = place((cw, ch), area, scale);
        unsafe {
            if scale == Scale::Fill {
                dc.SetTransform(&base);
                dc.PushAxisAlignedClip(&D2D_RECT_F { left: area.0, top: area.1, right: area.0 + area.2, bottom: area.1 + area.3 }, D2D1_ANTIALIAS_MODE_ALIASED);
            }
            dc.SetTransform(&(content_transform * base));
            match &content {
                Content::Photo(b) => {
                    dc.DrawBitmap(b, Some(&D2D_RECT_F { left: 0.0, top: 0.0, right: cw, bottom: ch }), 1.0, D2D1_INTERPOLATION_MODE_HIGH_QUALITY_CUBIC, None, None);
                }
                Content::Pdf(p) => {
                    if let (Some(pdf), Ok(unk)) = (self.gpu.as_ref().and_then(|g| g.pdf.clone()), p.cast::<IUnknown>()) {
                        let params = PDF_RENDER_PARAMS {
                            SourceRect: D2D_RECT_F::default(),
                            DestinationWidth: cw.round().max(1.0) as u32,
                            DestinationHeight: ch.round().max(1.0) as u32,
                            BackgroundColor: D2D_COLOR_F { r: 1.0, g: 1.0, b: 1.0, a: 1.0 },
                            IgnoreHighContrast: true,
                        };
                        if let Err(e) = pdf.RenderPageToDeviceContext(&unk, &dc, Some(&params)) {
                            crate::trace::mark(format!("print: pdf page failed: {}", e.message()));
                        }
                    }
                }
            }
            if scale == Scale::Fill {
                dc.SetTransform(&base);
                dc.PopAxisAlignedClip();
            }
            dc.SetTransform(&Matrix3x2::identity());
        }
    }
}

/// Where content `cw` x `ch` goes in `area` (x, y, w, h): turned a quarter when its shape and the area's
/// disagree, then fitted (whole, centred), filled (cropped, centred) or at actual size (centred).
fn place((cw, ch): (f32, f32), area: (f32, f32, f32, f32), scale: Scale) -> Matrix3x2 {
    let (ax, ay, aw, ah) = area;
    let turn = (cw > ch) != (aw > ah) && (cw - ch).abs() > 0.5 && (aw - ah).abs() > 0.5;
    let (ew, eh) = if turn { (ch, cw) } else { (cw, ch) };
    let k = match scale {
        Scale::Fit => (aw / ew).min(ah / eh),
        Scale::Fill => (aw / ew).max(ah / eh),
        Scale::Actual => 1.0,
    };
    // The content's middle to the origin, scaled, turned (90 degrees clockwise: (x, y) -> (-y, x)), then to the
    // area's middle.
    let mut m = Matrix3x2::translation(-cw / 2.0, -ch / 2.0) * Matrix3x2::scale(k, k);
    if turn {
        m = m * Matrix3x2 { M11: 0.0, M12: 1.0, M21: -1.0, M22: 0.0, M31: 0.0, M32: 0.0 };
    }
    m * Matrix3x2::translation(ax + aw / 2.0, ay + ah / 2.0)
}

fn make_gpu() -> Result<Gpu> {
    unsafe {
        let create = |kind: D3D_DRIVER_TYPE| {
            let mut d3d = None;
            D3D11CreateDevice(None, kind, HMODULE::default(), D3D11_CREATE_DEVICE_BGRA_SUPPORT, None, D3D11_SDK_VERSION, Some(&mut d3d), None, None).map(|_| d3d.unwrap())
        };
        let d3d: ID3D11Device = create(D3D_DRIVER_TYPE_HARDWARE).or_else(|_| create(D3D_DRIVER_TYPE_WARP))?;
        if let Ok(mt) = d3d.GetImmediateContext().and_then(|c| c.cast::<ID3D11Multithread>()) {
            let _ = mt.SetMultithreadProtected(true);
        }
        let dxgi: IDXGIDevice = d3d.cast()?;
        let factory: ID2D1Factory1 = D2D1CreateFactory(D2D1_FACTORY_TYPE_MULTI_THREADED, None)?;
        let d2d = factory.CreateDevice(&dxgi)?;
        let dc = d2d.CreateDeviceContext(D2D1_DEVICE_CONTEXT_OPTIONS_NONE)?;
        let wic: IWICImagingFactory = CoCreateInstance(&CLSID_WICImagingFactory2, None, CLSCTX_INPROC_SERVER)?;
        let pdf = PdfCreateRenderer(&dxgi).ok();
        Ok(Gpu { d3d, dc, d2d, wic, pdf })
    }
}

#[implement(IPrintDocumentSource, IPrintDocumentPageSource, IPrintPreviewPageCollection)]
struct Source {
    state: Arc<State>,
}

impl IPrintDocumentSource_Impl for Source_Impl {}

impl IPrintDocumentPageSource_Impl for Source_Impl {
    fn GetPreviewPageCollection(&self, target: Ref<IPrintDocumentPackageTarget>) -> Result<IPrintPreviewPageCollection> {
        let preview: IPrintPreviewDxgiPackageTarget = unsafe { target.ok()?.GetPackageTarget(&ID_PREVIEWPACKAGETARGET_DXGI) }?;
        *self.state.preview.lock().unwrap() = Some(Sendable(preview));
        Ok(self.to_interface())
    }

    /// Print: each page picked drawn into a command list, handed to a print control for the job.
    fn MakeDocument(&self, options: Ref<IInspectable>, target: Ref<IPrintDocumentPackageTarget>) -> Result<()> {
        let options: PrintTaskOptions = options.ok()?.cast()?;
        let target = target.ok()?;
        let mut inner = self.state.inner.lock().unwrap();
        inner.desc = options.GetPageDescription(1)?;
        inner.scale = read_scale(&options);
        let total = inner.pages().len() as u32;
        // A set typed in ("1-3, 5"): those pages, in that order; otherwise every page.
        let mut picked: Vec<u32> = Vec::new();
        if let Ok(ranges) = options.CustomPageRanges() {
            for r in ranges {
                let (a, b) = (r.FirstPageNumber().unwrap_or(1).max(1) as u32, r.LastPageNumber().unwrap_or(0).max(0) as u32);
                picked.extend((a..=b.max(a)).filter(|&p| p <= total));
            }
        }
        if picked.is_empty() {
            picked = (1..=total).collect();
        }
        inner.write(target, &picked)
    }
}

impl Inner {
    /// The print job: pages `picked` (1-based) each drawn into a command list, handed to a print control.
    fn write(&mut self, target: &IPrintDocumentPackageTarget, picked: &[u32]) -> Result<()> {
        let gpu = self.gpu()?;
        let (d2d, dc, wic) = (gpu.d2d.clone(), gpu.dc.clone(), gpu.wic.clone());
        let props = D2D1_PRINT_CONTROL_PROPERTIES { fontSubset: D2D1_PRINT_FONT_SUBSET_MODE_DEFAULT, rasterDPI: 300.0, colorSpace: D2D1_COLOR_SPACE_SRGB };
        let control = unsafe { d2d.CreatePrintControl(&wic, target, Some(&props)) }?;
        let page_size = D2D_SIZE_F { width: self.desc.PageSize.Width, height: self.desc.PageSize.Height };
        for &p in picked {
            unsafe {
                let list = dc.CreateCommandList()?;
                dc.SetTarget(&list);
                dc.BeginDraw();
                self.draw(p as usize - 1, Matrix3x2::identity());
                dc.EndDraw(None, None)?;
                list.Close()?;
                control.AddPage(&list, page_size, None, None, None)?;
            }
        }
        unsafe {
            dc.SetTarget(None);
            control.Close()
        }
    }
}

impl IPrintPreviewPageCollection_Impl for Source_Impl {
    fn Paginate(&self, _current: u32, options: Ref<IInspectable>) -> Result<()> {
        let options: PrintTaskOptions = options.ok()?.cast()?;
        let count = {
            let mut inner = self.state.inner.lock().unwrap();
            inner.desc = options.GetPageDescription(1)?;
            inner.scale = read_scale(&options);
            inner.pages().len() as u32
        };
        if let Some(p) = self.state.preview.lock().unwrap().as_ref() {
            unsafe { p.0.SetJobPageCount(FinalPageCount, count.max(1)) }?;
        }
        Ok(())
    }

    /// A preview page `width` x `height` DIPs, drawn into a surface of that many pixels.
    fn MakePage(&self, page: u32, width: f32, height: f32) -> Result<()> {
        let page = if page == JOB_PAGE_APPLICATION_DEFINED { 1 } else { page.max(1) };
        let preview = match self.state.preview.lock().unwrap().as_ref() {
            Some(p) => p.0.clone(),
            None => return Ok(()),
        };
        let mut inner = self.state.inner.lock().unwrap();
        let (pw, ph) = (inner.desc.PageSize.Width, inner.desc.PageSize.Height);
        if pw <= 0.0 || ph <= 0.0 {
            return Ok(());
        }
        let (w, h) = if width > 0.0 && height > 0.0 { (width, height) } else { (pw, ph) };
        let gpu = inner.gpu()?;
        let (d3d, dc) = (gpu.d3d.clone(), gpu.dc.clone());
        unsafe {
            let desc = D3D11_TEXTURE2D_DESC {
                Width: w.ceil() as u32,
                Height: h.ceil() as u32,
                MipLevels: 1,
                ArraySize: 1,
                Format: DXGI_FORMAT_B8G8R8A8_UNORM,
                SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
                Usage: D3D11_USAGE_DEFAULT,
                BindFlags: (D3D11_BIND_RENDER_TARGET.0 | D3D11_BIND_SHADER_RESOURCE.0) as u32,
                ..Default::default()
            };
            let mut tex = None;
            d3d.CreateTexture2D(&desc, None, Some(&mut tex))?;
            let surface: IDXGISurface = tex.unwrap().cast()?;
            let props = D2D1_BITMAP_PROPERTIES1 {
                pixelFormat: D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED },
                dpiX: 96.0,
                dpiY: 96.0,
                bitmapOptions: D2D1_BITMAP_OPTIONS_TARGET | D2D1_BITMAP_OPTIONS_CANNOT_DRAW,
                colorContext: std::mem::ManuallyDrop::new(None),
            };
            let target = dc.CreateBitmapFromDxgiSurface(&surface, Some(&props))?;
            dc.SetTarget(&target);
            dc.BeginDraw();
            dc.Clear(Some(&D2D1_COLOR_F { r: 1.0, g: 1.0, b: 1.0, a: 1.0 }));
            inner.draw(page as usize - 1, Matrix3x2::scale(w / pw, h / ph));
            dc.EndDraw(None, None)?;
            dc.SetTarget(None);
            drop(inner);
            preview.DrawPage(page, &surface, 96.0, 96.0)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The print job itself, written to a stream instead of a printer (no dialog, nothing printed):
    /// `LOOKER_DECODE=x.pdf cargo test --release -- --ignored --nocapture writes_a_print_job` saves `x.job.xps`.
    #[test]
    #[ignore]
    fn writes_a_print_job() {
        use windows::Win32::Storage::Xps::Printing::{IPrintDocumentPackageTargetFactory, PrintDocumentPackageTargetFactory};
        use windows::Win32::System::Com::{STATFLAG_NONAME, STREAM_SEEK_SET};
        use windows::Win32::UI::Shell::SHCreateMemStream;
        unsafe {
            let _ = windows::Win32::System::Com::CoInitializeEx(None, windows::Win32::System::Com::COINIT_MULTITHREADED);
        }
        let src = PathBuf::from(std::env::var("LOOKER_DECODE").expect("LOOKER_DECODE"));
        let mut inner = Inner::new(vec![src.clone()]);
        inner.desc = PrintPageDescription {
            PageSize: windows::Foundation::Size { Width: 793.7, Height: 1122.5 },
            ImageableRect: windows::Foundation::Rect { X: 24.0, Y: 24.0, Width: 745.7, Height: 1074.5 },
            DpiX: 600,
            DpiY: 600,
        };
        let n = inner.pages().len() as u32;
        unsafe {
            let factory: IPrintDocumentPackageTargetFactory = CoCreateInstance(&PrintDocumentPackageTargetFactory, None, CLSCTX_INPROC_SERVER).unwrap();
            let out = SHCreateMemStream(None).unwrap();
            let target = factory.CreateDocumentPackageTargetForPrintJob(&HSTRING::from("Microsoft Print to PDF"), &HSTRING::from("Looker test"), &out, None).unwrap();
            inner.write(&target, &(1..=n).collect::<Vec<_>>()).unwrap();
            let mut stat = Default::default();
            out.Stat(&mut stat, STATFLAG_NONAME).unwrap();
            out.Seek(0, STREAM_SEEK_SET, None).unwrap();
            let mut bytes = vec![0u8; stat.cbSize as usize];
            let mut read = 0;
            let _ = out.Read(bytes.as_mut_ptr() as _, bytes.len() as u32, Some(&mut read));
            let path = src.with_extension("job.xps");
            std::fs::write(&path, &bytes[..read as usize]).unwrap();
            println!("{n} pages, {} bytes to {}", read, path.display());
            assert!(read > 1000);
        }
    }

    /// Draws page 1 of `LOOKER_DECODE` (a photo or PDF) as the preview would, on A4 portrait with 1/4" margins,
    /// fitted and filled, to PNGs beside the file: `LOOKER_DECODE=x.pdf cargo test --release -- --ignored
    /// --nocapture draws_print_pages`.
    #[test]
    #[ignore]
    fn draws_print_pages() {
        use windows::Win32::Graphics::Direct3D11::{D3D11_CPU_ACCESS_READ, D3D11_MAP_READ, D3D11_USAGE_STAGING, ID3D11Texture2D};
        unsafe {
            let _ = windows::Win32::System::Com::CoInitializeEx(None, windows::Win32::System::Com::COINIT_MULTITHREADED);
        }
        let src = PathBuf::from(std::env::var("LOOKER_DECODE").expect("LOOKER_DECODE"));
        let mut inner = Inner::new(vec![src.clone()]);
        inner.desc = PrintPageDescription {
            PageSize: windows::Foundation::Size { Width: 793.7, Height: 1122.5 },
            ImageableRect: windows::Foundation::Rect { X: 24.0, Y: 24.0, Width: 745.7, Height: 1074.5 },
            DpiX: 600,
            DpiY: 600,
        };
        println!("pages: {}", inner.pages().len());
        for scale in [Scale::Fit, Scale::Fill, Scale::Actual] {
            inner.scale = scale;
            let (w, h) = (397u32, 561u32);
            let gpu = inner.gpu().unwrap();
            let (d3d, dc, wic) = (gpu.d3d.clone(), gpu.dc.clone(), gpu.wic.clone());
            unsafe {
                let desc = D3D11_TEXTURE2D_DESC {
                    Width: w,
                    Height: h,
                    MipLevels: 1,
                    ArraySize: 1,
                    Format: DXGI_FORMAT_B8G8R8A8_UNORM,
                    SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
                    Usage: D3D11_USAGE_DEFAULT,
                    BindFlags: (D3D11_BIND_RENDER_TARGET.0 | D3D11_BIND_SHADER_RESOURCE.0) as u32,
                    ..Default::default()
                };
                let mut tex = None;
                d3d.CreateTexture2D(&desc, None, Some(&mut tex)).unwrap();
                let tex: ID3D11Texture2D = tex.unwrap();
                let surface: IDXGISurface = tex.cast().unwrap();
                let props = D2D1_BITMAP_PROPERTIES1 {
                    pixelFormat: D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED },
                    dpiX: 96.0,
                    dpiY: 96.0,
                    bitmapOptions: D2D1_BITMAP_OPTIONS_TARGET | D2D1_BITMAP_OPTIONS_CANNOT_DRAW,
                    colorContext: std::mem::ManuallyDrop::new(None),
                };
                let target = dc.CreateBitmapFromDxgiSurface(&surface, Some(&props)).unwrap();
                dc.SetTarget(&target);
                dc.BeginDraw();
                dc.Clear(Some(&D2D1_COLOR_F { r: 1.0, g: 1.0, b: 1.0, a: 1.0 }));
                inner.draw(0, Matrix3x2::scale(w as f32 / 793.7, h as f32 / 1122.5));
                dc.EndDraw(None, None).unwrap();
                dc.SetTarget(None);
                // Read back and save.
                let staging_desc = D3D11_TEXTURE2D_DESC { Usage: D3D11_USAGE_STAGING, BindFlags: 0, CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32, ..desc };
                let mut staging = None;
                d3d.CreateTexture2D(&staging_desc, None, Some(&mut staging)).unwrap();
                let staging: ID3D11Texture2D = staging.unwrap();
                let ctx = d3d.GetImmediateContext().unwrap();
                ctx.CopyResource(&staging, &tex);
                let mut map = Default::default();
                ctx.Map(&staging, 0, D3D11_MAP_READ, 0, Some(&mut map)).unwrap();
                let mut px = vec![0u8; (w * h * 4) as usize];
                for y in 0..h as usize {
                    let row = std::slice::from_raw_parts((map.pData as *const u8).add(y * map.RowPitch as usize), w as usize * 4);
                    px[y * w as usize * 4..(y + 1) * w as usize * 4].copy_from_slice(row);
                }
                ctx.Unmap(&staging, 0);
                let out = src.with_extension(format!("print-{scale:?}.png"));
                crate::imaging::wic::save_png(&wic, &out, w, h, &px).unwrap();
                println!("wrote {}", out.display());
            }
        }
    }
}
