using System;

namespace Looker.Rendering;

public enum ZoomMode
{
    Fit,
    Free,
    ActualSize,
}

/// <summary>A rectangle in viewport DIP space. Kept separate from Windows.Foundation.Rect so this stays pure.</summary>
public readonly record struct RectD(double X, double Y, double Width, double Height);

/// <summary>
/// All zoom/pan state as pure math (no Windows types) so it is fully unit-testable — the anchor
/// and clamping logic is the fiddly part worth pinning down with tests. Units: viewport in DIPs,
/// content in image pixels, <see cref="Scale"/> in DIPs-per-image-pixel. "Actual size" (100%) is
/// DPI-aware: one image pixel maps to one physical display pixel.
/// </summary>
public sealed class ZoomPanController
{
    private const double MinScaleFactor = 0.5;      // can zoom out to half of fit/actual
    private const double MaxScaleFactor = 8.0;      // ...and in to 8x actual pixels
    private const double MinVisibleFraction = 0.2;  // >=20% of the image stays on-screen when panning

    private double _viewportW;
    private double _viewportH;
    private double _contentW;
    private double _contentH;
    private double _rasterization = 1.0;   // physical px per DIP
    private double _scale = 1.0;           // DIPs per image px
    private double _offsetX;               // image top-left, viewport DIPs
    private double _offsetY;

    // Optional sub-rectangle of the content (content pixels) that "fit" and "centre" refer to instead of the
    // whole content: a multi-page PDF is laid out as one wide strip, and fitting means fitting the current
    // page, not the strip. Panning and clamping still use the whole content, so the neighbouring pages
    // remain reachable. Null = the whole content, i.e. ordinary images.
    private RectD? _focus;

    public ZoomMode Mode { get; private set; } = ZoomMode.Fit;
    public double Scale => _scale;

    public bool HasContent => _contentW > 0 && _contentH > 0 && _viewportW > 0 && _viewportH > 0;

    /// <summary>Scale at which the image (or the focus rect, when one is set) fits fully within the viewport
    /// (may upscale small images).</summary>
    public double FitScale => HasContent ? Math.Min(_viewportW / FocusW, _viewportH / FocusH) : 1.0;

    /// <summary>The rect "fit"/"centre" refer to: the focus rect, else the whole content.</summary>
    public RectD FocusRect => _focus ?? new RectD(0, 0, _contentW, _contentH);

    private double FocusW => _focus is { } f && f.Width > 0 ? f.Width : _contentW;
    private double FocusH => _focus is { } f && f.Height > 0 ? f.Height : _contentH;

    /// <summary>Scale at which one image pixel equals one physical display pixel (100%).</summary>
    public double ActualScale => 1.0 / _rasterization;

    /// <summary>The default "on open" scale: show at 100% when the image is smaller than the window,
    /// otherwise shrink to fit. Never upscales past actual size (a tiny image isn't blown up to fill
    /// a large window).</summary>
    public double DefaultScale => Math.Min(FitScale, ActualScale);

    public double MinScale => Math.Min(FitScale, ActualScale) * MinScaleFactor;
    public double MaxScale => Math.Max(FitScale, ActualScale) * MaxScaleFactor;

    /// <summary>Zoom relative to actual pixels, for the OSD readout (100% == actual size).</summary>
    public double ZoomPercent => ActualScale > 0 ? _scale / ActualScale * 100.0 : 100.0;

    /// <summary>New image: reset to Fit, centered on <paramref name="focus"/> (null = the whole content).</summary>
    public void Reset(double viewportW, double viewportH, double contentW, double contentH, double rasterization, RectD? focus = null)
    {
        _viewportW = viewportW;
        _viewportH = viewportH;
        _contentW = contentW;
        _contentH = contentH;
        _rasterization = rasterization > 0 ? rasterization : 1.0;
        _focus = focus;
        ApplyFit();
    }

    /// <summary>Move the focus (e.g. to another PDF page). In Fit mode the new focus is fitted; at any other
    /// zoom the scale is kept and the view is centred on it, so a reader zoomed into the text stays zoomed
    /// while turning pages.</summary>
    public void SetFocus(RectD? focus, bool recenter = true)
    {
        _focus = focus;
        if (!HasContent || !recenter)
            return; // recenter false: the user panned there themselves; only the fit/clamp reference moves
        if (Mode == ZoomMode.Fit)
        {
            ApplyFit();
        }
        else
        {
            CenterContent();
            ClampOffset();
        }
    }

    /// <summary>Viewport resized: keep the current zoom (refit if in Fit mode), re-clamp the pan.</summary>
    public void SetViewport(double viewportW, double viewportH)
    {
        _viewportW = viewportW;
        _viewportH = viewportH;
        if (Mode == ZoomMode.Fit)
            ApplyFit();
        else
            ClampOffset();
    }

    public void FitToViewport() => ApplyFit();

    public void SetActualSize()
    {
        if (!HasContent)
            return;
        SetScaleAt(ActualScale, _viewportW / 2.0, _viewportH / 2.0);
        Mode = ZoomMode.ActualSize;
    }

    /// <summary>Multiply the zoom by <paramref name="factor"/>, keeping the image point under the anchor fixed.</summary>
    public void ZoomAt(double factor, double anchorX, double anchorY)
    {
        if (!HasContent)
            return;
        SetScaleAt(_scale * factor, anchorX, anchorY);
        Mode = NearlyEqual(_scale, DefaultScale) ? ZoomMode.Fit
             : NearlyEqual(_scale, ActualScale) ? ZoomMode.ActualSize
             : ZoomMode.Free;
    }

    /// <summary>Double-click behaviour: Fit ↔ 100% at the clicked point.</summary>
    public void ToggleFitActual(double anchorX, double anchorY)
    {
        if (!HasContent)
            return;
        if (AtFitPosition())
        {
            SetScaleAt(ActualScale, anchorX, anchorY);
            Mode = ZoomMode.ActualSize;
        }
        else
        {
            ApplyFit();
        }
    }

    /// <summary>Drag pan by a viewport-space delta (clamped so the image can't be flung away).</summary>
    public void Pan(double dxDip, double dyDip)
    {
        if (!HasContent)
            return;
        _offsetX += dxDip;
        _offsetY += dyDip;
        ClampOffset();
    }

    public RectD GetImageRect() => new(_offsetX, _offsetY, _contentW * _scale, _contentH * _scale);

    /// <summary>True when the view is exactly what a fit would produce: the default scale *and* centred on the
    /// focus. The mode alone is not enough — a drag across a page strip leaves Fit mode set but the view on
    /// another page, and the user expects the next double-click to fit that page, not to jump to 100%.</summary>
    public bool AtFitPosition()
    {
        if (!HasContent || !NearlyEqual(_scale, DefaultScale))
            return false;
        (double x, double y) = CenteredOffset();
        return Math.Abs(_offsetX - x) < 0.5 && Math.Abs(_offsetY - y) < 0.5;
    }

    private void ApplyFit()
    {
        Mode = ZoomMode.Fit;
        _scale = HasContent ? DefaultScale : 1.0;
        CenterContent();
    }

    private void SetScaleAt(double newScale, double anchorX, double anchorY)
    {
        newScale = Clamp(newScale, MinScale, MaxScale);
        // Keep the image point currently under the anchor stationary as we rescale.
        double imageX = (anchorX - _offsetX) / _scale;
        double imageY = (anchorY - _offsetY) / _scale;
        _scale = newScale;
        _offsetX = anchorX - imageX * _scale;
        _offsetY = anchorY - imageY * _scale;
        ClampOffset();
    }

    private void CenterContent() => (_offsetX, _offsetY) = CenteredOffset();

    private (double X, double Y) CenteredOffset()
    {
        RectD f = FocusRect;
        return ((_viewportW - f.Width * _scale) / 2.0 - f.X * _scale,
                (_viewportH - f.Height * _scale) / 2.0 - f.Y * _scale);
    }

    private void ClampOffset()
    {
        // The "keep 20% on screen" rule is measured against the focus (one page) when there is one: against a
        // 100-page strip it would be 20 pages, which would forbid the very fit-to-page positions we start from.
        _offsetX = ClampAxis(_offsetX, _contentW * _scale, _viewportW, Math.Min(_contentW, FocusW) * _scale);
        _offsetY = ClampAxis(_offsetY, _contentH * _scale, _viewportH, Math.Min(_contentH, FocusH) * _scale);
    }

    private static double ClampAxis(double offset, double contentExtent, double viewportExtent, double keepExtent)
    {
        if (contentExtent <= viewportExtent)
            return (viewportExtent - contentExtent) / 2.0; // fits: center, no panning

        double keep = keepExtent * MinVisibleFraction;
        double max = viewportExtent - keep; // top/left edge furthest into the bottom/right
        double min = keep - contentExtent;  // ...and furthest into the top/left
        return Clamp(offset, min, max);
    }

    private static double Clamp(double value, double lo, double hi) => value < lo ? lo : (value > hi ? hi : value);

    private static bool NearlyEqual(double a, double b)
        => Math.Abs(a - b) <= 1e-6 * Math.Max(1.0, Math.Max(Math.Abs(a), Math.Abs(b)));
}
