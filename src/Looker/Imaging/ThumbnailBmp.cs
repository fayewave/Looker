using System;

namespace Looker.Imaging;

/// <summary>
/// Decodes the 32-bpp BMPs the Windows shell thumbnail cache serves for its small tiers (requests up to
/// ~128 px) into premultiplied BGRA, honouring the 4th byte of every pixel as alpha. Two reasons this isn't
/// left to WIC: the cache writes some tiers with a plain BITMAPINFOHEADER (no alpha mask), which WIC decodes
/// as opaque Bgr32 even though the alpha bytes are real (a transparent PNG's thumbnail comes back flattened),
/// and thumbnails from the PowerToys SVG provider carry alpha = 0 on every pixel, which WIC renders as a fully
/// transparent tile. A 100% transparent thumbnail is never intentional, so all-zero alpha is treated as opaque.
/// WinUI-free (unit tested).
/// </summary>
internal static class ThumbnailBmp
{
    private const int FileHeaderSize = 14;
    private const int BiRgb = 0;
    private const int BiBitfields = 3;

    /// <summary>
    /// True with top-down premultiplied BGRA pixels if <paramref name="bmp"/> is a 32-bpp BMP in the standard
    /// BGRA byte order (BI_RGB, or BI_BITFIELDS with the usual masks). Anything else returns false and the
    /// caller should hand the bytes to a real decoder.
    /// </summary>
    public static bool TryDecodeBgra32(byte[] bmp, out int width, out int height, out byte[] bgra)
    {
        width = height = 0;
        bgra = Array.Empty<byte>();
        if (bmp is null || bmp.Length < FileHeaderSize + 40 || bmp[0] != (byte)'B' || bmp[1] != (byte)'M')
            return false;

        int pixelOffset = BitConverter.ToInt32(bmp, 10);
        int infoHeaderSize = BitConverter.ToInt32(bmp, FileHeaderSize);
        int w = BitConverter.ToInt32(bmp, FileHeaderSize + 4);
        int rawHeight = BitConverter.ToInt32(bmp, FileHeaderSize + 8);
        int bitsPerPixel = BitConverter.ToInt16(bmp, FileHeaderSize + 14);
        int compression = BitConverter.ToInt32(bmp, FileHeaderSize + 16);
        if (infoHeaderSize < 40 || w <= 0 || rawHeight == 0 || bitsPerPixel != 32)
            return false;

        if (compression == BiBitfields)
        {
            // Masks follow the 40-byte header (or sit at the same offsets inside a V4/V5 header).
            int masks = FileHeaderSize + 40;
            if (bmp.Length < masks + 12 || pixelOffset < masks + 12)
                return false;
            if (BitConverter.ToUInt32(bmp, masks) != 0x00FF0000u ||
                BitConverter.ToUInt32(bmp, masks + 4) != 0x0000FF00u ||
                BitConverter.ToUInt32(bmp, masks + 8) != 0x000000FFu)
                return false;
        }
        else if (compression != BiRgb)
        {
            return false;
        }

        bool topDown = rawHeight < 0;
        int h = Math.Abs(rawHeight);
        int stride = w * 4; // 32-bpp rows are already 4-byte aligned
        long needed = (long)stride * h;
        if (needed > int.MaxValue || pixelOffset < FileHeaderSize + infoHeaderSize || pixelOffset + needed > bmp.Length)
            return false;

        var pixels = new byte[needed];
        for (int row = 0; row < h; row++)
        {
            int sourceRow = topDown ? row : h - 1 - row;
            Buffer.BlockCopy(bmp, pixelOffset + sourceRow * stride, pixels, row * stride, stride);
        }

        bool anyAlpha = false;
        for (int i = 3; i < pixels.Length; i += 4)
        {
            if (pixels[i] != 0)
            {
                anyAlpha = true;
                break;
            }
        }

        if (anyAlpha)
        {
            PixelUtil.PremultiplyBgra(pixels);
        }
        else
        {
            for (int i = 3; i < pixels.Length; i += 4)
                pixels[i] = 255;
        }

        width = w;
        height = h;
        bgra = pixels;
        return true;
    }
}
