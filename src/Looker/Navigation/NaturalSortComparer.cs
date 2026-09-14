using System;
using System.Collections.Generic;
using System.Runtime.InteropServices;

namespace Looker.Navigation;

/// <summary>
/// Orders strings with the shell's <c>StrCmpLogicalW</c> — the exact "natural" ordering
/// Explorer uses (so <c>img2</c> sorts before <c>img10</c>, digit runs compare numerically,
/// case-insensitively). Wrapping the OS API means we match Explorer rather than re-implementing
/// its quirks. (Direct P/Invoke here; CsWin32 arrives at M3 for the COM shell interfaces.)
/// </summary>
public sealed class NaturalSortComparer : IComparer<string>
{
    public static readonly NaturalSortComparer Instance = new();

    public int Compare(string? x, string? y)
    {
        if (ReferenceEquals(x, y)) return 0;
        if (x is null) return -1;
        if (y is null) return 1;
        return StrCmpLogicalW(x, y);
    }

    [DllImport("shlwapi.dll", CharSet = CharSet.Unicode, ExactSpelling = true)]
    private static extern int StrCmpLogicalW(string psz1, string psz2);
}
