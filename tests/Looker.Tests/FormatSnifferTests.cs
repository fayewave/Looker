using System.Text;
using Looker.Imaging;

namespace Looker.Tests;

public class FormatSnifferTests
{
    private static byte[] A(string s) => Encoding.ASCII.GetBytes(s);
    private static byte[] Cat(params byte[][] parts) => parts.SelectMany(p => p).ToArray();

    [Fact]
    public void DetectsJpeg() =>
        Assert.Equal(ImageFormat.Jpeg, FormatSniffer.Sniff(new byte[] { 0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x10 }));

    [Fact]
    public void DetectsPng() =>
        Assert.Equal(ImageFormat.Png, FormatSniffer.Sniff(new byte[] { 0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A }));

    [Fact]
    public void DetectsGif() =>
        Assert.Equal(ImageFormat.Gif, FormatSniffer.Sniff(A("GIF89a....")));

    [Fact]
    public void DetectsBmp() =>
        Assert.Equal(ImageFormat.Bmp, FormatSniffer.Sniff(A("BM....")));

    [Theory]
    [InlineData(new byte[] { 0x49, 0x49, 0x2A, 0x00 })] // little-endian
    [InlineData(new byte[] { 0x4D, 0x4D, 0x00, 0x2A })] // big-endian
    public void DetectsTiff(byte[] header) =>
        Assert.Equal(ImageFormat.Tiff, FormatSniffer.Sniff(header));

    [Theory]
    [InlineData(".dng")]
    [InlineData(".nef")]
    [InlineData(".arw")]
    [InlineData(".DNG")]
    [InlineData(".rw2")]
    [InlineData(".pef")]
    [InlineData(".srw")]
    [InlineData(".3fr")]
    public void TiffMagicWithRawExtensionIsRaw(string extension) =>
        Assert.Equal(ImageFormat.Raw, FormatSniffer.Sniff(new byte[] { 0x49, 0x49, 0x2A, 0x00, 0x08, 0, 0, 0 }, extension));

    [Theory]
    [InlineData(".tif")]
    [InlineData(".tiff")]
    [InlineData(null)]
    public void TiffMagicWithoutRawExtensionStaysTiff(string? extension) =>
        Assert.Equal(ImageFormat.Tiff, FormatSniffer.Sniff(new byte[] { 0x49, 0x49, 0x2A, 0x00, 0x08, 0, 0, 0 }, extension));

    [Fact]
    public void DetectsCr2ByContentEvenWithTiffExtension() =>
        Assert.Equal(ImageFormat.Raw, FormatSniffer.Sniff(Cat(new byte[] { 0x49, 0x49, 0x2A, 0x00, 0x10, 0, 0, 0 }, A("CR"), new byte[] { 2, 0 }), ".tif"));

    [Theory]
    [InlineData("IIRO")]
    [InlineData("IIRS")]
    [InlineData("MMOR")]
    [InlineData("FUJIFILMCCD-RAW 0201")]
    [InlineData("IIU\0\x18\0\0\0")]          // Panasonic RW2 / Leica RWL
    [InlineData("\0MRM\0\x01W\xF8")]          // Minolta MRW
    [InlineData("II\x1a\0\0\0HEAPCCDR\x02")] // Canon CRW (CIFF)
    public void DetectsRawByVendorMagic(string magic) =>
        Assert.Equal(ImageFormat.Raw, FormatSniffer.Sniff(A(magic)));

    [Fact]
    public void DetectsCr3ByFtypBrand() =>
        Assert.Equal(ImageFormat.Raw, FormatSniffer.Sniff(Cat(new byte[] { 0, 0, 0, 0x18 }, A("ftyp"), A("crx "))));

    [Fact]
    public void DetectsWebp() =>
        Assert.Equal(ImageFormat.Webp, FormatSniffer.Sniff(Cat(A("RIFF"), new byte[] { 0, 0, 0, 0 }, A("WEBP"))));

    [Fact]
    public void DetectsHeicByFtypBrand() =>
        Assert.Equal(ImageFormat.Heif, FormatSniffer.Sniff(Cat(new byte[] { 0, 0, 0, 0x18 }, A("ftyp"), A("heic"))));

    [Fact]
    public void DetectsAvifByFtypBrand() =>
        Assert.Equal(ImageFormat.Avif, FormatSniffer.Sniff(Cat(new byte[] { 0, 0, 0, 0x18 }, A("ftyp"), A("avif"))));

    [Fact]
    public void DetectsJpegXr() =>
        Assert.Equal(ImageFormat.JpegXr, FormatSniffer.Sniff(new byte[] { 0x49, 0x49, 0xBC, 0x01, 0x20 }));

    [Fact]
    public void DetectsCur() =>
        Assert.Equal(ImageFormat.Cur, FormatSniffer.Sniff(new byte[] { 0x00, 0x00, 0x02, 0x00, 0x01 }));

    [Fact]
    public void DetectsJp2SignatureBox() =>
        Assert.Equal(ImageFormat.Jpeg2000, FormatSniffer.Sniff(new byte[] { 0, 0, 0, 0x0C, 0x6A, 0x50, 0x20, 0x20, 0x0D, 0x0A, 0x87, 0x0A }));

    [Fact]
    public void DetectsJ2kCodestream() =>
        Assert.Equal(ImageFormat.Jpeg2000, FormatSniffer.Sniff(new byte[] { 0xFF, 0x4F, 0xFF, 0x51, 0x00 }));

    [Fact]
    public void JpegXlContainerIsNotJpeg2000() =>
        Assert.Equal(ImageFormat.JpegXl, FormatSniffer.Sniff(new byte[] { 0, 0, 0, 0x0C, 0x4A, 0x58, 0x4C, 0x20, 0x0D, 0x0A, 0x87, 0x0A }, ".jp2"));

    [Fact]
    public void DetectsXcf() =>
        Assert.Equal(ImageFormat.Xcf, FormatSniffer.Sniff(A("gimp xcf file\0")));

    [Fact]
    public void DetectsQoi() =>
        Assert.Equal(ImageFormat.Qoi, FormatSniffer.Sniff(A("qoif....")));

    [Fact]
    public void DetectsPcx() =>
        Assert.Equal(ImageFormat.Pcx, FormatSniffer.Sniff(new byte[] { 0x0A, 0x05, 0x01, 0x08 }));

    [Theory]
    [InlineData("P6\n1920 1280\n255\n")]
    [InlineData("P1 8 8\n")]
    public void DetectsNetpbm(string header) =>
        Assert.Equal(ImageFormat.Pnm, FormatSniffer.Sniff(A(header)));

    [Fact]
    public void PlainTextStartingWithPIsNotNetpbm() =>
        Assert.Equal(ImageFormat.Unknown, FormatSniffer.Sniff(A("Photo notes")));

    [Fact]
    public void DetectsPsd() =>
        Assert.Equal(ImageFormat.Psd, FormatSniffer.Sniff(A("8BPS....")));

    [Fact]
    public void DetectsIco() =>
        Assert.Equal(ImageFormat.Ico, FormatSniffer.Sniff(new byte[] { 0x00, 0x00, 0x01, 0x00, 0x01 }));

    [Fact]
    public void DetectsJpegXlCodestream() =>
        Assert.Equal(ImageFormat.JpegXl, FormatSniffer.Sniff(new byte[] { 0xFF, 0x0A }));

    [Theory]
    [InlineData("<svg xmlns=\"http://www.w3.org/2000/svg\">")]
    [InlineData("<?xml version=\"1.0\"?><svg>")]
    public void DetectsSvg(string text) =>
        Assert.Equal(ImageFormat.Svg, FormatSniffer.Sniff(A(text)));

    [Fact]
    public void DetectsPdfByHeader() =>
        Assert.Equal(ImageFormat.Pdf, FormatSniffer.Sniff(A("%PDF-1.7\n%\u00e2\u00e3")));

    [Fact]
    public void DetectsPdfByExtensionWhenHeaderIsMissing() =>
        Assert.Equal(ImageFormat.Pdf, FormatSniffer.Sniff(ReadOnlySpan<byte>.Empty, ".pdf"));

    [Fact]
    public void DetectsSvgWithUtf8Bom() =>
        Assert.Equal(ImageFormat.Svg, FormatSniffer.Sniff(Cat(new byte[] { 0xEF, 0xBB, 0xBF }, A("<svg>"))));

    [Fact]
    public void ContentBeatsExtension()
    {
        // A PNG mislabelled ".jpg" is still a PNG.
        byte[] png = { 0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A };
        Assert.Equal(ImageFormat.Png, FormatSniffer.Sniff(png, ".jpg"));
    }

    [Theory]
    [InlineData(".tga", ImageFormat.Tga)]  // TGA has no reliable magic → extension decides
    [InlineData(".cr2", ImageFormat.Raw)]
    [InlineData(".nef", ImageFormat.Raw)]
    [InlineData(".heic", ImageFormat.Heif)]
    [InlineData(".hif", ImageFormat.Heif)]
    [InlineData(".heics", ImageFormat.Heif)]
    [InlineData(".avifs", ImageFormat.Avif)]
    [InlineData(".apng", ImageFormat.Png)]
    [InlineData(".psb", ImageFormat.Psd)]
    [InlineData(".jxr", ImageFormat.JpegXr)]
    [InlineData(".wdp", ImageFormat.JpegXr)]
    [InlineData(".jpf", ImageFormat.Jpeg2000)]
    [InlineData(".ppm", ImageFormat.Pnm)]
    [InlineData(".crw", ImageFormat.Raw)]
    [InlineData(".mrw", ImageFormat.Raw)]
    public void FallsBackToExtensionWhenSignatureUnknown(string extension, ImageFormat expected)
    {
        byte[] unknown = { 0xAA, 0xBB, 0xCC, 0xDD };
        Assert.Equal(expected, FormatSniffer.Sniff(unknown, extension));
    }

    [Fact]
    public void ReturnsUnknownForUnrecognizedContentAndExtension() =>
        Assert.Equal(ImageFormat.Unknown, FormatSniffer.Sniff(new byte[] { 0xAA, 0xBB, 0xCC, 0xDD }, null));

    [Fact]
    public void EverySupportedExtensionSniffsToAKnownFormat()
    {
        foreach (string ext in SupportedFormats.Extensions)
            Assert.NotEqual(ImageFormat.Unknown, FormatSniffer.Sniff(ReadOnlySpan<byte>.Empty, ext));
    }

    [Fact]
    public void EveryFormatHasADisplayName()
    {
        foreach (ImageFormat format in Enum.GetValues<ImageFormat>())
        {
            if (format == ImageFormat.Unknown) continue;
            Assert.False(string.IsNullOrEmpty(ImageFormatNames.DisplayName(format, null)), format.ToString());
        }
    }
}
