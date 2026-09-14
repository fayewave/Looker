using Looker.Imaging;
using Looker.Rendering;

namespace Looker.Tests;

public class PdfLayoutTests
{
    [Fact]
    public void SinglePageIsTheWholeStrip()
    {
        PdfLayout layout = PdfLayout.Compute(new[] { (816.0, 1056.0) });

        Assert.Equal(1, layout.PageCount);
        Assert.Equal(new RectD(0, 0, 816, 1056), layout.Pages[0]);
        Assert.Equal(816, layout.Width);
        Assert.Equal(1056, layout.Height);
    }

    [Fact]
    public void PagesRunLeftToRightWithTheGapBetweenThemOnly()
    {
        PdfLayout layout = PdfLayout.Compute(new[] { (800.0, 1000.0), (800.0, 1000.0), (800.0, 1000.0) }, gap: 20);

        Assert.Equal(0, layout.Pages[0].X);
        Assert.Equal(820, layout.Pages[1].X);
        Assert.Equal(1640, layout.Pages[2].X);
        Assert.Equal(2440, layout.Width); // 3 pages + 2 gaps, no trailing gap
    }

    [Fact]
    public void ShorterPagesAreCentredOnTheTallest()
    {
        PdfLayout layout = PdfLayout.Compute(new[] { (800.0, 1000.0), (1000.0, 600.0) }, gap: 0);

        Assert.Equal(1000, layout.Height);
        Assert.Equal(0, layout.Pages[0].Y);
        Assert.Equal(200, layout.Pages[1].Y);
        Assert.Equal(1000, layout.MaxPageWidth);
        Assert.Equal(1000, layout.MaxPageHeight);
    }

    [Fact]
    public void DegeneratePageSizesStillGetASlot()
    {
        PdfLayout layout = PdfLayout.Compute(new[] { (0.0, double.NaN), (800.0, 1000.0) }, gap: 10);

        Assert.Equal(2, layout.PageCount);
        Assert.True(layout.Pages[0].Width > 0);
        Assert.Equal(layout.Pages[0].Width + 10, layout.Pages[1].X);
    }

    [Fact]
    public void PageAtPicksTheNearestCentre()
    {
        PdfLayout layout = PdfLayout.Compute(new[] { (800.0, 1000.0), (800.0, 1000.0), (800.0, 1000.0) }, gap: 20);

        Assert.Equal(0, layout.PageAt(100));
        Assert.Equal(1, layout.PageAt(1230));
        Assert.Equal(2, layout.PageAt(5000));
    }

    [Fact]
    public void EmptyDocumentIsRejected()
        => Assert.Throws<ArgumentException>(() => PdfLayout.Compute(Array.Empty<(double, double)>()));
}
