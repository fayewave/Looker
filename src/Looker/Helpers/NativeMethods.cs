using System;
using System.Runtime.InteropServices;

namespace Looker.Helpers;

/// <summary>
/// The app's few Win32 P/Invokes. Everything else goes through managed/WinRT APIs (StorageFile recycle,
/// WinRT Clipboard, Explorer /select, Windows.Graphics.Imaging transcode). Setting the desktop wallpaper
/// has no managed equivalent that works reliably from a packaged full-trust app, and the single-instance
/// redirect (M9) follows the documented Windows App SDK pattern, which needs an event handle pumped
/// through <see cref="CoWaitForMultipleObjects"/> plus a foreground hand-off.
/// </summary>
internal static class NativeMethods
{
    private const uint SPI_SETDESKWALLPAPER = 0x0014;
    private const uint SPIF_UPDATEINIFILE = 0x01;
    private const uint SPIF_SENDCHANGE = 0x02;

    public const uint INFINITE = 0xFFFFFFFF;

    [DllImport("user32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool SystemParametersInfoW(uint uiAction, uint uiParam, string pvParam, uint fWinIni);

    /// <summary>Set the desktop wallpaper to the image at <paramref name="imagePath"/> (a jpg/png/bmp that
    /// Windows can render). Persists across restarts and broadcasts the change to the shell.</summary>
    public static bool SetDesktopWallpaper(string imagePath)
        => SystemParametersInfoW(SPI_SETDESKWALLPAPER, 0, imagePath, SPIF_UPDATEINIFILE | SPIF_SENDCHANGE);

    // --- Single-instance redirect (Program.TryRedirectToExistingInstance) ---

    [DllImport("kernel32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
    public static extern IntPtr CreateEvent(IntPtr lpEventAttributes, [MarshalAs(UnmanagedType.Bool)] bool bManualReset, [MarshalAs(UnmanagedType.Bool)] bool bInitialState, string? lpName);

    [DllImport("kernel32.dll", SetLastError = true)]
    [return: MarshalAs(UnmanagedType.Bool)]
    public static extern bool SetEvent(IntPtr hEvent);

    [DllImport("kernel32.dll", SetLastError = true)]
    [return: MarshalAs(UnmanagedType.Bool)]
    public static extern bool CloseHandle(IntPtr hObject);

    [DllImport("ole32.dll")]
    public static extern uint CoWaitForMultipleObjects(uint dwFlags, uint dwMilliseconds, uint nHandles, IntPtr[] pHandles, out uint dwIndex);

    [DllImport("user32.dll")]
    [return: MarshalAs(UnmanagedType.Bool)]
    public static extern bool SetForegroundWindow(IntPtr hWnd);
}
