using System;
using System.IO;
using System.Numerics;
using System.Runtime.InteropServices.WindowsRuntime;
using System.Threading;
using System.Threading.Tasks;
using ImageMagick;
using Looker.Imaging;
using Looker.ViewModels;
using Microsoft.Graphics.Canvas;
using Microsoft.Graphics.Canvas.Svg;
using Microsoft.UI.Xaml.Media;
using Microsoft.UI.Xaml.Media.Imaging;
using Windows.Foundation;
using Windows.Graphics.Imaging;
using Windows.Storage;
using Windows.Storage.FileProperties;
using Windows.Storage.Streams;
using Windows.UI;

namespace Looker.Controls;

/// <summary>
/// Fills a <see cref="ThumbnailItem"/>. SVGs are rendered directly with Win2D (<see cref="LoadSvgAsync"/>):
/// the same <see cref="CanvasSvgDocument"/> renderer the viewer uses, a few ms on the GPU, and no dependency
/// on a third-party shell provider. Everything else comes from the Windows shell thumbnail cache in two
/// phases: a cache-only fast pass (<see cref="ThumbnailOptions.ReturnOnlyIfCached"/>) that returns instantly
/// for already-seen files, then a full extraction on a miss. The shell providers give free thumbnails for
/// RAW/HEIC and everything else the OS can render. Formats the shell has no handler for (JPEG XL, PSD, TGA,
/// DDS — it hands back a generic file icon) fall back to a small Magick.NET decode on a worker thread,
/// encoded to PNG and fed to a <see cref="BitmapImage"/>. Cancellable per container recycle.
/// </summary>
internal static class ThumbnailLoader
{
    // Minimum box for our own (Magick) thumbnails; the caller's requested size wins when larger.
    private const uint FallbackDecodeBox = 240;
    private const uint SvgMinBox = 64;

    // Shell thumbnail extraction (especially the full, uncached pass on RAW/HEIC/PSD) can be heavy and
    // routes through third-party shell providers. A hard row of freshly realized containers can ask for
    // a dozen at once; gate them so we never storm the shell with the whole visible page simultaneously.
    private static readonly SemaphoreSlim Gate = new(4, 4);

    /// <param name="pixelSize">Requested thumbnail edge in physical pixels (the strip derives it from its cell
    /// width and the current DPI, so larger cells fetch larger thumbnails).</param>
    public static async Task LoadAsync(ThumbnailItem item, uint pixelSize, CancellationToken ct)
    {
        StorageFile file;
        try
        {
            file = await StorageFile.GetFileFromPathAsync(item.Path);
        }
        catch
        {
            return; // gone, or not reachable by path — leave the empty cell
        }
        ct.ThrowIfCancellationRequested();

        await Gate.WaitAsync(ct);
        try
        {
            ImageSource? bitmap = IsSvg(item.Path) ? await LoadSvgAsync(item.Path, pixelSize, ct)
                : IsPdf(item.Path) ? await LoadPdfAsync(item.Path, pixelSize, ct)
                : null;
            bitmap ??= await LoadFromShellAsync(file, pixelSize, ct)
                ?? await LoadViaDecoderAsync(item.Path, pixelSize, ct);

            ct.ThrowIfCancellationRequested();
            if (bitmap is not null)
                item.Thumbnail = bitmap;
        }
        finally
        {
            Gate.Release();
        }
    }

    private static bool IsSvg(string path) =>
        string.Equals(Path.GetExtension(path), ".svg", StringComparison.OrdinalIgnoreCase);

    private static bool IsPdf(string path) =>
        string.Equals(Path.GetExtension(path), ".pdf", StringComparison.OrdinalIgnoreCase);

    /// <summary>
    /// The first page of a PDF through the inbox Windows.Data.Pdf renderer at cell size (on white, like the
    /// viewer). Deterministic and a few ms, whereas the shell's PDF handler depends on whichever PDF app is
    /// installed. Null on any failure so the shell chain still gets a go.
    /// </summary>
    private static async Task<ImageSource?> LoadPdfAsync(string path, uint pixelSize, CancellationToken ct)
    {
        byte[]? png;
        try
        {
            png = await Task.Run(async () =>
            {
                (Windows.Data.Pdf.PdfDocument document, IDisposable backing) = await PdfPageSet.OpenAsync(path, ct);
                using (backing)
                {
                    if (document.PageCount == 0)
                        return null;
                    using Windows.Data.Pdf.PdfPage page = document.GetPage(0);
                    (uint w, uint h) = PdfPageSet.DestinationSize(page.Size.Width, page.Size.Height, (int)Math.Max(pixelSize, SvgMinBox));
                    using var stream = new InMemoryRandomAccessStream();
                    var options = new Windows.Data.Pdf.PdfPageRenderOptions
                    {
                        DestinationWidth = w,
                        DestinationHeight = h,
                        BackgroundColor = Color.FromArgb(255, 255, 255, 255),
                    };
                    await page.RenderToStreamAsync(stream, options).AsTask(ct);
                    ct.ThrowIfCancellationRequested();
                    var bytes = new byte[stream.Size];
                    stream.Seek(0);
                    using Stream input = stream.AsStreamForRead();
                    await input.ReadExactlyAsync(bytes, ct);
                    return bytes;
                }
            }, ct);
        }
        catch (OperationCanceledException)
        {
            throw;
        }
        catch
        {
            return null; // encrypted, corrupt, … — let the shell have a go
        }
        if (png is null)
            return null;

        ct.ThrowIfCancellationRequested();
        var bitmap = new BitmapImage();
        using var ms = new MemoryStream(png);
        await bitmap.SetSourceAsync(ms.AsRandomAccessStream());
        return bitmap;
    }

    /// <summary>
    /// SVG rendered by Win2D at cell size on a transparent background, so the cell shows through like it does
    /// for a transparent PNG and the thumbnail matches the viewport (Explorer/PowerToys and Magick render on
    /// white instead). Null on any failure so the shell/Magick chain runs.
    /// </summary>
    private static async Task<ImageSource?> LoadSvgAsync(string path, uint pixelSize, CancellationToken ct)
    {
        byte[]? pixels = null;
        int width = 0, height = 0;
        try
        {
            await Task.Run(async () =>
            {
                CanvasDevice device = CanvasDevice.GetSharedDevice();
                if (!CanvasSvgDocument.IsSupported(device))
                    return;

                using FileStream fs = File.OpenRead(path);
                using IRandomAccessStream ras = fs.AsRandomAccessStream();
                using CanvasSvgDocument svg = await CanvasSvgDocument.LoadAsync(device, ras).AsTask(ct);

                double box = Math.Max(pixelSize, SvgMinBox);
                Size intrinsic = SvgDecoder.ReadIntrinsicSize(svg);
                double baseW = intrinsic.Width > 0 ? intrinsic.Width : box;
                double baseH = intrinsic.Height > 0 ? intrinsic.Height : box;
                double scale = Math.Min(box / baseW, box / baseH);
                if (scale <= 0 || double.IsInfinity(scale) || double.IsNaN(scale))
                    scale = 1.0;
                int w = Math.Max(1, (int)Math.Round(baseW * scale));
                int h = Math.Max(1, (int)Math.Round(baseH * scale));

                ct.ThrowIfCancellationRequested();
                using var target = new CanvasRenderTarget(device, w, h, 96);
                using (CanvasDrawingSession ds = target.CreateDrawingSession())
                {
                    ds.Clear(Color.FromArgb(0, 0, 0, 0));
                    // DrawSvg's viewport does not scale a document with an absolute width/height (it would
                    // render at intrinsic size and crop to the target), so scale the drawing session instead.
                    ds.Transform = Matrix3x2.CreateScale((float)scale);
                    ds.DrawSvg(svg, new Size(baseW, baseH));
                }
                pixels = target.GetPixelBytes();
                width = w;
                height = h;
            }, ct);
        }
        catch (OperationCanceledException)
        {
            throw;
        }
        catch
        {
            return null; // unsupported SVG, device lost, … — let the shell / Magick have a go
        }
        if (pixels is null)
            return null;

        ct.ThrowIfCancellationRequested();
        return await ToSourceAsync(pixels, width, height);
    }

    /// <summary>Premultiplied top-down BGRA → <see cref="SoftwareBitmapSource"/>. UI thread only.</summary>
    private static async Task<ImageSource> ToSourceAsync(byte[] premultipliedBgra, int width, int height)
    {
        var software = new SoftwareBitmap(BitmapPixelFormat.Bgra8, width, height, BitmapAlphaMode.Premultiplied);
        software.CopyFromBuffer(premultipliedBgra.AsBuffer());
        var source = new SoftwareBitmapSource();
        await source.SetBitmapAsync(software); // LoadAsync is awaited from the dispatcher, so this is the UI thread
        return source;
    }

    /// <summary>The shell thumbnail, or null when the shell has nothing better than a generic icon.</summary>
    private static async Task<ImageSource?> LoadFromShellAsync(StorageFile file, uint pixelSize, CancellationToken ct)
    {
        // A cache miss is NOT reported as null: ReturnOnlyIfCached hands back the file-type icon
        // (ThumbnailType.Icon) instead, so it has to be treated as a miss explicitly or the full
        // extraction below never runs and every uncached file silently ends up in the Magick fallback.
        StorageItemThumbnail? thumb = await TryGet(file, pixelSize, ThumbnailOptions.ReturnOnlyIfCached, ct);
        if (thumb is not null && thumb.Type != ThumbnailType.Image)
        {
            thumb.Dispose();
            thumb = null;
        }
        thumb ??= await TryGet(file, pixelSize, ThumbnailOptions.ResizeThumbnail, ct);

        if (thumb is null)
            return null;

        using (thumb)
        {
            ct.ThrowIfCancellationRequested();
            if (thumb.Type != ThumbnailType.Image || thumb.Size == 0)
                return null; // ThumbnailType.Icon = the shell had no handler and offered the file-type icon

            var bitmap = new BitmapImage { DecodePixelWidth = (int)pixelSize };
            try
            {
                if (string.Equals(thumb.ContentType, "image/bmp", StringComparison.OrdinalIgnoreCase))
                {
                    // The small cache tiers are 32-bit BMPs whose alpha WIC gets wrong both ways (dropped
                    // for the header type the cache uses on some tiers, all-zero from the PowerToys SVG
                    // provider), so decode them ourselves — see ThumbnailBmp.
                    var bytes = new byte[thumb.Size];
                    using (Stream input = thumb.AsStreamForRead())
                        await input.ReadExactlyAsync(bytes, ct);
                    if (ThumbnailBmp.TryDecodeBgra32(bytes, out int w, out int h, out byte[] bgra))
                        return await ToSourceAsync(bgra, w, h);
                    using var raw = new MemoryStream(bytes);
                    await bitmap.SetSourceAsync(raw.AsRandomAccessStream());
                }
                else
                {
                    await bitmap.SetSourceAsync(thumb);
                }
            }
            catch (OperationCanceledException)
            {
                throw;
            }
            catch
            {
                return null; // undecodable thumbnail stream — try our own decoder
            }
            return bitmap;
        }
    }

    /// <summary>
    /// Our own decode for files the shell can't thumbnail. Magick handles every format in the router's
    /// fallback chain; the result is shrunk to <see cref="FallbackDecodeBox"/> and PNG-encoded on the
    /// worker thread so the UI thread only pays for a tiny BitmapImage decode.
    /// </summary>
    private static async Task<BitmapImage?> LoadViaDecoderAsync(string path, uint pixelSize, CancellationToken ct)
    {
        byte[]? png;
        try
        {
            png = await Task.Run(() => RenderPngThumbnail(path, Math.Max(FallbackDecodeBox, pixelSize), ct), ct);
        }
        catch (OperationCanceledException)
        {
            throw;
        }
        catch
        {
            return null; // undecodable here too — leave the cell empty
        }
        if (png is null)
            return null;

        ct.ThrowIfCancellationRequested();
        var bitmap = new BitmapImage();
        using var stream = new MemoryStream(png);
        await bitmap.SetSourceAsync(stream.AsRandomAccessStream());
        return bitmap;
    }

    private static byte[]? RenderPngThumbnail(string path, uint box, CancellationToken ct)
    {
        ct.ThrowIfCancellationRequested();
        using var image = new MagickImage(path); // layered files (PSD) read as the merged composite
        ct.ThrowIfCancellationRequested();
        if (image.Width == 0 || image.Height == 0)
            return null;

        image.AutoOrient();
        image.Thumbnail(box, box); // fits inside the box, keeps aspect, strips metadata
        if (image.ColorSpace != ColorSpace.sRGB)
            image.ColorSpace = ColorSpace.sRGB;
        image.Format = MagickFormat.Png;
        ct.ThrowIfCancellationRequested();
        return image.ToByteArray();
    }

    // SingleItem, not PicturesView: every mode except SingleItem hands back a thumbnail already CROPPED to a
    // uniform aspect, so the strip could never show the whole image no matter how the Image is stretched.
    private static async Task<StorageItemThumbnail?> TryGet(StorageFile file, uint pixelSize, ThumbnailOptions options, CancellationToken ct)
    {
        try
        {
            StorageItemThumbnail thumb = await file.GetThumbnailAsync(ThumbnailMode.SingleItem, pixelSize, options);
            ct.ThrowIfCancellationRequested();
            return thumb;
        }
        catch (OperationCanceledException)
        {
            throw;
        }
        catch
        {
            return null;
        }
    }
}
