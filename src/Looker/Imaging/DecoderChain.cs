using System;
using System.Collections.Generic;

namespace Looker.Imaging;

/// <summary>An implementation the router can try, in order.</summary>
public enum DecoderKind
{
    Animated,
    Wic,
    Svg,
    Magick,
    Pdf,
}

/// <summary>
/// Pure decoder-ordering policy (no decoder/Win2D dependencies, so it is unit-testable). WIC is
/// tried first when a codec exists because it is faster and color-managed; Magick is the universal
/// fallback and always ends the chain so a corrupt/exotic file still gets one more attempt.
/// </summary>
public static class DecoderChain
{
    public static IReadOnlyList<DecoderKind> Plan(ImageFormat format, Func<ImageFormat, bool> canWicDecode)
    {
        switch (format)
        {
            case ImageFormat.Svg:
                return new[] { DecoderKind.Svg, DecoderKind.Magick };

            // Only the inbox PDF renderer can read these: Magick would need Ghostscript, which is not shipped.
            case ImageFormat.Pdf:
                return new[] { DecoderKind.Pdf };

            // Can be animated (GIF, APNG): try the animated decoder first; it declines a static
            // file (returns null) so WIC handles those on the fast path.
            case ImageFormat.Gif:
            case ImageFormat.Png:
                return new[] { DecoderKind.Animated, DecoderKind.Wic, DecoderKind.Magick };

            // Always WIC-native raster formats.
            case ImageFormat.Jpeg:
            case ImageFormat.Bmp:
            case ImageFormat.Tiff:
            case ImageFormat.Ico:
            case ImageFormat.JpegXr: // inbox WIC codec; Magick has no JXR coder, so the fallback is only a formality
                return new[] { DecoderKind.Wic, DecoderKind.Magick };

            // WebP can be animated and is codec-dependent for the static path.
            case ImageFormat.Webp:
                return canWicDecode(format)
                    ? new[] { DecoderKind.Animated, DecoderKind.Wic, DecoderKind.Magick }
                    : new[] { DecoderKind.Animated, DecoderKind.Magick };

            // WIC only with the right Store codec pack installed — else straight to Magick.
            case ImageFormat.Heif:
            case ImageFormat.Avif:
            case ImageFormat.JpegXl:
            case ImageFormat.Raw:
            case ImageFormat.Psd:
            case ImageFormat.Tga:
            case ImageFormat.Dds:
            case ImageFormat.Jpeg2000:
            case ImageFormat.Cur:
            case ImageFormat.Pcx:
            case ImageFormat.Pnm:
            case ImageFormat.Xcf:
            case ImageFormat.Qoi:
                return canWicDecode(format)
                    ? new[] { DecoderKind.Wic, DecoderKind.Magick }
                    : new[] { DecoderKind.Magick };

            default:
                return new[] { DecoderKind.Wic, DecoderKind.Magick };
        }
    }
}
