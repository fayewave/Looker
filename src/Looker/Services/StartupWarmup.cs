using System;
using System.IO;
using System.Threading;
using System.Threading.Tasks;
using Looker.Helpers;
using Looker.Imaging;
using Microsoft.Graphics.Canvas;
using Windows.Storage;

namespace Looker.Services;

/// <summary>
/// Cold-start overlap. Everything the first frame needs that does <em>not</em> have to happen on the UI
/// thread is kicked off from <c>Main</c> onto the thread pool, so it runs while the WinAppSDK runtime, the
/// XAML runtime and the window are being constructed (a ~300 ms UI-thread-bound stretch that otherwise
/// leaves every other core idle):
/// <list type="bullet">
///   <item>the shared Win2D <see cref="CanvasDevice"/> (D3D11 + D2D device creation, ~30 ms) — the
///     <c>CanvasControl</c> picks the same shared device up in <c>CreateResources</c>;</item>
///   <item>the <see cref="DecoderRouter"/> with its WIC <see cref="CodecInventory"/> enumeration;</item>
///   <item>the <see cref="ApplicationData"/> settings store (first open is a ~20 ms hive load);</item>
///   <item>and, when launched with a file, the file's own low-res + fit-resolution decode, targeted at the
///     saved window size — so the pixels are normally ready before the window even exists and the
///     viewport's first draw is the sharp image, not a blank surface.</item>
/// </list>
/// The viewport takes the prepared image once via <see cref="TakePrepared"/>; if the guessed bucket is off
/// (window size changed, monitor DPI) it simply becomes a cache miss and the normal decode path runs.
/// </summary>
public static class StartupWarmup
{
    // Matches ImageViewport.LowResTargetDim: the instant placeholder resolution.
    public const int LowResTargetDim = 1280;
    public const int LowResBucket = -1;

    private static readonly TaskCompletionSource<CanvasDevice> DeviceGate = new(TaskCreationOptions.RunContinuationsAsynchronously);
    private static int _deviceStarted;
    private static Task<IImageDecoder>? _decoder;
    private static PreparedImage? _initial;

    /// <summary>The shared Win2D device — the same object the <c>CanvasControl</c> uses (<c>UseSharedDevice</c>
    /// is the default) — once <see cref="StartDevice"/> has been called.</summary>
    public static Task<CanvasDevice> SharedDeviceAsync => DeviceGate.Task;

    /// <summary>The app-wide decoder (WIC → Magick chain), built off-thread.</summary>
    public static Task<IImageDecoder> DecoderAsync
        => _decoder ??= Task.Run(() =>
        {
            var router = new DecoderRouter(CodecInventory.Create());
            StartupTrace.Mark("warmup: decoder + WIC inventory ready");
            return (IImageDecoder)router;
        });

    /// <summary>Start the light warm-ups (settings store, WIC inventory). Call once, as early in <c>Main</c> as possible.</summary>
    public static void Begin()
    {
        _ = DecoderAsync;
        _ = Task.Run(() =>
        {
            try { _ = ApplicationData.Current.LocalSettings; }
            catch { /* no package identity: SettingsService degrades to defaults anyway */ }
            StartupTrace.Mark("warmup: settings store opened");
        });
    }

    /// <summary>
    /// Create the shared Win2D device on the pool. Idempotent. Deliberately <em>not</em> started from Main: the
    /// first D3D device in the process loads the GPU driver (tens of MB) under the loader lock, and doing that
    /// while the UI thread is itself loading the XAML/Windowing DLLs just stalls the UI thread (measured as
    /// +40 ms on the title-bar step). The window constructor calls this once its own DLL-heavy stretch is over,
    /// which still leaves ~100 ms before the Win2D control asks for the device.
    /// </summary>
    public static void StartDevice()
    {
        if (Interlocked.Exchange(ref _deviceStarted, 1) != 0)
            return;
        _ = Task.Run(() =>
        {
            try
            {
                CanvasDevice device = CanvasDevice.GetSharedDevice();
                StartupTrace.Mark("warmup: shared Win2D device created");
                DeviceGate.TrySetResult(device);
            }
            catch (Exception ex)
            {
                DeviceGate.TrySetException(ex);
            }
        });
    }

    /// <summary>Begin decoding the file the app was launched with, before any window exists.</summary>
    public static void BeginInitialDecode(string path)
    {
        long modified;
        try { modified = File.GetLastWriteTimeUtc(path).Ticks; }
        catch { return; }

        var image = new ImageRef(path, modified);
        var cts = new CancellationTokenSource();
        var prepared = new PreparedImage(image, cts);
        prepared.Start();
        _initial = prepared;
    }

    /// <summary>Take (once) the prepared decode for <paramref name="image"/>, or null if none matches.
    /// A prepared image for a different file (the user navigated before the first frame) is discarded.</summary>
    public static PreparedImage? TakePrepared(ImageRef image)
    {
        PreparedImage? prepared = Interlocked.Exchange(ref _initial, null);
        if (prepared is null)
            return null;
        if (prepared.Image.Path == image.Path && prepared.Image.ModifiedTicks == image.ModifiedTicks)
            return prepared;
        prepared.Discard();
        return null;
    }

    /// <summary>Guess the fit-resolution cache bucket the viewport will ask for, from the saved window
    /// placement (physical pixels, like the viewport's DIP × RasterizationScale). Falls back to the
    /// low-res size when nothing is saved; the viewport upgrades if the guess is low.</summary>
    private static int PredictFitBucket()
    {
        try
        {
            var settings = new SettingsService();
            if (settings.WindowPlacement is { } p && p.Width > 0 && p.Height > 0)
            {
                int w = p.Width, h = p.Height;
                if (p.Maximized)
                {
                    // Maximized fills the work area of the monitor the saved rectangle is on.
                    var rect = new Windows.Graphics.RectInt32(p.X, p.Y, p.Width, p.Height);
                    Windows.Graphics.RectInt32 work = Microsoft.UI.Windowing.DisplayArea
                        .GetFromRect(rect, Microsoft.UI.Windowing.DisplayAreaFallback.Nearest).WorkArea;
                    if (work.Width > 0 && work.Height > 0) { w = work.Width; h = work.Height; }
                }
                // The viewport is the window minus the chrome that will be showing (MainWindow.xaml: 48px title
                // bar, the saved floating info card width, the saved strip height + margins). DIP constants against physical pixels
                // are close enough here: the bucket is quantized to 256px steps.
                h -= 48;
                h -= 40; // the toolbar row (32px buttons + 8px padding below)
                h -= 28; // the status row under the strip (MainWindow.xaml row 4) shows whenever an image does
                if (settings.InfoVisible) w -= settings.InfoWidth; // InfoPanel.PanelWidth (card + margins)
                if (settings.ExplorerVisible) w -= settings.ExplorerWidth; // FileExplorer.PanelWidth, same shape
                if (settings.StripVisible) h -= settings.StripHeight + 10;
                return CacheKey.BucketFor(Math.Max(Math.Max(w, h), 1));
            }
        }
        catch { /* fall through */ }
        return CacheKey.BucketFor(LowResTargetDim);
    }

    /// <summary>A launch image decoded ahead of the window: the low-res placeholder and the fit-resolution
    /// bitmap (at <see cref="SharpBucket"/>), both on the shared device. Ownership of each successfully
    /// decoded image passes to whoever awaits it; <see cref="Discard"/> releases anything unclaimed.</summary>
    public sealed class PreparedImage
    {
        private readonly CancellationTokenSource _cts;

        internal PreparedImage(ImageRef image, CancellationTokenSource cts)
        {
            Image = image;
            _cts = cts;
        }

        public ImageRef Image { get; }
        public int SharpBucket { get; private set; }
        public CanvasDevice? Device { get; private set; }
        public Task<DecodedImage?> Low { get; private set; } = Task.FromResult<DecodedImage?>(null);
        public Task<DecodedImage?> Sharp { get; private set; } = Task.FromResult<DecodedImage?>(null);

        internal void Start()
        {
            SharpBucket = 0;
            var ready = new TaskCompletionSource<(CanvasDevice, IImageDecoder, ImageFormat)>();
            Low = DecodeTierAsync(ready.Task, LowResTargetDim, firstFrameOnly: true);
            Sharp = DecodeTierAsync(ready.Task, 0, firstFrameOnly: false);

            _ = Task.Run(async () =>
            {
                try
                {
                    StartupTrace.Mark("warmup: initial decode begins");
                    CancellationToken ct = _cts.Token;
                    SharpBucket = PredictFitBucket();
                    ImageFormat format = await SniffAsync(Image.Path, ct).ConfigureAwait(false);
                    CanvasDevice device = await SharedDeviceAsync.ConfigureAwait(false);
                    IImageDecoder decoder = await DecoderAsync.ConfigureAwait(false);
                    Device = device;
                    StartupTrace.Mark($"warmup: device + decoder ready, format={format}, sharp bucket={SharpBucket}");
                    ready.SetResult((device, decoder, format));
                }
                catch (Exception ex)
                {
                    ready.SetException(ex);
                }
            });
        }

        private async Task<DecodedImage?> DecodeTierAsync(Task<(CanvasDevice, IImageDecoder, ImageFormat)> ready, int target, bool firstFrameOnly)
        {
            try
            {
                (CanvasDevice device, IImageDecoder decoder, ImageFormat format) = await ready.ConfigureAwait(false);
                int box = target > 0 ? target : SharpBucket;
                var request = new DecodeRequest(Image.Path, Image.ModifiedTicks, box, box, FullResolution: false, firstFrameOnly, _cts.Token);
                DecodedImage? decoded = await decoder.DecodeAsync(request, format, device).ConfigureAwait(false);
                StartupTrace.Mark($"warmup: {(firstFrameOnly ? "low" : "sharp")} decode done ({(decoded is null ? "null" : "ok")})");
                return decoded;
            }
            catch
            {
                return null; // the viewport's own decode reports errors; the warm-up just doesn't help
            }
        }

        /// <summary>Cancel what is still running and release whatever finished.</summary>
        public void Discard()
        {
            _cts.Cancel();
            ReleaseWhenDone(Low);
            ReleaseWhenDone(Sharp);
        }

        private static void ReleaseWhenDone(Task<DecodedImage?> task)
            => task.ContinueWith(t => t.Result?.Release(), TaskContinuationOptions.OnlyOnRanToCompletion | TaskContinuationOptions.ExecuteSynchronously);

        private static async Task<ImageFormat> SniffAsync(string path, CancellationToken ct)
        {
            using FileStream fs = File.OpenRead(path);
            byte[] buffer = new byte[FormatSniffer.HeaderSize];
            int read = await fs.ReadAsync(buffer.AsMemory(0, buffer.Length), ct).ConfigureAwait(false);
            return FormatSniffer.Sniff(buffer.AsSpan(0, read), Path.GetExtension(path));
        }
    }
}
