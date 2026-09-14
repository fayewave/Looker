using System;
using System.Threading;
using System.Threading.Tasks;
using ImageMagick;
using Microsoft.Graphics.Canvas;
using Windows.Foundation;
using Windows.Graphics.DirectX;

namespace Looker.Imaging;

/// <summary>
/// The universal fallback decoder (ImageMagick via Magick.NET). Handles what WIC can't on a given
/// machine — PSD, TGA, DDS, JPEG XL, and HEIC/AVIF/RAW when the Store codec pack is missing. Slower
/// and heavier than WIC, so the router only reaches it when WIC declines or fails. Runs entirely on
/// a worker thread.
/// </summary>
public sealed class MagickDecoder : IImageDecoder
{
    public bool CanDecode(ImageFormat format) => true; // fallback: willing to attempt anything

    public Task<DecodedImage?> DecodeAsync(DecodeRequest request, ImageFormat format, CanvasDevice device)
        => Task.Run(() => DecodeCore(request, format, device), request.Cancellation);

    private static DecodedImage? DecodeCore(DecodeRequest request, ImageFormat format, CanvasDevice device)
    {
        CancellationToken ct = request.Cancellation;
        ct.ThrowIfCancellationRequested();

        // A single MagickImage read of a layered file (e.g. PSD) yields the merged composite.
        using var image = new MagickImage(request.Path);
        ct.ThrowIfCancellationRequested();

        image.AutoOrient();
        if (image.ColorSpace != ColorSpace.sRGB)
            image.ColorSpace = ColorSpace.sRGB; // transforms CMYK/other into sRGB

        uint nativeW = image.Width;
        uint nativeH = image.Height;
        if (nativeW == 0 || nativeH == 0)
            return null;

        // Scale down to fit the target (never upscale at decode).
        if (!request.FullResolution && request.TargetWidth > 0 && request.TargetHeight > 0)
        {
            double fit = Math.Min((double)request.TargetWidth / nativeW, (double)request.TargetHeight / nativeH);
            if (fit < 1.0)
            {
                var geometry = new MagickGeometry(
                    (uint)Math.Max(1, Math.Round(nativeW * fit)),
                    (uint)Math.Max(1, Math.Round(nativeH * fit)))
                {
                    IgnoreAspectRatio = true,
                };
                image.Resize(geometry);
            }
        }
        ct.ThrowIfCancellationRequested();

        image.Alpha(AlphaOption.Set); // guarantee a defined alpha channel for BGRA output

        uint width = image.Width;
        uint height = image.Height;
        using IPixelCollection<byte> pixels = image.GetPixels();
        byte[] bgra = pixels.ToByteArray(PixelMapping.BGRA)
            ?? throw new InvalidOperationException("Magick produced no pixel data");

        ct.ThrowIfCancellationRequested();

        // Magick emits straight alpha; Win2D bitmaps require premultiplied. CanvasBitmap is
        // thread-agile, so this is safe off the UI thread.
        PixelUtil.PremultiplyBgra(bgra);
        CanvasBitmap bitmap = CanvasBitmap.CreateFromBytes(
            device, bgra, (int)width, (int)height,
            DirectXPixelFormat.B8G8R8A8UIntNormalized, 96f, CanvasAlphaMode.Premultiplied);

        return new DecodedImage
        {
            Bitmap = bitmap,
            OrientedNativeSize = new Size(nativeW, nativeH),
            IsFullResolution = request.FullResolution || (width == nativeW && height == nativeH),
            Format = format,
        };
    }
}
