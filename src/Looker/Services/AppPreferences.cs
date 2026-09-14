using Windows.UI;

namespace Looker.Services;

/// <summary>What the mouse wheel does over the image (Settings). Stored as its int value.</summary>
public enum WheelMode
{
    /// <summary>Wheel zooms in/out around the pointer (default).</summary>
    Zoom = 0,
    /// <summary>Wheel down = next image, wheel up = previous image.</summary>
    Navigate = 1,
}

/// <summary>Where a pointer-driven zoom (wheel, double-click) is anchored (Settings). Stored as its int value.
/// Keyboard zoom always anchors on the middle of the view.</summary>
public enum ZoomAnchor
{
    /// <summary>Zoom around the mouse pointer, so the pixel under it stays put (default).</summary>
    Pointer = 0,
    /// <summary>Zoom into the middle of the view, like the keyboard shortcuts.</summary>
    Center = 1,
}

/// <summary>Window chrome colour (Settings). The preview checkerboard is deliberately not part of a theme: it
/// stays the same near-black pattern so an image reads identically under either.</summary>
public enum AppTheme
{
    /// <summary>Solid black window (default).</summary>
    Black = 0,
    /// <summary>Dark grey window, matching the stock WinUI dark layer.</summary>
    DarkGrey = 1,
}

/// <summary>The handful of colours a theme drives: the window/panel fill and the two dialog layers. Everything
/// else (buttons, text, accent, checkerboard) is theme-independent.</summary>
public readonly record struct ThemeColors(Color Window, Color DialogStrip, Color DialogOverlay)
{
    public static ThemeColors For(AppTheme theme) => theme switch
    {
        AppTheme.DarkGrey => new ThemeColors(
            Color.FromArgb(0xFF, 0x1F, 0x1F, 0x1F),
            Color.FromArgb(0xFF, 0x26, 0x26, 0x26),
            Color.FromArgb(0xFF, 0x2E, 0x2E, 0x2E)),
        _ => new ThemeColors(
            Color.FromArgb(0xFF, 0x00, 0x00, 0x00),
            Color.FromArgb(0xFF, 0x0C, 0x0C, 0x0C),
            Color.FromArgb(0xFF, 0x14, 0x14, 0x14)),
    };
}
