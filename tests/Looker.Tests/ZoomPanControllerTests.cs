using Looker.Rendering;

namespace Looker.Tests;

public class ZoomPanControllerTests
{
    private const double Tol = 1e-6;

    // Standard fixture: 2000x1000 image in a 1000x800 viewport at 100% DPI.
    private static ZoomPanController Standard()
    {
        var z = new ZoomPanController();
        z.Reset(viewportW: 1000, viewportH: 800, contentW: 2000, contentH: 1000, rasterization: 1.0);
        return z;
    }

    private static (double X, double Y) Offset(ZoomPanController z)
    {
        RectD r = z.GetImageRect();
        return (r.X, r.Y);
    }

    [Fact]
    public void ResetFitsAndCenters()
    {
        var z = Standard();
        Assert.Equal(ZoomMode.Fit, z.Mode);
        Assert.Equal(0.5, z.Scale, Tol);          // min(1000/2000, 800/1000)
        RectD r = z.GetImageRect();
        Assert.Equal(0, r.X, Tol);                // 1000 wide fills viewport width
        Assert.Equal(150, r.Y, Tol);              // (800 - 500) / 2, letterboxed vertically
        Assert.Equal(1000, r.Width, Tol);
        Assert.Equal(500, r.Height, Tol);
    }

    [Fact]
    public void ActualSizeIsDpiAware()
    {
        var lowDpi = new ZoomPanController();
        lowDpi.Reset(1000, 800, 2000, 1000, rasterization: 1.0);
        Assert.Equal(1.0, lowDpi.ActualScale, Tol);

        var highDpi = new ZoomPanController();
        highDpi.Reset(1000, 800, 2000, 1000, rasterization: 2.0);
        Assert.Equal(0.5, highDpi.ActualScale, Tol);   // 1 image px == 1 physical px == 0.5 DIP

        highDpi.SetActualSize();
        Assert.Equal(100.0, highDpi.ZoomPercent, 1e-3); // "100%" regardless of DPI
    }

    [Fact]
    public void ZoomAtPointKeepsThePointUnderTheCursorFixed()
    {
        var z = Standard();
        const double ax = 300, ay = 400;

        // Image-space point currently beneath the anchor, before zooming.
        (double ox, double oy) = Offset(z);
        double imageX = (ax - ox) / z.Scale;
        double imageY = (ay - oy) / z.Scale;

        z.ZoomAt(2.0, ax, ay);

        // After zooming, that same image point must still project to the anchor.
        (double nx, double ny) = Offset(z);
        Assert.Equal(ax, nx + imageX * z.Scale, 1e-4);
        Assert.Equal(ay, ny + imageY * z.Scale, 1e-4);
        Assert.Equal(1.0, z.Scale, Tol);
    }

    [Fact]
    public void ScaleIsClampedToMinAndMax()
    {
        var z = Standard();
        for (int i = 0; i < 50; i++) z.ZoomAt(2.0, 500, 400);
        Assert.Equal(z.MaxScale, z.Scale, Tol);

        for (int i = 0; i < 50; i++) z.ZoomAt(0.5, 500, 400);
        Assert.Equal(z.MinScale, z.Scale, Tol);
    }

    [Fact]
    public void PanIsClampedToKeepImageOnScreen()
    {
        var z = Standard();
        z.SetActualSize();               // scale 1.0 → 2000x1000 image, larger than viewport
        z.Pan(1_000_000, 1_000_000);     // fling toward bottom-right
        RectD r = z.GetImageRect();
        // Left/top edge can advance at most viewport - 20% of image.
        Assert.Equal(1000 - 2000 * 0.2, r.X, Tol);   // 600
        Assert.Equal(800 - 1000 * 0.2, r.Y, Tol);    // 600

        z.Pan(-1_000_000, -1_000_000);
        r = z.GetImageRect();
        Assert.Equal(2000 * 0.2 - 2000, r.X, Tol);   // -1600
        Assert.Equal(1000 * 0.2 - 1000, r.Y, Tol);   // -800
    }

    [Fact]
    public void ImageSmallerThanViewportStaysCenteredAndCannotPan()
    {
        var z = new ZoomPanController();
        z.Reset(1000, 800, 200, 100, 1.0);   // fit upscales to 1000x500, fills width exactly
        (double x0, double y0) = Offset(z);
        z.Pan(500, 500);
        (double x1, double y1) = Offset(z);
        Assert.Equal(x0, x1, Tol);
        Assert.Equal(y0, y1, Tol);
    }

    [Fact]
    public void DoubleClickTogglesBetweenFitAndActual()
    {
        var z = Standard();
        Assert.Equal(ZoomMode.Fit, z.Mode);

        z.ToggleFitActual(300, 400);
        Assert.Equal(ZoomMode.ActualSize, z.Mode);
        Assert.Equal(z.ActualScale, z.Scale, Tol);

        z.ToggleFitActual(300, 400);
        Assert.Equal(ZoomMode.Fit, z.Mode);
        Assert.Equal(z.FitScale, z.Scale, Tol);
    }

    [Fact]
    public void ResizingInFitModeRefits()
    {
        var z = Standard();
        z.SetViewport(500, 500);
        Assert.Equal(ZoomMode.Fit, z.Mode);
        Assert.Equal(0.25, z.Scale, Tol);        // min(500/2000, 500/1000)
    }

    [Fact]
    public void SmallImageOpensAtActualSizeNotUpscaledToFit()
    {
        var z = new ZoomPanController();
        z.Reset(1000, 800, 200, 100, rasterization: 1.0); // image far smaller than the window

        Assert.Equal(ZoomMode.Fit, z.Mode);
        Assert.Equal(1.0, z.Scale, Tol);            // 100%, not the 5x that would fill the window
        Assert.Equal(100.0, z.ZoomPercent, 1e-3);

        RectD r = z.GetImageRect();
        Assert.Equal(200, r.Width, Tol);            // native pixels, centered
        Assert.Equal(100, r.Height, Tol);
        Assert.Equal((1000 - 200) / 2.0, r.X, Tol);
        Assert.Equal((800 - 100) / 2.0, r.Y, Tol);
    }

    [Fact]
    public void SmallImageDefaultIsActualSizeAtHighDpi()
    {
        var z = new ZoomPanController();
        z.Reset(1000, 800, 200, 100, rasterization: 2.0); // 200% DPI

        Assert.Equal(z.ActualScale, z.Scale, Tol);  // 0.5 DIP/px → 1 image px == 1 physical px
        Assert.Equal(100.0, z.ZoomPercent, 1e-3);
    }

    [Fact]
    public void LargeImageStillShrinksToFit()
    {
        var z = Standard();                          // 2000x1000 in 1000x800
        Assert.Equal(ZoomMode.Fit, z.Mode);
        Assert.Equal(z.FitScale, z.Scale, Tol);      // 0.5 — capped path still fits big images
        Assert.True(z.Scale < z.ActualScale);
    }

    [Fact]
    public void ZoomPercentReadsRelativeToActualPixels()
    {
        var z = Standard();                       // fit scale 0.5, actual scale 1.0
        Assert.Equal(50.0, z.ZoomPercent, 1e-3);
        z.SetActualSize();
        Assert.Equal(100.0, z.ZoomPercent, 1e-3);
    }

    // --- Focus rect (multi-page strip) ---

    [Fact]
    public void FitWithFocusFitsTheFocusNotTheStrip()
    {
        var z = new ZoomPanController();
        // Two 800×1000 pages with a 200 gap laid out as a 1800×1000 strip; viewport 800×800.
        z.Reset(800, 800, 1800, 1000, 1.0, focus: new RectD(0, 0, 800, 1000));

        Assert.Equal(ZoomMode.Fit, z.Mode);
        Assert.Equal(0.8, z.Scale, Tol); // 800/1000: the page fits, the strip would have been 800/1800
        RectD rect = z.GetImageRect();
        // Page 1 (x 0..640 at 0.8) is centred: 80 px either side.
        Assert.Equal(80, rect.X, Tol);
        Assert.Equal(0, rect.Y, Tol);
    }

    [Fact]
    public void MovingTheFocusInFitModeCentresTheNewPage()
    {
        var z = new ZoomPanController();
        z.Reset(800, 800, 1800, 1000, 1.0, focus: new RectD(0, 0, 800, 1000));

        z.SetFocus(new RectD(1000, 0, 800, 1000));

        Assert.Equal(ZoomMode.Fit, z.Mode);
        RectD rect = z.GetImageRect();
        // Page 2 starts at 1000 content px = 800 DIPs into the strip; centring it puts the strip at 80 - 800.
        Assert.Equal(80 - 800, rect.X, Tol);
    }

    [Fact]
    public void MovingTheFocusWhileZoomedKeepsTheScale()
    {
        var z = new ZoomPanController();
        z.Reset(800, 800, 1800, 1000, 1.0, focus: new RectD(0, 0, 800, 1000));
        z.ZoomAt(2.0, 400, 400);
        double scale = z.Scale;

        z.SetFocus(new RectD(1000, 0, 800, 1000));

        Assert.Equal(scale, z.Scale, Tol);
        Assert.NotEqual(ZoomMode.Fit, z.Mode);
        RectD rect = z.GetImageRect();
        double pageCentreX = rect.X + 1400 * scale; // page 2 centre in viewport DIPs
        Assert.Equal(400, pageCentreX, Tol);
    }

    [Fact]
    public void PanClampKeepsAPageNotTheWholeStripOnScreen()
    {
        var z = new ZoomPanController();
        // A 100-page strip: a whole-content 20% rule would forbid even the fit position of page 1.
        z.Reset(800, 800, 100 * 1000, 1000, 1.0, focus: new RectD(0, 0, 800, 1000));
        double before = z.GetImageRect().X;

        z.Pan(0, 0);

        Assert.Equal(before, z.GetImageRect().X, Tol); // clamping a fit position must be a no-op
        z.Pan(-1e9, 0); // fling to the far right end
        RectD rect = z.GetImageRect();
        // At least 20% of one page (0.2 × 800 × 0.8 = 128 DIPs) of the strip's right end stays visible.
        Assert.Equal(128 - 100 * 1000 * 0.8, rect.X, Tol);
    }

    [Fact]
    public void DoubleClickAfterDraggingToAnotherPageFitsThatPageFirst()
    {
        var z = new ZoomPanController();
        z.Reset(800, 800, 1800, 1000, 1.0, focus: new RectD(0, 0, 800, 1000));
        Assert.True(z.AtFitPosition());

        // Drag left most of a page (the viewport now sits over page 2, not exactly centred on it) and, as the
        // viewport does, re-focus it in place.
        z.Pan(-700, 0);
        z.SetFocus(new RectD(1000, 0, 800, 1000), recenter: false);
        Assert.False(z.AtFitPosition());

        z.ToggleFitActual(400, 400);

        Assert.Equal(ZoomMode.Fit, z.Mode);
        Assert.True(z.AtFitPosition()); // fitted and centred on page 2, not jumped to 100%
        Assert.Equal(80 - 800, z.GetImageRect().X, Tol);

        z.ToggleFitActual(400, 400);
        Assert.Equal(ZoomMode.ActualSize, z.Mode); // the second double-click then goes to 100% as usual
    }
}
