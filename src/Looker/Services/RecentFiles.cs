using System;
using System.Collections.Generic;

namespace Looker.Services;

/// <summary>
/// Pure most-recently-used list logic (WinUI-free; linked into the unit tests). Storage is a single
/// newline-joined string so it fits one LocalSettings value.
/// </summary>
public static class RecentFiles
{
    public const int Capacity = 12;
    private const char Separator = '\n';

    public static IReadOnlyList<string> Parse(string? stored)
        => string.IsNullOrEmpty(stored)
            ? Array.Empty<string>()
            : stored.Split(Separator, StringSplitOptions.RemoveEmptyEntries);

    public static string Serialize(IEnumerable<string> paths) => string.Join(Separator, paths);

    /// <summary>Return <paramref name="existing"/> with <paramref name="path"/> moved/inserted at the front
    /// (case-insensitive dedupe, Windows paths) and trimmed to <paramref name="capacity"/>.</summary>
    public static List<string> Push(IEnumerable<string> existing, string path, int capacity = Capacity)
    {
        var result = new List<string>(capacity) { path };
        foreach (string p in existing)
        {
            if (result.Count >= capacity)
                break;
            if (!string.Equals(p, path, StringComparison.OrdinalIgnoreCase))
                result.Add(p);
        }
        return result;
    }

    /// <summary>Return <paramref name="existing"/> without <paramref name="path"/> (case-insensitive).</summary>
    public static List<string> Remove(IEnumerable<string> existing, string path)
    {
        var result = new List<string>(Capacity);
        foreach (string p in existing)
        {
            if (!string.Equals(p, path, StringComparison.OrdinalIgnoreCase))
                result.Add(p);
        }
        return result;
    }
}
