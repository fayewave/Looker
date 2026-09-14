using System;
using System.Collections.Generic;
using System.IO;
using System.Numerics;
using System.Threading;
using System.Threading.Tasks;
using Looker.Imaging;
using Looker.Rendering;
using Looker.Services;
using Microsoft.Graphics.Canvas;
using Microsoft.Graphics.Canvas.Brushes;
using Microsoft.Graphics.Canvas.Text;
using Microsoft.Graphics.Canvas.UI;
using Microsoft.Graphics.Canvas.UI.Xaml;
using Microsoft.UI.Dispatching;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Input;
using Windows.Foundation;
using Windows.UI;

namespace Looker.Controls;

/// <summary>
/// The Win2D image surface and the speed core's front end. Owns the GPU device, the
/// <see cref="ImageCache"/>, the <see cref="PreloadScheduler"/>, and the current display bitmap +
/// <see cref="ZoomPanController"/>. Navigation goes through <see cref="ShowAsync"/>, which serves
/// the best-available bitmap instantly (cached sharp → cached low → progressive decode) and warms
/// the cache around the current image so next/prev feels instant.
/// </summary>
public sealed partial class ImageViewport : UserControl
{
    private const long DefaultCacheBudgetBytes = 512L * 1024 * 1024;

    // Instant placeholder resolution shown while the sharp decode lands. This must be high enough that,
    // stretched to fill the window, it doesn't read as blocky — a 2560px display fits an image ~1350px+
    // wide, so a 512px placeholder (the old value) looked heavily pixelated for the ~300ms a large JPEG
    // takes to decode. 1280 is still a fast ~1/4-scale JPEG DCT decode but looks essentially sharp.
    private const int LowResTargetDim = 1280;

    // The low-res placeholder gets its own reserved cache-key bucket so it can never collide with a real
    // high-res bucket (BucketFor only ever yields positive multiples of 256). Before this, a ~768px viewport
    // made the placeholder's BucketFor(512)=768 key equal the high key, so a 512px decode could be served as
    // the "instant sharp path" forever.
    private const int LowResBucket = -1;

    // Vector sources (SVG, PDF pages) re-rasterize when zoomed instead of stopping at a nominal size, up to
    // this edge per bitmap: 4096² × 4 B = 64 MB a page, the most a zoomed-in PDF should hold per page.
    private const int VectorMaxDimension = 4096;

    // Navigation coalescing. A ShowAsync that lands within BurstWindowMs of the previous one is part of a
    // held key / wheel run: its decodes wait a short settle before starting, so the run only ever costs the
    // decodes for images the user actually stops on (the next step cancels a still-waiting decode for free)
    // and the pool never fills with work for images already skipped. Cached frames still show instantly.
    private const int BurstWindowMs = 200;
    private const int LowSettleMs = 50;
    private const int SharpSettleMs = 120;

    private readonly ZoomPanController _zoom = new();
    private readonly ImageCache _cache = new(DefaultCacheBudgetBytes);
    private PreloadScheduler? _preload;

    private DecodedImage? _current;
    private CanvasDevice? _device;
    private ImageRef? _currentRef;
    private IReadOnlyList<PreloadItem> _lastPreload = Array.Empty<PreloadItem>();
    private CancellationTokenSource? _displayCts;
    private ImageRef? _loading;      // image whose display decode is in flight (null once it landed/was cancelled)
    private ImageRef? _displayedRef; // image _current belongs to (may lag _currentRef while the next one decodes)
    private long _lastNavTicks = long.MinValue;
    private string? _errorMessage;

    // The cache bucket the currently displayed sharp bitmap was decoded for (0 = only a low-res placeholder
    // is up). When the viewport or the zoom level later grows past this, MaybeUpgradeResolutionAsync
    // re-decodes so an image opened at a small size (or zoomed to 100%+) doesn't stay soft forever.
    private int _displayedHighBucket;
    private CancellationTokenSource? _upgradeCts;
    private bool _xamlRootHooked;
    private long _lastDrawSig; // diagnostics throttle for the draw-size log

    private bool _panning;
    private Point _lastPointer;

    // Animation stepper: a one-shot timer re-armed with each frame's own delay (variable timing).
    private DispatcherQueueTimer? _animationTimer;
    private int _frameIndex;
    private bool _animationPaused;

    // Slideshow cross-fade (M8): the outgoing image is retained and drawn on top of the incoming one at
    // a falling opacity for FadeDuration, giving a 300 ms dissolve. Only armed when CrossFadeNext is set
    // (by the slideshow) so ordinary navigation stays a hard cut.
    private static readonly TimeSpan FadeDuration = TimeSpan.FromMilliseconds(300);
    private DispatcherQueueTimer? _fadeTimer;
    private DecodedImage? _fadeOutImage;
    private RectD _fadeOutRect;
    private double _fadeProgress; // 0 → 1; outgoing opacity = 1 − progress

    // Rotation preview (M8 revision): rotate is non-destructive until the user saves. This is the accumulated
    // clockwise quarter-turn count applied at draw time only; it's cleared on navigation and baked into the
    // file by MainViewModel.SaveRotationAsync.
    private int _previewQuarterTurns;

    // Multi-page (PDF): the page the page bar points at. Only the bar's buttons move it — the arrow keys keep
    // stepping through the folder — and the fit/centre maths follows it via the zoom controller's focus rect.
    private int _pageIndex;
    private readonly DispatcherQueue _dispatcher;

    /// <summary>Raised whenever the zoom level changes via a user gesture (wheel/keys/double-click), with
    /// the new zoom relative to actual size (100 = 1:1). Not raised on navigation (that resets to fit).</summary>
    public event Action<double>? ZoomChanged;

    /// <summary>Raised whenever the effective zoom may have changed for *any* reason — a gesture, a navigation
    /// reset to fit, a viewport resize, the info card opening, a rotation preview — so a passive readout (the
    /// status row) can re-read <see cref="ZoomPercent"/>. Unlike <see cref="ZoomChanged"/> it does not mean the
    /// user did something, so it must not drive the OSD.</summary>
    public event Action? ViewChanged;

    /// <summary>Current zoom relative to actual size (100 = 1:1 device pixels).</summary>
    public double ZoomPercent => _zoom.ZoomPercent;

    /// <summary>When set, the next image swap dissolves from the current image instead of cutting. Consumed
    /// (reset to false) by that swap. The slideshow sets it before advancing.</summary>
    public bool CrossFadeNext { get; set; }

    /// <summary>Accumulated clockwise quarter-turns of the current unsaved rotation preview (0 = none).</summary>
    public int PreviewQuarterTurns => _previewQuarterTurns;

    /// <summary>Raised with true when an unsaved rotation preview exists and false when it's cleared/saved, so
    /// the host can show or hide the Save affordance.</summary>
    public event Action<bool>? RotationPreviewChanged;

    /// <summary>Raised (on the UI thread) whenever what the viewport shows changes: a placeholder or sharp
    /// frame of the current image landed, it cleared, or it failed. The host refreshes anything derived from
    /// the on-screen image (status row size, info panel) from here rather than awaiting the decode.</summary>
    public event Action? DisplayChanged;

    /// <summary>Raised when <see cref="CurrentPage"/> moves (page bar buttons), not on navigation.</summary>
    public event Action? PageChanged;

    /// <summary>Number of pages of the displayed document; 0 for a single image (or nothing / not yet up).</summary>
    public int PageCount => DisplayedIsCurrent && _current?.Pages is { } pages ? pages.PageCount : 0;

    /// <summary>Zero-based page the page bar points at.</summary>
    public int CurrentPage => _pageIndex;

    public ImageViewport()
    {
        InitializeComponent();
        _dispatcher = DispatcherQueue;
        Looker.Helpers.StartupTrace.Mark("ImageViewport constructed (CanvasControl)");
    }

    private bool _firstDrawTraced;

    /// <summary>Set the decode cache budget (bytes). Applied from persisted settings at startup.</summary>
    public void SetCacheBudgetBytes(long bytes)
    {
        if (bytes > 0)
            _cache.BudgetBytes = bytes;
    }

    /// <summary>Drop every cached decode (Settings ▸ Reset). The bitmap on screen is refcounted and survives; the
    /// next navigation simply decodes again.</summary>
    public void ClearCache() => _cache.Clear();

    /// <summary>
    /// Show <paramref name="current"/> (null clears) and warm the cache for <paramref name="preload"/>
    /// (neighbours in travel-direction priority). Cancels any in-flight progressive decode. Returns once the
    /// first frame of the image is up (cached sharp, or the low-res placeholder) — the sharp decode keeps
    /// going in the background and announces itself via <see cref="DisplayChanged"/> — so callers never
    /// wait on a decode; on a superseding navigation it returns when that decode is cancelled.
    /// </summary>
    public Task ShowAsync(ImageRef? current, IReadOnlyList<PreloadItem> preload)
    {
        _lastPreload = preload;

        // Same image, already sharp or still decoding (OpenFileAsync re-shows the launch file once its folder
        // is enumerated; a sort change re-shows the current one): keep what is up / in flight and just refresh
        // the neighbour window around it. A failed or placeholder-only image falls through and reloads.
        if (current is { } same
            && ((_loading is { } loading && loading.Equals(same))
                || (DisplayedIsCurrent && _displayedRef!.Value.Equals(same) && _displayedHighBucket > 0)))
        {
            SchedulePreload(preload, ComputeHighBucket());
            return Task.CompletedTask;
        }

        long now = Environment.TickCount64;
        bool burst = now - _lastNavTicks < BurstWindowMs;
        _lastNavTicks = now;

        _currentRef = current;
        ClearRotationPreview(); // any navigation/refresh discards an unsaved rotation preview
        CancelDisplayDecode();
        CancelUpgrade();

        if (current is null)
        {
            StopAnimation();
            StopCrossFade();
            DetachPages(_current);
            _current?.Release();
            _current = null;
            _errorMessage = null;
            _displayedHighBucket = 0;
            _pageIndex = 0;
            Canvas?.Invalidate();
            DisplayChanged?.Invoke();
            return Task.CompletedTask;
        }

        if (_device is null)
            return Task.CompletedTask; // deferred: OnDeviceReadyAsync will load _currentRef once the device exists

        return LoadCurrentAsync(current.Value, burst);
    }

    /// <summary>Completes when the first frame is showing (or the load was superseded/failed).</summary>
    private async Task LoadCurrentAsync(ImageRef image, bool burst)
    {
        CanvasDevice? device = _device;
        if (device is null)
            return;

        int highBucket = ComputeHighBucket();
        var lowKey = new CacheKey(image.Path, image.ModifiedTicks, LowResBucket);
        _displayedHighBucket = 0;

        // 0) Launch image decoded ahead of the window (StartupWarmup): move it into the cache so the paths
        //    below hit. Awaiting a still-running warm-up is still cheaper than starting the same decode again.
        //    The warm-up sized its sharp decode from the saved window placement (physical px), which is often
        //    *more* accurate than ComputeHighBucket here — XamlRoot, hence the true DPI, may not be attached
        //    yet — so an equal-or-larger prepared bucket is used as-is rather than decoding a smaller one.
        int displayBucket = highBucket;
        if (StartupWarmup.TakePrepared(image) is { } prepared)
        {
            int adopted = await AdoptPreparedAsync(prepared, device, lowKey);
            if (adopted >= highBucket)
                displayBucket = adopted; // show the prepared sharp bitmap; neighbours still preload at highBucket
        }
        var highKey = new CacheKey(image.Path, image.ModifiedTicks, displayBucket);

        // 1) Instant sharp path — high-res already cached (preloaded neighbour, revisited image, or the launch
        //    image prepared by the warm-up).
        if (_cache.TryGet(highKey, out ICacheValue cachedHigh))
        {
            PublishSharp((DecodedImage)cachedHigh, displayBucket, resetZoom: true, "cached");
            SchedulePreload(_lastPreload, highBucket);
            return;
        }
        highKey = new CacheKey(image.Path, image.ModifiedTicks, highBucket);

        var cts = new CancellationTokenSource();
        _displayCts = cts;
        _loading = image;

        // A preload of this very image may be mid-decode: adopt it (waits for the work already done) before
        // the new window — which no longer contains it — would cancel it. Then start the neighbours now, not
        // after this image's sharp decode: with a held key, the low ring running ahead of travel is what keeps
        // the screen moving.
        Task? lowInFlight = _preload is { } pre && pre.TryAdopt(lowKey, out Task lowTask) ? lowTask : null;
        Task? highInFlight = _preload is { } pre2 && pre2.TryAdopt(highKey, out Task highTask) ? highTask : null;
        SchedulePreload(_lastPreload, highBucket);

        var firstFrame = new TaskCompletionSource(TaskCreationOptions.RunContinuationsAsynchronously);
        _ = DecodeStagesAsync(image, highBucket, lowKey, highKey, device, cts, burst, lowInFlight, highInFlight, firstFrame);
        await firstFrame.Task;
    }

    /// <summary>The progressive load proper: placeholder first (signalling <paramref name="firstFrame"/>),
    /// then the sharp decode swapped in over it. Runs detached from the caller so the UI never awaits it.</summary>
    private async Task DecodeStagesAsync(
        ImageRef image, int highBucket, CacheKey lowKey, CacheKey highKey, CanvasDevice device,
        CancellationTokenSource cts, bool burst, Task? lowInFlight, Task? highInFlight, TaskCompletionSource firstFrame)
    {
        CancellationToken ct = cts.Token;
        bool shown = false;
        ImageFormat format = ImageFormat.Unknown;
        bool sniffed = false;

        try
        {
            // 2) Instant soft path — show a low-res version immediately, decoding sharp behind it.
            //    Low-res placeholder is always a static first frame (fast, even for animations).
            if (lowInFlight is not null && !_cache.Contains(lowKey))
                await lowInFlight.WaitAsync(ct);

            if (_cache.TryGet(lowKey, out ICacheValue cachedLow))
            {
                if (Superseded(cts)) { cachedLow.Release(); return; }
                Display((DecodedImage)cachedLow, resetZoom: true);
                shown = true;
            }
            else
            {
                if (burst)
                    await Task.Delay(LowSettleMs, ct); // a held key: only decode if the user stays here
                // Sniff once per navigation (magic bytes → format), shared by both decode tiers.
                format = await SniffAsync(image.Path, ct);
                sniffed = true;
                DecodedImage low = await DecodeAsync(image, LowResTargetDim, format, firstFrameOnly: true, device, ct);
                _cache.Insert(lowKey, low);
                if (Superseded(cts)) { low.Release(); return; }
                Display(low, resetZoom: true);
                shown = true;
            }
            firstFrame.TrySetResult();

            // 3) Sharp decode swapped in over the low-res (same OrientedNativeSize → no jump, keeps zoom).
            //    Not FirstFrameOnly → animated formats become playable here.
            if (highInFlight is not null && !_cache.Contains(highKey))
                await highInFlight.WaitAsync(ct);

            if (_cache.TryGet(highKey, out ICacheValue preloaded))
            {
                if (Superseded(cts)) { preloaded.Release(); return; }
                PublishSharp((DecodedImage)preloaded, highBucket, resetZoom: false, "preloaded");
                return;
            }

            if (burst)
                await Task.Delay(SharpSettleMs, ct);
            if (!sniffed)
                format = await SniffAsync(image.Path, ct);
            DecodedImage high = await DecodeAsync(image, highBucket, format, firstFrameOnly: false, device, ct);
            _cache.Insert(highKey, high);
            if (Superseded(cts)) { high.Release(); return; }
            PublishSharp(high, highBucket, resetZoom: !shown, "primary");
        }
        catch (OperationCanceledException)
        {
            // Superseded by a newer navigation — leave whatever is showing.
        }
        catch (Exception ex)
        {
            if (!Superseded(cts))
                ShowError($"Can't display this image\n{ex.GetType().Name}: {ex.Message}");
        }
        finally
        {
            firstFrame.TrySetResult();
            if (!Superseded(cts))
            {
                _loading = null;
                Canvas?.Invalidate();
                // Self-check: if this image was decoded at a stale/too-small viewport or DPI (e.g. XamlRoot not
                // yet attached), the true needed resolution may now be larger — upgrade it. No-op if already sharp.
                _ = MaybeUpgradeResolutionAsync();
            }
        }
    }

    /// <summary>Move a <see cref="StartupWarmup.PreparedImage"/> into the cache (low-res under the placeholder
    /// key, sharp under the bucket it was decoded for) and return that sharp bucket (0 if none). Anything that
    /// failed, or was decoded on a different device than the one this control ended up with, is dropped and the
    /// normal path decodes instead.</summary>
    private async Task<int> AdoptPreparedAsync(StartupWarmup.PreparedImage prepared, CanvasDevice device, CacheKey lowKey)
    {
        StartupWarmup.StartDevice(); // no-op normally; guarantees the prepared decode's device gate is open
        DecodedImage? low = null, sharp = null;
        try
        {
            low = await prepared.Low;
            sharp = await prepared.Sharp;
        }
        catch { /* the warm-up decode failed; fall through to the regular decode */ }

        bool sameDevice = ReferenceEquals(prepared.Device, device);
        Looker.Helpers.StartupTrace.Mark($"adopt prepared: low={(low is not null)} sharp={(sharp is not null)} bucket={prepared.SharpBucket} sameDevice={sameDevice}");
        if (!sameDevice)
        {
            low?.Release();
            sharp?.Release();
            return 0;
        }

        if (low is not null)
        {
            _cache.Insert(lowKey, low);
            low.Release(); // the cache holds it now
        }
        if (sharp is null || prepared.SharpBucket <= 0)
            return 0;

        _cache.Insert(new CacheKey(prepared.Image.Path, prepared.Image.ModifiedTicks, prepared.SharpBucket), sharp);
        sharp.Release();
        return prepared.SharpBucket;
    }

    // --- Current-image accessors for M7 file ops / info panel ---

    /// <summary>The sniffed format of the displayed image (null when nothing is shown), so the file-ops
    /// layer can decide whether the clipboard/wallpaper can use the file directly or needs a PNG render.</summary>
    public ImageFormat? CurrentFormat => DisplayedIsCurrent ? _current?.Format : null;

    /// <summary>The image's true native (EXIF-oriented) pixel size — NOT the downscaled decode size — so the
    /// info panel reports the file's real resolution rather than the fit-to-viewport buffer.</summary>
    public Size? CurrentNativeSize
    {
        get
        {
            if (!DisplayedIsCurrent || _current is not { } current)
                return null;
            if (current.Pages is { } pages)
            {
                RectD page = pages.Layout.Pages[Math.Clamp(_pageIndex, 0, pages.PageCount - 1)];
                return new Size(page.Width, page.Height); // the page on the bar, not the whole strip
            }
            return current.OrientedNativeSize;
        }
    }

    /// <summary>The bitmap that stands for "the current image" in exports: the current page of a PDF when it
    /// has rendered (else page 0), otherwise the display bitmap.</summary>
    private CanvasBitmap? ExportBitmap(DecodedImage? image)
    {
        if (image is null)
            return null;
        if (image.Pages is { } pages)
            return pages.TryGetPage(_pageIndex) ?? image.Bitmap;
        return image.Bitmap;
    }

    // While the next image decodes the previous one stays on screen; anything describing "the current image"
    // must not report that stale bitmap.
    private bool DisplayedIsCurrent => _current is not null && _displayedRef.HasValue && _displayedRef.Equals(_currentRef);

    /// <summary>Save the current sharp bitmap (first frame for animations) as a PNG at
    /// <paramref name="destPath"/>. Used to give the clipboard/wallpaper a raster for exotic formats.</summary>
    public async Task<bool> SaveCurrentPngAsync(string destPath)
    {
        CanvasBitmap? bitmap = ExportBitmap(_current);
        if (bitmap is null)
            return false;

        try
        {
            using var fs = new FileStream(destPath, FileMode.Create, FileAccess.ReadWrite);
            using Windows.Storage.Streams.IRandomAccessStream ras = fs.AsRandomAccessStream();
            await bitmap.SaveAsync(ras, CanvasBitmapFileFormat.Png);
            return true;
        }
        catch
        {
            return false;
        }
    }

    /// <summary>Read back the current display bitmap's BGRA8 pixels for the info-panel histogram. Returns
    /// false when nothing is shown.</summary>
    public bool TryGetDisplayPixels(out byte[] pixels, out int width, out int height)
    {
        pixels = Array.Empty<byte>();
        width = 0;
        height = 0;

        CanvasBitmap? bitmap = DisplayedIsCurrent ? ExportBitmap(_current) : null;
        if (bitmap is null)
            return false;

        try
        {
            width = (int)bitmap.SizeInPixels.Width;
            height = (int)bitmap.SizeInPixels.Height;
            if (width <= 0 || height <= 0)
                return false;
            pixels = bitmap.GetPixelBytes();
            return pixels.Length >= (long)width * height * 4;
        }
        catch
        {
            return false;
        }
    }

    // --- Keyboard-invoked zoom commands (called from MainWindow accelerators) ---

    public void ZoomInStep() => ZoomAtCenter(1.25);

    public void ZoomOutStep() => ZoomAtCenter(0.8);

    public void FitToWindow()
    {
        if (_current is null) return;
        _zoom.FitToViewport();
        Canvas?.Invalidate();
        RaiseZoom();
    }

    public void ZoomActualSize()
    {
        if (_current is null) return;
        _zoom.SetActualSize();
        Canvas?.Invalidate();
        RaiseZoom();
        _ = MaybeUpgradeResolutionAsync();
    }

    private void ZoomAtCenter(double factor)
    {
        if (_current is null) return;
        Size s = GetViewportSize();
        _zoom.ZoomAt(factor, s.Width / 2.0, s.Height / 2.0);
        Canvas?.Invalidate();
        RaiseZoom();
        _ = MaybeUpgradeResolutionAsync();
    }

    private void RaiseZoom()
    {
        ZoomChanged?.Invoke(_zoom.ZoomPercent);
        ViewChanged?.Invoke();
    }

    // --- Rotation preview (non-destructive; committed by the host's Save) ---

    /// <summary>Rotate the on-screen image 90° (right = clockwise) as a preview only — the file is untouched
    /// until the host saves. Re-fits to the rotated bounds so the whole image stays visible.</summary>
    public void PreviewRotate(bool clockwise)
    {
        if (_current is null || _current.Pages is not null) // a PDF is not a photo; nothing to bake into the file
            return;
        _previewQuarterTurns = (_previewQuarterTurns + (clockwise ? 1 : 3)) & 3;
        ApplyPreviewZoom();
        Canvas?.Invalidate();
        RotationPreviewChanged?.Invoke(_previewQuarterTurns != 0);
    }

    /// <summary>Discard any unsaved rotation preview and return to the file's true orientation.</summary>
    public void ClearRotationPreview()
    {
        if (_previewQuarterTurns == 0)
            return;
        _previewQuarterTurns = 0;
        ApplyPreviewZoom();
        Canvas?.Invalidate();
        RotationPreviewChanged?.Invoke(false);
    }

    // --- Pages (PDF) ---

    /// <summary>Point the view at page <paramref name="index"/> (clamped): fits it when in Fit mode, else keeps
    /// the zoom and centres on it. No-op for single images or while the previous image is still up.</summary>
    public void GoToPage(int index)
    {
        if (!DisplayedIsCurrent || _current?.Pages is not { } pages)
            return;
        index = Math.Clamp(index, 0, pages.PageCount - 1);
        if (index == _pageIndex)
            return;
        _pageIndex = index;
        _zoom.SetFocus(pages.Layout.Pages[index]);
        pages.RequestPage(index);
        Canvas?.Invalidate();
        ViewChanged?.Invoke(); // a differently sized page refits to another scale
        PageChanged?.Invoke();
    }

    public void NextPage() => GoToPage(_pageIndex + 1);

    /// <summary>After a pan or zoom, make the page nearest the middle of the view the current one (bar text,
    /// fit reference) without moving the view — so a drag across the strip is reflected in the page bar and the
    /// next button continues from where the user actually is.</summary>
    private void SyncPageToView()
    {
        if (_current?.Pages is not { } pages || !DisplayedIsCurrent)
            return;
        Size v = GetViewportSize();
        RectD strip = _zoom.GetImageRect();
        double scale = _zoom.Scale;
        if (scale <= 0)
            return;
        int index = pages.Layout.PageAt((v.Width / 2.0 - strip.X) / scale);
        if (index == _pageIndex)
            return;
        _pageIndex = index;
        _zoom.SetFocus(pages.Layout.Pages[index], recenter: false);
        PageChanged?.Invoke();
    }

    public void PreviousPage() => GoToPage(_pageIndex - 1);

    private RectD? FocusRectFor(DecodedImage image)
        => image.Pages is { } pages ? pages.Layout.Pages[Math.Clamp(_pageIndex, 0, pages.PageCount - 1)] : null;

    // Page renders finish on the pool; the canvas is redrawn from the UI thread once they land.
    private void AttachPages(DecodedImage image)
    {
        if (image.Pages is { } pages)
            pages.PageRendered = () => _dispatcher.TryEnqueue(() => Canvas?.Invalidate());
    }

    private static void DetachPages(DecodedImage? image)
    {
        if (image?.Pages is { } pages)
            pages.PageRendered = null;
    }

    /// <summary>Draw every page whose slot intersects the viewport; pages not rendered yet show as blank
    /// white sheets and are requested, so a scroll across the strip fills in as the renders land.</summary>
    private void DrawPages(CanvasDrawingSession ds, DecodedImage current, PdfPageSet pages)
    {
        RectD strip = _zoom.GetImageRect();
        double scale = _zoom.Scale;
        Size viewport = GetViewportSize();
        IReadOnlyList<RectD> layout = pages.Layout.Pages;
        for (int i = 0; i < layout.Count; i++)
        {
            RectD page = layout[i];
            double x = strip.X + page.X * scale;
            double y = strip.Y + page.Y * scale;
            double w = page.Width * scale;
            double h = page.Height * scale;
            if (x + w < 0 || y + h < 0 || x > viewport.Width || y > viewport.Height)
                continue;

            var destination = new Rect(x, y, w, h);
            CanvasBitmap? bitmap = pages.TryGetPage(i) ?? (i == 0 ? current.Bitmap : null);
            if (bitmap is null)
            {
                ds.FillRectangle(destination, PagePlaceholder);
                pages.RequestPage(i);
                continue;
            }
            var source = new Rect(0, 0, bitmap.Size.Width, bitmap.Size.Height);
            ds.DrawImage(bitmap, destination, source, 1f, CanvasImageInterpolation.HighQualityCubic);
        }
    }

    private static readonly Color PagePlaceholder = Color.FromArgb(0xFF, 0xFF, 0xFF, 0xFF);

    // Re-fit the zoom to the current preview orientation: odd quarter-turns swap the effective width/height.
    private void ApplyPreviewZoom()
    {
        if (_current is null)
            return;
        Size v = GetViewportSize();
        double w = _current.OrientedNativeSize.Width;
        double h = _current.OrientedNativeSize.Height;
        bool swap = (_previewQuarterTurns & 1) == 1;
        _zoom.Reset(v.Width, v.Height, swap ? h : w, swap ? w : h, GetRasterization());
        ViewChanged?.Invoke();
    }

    // --- Win2D lifecycle ---

    private void OnCreateResources(CanvasControl sender, CanvasCreateResourcesEventArgs args)
    {
        Looker.Helpers.StartupTrace.Mark($"Win2D CreateResources ({args.Reason})");
        _device = sender.Device;
        _preload ??= new PreloadScheduler(PreloadScheduler.DefaultConcurrency, _cache.Contains, PreloadOneAsync);

        if (args.Reason == CanvasCreateResourcesReason.NewDevice)
        {
            // Every cached/displayed bitmap belongs to the lost device — start clean on the new one.
            StopAnimation();
            StopCrossFade();
            CancelDisplayDecode();
            CancelUpgrade();
            _preload.CancelAll();
            _cache.Clear();
            DetachPages(_current);
            _current?.Release();
            _current = null;
            _displayedHighBucket = 0;
            _checker = null; // device-bound; rebuilt lazily on the next draw
        }

        // Deliberately NOT args.TrackAsyncAction(...): tracking would hold back every draw until the whole
        // load (including the sharp decode) finished, hiding the low-res placeholder for the launch image.
        // Each stage of the load invalidates the canvas itself as it lands.
        _ = OnDeviceReadyAsync();
    }

    private async Task OnDeviceReadyAsync()
    {
        HookXamlRootOnce(); // XamlRoot (true DPI) is usually attached by now — hook it before the first decode
        if (_currentRef is not null)
            await LoadCurrentAsync(_currentRef.Value, burst: false);
    }

    private void OnDraw(CanvasControl sender, CanvasDrawEventArgs args)
    {
        CanvasDrawingSession ds = args.DrawingSession;
        DecodedImage? current = _current;
        bool firstDraw = !_firstDrawTraced;
        if (firstDraw)
        {
            _firstDrawTraced = true;
            Looker.Helpers.StartupTrace.Mark($"first Win2D Draw start (image={(current is not null)})");
        }
        try { DrawCore(ds, current); }
        finally
        {
            if (firstDraw)
            {
                Looker.Helpers.StartupTrace.Mark("first Win2D Draw end");
                Looker.Helpers.StartupTrace.Flush("first draw");
            }
        }
    }

    // Preview-area background: a subtle near-black checkerboard (two greys a hair apart) tiled by an image
    // brush. The tile is rendered at the control's DPI so the squares stay crisp and integer-sized; it is
    // rebuilt on a DPI change and dropped with the device on loss (CanvasImageBrush is device-bound).
    private const float CheckerCellDips = 8f;
    private Color _emptyBackground = Color.FromArgb(0xFF, 0x00, 0x00, 0x00); // = RootGrid (theme-driven)

    /// <summary>Colour drawn when nothing is open (the landing page sits on the plain window colour, not the
    /// checkerboard). Set by the window when the theme changes; the checkerboard itself never follows the theme.</summary>
    public void SetEmptyBackground(Color color)
    {
        if (_emptyBackground == color) return;
        _emptyBackground = color;
        Canvas.Invalidate();
    }
    private static readonly Color CheckerDark = Color.FromArgb(0xFF, 0x0F, 0x0F, 0x0F);
    private static readonly Color CheckerLight = Color.FromArgb(0xFF, 0x16, 0x16, 0x16);
    private CanvasImageBrush? _checker;
    private float _checkerDpi;

    private CanvasImageBrush GetCheckerBrush()
    {
        float dpi = Canvas.Dpi;
        if (_checker is not null && _checkerDpi == dpi)
            return _checker;

        _checker?.Dispose();
        float tileSize = CheckerCellDips * 2;
        var tile = new CanvasRenderTarget(Canvas, tileSize, tileSize, dpi);
        using (CanvasDrawingSession t = tile.CreateDrawingSession())
        {
            t.Clear(CheckerDark);
            t.FillRectangle(0, 0, CheckerCellDips, CheckerCellDips, CheckerLight);
            t.FillRectangle(CheckerCellDips, CheckerCellDips, CheckerCellDips, CheckerCellDips, CheckerLight);
        }
        _checker = new CanvasImageBrush(Canvas, tile)
        {
            ExtendX = CanvasEdgeBehavior.Wrap,
            ExtendY = CanvasEdgeBehavior.Wrap,
            Interpolation = CanvasImageInterpolation.NearestNeighbor,
        };
        _checkerDpi = dpi;
        return _checker;
    }

    private void DrawCore(CanvasDrawingSession ds, DecodedImage? current)
    {
        Size canvasSize = Canvas.Size;
        if (current is null && _fadeOutImage is null)
            ds.Clear(_emptyBackground); // nothing open: the landing page sits on the plain window colour, not the checkerboard
        else
            ds.FillRectangle(0, 0, (float)canvasSize.Width, (float)canvasSize.Height, GetCheckerBrush());

        // Everything from here draws in the inner viewport (between the explorer and info cards): shift right by
        // the left inset so the zoom controller's coordinates land in the visible area. Restored at the end.
        Matrix3x2 rootTransform = ds.Transform;
        if (_leftInset > 0)
            ds.Transform = Matrix3x2.CreateTranslation((float)_leftInset, 0f) * rootTransform;
        try
        {
            DrawContent(ds, current);
        }
        finally
        {
            ds.Transform = rootTransform;
        }
    }

    private void DrawContent(CanvasDrawingSession ds, DecodedImage? current)
    {
        if (current?.Pages is { } pages)
        {
            DrawPages(ds, current, pages);
        }
        else if (current is not null)
        {
            CanvasBitmap frame = current.Frames is { } frames && _frameIndex < frames.Count
                ? frames[_frameIndex].Bitmap
                : current.Bitmap;
            RectD rect = _zoom.GetImageRect();
            var source = new Rect(0, 0, frame.Size.Width, frame.Size.Height);
            int quarterTurns = _previewQuarterTurns & 3;
            if (quarterTurns == 0)
            {
                var destination = new Rect(rect.X, rect.Y, rect.Width, rect.Height);
                ds.DrawImage(frame, destination, source, 1f, CanvasImageInterpolation.HighQualityCubic);
            }
            else
            {
                // rect is the rotated image's bounding box (ApplyPreviewZoom fit it). Draw the unrotated frame
                // into a box that, once rotated about the box centre, lands exactly on rect; for odd turns the
                // pre-rotation box has width/height swapped.
                double cx = rect.X + rect.Width / 2.0;
                double cy = rect.Y + rect.Height / 2.0;
                bool swap = (quarterTurns & 1) == 1;
                double dw = swap ? rect.Height : rect.Width;
                double dh = swap ? rect.Width : rect.Height;
                var destination = new Rect(cx - dw / 2.0, cy - dh / 2.0, dw, dh);

                Matrix3x2 previous = ds.Transform;
                ds.Transform = Matrix3x2.CreateRotation((float)(quarterTurns * Math.PI / 2.0), new Vector2((float)cx, (float)cy)) * previous;
                ds.DrawImage(frame, destination, source, 1f, CanvasImageInterpolation.HighQualityCubic);
                ds.Transform = previous;
            }

            // Diagnostics: log the actual bitmap being drawn and the size it's stretched to, throttled to
            // when either changes. bmp<<rect ⇒ upscaled (stuck placeholder or zoomed past the decode).
            long sig = ((long)frame.SizeInPixels.Width << 32) ^ frame.SizeInPixels.Height
                       ^ ((long)Math.Round(rect.Width) << 16) ^ (long)Math.Round(rect.Height);
            if (sig != _lastDrawSig)
            {
                _lastDrawSig = sig;
                DiagLog($"DRAW bmp={frame.SizeInPixels.Width}x{frame.SizeInPixels.Height} rect={rect.Width:0}x{rect.Height:0} nativeBucket={_displayedHighBucket} name={System.IO.Path.GetFileName(_currentRef?.Path ?? string.Empty)}");
            }
        }
        else if (_errorMessage is not null)
        {
            // Decode failure. The "nothing open yet" case is handled by the XAML EmptyState overlay, so an
            // empty viewport draws blank (Mica shows through) rather than doubling up on placeholder text.
            DrawStatus(ds, GetViewportSize(), _errorMessage);
        }

        // Cross-fade: draw the outgoing image over the incoming one at a falling opacity.
        if (_fadeOutImage is { } fade)
        {
            var destination = new Rect(_fadeOutRect.X, _fadeOutRect.Y, _fadeOutRect.Width, _fadeOutRect.Height);
            var source = new Rect(0, 0, fade.Bitmap.Size.Width, fade.Bitmap.Size.Height);
            ds.DrawImage(fade.Bitmap, destination, source, (float)Math.Clamp(1.0 - _fadeProgress, 0, 1), CanvasImageInterpolation.HighQualityCubic);
        }
    }

    // --- Cross-fade stepper ---

    private void StartCrossFade(DecodedImage outgoing, RectD rect)
    {
        StopCrossFade();
        outgoing.Retain(); // keep it alive past the swap's Release for the duration of the dissolve
        _fadeOutImage = outgoing;
        _fadeOutRect = rect;
        _fadeProgress = 0;

        _fadeTimer = DispatcherQueue.CreateTimer();
        _fadeTimer.Interval = TimeSpan.FromMilliseconds(16);
        _fadeTimer.IsRepeating = true;
        _fadeTimer.Tick += OnFadeTick;
        _fadeTimer.Start();
    }

    private void OnFadeTick(DispatcherQueueTimer sender, object args)
    {
        _fadeProgress += 16.0 / FadeDuration.TotalMilliseconds;
        if (_fadeProgress >= 1.0)
            StopCrossFade();
        Canvas?.Invalidate();
    }

    private void StopCrossFade()
    {
        if (_fadeTimer is not null)
        {
            _fadeTimer.Stop();
            _fadeTimer.Tick -= OnFadeTick;
            _fadeTimer = null;
        }
        _fadeOutImage?.Release();
        _fadeOutImage = null;
        _fadeProgress = 0;
    }

    // --- Input ---

    /// <summary>When true the wheel steps through the folder (<see cref="WheelStepRequested"/>) instead of zooming.
    /// Mirrors <c>SettingsService.WheelMode</c>.</summary>
    public bool WheelNavigates { get; set; }

    /// <summary>When false, wheel and double-click zoom into the middle of the view instead of around the pointer.
    /// Mirrors <c>SettingsService.ZoomAnchor</c>. Keyboard zoom always uses the middle.</summary>
    public bool ZoomAtPointer { get; set; } = true;

    /// <summary>The zoom anchor for a pointer gesture at <paramref name="position"/> (canvas coordinates): the
    /// pointer itself, or the middle of the inner viewport when <see cref="ZoomAtPointer"/> is off.</summary>
    private Point ZoomAnchorFor(Point position)
    {
        if (ZoomAtPointer)
            return new Point(position.X - _leftInset, position.Y);
        Size s = GetViewportSize();
        return new Point(s.Width / 2.0, s.Height / 2.0);
    }

    /// <summary>Raised in navigate mode once per wheel notch: +1 = next image (wheel down), -1 = previous (wheel up).
    /// Sub-notch deltas from high-resolution wheels accumulate until they reach a full notch.</summary>
    public event EventHandler<int>? WheelStepRequested;
    private int _wheelAccumulator;

    private void OnPointerWheelChanged(object sender, PointerRoutedEventArgs e)
    {
        if (_current is null) return;
        var pp = e.GetCurrentPoint(Canvas);
        // Navigate mode still zooms with Ctrl held (the usual "Ctrl+wheel = zoom" convention), so a PDF page or
        // a photo can be read closer without flipping the setting back.
        bool ctrl = (e.KeyModifiers & Windows.System.VirtualKeyModifiers.Control) != 0;
        if (WheelNavigates && !ctrl)
        {
            e.Handled = true;
            _wheelAccumulator += pp.Properties.MouseWheelDelta;
            while (Math.Abs(_wheelAccumulator) >= 120)
            {
                int direction = _wheelAccumulator > 0 ? -1 : 1;
                _wheelAccumulator += direction * 120;
                WheelStepRequested?.Invoke(this, direction);
            }
            return;
        }
        double factor = Math.Pow(1.2, pp.Properties.MouseWheelDelta / 120.0);
        Point anchor = ZoomAnchorFor(pp.Position);
        _zoom.ZoomAt(factor, anchor.X, anchor.Y);
        SyncPageToView();
        Canvas.Invalidate();
        RaiseZoom();
        e.Handled = true;
        _ = MaybeUpgradeResolutionAsync(); // debounced — a wheel burst coalesces into one decode
    }

    private void OnPointerPressed(object sender, PointerRoutedEventArgs e)
    {
        if (_current is null) return;
        var pp = e.GetCurrentPoint(Canvas);
        if (!pp.Properties.IsLeftButtonPressed) return;
        _panning = true;
        _lastPointer = pp.Position;
        Canvas.CapturePointer(e.Pointer);
    }

    private void OnPointerMoved(object sender, PointerRoutedEventArgs e)
    {
        if (!_panning) return;
        Point p = e.GetCurrentPoint(Canvas).Position;
        _zoom.Pan(p.X - _lastPointer.X, p.Y - _lastPointer.Y);
        _lastPointer = p;
        SyncPageToView(); // dragging across the strip moves the page bar with it
        Canvas.Invalidate();
    }

    private void OnPointerReleased(object sender, PointerRoutedEventArgs e) => EndPan(e);

    private void OnPointerCaptureLost(object sender, PointerRoutedEventArgs e) => EndPan(e);

    private void EndPan(PointerRoutedEventArgs e)
    {
        if (!_panning) return;
        _panning = false;
        Canvas?.ReleasePointerCapture(e.Pointer);
    }

    private void OnDoubleTapped(object sender, DoubleTappedRoutedEventArgs e)
    {
        if (_current is null) return;
        Point anchor = ZoomAnchorFor(e.GetPosition(Canvas));
        _zoom.ToggleFitActual(anchor.X, anchor.Y);
        SyncPageToView();
        Canvas.Invalidate();
        RaiseZoom();
        _ = MaybeUpgradeResolutionAsync();
    }

    private void OnCanvasSizeChanged(object sender, SizeChangedEventArgs e)
    {
        _zoom.SetViewport(Math.Max(1, e.NewSize.Width - _leftInset - _rightInset), e.NewSize.Height);
        Canvas?.Invalidate();
        ViewChanged?.Invoke();
        HookXamlRootOnce();
        _ = MaybeUpgradeResolutionAsync();
    }

    private void OnLoaded(object sender, RoutedEventArgs e)
    {
        // Loaded is the first reliable point where XamlRoot (hence RasterizationScale) exists. If the image
        // was decoded before this (with the 1.0 fallback DPI), hooking here re-checks and upgrades it.
        HookXamlRootOnce();
    }

    // The needed decode resolution scales with RasterizationScale, which is 1.0 (the fallback) until XamlRoot
    // is attached — so an image decoded early on a scaled display comes out soft. Once XamlRoot exists we hook
    // its Changed event (DPI/monitor moves) AND immediately re-check, since the true scale is now known.
    private void HookXamlRootOnce()
    {
        if (_xamlRootHooked || Canvas?.XamlRoot is not { } xamlRoot)
            return;
        _xamlRootHooked = true;
        xamlRoot.Changed += (_, _) => _ = MaybeUpgradeResolutionAsync();
        _ = MaybeUpgradeResolutionAsync(); // the real DPI just became known — re-evaluate the current image
    }

    /// <summary>
    /// Re-decode the current image at a higher resolution when the viewport, DPI, or zoom level has grown
    /// past what the displayed bitmap was decoded for — otherwise an image opened while the viewport was
    /// small (initial layout, before maximize, with the info panel open, or on a lower-DPI monitor) stays
    /// upscaled and soft, and zooming to 100%+ shows a blocky stretch of the fit-resolution decode.
    /// Debounced so a resize drag or wheel burst coalesces into one decode; keeps the current bitmap on
    /// screen meanwhile and swaps in place without disturbing zoom/pan.
    /// </summary>
    private async Task MaybeUpgradeResolutionAsync()
    {
        // Only ever upgrades something already on screen; the primary load handles the first display.
        if (_currentRef is not { } image || _device is null || _current is null)
            return;

        int want = ComputeNeededBucket();
        if (want <= _displayedHighBucket)
            return; // already decoded at or above the needed resolution

        DiagLog($"UPGRADE want={want} displayed={_displayedHighBucket} vp={FormatViewport()}");

        CancelUpgrade();
        var cts = new CancellationTokenSource();
        _upgradeCts = cts;
        CancellationToken ct = cts.Token;

        try
        {
            await Task.Delay(160, ct); // coalesce a resize drag / layout burst

            CanvasDevice? device = _device;
            if (device is null)
                return;

            int needed = ComputeNeededBucket(); // recompute after the debounce — it may have grown further
            if (needed <= _displayedHighBucket)
                return;

            var key = new CacheKey(image.Path, image.ModifiedTicks, needed);
            DecodedImage sharp;
            if (_cache.TryGet(key, out ICacheValue cached))
            {
                sharp = (DecodedImage)cached;
            }
            else
            {
                ImageFormat format = await SniffAsync(image.Path, ct);
                sharp = await DecodeAsync(image, needed, format, firstFrameOnly: false, device, ct);
                _cache.Insert(key, sharp);
            }

            // Bail (releasing our working reference) if superseded or the user navigated away meanwhile.
            if (ct.IsCancellationRequested
                || !ReferenceEquals(_upgradeCts, cts)
                || _currentRef is not { } current
                || current.Path != image.Path
                || current.ModifiedTicks != image.ModifiedTicks)
            {
                sharp.Release();
                return;
            }

            PublishSharp(sharp, needed, resetZoom: false, "upgrade"); // swap in place, preserving zoom/pan
        }
        catch (OperationCanceledException)
        {
            // superseded by a newer upgrade/navigation
        }
        catch
        {
            // decode failed — keep the current (soft) bitmap rather than erroring
        }
    }

    /// <summary>
    /// Show a sharp decode for the current image, but never replace an already-shown equal-or-higher
    /// resolution one. This makes convergence order-independent: with the primary decode and a resolution
    /// upgrade possibly racing, the display always ends at the highest bucket decoded — a smaller decode that
    /// lands late is dropped (its working reference released) rather than clobbering the sharp one.
    /// </summary>
    private void PublishSharp(DecodedImage image, int bucket, bool resetZoom, string source)
    {
        Looker.Helpers.StartupTrace.Mark($"PublishSharp {source} bucket={bucket}");
        if (bucket <= _displayedHighBucket)
        {
            DiagLog($"skip {source} bucket={bucket} displayed={_displayedHighBucket}");
            image.Release();
            return;
        }

        Display(image, resetZoom);
        _displayedHighBucket = bucket;
        DiagLog($"show {source} bucket={bucket} bmp={image.Bitmap.SizeInPixels.Width}x{image.Bitmap.SizeInPixels.Height} native={image.OrientedNativeSize.Width:0}x{image.OrientedNativeSize.Height:0} vp={FormatViewport()} name={System.IO.Path.GetFileName(_currentRef?.Path ?? string.Empty)}");
    }

    private void CancelUpgrade()
    {
        _upgradeCts?.Cancel();
        _upgradeCts?.Dispose();
        _upgradeCts = null;
    }

    private void OnUnloaded(object sender, RoutedEventArgs e)
    {
        StopAnimation();
        StopCrossFade();
        CancelDisplayDecode();
        CancelUpgrade();
        _preload?.CancelAll();
        DetachPages(_current);
        _current?.Release();
        _current = null;
        _cache.Clear();
        Canvas.RemoveFromVisualTree();
    }

    // --- Animation stepper ---

    /// <summary>Space toggles play/pause for the current animation (no-op for a static image).</summary>
    public void ToggleAnimationPause()
    {
        if (_current is not { IsAnimated: true } || _animationTimer is null)
            return;

        _animationPaused = !_animationPaused;
        if (_animationPaused)
            _animationTimer.Stop();
        else
            ArmNextFrame();
    }

    private void StartOrRestartAnimation()
    {
        StopAnimation();
        _frameIndex = 0;
        _animationPaused = false;

        if (_current is not { IsAnimated: true })
            return;

        _animationTimer = DispatcherQueue.CreateTimer();
        _animationTimer.IsRepeating = false; // re-armed per frame with that frame's own delay
        _animationTimer.Tick += OnAnimationTick;
        ArmNextFrame();
    }

    private void ArmNextFrame()
    {
        if (_animationTimer is null || _current?.Frames is not { Count: > 0 } frames)
            return;

        if (_frameIndex >= frames.Count)
            _frameIndex = 0;

        _animationTimer.Interval = frames[_frameIndex].Delay;
        _animationTimer.Start();
    }

    private void OnAnimationTick(DispatcherQueueTimer sender, object args)
    {
        if (_current?.Frames is not { Count: > 0 } frames)
        {
            StopAnimation();
            return;
        }

        _frameIndex = (_frameIndex + 1) % frames.Count;
        Canvas?.Invalidate();

        if (!_animationPaused)
            ArmNextFrame();
    }

    private void StopAnimation()
    {
        if (_animationTimer is null)
            return;

        _animationTimer.Stop();
        _animationTimer.Tick -= OnAnimationTick;
        _animationTimer = null;
    }

    // --- Display / decode helpers ---

    private void Display(DecodedImage image, bool resetZoom)
    {
        bool swapping = !ReferenceEquals(_current, image);
        if (swapping)
        {
            // Cross-fade (slideshow): retain the outgoing image at its current on-screen rect and dissolve
            // it out over the incoming one. Any non-fade swap instead cancels a fade still in flight so a
            // stale ghost never lingers over a hard-cut navigation.
            if (CrossFadeNext && _current is { } outgoing)
                StartCrossFade(outgoing, _zoom.GetImageRect());
            else
                StopCrossFade();

            DetachPages(_current);
            _current?.Release();
            _current = image; // takes ownership of the caller's working reference
            AttachPages(image);
        }
        CrossFadeNext = false;

        _errorMessage = null;

        if (resetZoom)
        {
            _pageIndex = 0; // a new document opens on its first page; a tier swap (resetZoom false) keeps the page
            Size v = GetViewportSize();
            _zoom.Reset(v.Width, v.Height, image.OrientedNativeSize.Width, image.OrientedNativeSize.Height, GetRasterization(), FocusRectFor(image));
            ViewChanged?.Invoke();
        }

        _displayedRef = _currentRef;
        StartOrRestartAnimation();
        Canvas?.Invalidate();
        DisplayChanged?.Invoke();
    }

    private void ShowError(string message)
    {
        DetachPages(_current);
        _current?.Release();
        _current = null;
        _errorMessage = message;
        DisplayChanged?.Invoke();
    }

    private void SchedulePreload(IReadOnlyList<PreloadItem> preload, int highBucket)
    {
        if (_preload is null)
            return;

        var keys = new List<CacheKey>(preload.Count);
        foreach (PreloadItem item in preload)
        {
            int bucket = item.Tier == PreloadTier.High ? highBucket : LowResBucket;
            keys.Add(new CacheKey(item.Image.Path, item.Image.ModifiedTicks, bucket));
        }

        // An empty list cancels everything in flight (all fell out of the window).
        _preload.Schedule(keys);
    }

    private async Task PreloadOneAsync(CacheKey key, CancellationToken ct)
    {
        CanvasDevice? device = _device;
        if (device is null)
            return;

        var image = new ImageRef(key.Path, key.ModifiedTicks);
        ImageFormat format = await SniffAsync(image.Path, ct);
        // The low-res ring is a static first frame at the placeholder resolution; the sharp tier decodes at
        // the (positive) target bucket and preloads full animations.
        bool isLow = key.Bucket == LowResBucket;
        int target = isLow ? LowResTargetDim : key.Bucket;
        DecodedImage decoded = await DecodeAsync(image, target, format, firstFrameOnly: isLow, device, ct);
        _cache.Insert(key, decoded);
        decoded.Release(); // the cache holds it now
    }

    private static async Task<DecodedImage> DecodeAsync(ImageRef image, int targetBox, ImageFormat format, bool firstFrameOnly, CanvasDevice device, CancellationToken ct)
    {
        var request = new DecodeRequest(image.Path, image.ModifiedTicks, targetBox, targetBox, FullResolution: false, firstFrameOnly, ct);
        IImageDecoder decoder = await StartupWarmup.DecoderAsync;
        // Run the whole chain on the pool: called from the UI thread, the decoders' awaits would otherwise
        // resume there (WinRT async completions, DPI fix-up, bitmap hand-off) and compete with layout/input.
        DecodedImage? decoded = await Task.Run(() => decoder.DecodeAsync(request, format, device), ct);
        return decoded ?? throw new InvalidOperationException("No decoder could read this file");
    }

    private void CancelDisplayDecode()
    {
        _loading = null;
        _displayCts?.Cancel();
        _displayCts?.Dispose();
        _displayCts = null;
    }

    private bool Superseded(CancellationTokenSource cts)
        => cts.IsCancellationRequested || !ReferenceEquals(_displayCts, cts);

    private int ComputeHighBucket()
    {
        Size v = GetViewportSize();
        double ras = GetRasterization();
        int maxDim = (int)Math.Ceiling(Math.Max(v.Width, v.Height) * ras);
        return CacheKey.BucketFor(Math.Max(1, maxDim));
    }

    /// <summary>
    /// The bucket the current image actually needs right now: the fit-to-viewport bucket, raised to the
    /// on-screen drawn size when zoomed past fit (so 100%/wheel zoom gets a true-resolution decode instead
    /// of a blocky stretch), capped at the image's native size — decoding past native is pure waste.
    /// Animations stay at the fit bucket: re-decoding every frame at zoom resolution would explode memory.
    /// </summary>
    private int ComputeNeededBucket()
    {
        int fit = ComputeHighBucket();
        DecodedImage? current = _current;
        if (current is null || current.IsAnimated)
            return fit;

        // The unit the bucket sizes is one page for a PDF (the strip itself is never one bitmap), else the image.
        double unitMax = current.Pages is { } pages
            ? Math.Max(pages.Layout.MaxPageWidth, pages.Layout.MaxPageHeight)
            : Math.Max(current.OrientedNativeSize.Width, current.OrientedNativeSize.Height);
        double drawnMax = unitMax * _zoom.Scale * GetRasterization();
        double cap = current.IsVector ? VectorMaxDimension : unitMax;
        int zoomTarget = (int)Math.Ceiling(Math.Min(drawnMax, cap));
        if (zoomTarget <= 0)
            return fit;

        return Math.Max(fit, CacheKey.BucketFor(zoomTarget));
    }

    private static Task<ImageFormat> SniffAsync(string path, CancellationToken ct)
        => Task.Run(() => SniffCoreAsync(path, ct), ct);

    private static async Task<ImageFormat> SniffCoreAsync(string path, CancellationToken ct)
    {
        try
        {
            using FileStream fs = File.OpenRead(path);
            byte[] buffer = new byte[FormatSniffer.HeaderSize];
            int read = await fs.ReadAsync(buffer.AsMemory(0, buffer.Length), ct);
            return FormatSniffer.Sniff(buffer.AsSpan(0, read), Path.GetExtension(path));
        }
        catch (OperationCanceledException)
        {
            throw;
        }
        catch
        {
            return FormatSniffer.Sniff(ReadOnlySpan<byte>.Empty, Path.GetExtension(path));
        }
    }

    // Width reserved on the right for the floating info panel and on the left for the file explorer card: the
    // canvas (and its checkerboard) still spans the full column, but fit/zoom math treats the viewport as the
    // space between the cards, so the image centres in the visible area instead of disappearing under one.
    // The zoom controller works in that inner space; DrawCore shifts its output right by LeftInset and the
    // pointer handlers shift input left by the same amount.
    private double _rightInset;
    private double _leftInset;

    public double RightInset
    {
        get => _rightInset;
        set
        {
            if (_rightInset == value)
                return;
            _rightInset = value;
            OnInsetsChanged();
        }
    }

    public double LeftInset
    {
        get => _leftInset;
        set
        {
            if (_leftInset == value)
                return;
            _leftInset = value;
            OnInsetsChanged();
        }
    }

    private void OnInsetsChanged()
    {
        Size v = GetViewportSize();
        _zoom.SetViewport(v.Width, v.Height);
        ViewChanged?.Invoke();
        Canvas?.Invalidate();
        _ = MaybeUpgradeResolutionAsync();
    }

    private Size GetViewportSize()
    {
        Size s = Canvas?.Size ?? default;
        double w = s.Width > 0 ? s.Width : 1280;
        double h = s.Height > 0 ? s.Height : 800;
        return new Size(Math.Max(1, w - _leftInset - _rightInset), h);
    }

    private double GetRasterization() => Canvas?.XamlRoot?.RasterizationScale ?? 1.0;

    // --- Temporary decode diagnostics (remove once the "opens soft" bug is confirmed fixed) ---

    private static readonly string DiagPath = System.IO.Path.Combine(System.IO.Path.GetTempPath(), "looker-decode.log");

    private static void DiagLog(string message)
    {
        try { System.IO.File.AppendAllText(DiagPath, $"{DateTime.Now:HH:mm:ss.fff} {message}\n"); }
        catch { /* diagnostics are best-effort */ }
    }

    private string FormatViewport()
    {
        Size v = GetViewportSize();
        return $"{v.Width:0}x{v.Height:0}@{GetRasterization():0.##}";
    }

    private static void DrawStatus(CanvasDrawingSession ds, Size viewport, string text)
    {
        using var format = new CanvasTextFormat
        {
            FontSize = 14,
            HorizontalAlignment = CanvasHorizontalAlignment.Center,
            VerticalAlignment = CanvasVerticalAlignment.Center,
        };
        var box = new Rect(0, 0, viewport.Width, viewport.Height);
        ds.DrawText(text, box, Color.FromArgb(0x99, 0x88, 0x88, 0x88), format);
    }
}
