using System;
using System.Collections.Generic;
using System.Linq;
using System.Threading;
using Microsoft.Graphics.Canvas;
using Windows.Foundation;

namespace Looker.Imaging;

/// <summary>One frame of an animation: its bitmap and how long to show it.</summary>
public readonly record struct AnimationFrame(CanvasBitmap Bitmap, TimeSpan Delay);

/// <summary>
/// A decode result ready to draw: the GPU bitmap plus the source's oriented native size (in image
/// pixels, after EXIF orientation). For animations, <see cref="Frames"/> holds the composited frame
/// list (and <see cref="Bitmap"/> is the first frame); it is null for static images.
/// <para>
/// Reference-counted (starts at 1) so the same bitmap(s) can live in the <see cref="ImageCache"/>
/// and be displayed at once: eviction only <see cref="Release"/>s the cache's reference.
/// </para>
/// </summary>
public sealed class DecodedImage : ICacheValue
{
    private int _refCount = 1;

    public required CanvasBitmap Bitmap { get; init; }
    public required Size OrientedNativeSize { get; init; }
    public bool IsFullResolution { get; init; }
    public ImageFormat Format { get; init; }

    /// <summary>Composited animation frames (with per-frame delays); null for a static image.</summary>
    public IReadOnlyList<AnimationFrame>? Frames { get; init; }

    /// <summary>The lazily rendered pages of a PDF (<see cref="Bitmap"/> is page 0 and
    /// <see cref="OrientedNativeSize"/> the whole page strip); null for everything else.</summary>
    public PdfPageSet? Pages { get; init; }

    public bool IsAnimated => Frames is { Count: > 1 };

    /// <summary>Vector sources (SVG, PDF) can be re-rasterized sharper than their nominal size when zoomed in;
    /// raster ones are capped at their native pixels.</summary>
    public bool IsVector => Format is ImageFormat.Svg or ImageFormat.Pdf;

    /// <summary>Approximate VRAM footprint (Bgra8): sum of frame sizes, the page set's resident budget, or the
    /// single bitmap.</summary>
    public long ByteSize => Pages is not null ? Pages.ByteSize
        : Frames is null ? PixelBytes(Bitmap)
        : Frames.Sum(f => PixelBytes(f.Bitmap));

    public void Retain() => Interlocked.Increment(ref _refCount);

    public void Release()
    {
        if (Interlocked.Decrement(ref _refCount) != 0)
            return;

        if (Pages is not null)
        {
            Pages.Dispose(); // owns every page bitmap, Bitmap (page 0) included
        }
        else if (Frames is null)
        {
            Bitmap.Dispose();
        }
        else
        {
            // Frames own every bitmap (including Bitmap == Frames[0].Bitmap); dispose each once.
            foreach (AnimationFrame frame in Frames)
                frame.Bitmap.Dispose();
        }
    }

    private static long PixelBytes(CanvasBitmap bitmap)
        => (long)bitmap.SizeInPixels.Width * bitmap.SizeInPixels.Height * 4;
}
