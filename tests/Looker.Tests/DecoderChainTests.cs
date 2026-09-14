using Looker.Imaging;

namespace Looker.Tests;

public class DecoderChainTests
{
    private static bool WicYes(ImageFormat _) => true;
    private static bool WicNo(ImageFormat _) => false;

    [Theory]
    [InlineData(ImageFormat.Jpeg)]
    [InlineData(ImageFormat.Tiff)]
    [InlineData(ImageFormat.Bmp)]
    [InlineData(ImageFormat.Ico)]
    [InlineData(ImageFormat.JpegXr)]
    public void NativeRasterFormatsTryWicThenMagick(ImageFormat format)
    {
        Assert.Equal(new[] { DecoderKind.Wic, DecoderKind.Magick }, DecoderChain.Plan(format, WicNo));
    }

    [Theory]
    [InlineData(ImageFormat.Gif)]
    [InlineData(ImageFormat.Png)]
    public void AnimatableFormatsTryAnimatedFirstThenWic(ImageFormat format)
    {
        // Animated decoder declines static files (returns null) → WIC handles those.
        Assert.Equal(new[] { DecoderKind.Animated, DecoderKind.Wic, DecoderKind.Magick }, DecoderChain.Plan(format, WicNo));
    }

    [Fact]
    public void WebpTriesAnimatedThenWicWhenCodecPresent()
    {
        Assert.Equal(new[] { DecoderKind.Animated, DecoderKind.Wic, DecoderKind.Magick }, DecoderChain.Plan(ImageFormat.Webp, WicYes));
    }

    [Fact]
    public void WebpTriesAnimatedThenMagickWhenCodecAbsent()
    {
        Assert.Equal(new[] { DecoderKind.Animated, DecoderKind.Magick }, DecoderChain.Plan(ImageFormat.Webp, WicNo));
    }

    [Fact]
    public void PdfUsesOnlyThePdfRenderer()
    {
        Assert.Equal(new[] { DecoderKind.Pdf }, DecoderChain.Plan(ImageFormat.Pdf, WicYes));
        Assert.Equal(new[] { DecoderKind.Pdf }, DecoderChain.Plan(ImageFormat.Pdf, _ => false));
    }

    [Fact]
    public void SvgUsesSvgThenMagick()
    {
        Assert.Equal(new[] { DecoderKind.Svg, DecoderKind.Magick }, DecoderChain.Plan(ImageFormat.Svg, WicYes));
    }

    [Theory]
    [InlineData(ImageFormat.Heif)]
    [InlineData(ImageFormat.Avif)]
    [InlineData(ImageFormat.Raw)]
    public void CodecDependentFormatsTryWicWhenCodecPresent(ImageFormat format)
    {
        Assert.Equal(new[] { DecoderKind.Wic, DecoderKind.Magick }, DecoderChain.Plan(format, WicYes));
    }

    [Theory]
    [InlineData(ImageFormat.Heif)]
    [InlineData(ImageFormat.Avif)]
    [InlineData(ImageFormat.JpegXl)]
    [InlineData(ImageFormat.Psd)]
    [InlineData(ImageFormat.Raw)]
    [InlineData(ImageFormat.Jpeg2000)]
    [InlineData(ImageFormat.Cur)]
    [InlineData(ImageFormat.Pcx)]
    [InlineData(ImageFormat.Pnm)]
    [InlineData(ImageFormat.Xcf)]
    [InlineData(ImageFormat.Qoi)]
    public void CodecDependentFormatsSkipWicWhenCodecAbsent(ImageFormat format)
    {
        Assert.Equal(new[] { DecoderKind.Magick }, DecoderChain.Plan(format, WicNo));
    }

    [Fact]
    public void EveryChainEndsWithMagickFallback()
    {
        foreach (ImageFormat format in System.Enum.GetValues<ImageFormat>())
        {
            if (format == ImageFormat.Pdf)
                continue; // Magick reads PDF only through Ghostscript, which is not shipped: the PDF renderer stands alone
            var chain = DecoderChain.Plan(format, WicNo);
            Assert.Equal(DecoderKind.Magick, chain[^1]);
        }
    }
}
