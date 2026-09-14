using System;
using System.Collections.Generic;
using Looker.Rendering;

namespace Looker.Imaging;

/// <summary>
/// Where each page of a PDF sits when the document is laid out as one horizontal strip: pages left to right
/// in reading order with a fixed gap between them, each centred vertically on the tallest page. Units are
/// content pixels (the renderer's 96 dpi page size), the same space the zoom controller works in — the strip
/// is the "image" and one page is its focus rect. Pure math so it is unit-tested; no Windows types.
/// </summary>
public sealed class PdfLayout
{
    /// <summary>Space between pages in content pixels (a third of an inch at 96 dpi).</summary>
    public const double DefaultGap = 32;

    private PdfLayout(IReadOnlyList<RectD> pages, double width, double height, double maxPageWidth, double maxPageHeight)
    {
        Pages = pages;
        Width = width;
        Height = height;
        MaxPageWidth = maxPageWidth;
        MaxPageHeight = maxPageHeight;
    }

    /// <summary>One rect per page, in strip coordinates.</summary>
    public IReadOnlyList<RectD> Pages { get; }

    /// <summary>Whole strip: every page plus the gaps between them.</summary>
    public double Width { get; }

    /// <summary>Whole strip: the tallest page.</summary>
    public double Height { get; }

    /// <summary>Largest page edge in each direction; the per-page render target is sized from these.</summary>
    public double MaxPageWidth { get; }
    public double MaxPageHeight { get; }

    public int PageCount => Pages.Count;

    public static PdfLayout Compute(IReadOnlyList<(double Width, double Height)> pageSizes, double gap = DefaultGap)
    {
        if (pageSizes.Count == 0)
            throw new ArgumentException("A PDF needs at least one page", nameof(pageSizes));

        double maxW = 0, maxH = 0;
        foreach ((double w, double h) in pageSizes)
        {
            maxW = Math.Max(maxW, Sane(w));
            maxH = Math.Max(maxH, Sane(h));
        }

        var rects = new RectD[pageSizes.Count];
        double x = 0;
        for (int i = 0; i < pageSizes.Count; i++)
        {
            double w = Sane(pageSizes[i].Width);
            double h = Sane(pageSizes[i].Height);
            rects[i] = new RectD(x, (maxH - h) / 2.0, w, h);
            x += w + (i < pageSizes.Count - 1 ? gap : 0);
        }

        return new PdfLayout(rects, x, maxH, maxW, maxH);
    }

    /// <summary>The page whose centre is nearest <paramref name="x"/> (strip coordinates).</summary>
    public int PageAt(double x)
    {
        int best = 0;
        double bestDistance = double.MaxValue;
        for (int i = 0; i < Pages.Count; i++)
        {
            double distance = Math.Abs(Pages[i].X + Pages[i].Width / 2.0 - x);
            if (distance < bestDistance)
            {
                bestDistance = distance;
                best = i;
            }
        }
        return best;
    }

    // A page with no usable size (corrupt MediaBox) still gets a slot rather than collapsing the strip.
    private static double Sane(double v) => v > 0 && !double.IsInfinity(v) && !double.IsNaN(v) ? v : 1;
}
