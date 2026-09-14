using System;
using Microsoft.UI.Windowing;
using Windows.Graphics;

namespace Looker.Services;

/// <summary>
/// Saves and restores the window's size + position across sessions using only stable AppWindow APIs
/// (the first-party <c>AppWindow.PersistedStateId</c> is still experimental — see plan). The saved
/// rectangle is always the *restored* (non-maximized) bounds plus a maximized flag, so un-maximizing
/// after a restart returns to the pre-maximize size. Restore is validated against the current monitor
/// layout: a rectangle saved on a now-unplugged monitor is pulled back onto a visible work area rather
/// than opening off-screen.
/// </summary>
public sealed class WindowStateService
{
    // Fallback window when nothing valid is saved (logical size; AppWindow works in physical px, but at
    // 100% DPI these match and the clamp below fixes any overshoot on the actual monitor).
    private const int DefaultWidth = 1200;
    private const int DefaultHeight = 800;

    // If less than this fraction of a restored window would land on a work area, treat the saved spot as
    // stale (monitor removed / resolution shrank) and re-home it.
    private const double MinVisibleFraction = 0.3;

    private readonly AppWindow _appWindow;
    private readonly SettingsService _settings;

    // The last known *non-maximized* bounds. AppWindow reports the maximized rectangle while maximized,
    // so we shadow the restored bounds by recording them whenever the presenter is in the Normal state.
    private RectInt32 _lastNormalBounds;
    private bool _haveNormalBounds;

    public WindowStateService(AppWindow appWindow, SettingsService settings)
    {
        _appWindow = appWindow;
        _settings = settings;
        _appWindow.Changed += OnAppWindowChanged;
    }

    private OverlappedPresenter? Presenter => _appWindow.Presenter as OverlappedPresenter;

    private void OnAppWindowChanged(AppWindow sender, AppWindowChangedEventArgs args)
    {
        // Track the restored rectangle continuously so it's known at save time even when maximized.
        if ((args.DidSizeChange || args.DidPositionChange)
            && Presenter is { State: OverlappedPresenterState.Restored })
        {
            _lastNormalBounds = new RectInt32(_appWindow.Position.X, _appWindow.Position.Y, _appWindow.Size.Width, _appWindow.Size.Height);
            _haveNormalBounds = true;
        }
    }

    /// <summary>Place the window from saved state. Call before <c>Window.Activate()</c>.</summary>
    public void Restore()
    {
        WindowPlacement? saved = _settings.WindowPlacement;

        RectInt32 target = saved is { } p && p.Width > 0 && p.Height > 0
            ? new RectInt32(p.X, p.Y, p.Width, p.Height)
            : DefaultCentered();

        target = EnsureVisible(target);

        _appWindow.MoveAndResize(target);
        _lastNormalBounds = target;
        _haveNormalBounds = true;

        if (saved is { Maximized: true })
            Presenter?.Maximize();
    }

    /// <summary>Persist the current placement. Call from <c>Window.Closed</c>.</summary>
    public void Save()
    {
        bool maximized = Presenter is { State: OverlappedPresenterState.Maximized };

        // Prefer the shadowed restored bounds; fall back to the live rectangle when we never saw a Normal
        // state (e.g. launched straight into maximized) — better a valid-ish maximized rect than nothing.
        RectInt32 bounds = _haveNormalBounds
            ? _lastNormalBounds
            : new RectInt32(_appWindow.Position.X, _appWindow.Position.Y, _appWindow.Size.Width, _appWindow.Size.Height);

        if (bounds.Width <= 0 || bounds.Height <= 0)
            return;

        _settings.WindowPlacement = new WindowPlacement(bounds.X, bounds.Y, bounds.Width, bounds.Height, maximized);
    }

    // Pull a rectangle onto a real work area: if it's mostly off-screen (monitor removed, resolution
    // change), clamp it inside the nearest display's work area; if it's still garbage, center a default.
    private RectInt32 EnsureVisible(RectInt32 rect)
    {
        DisplayArea area = DisplayArea.GetFromRect(rect, DisplayAreaFallback.Nearest)
                           ?? DisplayArea.Primary;
        RectInt32 work = area.WorkArea;

        // Never larger than the work area.
        int width = Math.Min(rect.Width, work.Width);
        int height = Math.Min(rect.Height, work.Height);
        if (width <= 0 || height <= 0)
            return CenterIn(work, DefaultWidth, DefaultHeight);

        var candidate = new RectInt32(rect.X, rect.Y, width, height);
        if (VisibleFraction(candidate, work) >= MinVisibleFraction)
            return candidate;

        // Too little on-screen — clamp the top-left so the whole window sits inside the work area.
        int x = Math.Clamp(rect.X, work.X, work.X + work.Width - width);
        int y = Math.Clamp(rect.Y, work.Y, work.Y + work.Height - height);
        return new RectInt32(x, y, width, height);
    }

    private static RectInt32 DefaultCentered()
        => CenterIn(DisplayArea.Primary.WorkArea, DefaultWidth, DefaultHeight);

    private static RectInt32 CenterIn(RectInt32 work, int width, int height)
    {
        width = Math.Min(width, work.Width);
        height = Math.Min(height, work.Height);
        int x = work.X + (work.Width - width) / 2;
        int y = work.Y + (work.Height - height) / 2;
        return new RectInt32(x, y, width, height);
    }

    // Fraction of the rectangle's area that overlaps the work area (0 when fully off-screen).
    private static double VisibleFraction(RectInt32 rect, RectInt32 work)
    {
        long area = (long)rect.Width * rect.Height;
        if (area <= 0)
            return 0;

        int ix = Math.Max(rect.X, work.X);
        int iy = Math.Max(rect.Y, work.Y);
        int ir = Math.Min(rect.X + rect.Width, work.X + work.Width);
        int ib = Math.Min(rect.Y + rect.Height, work.Y + work.Height);
        long overlap = (long)Math.Max(0, ir - ix) * Math.Max(0, ib - iy);
        return (double)overlap / area;
    }
}
