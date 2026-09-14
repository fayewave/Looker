using System;
using System.Diagnostics;
using System.IO;
using System.Text;

namespace Looker.Helpers;

/// <summary>
/// Cold-start timeline: <see cref="Mark"/> records milliseconds since process creation (not since Main, so
/// host/runtime startup and module initializers are visible). Marks are buffered and written to
/// <c>%TEMP%\looker-startup.log</c> in one go by <see cref="Flush"/> (first Win2D draw, and again a little
/// later) so the tracing itself adds no I/O to the path being measured. The second flush closes the trace:
/// marks after that point (navigation, later decodes) are dropped rather than buffered for the session.
/// </summary>
public static class StartupTrace
{
    private static readonly StringBuilder Buffer = new();
    private static readonly object Gate = new();
    private static readonly string LogPath = Path.Combine(Path.GetTempPath(), "looker-startup.log");
    private static readonly DateTime ProcessStart = SafeProcessStart();
    private static double _last;
    private static bool _flushedOnce;
    private static volatile bool _closed;

    private static DateTime SafeProcessStart()
    {
        try { return Process.GetCurrentProcess().StartTime; }
        catch { return DateTime.Now; }
    }

    /// <summary>Milliseconds since the process was created.</summary>
    public static double Now => (DateTime.Now - ProcessStart).TotalMilliseconds;

    public static void Mark(string label)
    {
        if (_closed)
            return;
        double now = Now;
        lock (Gate)
        {
            Buffer.Append(CultureInvariant(now)).Append(" ms  (+").Append(CultureInvariant(now - _last)).Append(")  [t").Append(Environment.CurrentManagedThreadId).Append("] ").Append(label).Append('\n');
            _last = now;
        }
    }

    private static string CultureInvariant(double ms) => ms.ToString("0", System.Globalization.CultureInfo.InvariantCulture).PadLeft(5);

    /// <summary>Write buffered marks. <paramref name="final"/> also closes the trace for the rest of the session.</summary>
    public static void Flush(string reason, bool final = false)
    {
        string text;
        lock (Gate)
        {
            if (final)
                _closed = true;
            if (Buffer.Length == 0)
                return;
            text = Buffer.ToString();
            Buffer.Clear();
        }
        try
        {
            string header = _flushedOnce ? string.Empty : $"\n=== {DateTime.Now:O}  pid {Environment.ProcessId}  ===\n";
            _flushedOnce = true;
            File.AppendAllText(LogPath, header + text + $"-- flushed at {reason}\n");
        }
        catch { /* diagnostics are best-effort */ }
    }
}
