using System;
using System.Collections.Generic;
using Looker.Navigation;
using Windows.Storage;

namespace Looker.Services;

/// <summary>Persisted window placement (physical pixels). <see cref="Maximized"/> stores whether the
/// window was maximized; the bounds are then the *restored* (pre-maximize) rectangle so un-maximizing
/// after a restart lands where the user left it.</summary>
public readonly record struct WindowPlacement(int X, int Y, int Width, int Height, bool Maximized);

/// <summary>
/// Thin wrapper over <see cref="ApplicationData"/> LocalSettings for the handful of preferences that
/// must survive restart. The app runs with package identity (registered MSIX), so LocalSettings is
/// available; every access is still guarded so a missing store degrades to sane defaults rather than
/// crashing.
/// </summary>
public sealed class SettingsService
{
    private const string SortFieldKey = "SortField";
    private const string SortDirectionKey = "SortDirection";
    private const string StripVisibleKey = "StripVisible";
    private const string StripHeightKey = "StripHeight";
    private const string InfoVisibleKey = "InfoVisible";
    private const string InfoWidthKey = "InfoWidth";
    private const string ExplorerVisibleKey = "ExplorerVisible";
    private const string ExplorerWidthKey = "ExplorerWidth";
    private const string WindowXKey = "WindowX";
    private const string WindowYKey = "WindowY";
    private const string WindowWKey = "WindowW";
    private const string WindowHKey = "WindowH";
    private const string WasMaximizedKey = "WasMaximized";
    private const string SlideshowSecondsKey = "SlideshowSeconds";
    private const string CacheBudgetMBKey = "CacheBudgetMB";
    private const string RecentFilesKey = "RecentFiles";
    private const string DefaultHintDismissedKey = "DefaultHintDismissed";
    private const string WheelModeKey = "WheelMode";
    private const string ZoomAnchorKey = "ZoomAnchor";
    private const string RememberWindowKey = "RememberWindow";
    private const string ThemeKey = "Theme";
    private const string RecentsEnabledKey = "RecentsEnabled";

    private readonly ApplicationDataContainer? _local = TryGetContainer();

    private static ApplicationDataContainer? TryGetContainer()
    {
        try { return ApplicationData.Current.LocalSettings; }
        catch { return null; } // no package identity (e.g. unit host) — run on defaults
    }

    public SortMode Sort
    {
        get
        {
            SortField field = ReadEnum(SortFieldKey, SortField.Name);
            SortDirection direction = ReadEnum(SortDirectionKey, SortDirection.Ascending);
            return new SortMode(field, direction);
        }
        set
        {
            Write(SortFieldKey, (int)value.Field);
            Write(SortDirectionKey, (int)value.Direction);
        }
    }

    public bool StripVisible
    {
        get => Read(StripVisibleKey, false);
        set => Write(StripVisibleKey, value);
    }

    /// <summary>Thumbnail strip height in DIPs, set by dragging the strip's top edge. Default 96; the range
    /// mirrors <c>ThumbnailStrip.Min/MaxStripHeight</c>.</summary>
    public int StripHeight
    {
        get => Math.Clamp(Read(StripHeightKey, 96), 56, 480);
        set => Write(StripHeightKey, Math.Clamp(value, 56, 480));
    }

    public bool InfoVisible
    {
        get => Read(InfoVisibleKey, false);
        set => Write(InfoVisibleKey, value);
    }

    /// <summary>Info card width in DIPs (card + margins), set by dragging its left edge. Default 320; the range
    /// mirrors <c>InfoPanel.Min/MaxPanelWidth</c>.</summary>
    public int InfoWidth
    {
        get => Math.Clamp(Read(InfoWidthKey, 320), 260, 640);
        set => Write(InfoWidthKey, Math.Clamp(value, 260, 640));
    }

    public bool ExplorerVisible
    {
        get => Read(ExplorerVisibleKey, false);
        set => Write(ExplorerVisibleKey, value);
    }

    /// <summary>File explorer card width in DIPs (card + margins), set by dragging its right edge. Default 320; the
    /// range mirrors <c>FileExplorer.Min/MaxPanelWidth</c>.</summary>
    public int ExplorerWidth
    {
        get => Math.Clamp(Read(ExplorerWidthKey, 320), 260, 640);
        set => Write(ExplorerWidthKey, Math.Clamp(value, 260, 640));
    }

    /// <summary>Slideshow dwell time per image, seconds. Default 4, clamped to a sane range.</summary>
    public int SlideshowSeconds
    {
        get => Math.Clamp(Read(SlideshowSecondsKey, 4), 1, 120);
        set => Write(SlideshowSecondsKey, Math.Clamp(value, 1, 120));
    }

    /// <summary>Decode cache budget in megabytes (applied at launch). Default 512, range 128–2048.</summary>
    public int CacheBudgetMB
    {
        get => Math.Clamp(Read(CacheBudgetMBKey, 512), 128, 2048);
        set => Write(CacheBudgetMBKey, Math.Clamp(value, 128, 2048));
    }

    /// <summary>Whether Looker remembers recently shown photos at all. Off: nothing is recorded and the landing
    /// page hides the list (the host clears the stored list when this is switched off).</summary>
    public bool RecentsEnabled
    {
        get => Read(RecentsEnabledKey, true);
        set => Write(RecentsEnabledKey, value);
    }

    /// <summary>Most recently opened files, newest first (M9 empty-state list). Entries are not
    /// validated here; the empty state drops paths that no longer exist when it renders.</summary>
    public IReadOnlyList<string> RecentFiles => Services.RecentFiles.Parse(Read(RecentFilesKey, string.Empty));

    /// <summary>Move <paramref name="path"/> to the front. Called for every image shown (browsing counts, not
    /// just explicit opens), so it returns without a store write when the path is already first.</summary>
    public void PushRecent(string path)
    {
        if (!RecentsEnabled)
            return;
        IReadOnlyList<string> existing = RecentFiles;
        if (existing.Count > 0 && string.Equals(existing[0], path, StringComparison.OrdinalIgnoreCase))
            return;
        Write(RecentFilesKey, Services.RecentFiles.Serialize(Services.RecentFiles.Push(existing, path)));
    }

    public void RemoveRecent(string path)
        => Write(RecentFilesKey, Services.RecentFiles.Serialize(Services.RecentFiles.Remove(RecentFiles, path)));

    public void ClearRecent() => Write(RecentFilesKey, string.Empty);

    /// <summary>The landing page's "Set Looker as your default photo viewer" button was dismissed with its X:
    /// never show it again.</summary>
    public bool DefaultHintDismissed
    {
        get => Read(DefaultHintDismissedKey, false);
        set => Write(DefaultHintDismissedKey, value);
    }

    /// <summary>What the mouse wheel does over the image. Default: zoom.</summary>
    public WheelMode WheelMode
    {
        get => ReadEnum(WheelModeKey, WheelMode.Zoom);
        set => Write(WheelModeKey, (int)value);
    }

    /// <summary>Where wheel and double-click zoom are anchored. Default: the pointer.</summary>
    public ZoomAnchor ZoomAnchor
    {
        get => ReadEnum(ZoomAnchorKey, ZoomAnchor.Pointer);
        set => Write(ZoomAnchorKey, (int)value);
    }

    /// <summary>Whether the window size and position are saved on close and restored on launch. Default on.
    /// Off: nothing is saved (the host also clears the stored placement when this is switched off), so every
    /// launch opens the default centred window.</summary>
    public bool RememberWindowPlacement
    {
        get => Read(RememberWindowKey, true);
        set => Write(RememberWindowKey, value);
    }

    /// <summary>Window chrome colour. Default: black.</summary>
    public AppTheme Theme
    {
        get => ReadEnum(ThemeKey, AppTheme.Black);
        set => Write(ThemeKey, (int)value);
    }

    /// <summary>Forget every stored preference (sort, panels, window placement, slideshow, cache budget, recent list,
    /// wheel mode, zoom anchor, window memory, theme, dismissed hints): the next read of each returns its default. The caller re-applies the
    /// live state.</summary>
    public void ResetAll()
    {
        if (_local is null)
            return;
        try { _local.Values.Clear(); }
        catch { /* transient store failure: nothing cleared this time */ }
    }

    /// <summary>The saved window placement, or null when none has been stored (first run / cleared store).</summary>
    public WindowPlacement? WindowPlacement
    {
        get
        {
            if (_local is null
                || !_local.Values.TryGetValue(WindowWKey, out object? rawW) || rawW is not int w || w <= 0
                || !_local.Values.TryGetValue(WindowHKey, out object? rawH) || rawH is not int h || h <= 0)
                return null;

            int x = Read(WindowXKey, 0);
            int y = Read(WindowYKey, 0);
            bool max = Read(WasMaximizedKey, false);
            return new WindowPlacement(x, y, w, h, max);
        }
        set
        {
            if (value is not { } p)
                return;
            Write(WindowXKey, p.X);
            Write(WindowYKey, p.Y);
            Write(WindowWKey, p.Width);
            Write(WindowHKey, p.Height);
            Write(WasMaximizedKey, p.Maximized);
        }
    }

    /// <summary>Forget the saved window placement: the next launch opens the default centred window.</summary>
    public void ClearWindowPlacement()
    {
        if (_local is null)
            return;
        foreach (string key in new[] { WindowXKey, WindowYKey, WindowWKey, WindowHKey, WasMaximizedKey })
        {
            try { _local.Values.Remove(key); }
            catch { /* transient store failure */ }
        }
    }

    private TEnum ReadEnum<TEnum>(string key, TEnum fallback) where TEnum : struct, Enum
    {
        if (_local?.Values.TryGetValue(key, out object? raw) == true && raw is int i && Enum.IsDefined(typeof(TEnum), i))
            return (TEnum)Enum.ToObject(typeof(TEnum), i);
        return fallback;
    }

    private T Read<T>(string key, T fallback)
    {
        if (_local?.Values.TryGetValue(key, out object? raw) == true && raw is T value)
            return value;
        return fallback;
    }

    private void Write(string key, object value)
    {
        if (_local is null)
            return;
        try { _local.Values[key] = value; }
        catch { /* transient store failure: preference simply isn't persisted this session */ }
    }
}
