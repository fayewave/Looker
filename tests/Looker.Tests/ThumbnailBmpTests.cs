using Looker.Imaging;

namespace Looker.Tests;

public class ThumbnailBmpTests
{
    /// <summary>A 32-bpp BMP with the given BGRA rows in file order (bottom-up unless height is negative).</summary>
    private static byte[] Bmp32(int width, int height, byte[] bgra, int infoHeaderSize = 40, int compression = 0, uint[]? masks = null)
    {
        int offset = 14 + infoHeaderSize + (masks is null ? 0 : 12);
        var b = new byte[offset + bgra.Length];
        b[0] = (byte)'B'; b[1] = (byte)'M';
        BitConverter.GetBytes(b.Length).CopyTo(b, 2);
        BitConverter.GetBytes(offset).CopyTo(b, 10);
        BitConverter.GetBytes(infoHeaderSize).CopyTo(b, 14);
        BitConverter.GetBytes(width).CopyTo(b, 18);
        BitConverter.GetBytes(height).CopyTo(b, 22);
        BitConverter.GetBytes((short)1).CopyTo(b, 26);
        BitConverter.GetBytes((short)32).CopyTo(b, 28);
        BitConverter.GetBytes(compression).CopyTo(b, 30);
        if (masks is not null)
        {
            for (int i = 0; i < 3; i++)
                BitConverter.GetBytes(masks[i]).CopyTo(b, 14 + 40 + i * 4);
        }
        bgra.CopyTo(b, offset);
        return b;
    }

    [Fact]
    public void BottomUpRowsAreFlippedAndAlphaPremultiplied()
    {
        // Two rows of one pixel: file order is bottom row first.
        byte[] bmp = Bmp32(1, 2, new byte[] { 200, 100, 50, 128, 10, 20, 30, 255 });

        Assert.True(ThumbnailBmp.TryDecodeBgra32(bmp, out int w, out int h, out byte[] bgra));
        Assert.Equal((1, 2), (w, h));
        Assert.Equal(new byte[] { 10, 20, 30, 255 }, bgra[..4]);          // top row
        Assert.Equal(new byte[] { 100, 50, 25, 128 }, bgra[4..]);         // bottom row, premultiplied by 128/255
    }

    [Fact]
    public void TopDownRowsKeepTheirOrder()
    {
        byte[] bmp = Bmp32(1, -2, new byte[] { 1, 2, 3, 255, 4, 5, 6, 255 });

        Assert.True(ThumbnailBmp.TryDecodeBgra32(bmp, out _, out int h, out byte[] bgra));
        Assert.Equal(2, h);
        Assert.Equal(new byte[] { 1, 2, 3, 255, 4, 5, 6, 255 }, bgra);
    }

    [Fact]
    public void AllZeroAlphaIsTreatedAsOpaque()
    {
        byte[] bmp = Bmp32(2, 1, new byte[] { 10, 20, 30, 0, 40, 50, 60, 0 }, infoHeaderSize: 124, compression: 3,
            masks: new uint[] { 0x00FF0000, 0x0000FF00, 0x000000FF });

        Assert.True(ThumbnailBmp.TryDecodeBgra32(bmp, out _, out _, out byte[] bgra));
        Assert.Equal(new byte[] { 10, 20, 30, 255, 40, 50, 60, 255 }, bgra);
    }

    [Fact]
    public void FullyTransparentPixelsAreKeptWhenOthersHaveAlpha()
    {
        byte[] bmp = Bmp32(2, 1, new byte[] { 10, 20, 30, 0, 40, 50, 60, 255 });

        Assert.True(ThumbnailBmp.TryDecodeBgra32(bmp, out _, out _, out byte[] bgra));
        Assert.Equal(new byte[] { 0, 0, 0, 0, 40, 50, 60, 255 }, bgra);
    }

    [Fact]
    public void NonStandardMasksAndOtherDepthsAreRejected()
    {
        byte[] rgbaMasks = Bmp32(1, 1, new byte[] { 0, 0, 0, 255 }, compression: 3,
            masks: new uint[] { 0xFF000000, 0x00FF0000, 0x0000FF00 });
        Assert.False(ThumbnailBmp.TryDecodeBgra32(rgbaMasks, out _, out _, out _));

        byte[] bmp24 = Bmp32(1, 1, new byte[] { 0, 0, 0, 255 });
        BitConverter.GetBytes((short)24).CopyTo(bmp24, 28);
        Assert.False(ThumbnailBmp.TryDecodeBgra32(bmp24, out _, out _, out _));

        Assert.False(ThumbnailBmp.TryDecodeBgra32(new byte[] { 0x89, 0x50, 0x4E, 0x47, 0, 0, 0, 0, 0, 0, 0, 0 }, out _, out _, out _));
        Assert.False(ThumbnailBmp.TryDecodeBgra32(new byte[] { (byte)'B', (byte)'M' }, out _, out _, out _));
    }

    [Fact]
    public void TruncatedPixelDataIsRejected()
    {
        byte[] bmp = Bmp32(2, 2, new byte[] { 0, 0, 0, 255, 0, 0, 0, 255 }); // claims 2x2, carries 2 pixels
        Assert.False(ThumbnailBmp.TryDecodeBgra32(bmp, out _, out _, out _));

        byte[] bogusOffset = Bmp32(1, 1, new byte[] { 0, 0, 0, 255 });
        BitConverter.GetBytes(bogusOffset.Length + 100).CopyTo(bogusOffset, 10);
        Assert.False(ThumbnailBmp.TryDecodeBgra32(bogusOffset, out _, out _, out _));
    }
}
