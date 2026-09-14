using System;
using System.Collections.Generic;
using System.IO;
using System.Threading;
using System.Threading.Tasks;
using Microsoft.Graphics.Canvas;
using SixLabors.ImageSharp;
using SixLabors.ImageSharp.Formats.Gif;
using SixLabors.ImageSharp.Formats.Png;
using SixLabors.ImageSharp.Formats.Webp;
using SixLabors.ImageSharp.PixelFormats;
using SixLabors.ImageSharp.Processing;
using Windows.Graphics.DirectX;
using ISImage = SixLabors.ImageSharp.Image;
using Size = Windows.Foundation.Size;

namespace Looker.Imaging;

/// <summary>
/// Decodes animated GIF / WebP / APNG into a list of already-composited frames (ImageSharp applies
/// disposal/blend), each as a <see cref="CanvasBitmap"/> with its delay. WIC only yields the first
/// frame for these, so this is the animated path. Static files (and single-frame GIFs) return null
/// so the router falls through to the faster WIC path. Bounded by frame-count and memory caps.
/// </summary>
public sealed class AnimatedImageDecoder : IImageDecoder
{
    private const int MaxFrames = 400;
    private const long MaxFrameBytes = 384L * 1024 * 1024;
    private static readonly TimeSpan MinFrameDelay = TimeSpan.FromMilliseconds(10);

    public bool CanDecode(ImageFormat format)
        => format is ImageFormat.Gif or ImageFormat.Webp or ImageFormat.Png;

    public Task<DecodedImage?> DecodeAsync(DecodeRequest request, ImageFormat format, CanvasDevice device)
        => Task.Run(() => DecodeCore(request, format, device), request.Cancellation);

    private static DecodedImage? DecodeCore(DecodeRequest request, ImageFormat format, CanvasDevice device)
    {
        CancellationToken ct = request.Cancellation;

        // Cheap probes so static PNG/WebP never pay for a full ImageSharp load.
        if (format == ImageFormat.Png && !IsAnimatedPng(request.Path)) return null;
        if (format == ImageFormat.Webp && !IsAnimatedWebp(request.Path)) return null;

        using Image<Bgra32> image = ISImage.Load<Bgra32>(request.Path);
        ct.ThrowIfCancellationRequested();

        if (image.Frames.Count <= 1)
            return null; // not actually animated — let the static decoder handle it

        int nativeW = image.Width;
        int nativeH = image.Height;

        // Scale every frame down to fit the target (never upscale).
        if (!request.FullResolution && request.TargetWidth > 0 && request.TargetHeight > 0)
        {
            double fit = Math.Min((double)request.TargetWidth / nativeW, (double)request.TargetHeight / nativeH);
            if (fit < 1.0)
            {
                int w = Math.Max(1, (int)Math.Round(nativeW * fit));
                int h = Math.Max(1, (int)Math.Round(nativeH * fit));
                image.Mutate(x => x.Resize(w, h));
            }
        }
        ct.ThrowIfCancellationRequested();

        int frameW = image.Width;
        int frameH = image.Height;
        int stride = frameW * 4;
        var frames = new List<AnimationFrame>();
        long totalBytes = 0;

        try
        {
            for (int i = 0; i < image.Frames.Count && i < MaxFrames; i++)
            {
                ct.ThrowIfCancellationRequested();
                totalBytes += (long)frameW * frameH * 4;
                if (totalBytes > MaxFrameBytes && frames.Count > 0)
                    break; // memory cap: play what we captured, drop the tail

                byte[] bgra = new byte[stride * frameH];
                using (Image<Bgra32> single = image.Frames.CloneFrame(i))
                    single.CopyPixelDataTo(bgra);

                // ImageSharp Bgra32 is straight alpha; Win2D bitmaps require premultiplied.
                PixelUtil.PremultiplyBgra(bgra);
                CanvasBitmap bitmap = CanvasBitmap.CreateFromBytes(
                    device, bgra, frameW, frameH,
                    DirectXPixelFormat.B8G8R8A8UIntNormalized, 96f, CanvasAlphaMode.Premultiplied);

                frames.Add(new AnimationFrame(bitmap, GetFrameDelay(image.Frames[i], format)));
            }
        }
        catch
        {
            foreach (AnimationFrame f in frames)
                f.Bitmap.Dispose();
            throw;
        }

        if (frames.Count == 0)
            return null;

        return new DecodedImage
        {
            Bitmap = frames[0].Bitmap,
            OrientedNativeSize = new Size(nativeW, nativeH),
            Frames = frames,
            Format = format,
            IsFullResolution = frameW == nativeW && frameH == nativeH,
        };
    }

    private static TimeSpan GetFrameDelay(ImageFrame<Bgra32> frame, ImageFormat format)
    {
        try
        {
            switch (format)
            {
                case ImageFormat.Gif:
                    int centiseconds = frame.Metadata.GetGifMetadata().FrameDelay;
                    return Clamp(TimeSpan.FromMilliseconds(centiseconds * 10));

                case ImageFormat.Webp:
                    uint ms = frame.Metadata.GetWebpMetadata().FrameDelay;
                    return Clamp(TimeSpan.FromMilliseconds(ms));

                case ImageFormat.Png:
                    PngFrameMetadata png = frame.Metadata.GetPngMetadata();
                    double seconds = png.FrameDelay.ToDouble();
                    return Clamp(TimeSpan.FromSeconds(seconds));

                default:
                    return TimeSpan.FromMilliseconds(100);
            }
        }
        catch
        {
            return TimeSpan.FromMilliseconds(100);
        }
    }

    private static TimeSpan Clamp(TimeSpan delay) => delay < MinFrameDelay ? MinFrameDelay : delay;

    private static bool IsAnimatedPng(string path)
    {
        try
        {
            using FileStream fs = File.OpenRead(path);
            Span<byte> buffer = stackalloc byte[2048];
            int read = fs.Read(buffer);
            return buffer[..read].IndexOf("acTL"u8) >= 0;
        }
        catch
        {
            return false;
        }
    }

    private static bool IsAnimatedWebp(string path)
    {
        try
        {
            using FileStream fs = File.OpenRead(path);
            Span<byte> buffer = stackalloc byte[64];
            int read = fs.Read(buffer);
            ReadOnlySpan<byte> h = buffer[..read];

            if (h.Length >= 21 &&
                h[..4].SequenceEqual("RIFF"u8) &&
                h.Slice(8, 4).SequenceEqual("WEBP"u8) &&
                h.Slice(12, 4).SequenceEqual("VP8X"u8))
            {
                return (h[20] & 0x02) != 0; // VP8X animation flag
            }

            return h.IndexOf("ANMF"u8) >= 0;
        }
        catch
        {
            return false;
        }
    }
}
