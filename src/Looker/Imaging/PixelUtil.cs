namespace Looker.Imaging;

internal static class PixelUtil
{
    /// <summary>
    /// Premultiply a straight-alpha BGRA8 buffer in place. Win2D/Direct2D bitmaps require
    /// premultiplied alpha (creating one with <c>CanvasAlphaMode.Straight</c> throws), so decoders
    /// that emit straight alpha (Magick, ImageSharp) must convert before <c>CreateFromBytes</c>.
    /// </summary>
    public static void PremultiplyBgra(byte[] bgra)
    {
        for (int i = 0; i + 3 < bgra.Length; i += 4)
        {
            byte a = bgra[i + 3];
            if (a == 255)
                continue;
            if (a == 0)
            {
                bgra[i] = bgra[i + 1] = bgra[i + 2] = 0;
                continue;
            }
            bgra[i] = (byte)((bgra[i] * a + 127) / 255);
            bgra[i + 1] = (byte)((bgra[i + 1] * a + 127) / 255);
            bgra[i + 2] = (byte)((bgra[i + 2] * a + 127) / 255);
        }
    }
}
