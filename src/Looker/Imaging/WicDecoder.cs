using System;
using System.IO;
using System.Threading;
using System.Threading.Tasks;
using Microsoft.Graphics.Canvas;
using Windows.Foundation;
using Windows.Graphics.Imaging;
using Windows.Storage.Streams;

namespace Looker.Imaging;

/// <summary>
/// The primary decoder: Windows Imaging Component via <see cref="BitmapDecoder"/>. Decodes
/// scaled-to-target (near-free for JPEG's native DCT scaling), respects EXIF orientation, color
/// manages to sRGB, and normalizes to Bgra8 premultiplied. The GPU bitmap is built off the UI
/// thread. Files are opened with plain <see cref="System.IO"/> — the app is runFullTrust, so no
/// StorageFile broker is needed for arbitrary paths.
/// </summary>
public sealed class WicDecoder : IImageDecoder
{
    private readonly CodecInventory? _inventory;

    public WicDecoder(CodecInventory? inventory = null) => _inventory = inventory;

    public bool CanDecode(ImageFormat format) => format switch
    {
        ImageFormat.Jpeg or ImageFormat.Png or ImageFormat.Bmp or ImageFormat.Gif
            or ImageFormat.Tiff or ImageFormat.Webp or ImageFormat.Ico or ImageFormat.JpegXr or ImageFormat.Unknown => true,
        _ => false,
    };

    public async Task<DecodedImage?> DecodeAsync(DecodeRequest request, ImageFormat format, CanvasDevice device)
    {
        CancellationToken ct = request.Cancellation;

        using FileStream fs = File.OpenRead(request.Path);
        using IRandomAccessStream ras = fs.AsRandomAccessStream();

        // Camera RAW: left to its own sniffing WIC picks the inbox DNG/TIFF decoder, which returns the
        // small embedded preview. Ask for the Raw Image Extension codec explicitly when it is installed.
        BitmapDecoder decoder = format == ImageFormat.Raw
            && _inventory is not null
            && _inventory.TryGetDecoderId(Path.GetExtension(request.Path), CodecInventory.RawImageDecoderId, out Guid decoderId)
                ? await BitmapDecoder.CreateAsync(decoderId, ras).AsTask(ct)
                : await BitmapDecoder.CreateAsync(ras).AsTask(ct);

        uint orientedW = decoder.OrientedPixelWidth;
        uint orientedH = decoder.OrientedPixelHeight;
        if (orientedW == 0 || orientedH == 0)
            return null;

        var transform = new BitmapTransform();
        bool scaled = false;
        if (!request.FullResolution && request.TargetWidth > 0 && request.TargetHeight > 0)
        {
            double fit = Math.Min((double)request.TargetWidth / orientedW, (double)request.TargetHeight / orientedH);
            if (fit < 1.0)
            {
                // The scale ratio is orientation-independent, but the transform runs *before* EXIF
                // orientation in the WIC pipeline, so apply it to the unoriented pixel dimensions.
                transform.ScaledWidth = (uint)Math.Max(1.0, Math.Round(decoder.PixelWidth * fit));
                transform.ScaledHeight = (uint)Math.Max(1.0, Math.Round(decoder.PixelHeight * fit));
                transform.InterpolationMode = BitmapInterpolationMode.Fant;
                scaled = true;
            }
        }

        SoftwareBitmap software = await decoder.GetSoftwareBitmapAsync(
            BitmapPixelFormat.Bgra8,
            BitmapAlphaMode.Premultiplied,
            transform,
            ExifOrientationMode.RespectExifOrientation,
            ColorManagementMode.ColorManageToSRgb).AsTask(ct);

        ct.ThrowIfCancellationRequested();

        // The decoder propagates the file's print-resolution metadata (e.g. a 300 dpi JFIF/EXIF tag) into
        // the SoftwareBitmap, and CanvasBitmap inherits it — Win2D then DPI-compensates such bitmaps at
        // draw time, visibly degrading the render (a 300 dpi file draws pixelated next to a 96 dpi twin
        // with identical pixels). The viewer works purely in pixels (fit/zoom do their own DPI math), so
        // every display bitmap must be 1 pixel = 1 DIP.
        software.DpiX = 96;
        software.DpiY = 96;

        CanvasBitmap bitmap;
        try
        {
            // CanvasBitmap is thread-agile: build the GPU resource off the UI thread.
            bitmap = await Task.Run(() => CanvasBitmap.CreateFromSoftwareBitmap(device, software), ct);
        }
        finally
        {
            software.Dispose();
        }

        if (ct.IsCancellationRequested)
        {
            bitmap.Dispose();
            throw new OperationCanceledException(ct);
        }

        return new DecodedImage
        {
            Bitmap = bitmap,
            OrientedNativeSize = new Size(orientedW, orientedH),
            IsFullResolution = !scaled,
            Format = format,
        };
    }
}
