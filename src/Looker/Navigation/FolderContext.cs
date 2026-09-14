using System;
using System.Collections.Generic;
using System.IO;
using System.Threading.Tasks;
using Looker.Imaging;

namespace Looker.Navigation;

/// <summary>Result of a live (watcher-driven) list mutation, so the caller can mirror it into the
/// thumbnail collection without a full rebuild. <see cref="Index"/> is the affected list position;
/// <see cref="AffectsCurrent"/> is set when the currently displayed file was the one removed.</summary>
public readonly record struct FolderMutation(bool Changed, int Index, bool AffectsCurrent, bool CountChanged = false)
{
    public static readonly FolderMutation None = new(false, -1, false);

    /// <summary>An unsupported file came or went: the nav list is untouched (nothing for the strip to do) but
    /// the "3 / 128" position counter, which counts every file in the folder, needs re-rendering.</summary>
    public static readonly FolderMutation CountOnly = new(false, -1, false, CountChanged: true);
}

/// <summary>
/// The current folder as an ordered, navigable list of supported images. Enumeration runs off
/// the UI thread; ordering matches Explorer via <see cref="NaturalSortComparer"/>. Nav wraps at
/// the ends for prev/next; Home/End clamp. Live folder changes (M6) arrive through
/// <see cref="OnFileCreated"/>/<see cref="OnFileDeleted"/>/<see cref="OnFileModified"/>, each keeping
/// the current selection pinned to the same file where possible.
/// </summary>
public sealed class FolderContext
{
    private readonly record struct FileEntry(string Path, string Name, long ModifiedTicks, long Size);

    private readonly List<FileEntry> _entries = new();
    // Every other (unsupported, non-hidden) file in the folder, unsorted: only counted and ranked, never shown.
    private readonly List<FileEntry> _others = new();
    private SortMode _sort = SortMode.Default;

    public string? FolderPath { get; private set; }
    public int Count => _entries.Count;
    public int CurrentIndex { get; private set; } = -1;
    public SortMode Sort => _sort;

    public string? CurrentPath =>
        CurrentIndex >= 0 && CurrentIndex < _entries.Count ? _entries[CurrentIndex].Path : null;

    public long CurrentModifiedTicks =>
        CurrentIndex >= 0 && CurrentIndex < _entries.Count ? _entries[CurrentIndex].ModifiedTicks : 0;

    /// <summary>Every file in the folder, supported or not (hidden/system files excluded, like the explorer card).</summary>
    public int TotalFileCount => _entries.Count + _others.Count;

    /// <summary>Human position for the status row, e.g. "3 / 128" (1-based; empty when no folder). Counts *every*
    /// file in the folder, not just the ones Looker can show: the position is the current image's rank among all
    /// of them under the active sort, so it matches where the file sits in the explorer card and in File
    /// Explorer, and the total is the folder's file count.</summary>
    public string PositionLabel
    {
        get
        {
            if (Count == 0 || CurrentIndex < 0 || CurrentIndex >= _entries.Count)
                return string.Empty;
            FileEntry current = _entries[CurrentIndex];
            Comparison<FileEntry> comparison = BuildComparison(_sort);
            int before = 0;
            foreach (FileEntry other in _others)
            {
                if (comparison(other, current) < 0)
                    before++;
            }
            return $"{CurrentIndex + 1 + before} / {TotalFileCount}";
        }
    }

    public ImageRef? CurrentRef => RefAt(CurrentIndex);

    /// <summary>The file at <paramref name="index"/> (no wrapping), or null if out of range.</summary>
    public ImageRef? RefAt(int index)
    {
        if (index < 0 || index >= _entries.Count)
            return null;
        FileEntry entry = _entries[index];
        return new ImageRef(entry.Path, entry.ModifiedTicks);
    }

    /// <summary>Wrap an index into range (for building the preload window across folder ends).</summary>
    public int WrapIndex(int index)
    {
        if (_entries.Count == 0)
            return -1;
        int n = _entries.Count;
        return ((index % n) + n) % n;
    }

    /// <summary>Enumerate the folder containing <paramref name="filePath"/> and select that file.</summary>
    public async Task LoadFileAsync(string filePath, SortMode sort)
    {
        string? folder = Path.GetDirectoryName(filePath);
        await LoadCoreAsync(folder, sort);
        CurrentIndex = IndexOfPath(filePath);
        if (CurrentIndex < 0 && _entries.Count > 0)
            CurrentIndex = 0;
    }

    /// <summary>Enumerate <paramref name="folderPath"/> and select the first image.</summary>
    public async Task LoadFolderAsync(string folderPath, SortMode sort)
    {
        await LoadCoreAsync(folderPath, sort);
        CurrentIndex = _entries.Count > 0 ? 0 : -1;
    }

    private async Task LoadCoreAsync(string? folder, SortMode sort)
    {
        _sort = sort;
        (List<FileEntry> entries, List<FileEntry> others) = await Task.Run(() => Enumerate(folder));
        _entries.Clear();
        _entries.AddRange(entries);
        _others.Clear();
        _others.AddRange(others);
        SortEntries();
        FolderPath = folder;
    }

    /// <summary>The folder's files split into the ones Looker can show (the nav list) and the rest (counted only).
    /// Hidden/system files (Thumbs.db, desktop.ini) stay out of the count, as they do in the explorer card.</summary>
    private static (List<FileEntry> Supported, List<FileEntry> Others) Enumerate(string? folder)
    {
        var list = new List<FileEntry>();
        var others = new List<FileEntry>();
        if (string.IsNullOrEmpty(folder))
            return (list, others);

        try
        {
            foreach (FileInfo fi in new DirectoryInfo(folder).EnumerateFiles())
            {
                if (SupportedFormats.IsSupported(fi.Name))
                    list.Add(new FileEntry(fi.FullName, fi.Name, fi.LastWriteTimeUtc.Ticks, fi.Length));
                else if (!IsHidden(fi))
                    others.Add(new FileEntry(fi.FullName, fi.Name, fi.LastWriteTimeUtc.Ticks, fi.Length));
            }
        }
        catch (Exception ex) when (ex is UnauthorizedAccessException or DirectoryNotFoundException or IOException)
        {
            // Folder vanished or is inaccessible: navigate what we managed to read (possibly nothing).
        }

        return (list, others);
    }

    private static bool IsHidden(FileInfo fi)
        => (fi.Attributes & (FileAttributes.Hidden | FileAttributes.System)) != 0;

    /// <summary>Forget the folder: no entries, no selection (the Home button's way back to the landing page).</summary>
    public void Clear()
    {
        _entries.Clear();
        _others.Clear();
        FolderPath = null;
        CurrentIndex = -1;
    }

    public void SetSort(SortMode sort)
    {
        string? current = CurrentPath;
        _sort = sort;
        SortEntries();
        CurrentIndex = current is not null ? IndexOfPath(current) : (_entries.Count > 0 ? 0 : -1);
    }

    // prev/next wrap at the ends (plan: "wraparound at ends"); Home/End clamp.
    public bool MoveNext() => SetIndexWrapped(CurrentIndex + 1);
    public bool MovePrevious() => SetIndexWrapped(CurrentIndex - 1);
    public bool MoveFirst() => SetIndex(0);
    public bool MoveLast() => SetIndex(_entries.Count - 1);

    /// <summary>Jump to an absolute index (thumbnail click). No wrapping.</summary>
    public bool MoveTo(int index) => SetIndex(index);

    // --- Live folder mutations (M6 FileSystemWatcher) ---
    // Each keeps _entries sorted and CurrentIndex pointed at the same file where possible, and
    // returns where the list changed so the thumbnail strip mirrors it with one insert/remove.

    /// <summary>A file appeared: a supported one is inserted in sorted position; any other is only counted
    /// (<see cref="FolderMutation.CountOnly"/>).</summary>
    public FolderMutation OnFileCreated(string path)
    {
        if (IndexOfPath(path) >= 0 || IndexOfOther(path) >= 0)
            return FolderMutation.None;

        FileInfo fi;
        try
        {
            fi = new FileInfo(path);
            if (!fi.Exists)
                return FolderMutation.None;
        }
        catch (Exception ex) when (ex is IOException or UnauthorizedAccessException)
        {
            return FolderMutation.None;
        }

        var entry = new FileEntry(fi.FullName, fi.Name, fi.LastWriteTimeUtc.Ticks, fi.Length);
        if (!SupportedFormats.IsSupported(path))
        {
            if (IsHidden(fi))
                return FolderMutation.None;
            _others.Add(entry);
            return FolderMutation.CountOnly;
        }
        int index = FindInsertIndex(entry);
        _entries.Insert(index, entry);
        if (index <= CurrentIndex)
            CurrentIndex++; // keep pointing at the same displayed file
        return new FolderMutation(true, index, false);
    }

    /// <summary>A file vanished: remove it and clamp the selection (advancing if it was current). An unsupported
    /// file only leaves the count (<see cref="FolderMutation.CountOnly"/>).</summary>
    public FolderMutation OnFileDeleted(string path)
    {
        int index = IndexOfPath(path);
        if (index < 0)
        {
            int other = IndexOfOther(path);
            if (other < 0)
                return FolderMutation.None;
            _others.RemoveAt(other);
            return FolderMutation.CountOnly;
        }

        bool affectsCurrent = index == CurrentIndex;
        _entries.RemoveAt(index);

        if (index < CurrentIndex)
            CurrentIndex--;
        else if (affectsCurrent && CurrentIndex >= _entries.Count)
            CurrentIndex = _entries.Count - 1; // fell off the end → land on the new last (or -1 if empty)

        return new FolderMutation(true, index, affectsCurrent);
    }

    /// <summary>A file's content/timestamp changed: refresh its mtime (for cache invalidation). Under a
    /// date sort the new mtime can move it, so the position may change.</summary>
    public FolderMutation OnFileModified(string path)
    {
        int index = IndexOfPath(path);
        int other = index < 0 ? IndexOfOther(path) : -1;
        if (index < 0 && other < 0)
            return FolderMutation.None;

        long ticks;
        long size;
        try
        {
            var fi = new FileInfo(path);
            if (!fi.Exists)
                return FolderMutation.None;
            ticks = fi.LastWriteTimeUtc.Ticks;
            size = fi.Length;
        }
        catch (Exception ex) when (ex is IOException or UnauthorizedAccessException)
        {
            return FolderMutation.None;
        }

        if (index < 0)
        {
            // Only its rank under a date/size sort can move; the counter picks that up on the next navigation.
            _others[other] = _others[other] with { ModifiedTicks = ticks, Size = size };
            return FolderMutation.None;
        }

        FileEntry old = _entries[index];
        if (ticks == old.ModifiedTicks && size == old.Size)
            return FolderMutation.None; // spurious watcher event: nothing observable changed

        _entries[index] = old with { ModifiedTicks = ticks, Size = size };
        return new FolderMutation(true, index, index == CurrentIndex);
    }

    /// <summary>Sorted insert position for <paramref name="entry"/> under the active sort.</summary>
    private int FindInsertIndex(FileEntry entry)
    {
        Comparison<FileEntry> comparison = BuildComparison(_sort);
        int i = 0;
        while (i < _entries.Count && comparison(_entries[i], entry) <= 0)
            i++;
        return i;
    }

    private bool SetIndexWrapped(int index)
    {
        if (_entries.Count == 0)
            return false;
        int n = _entries.Count;
        index = ((index % n) + n) % n;
        return SetIndex(index);
    }

    private bool SetIndex(int index)
    {
        if (index < 0 || index >= _entries.Count || index == CurrentIndex)
            return false;
        CurrentIndex = index;
        return true;
    }

    private void SortEntries()
    {
        Comparison<FileEntry> comparison = BuildComparison(_sort);
        _entries.Sort(comparison);
    }

    private static Comparison<FileEntry> BuildComparison(SortMode sort)
    {
        int dir = sort.Direction == SortDirection.Ascending ? 1 : -1;
        IComparer<string> natural = NaturalSortComparer.Instance;

        return sort.Field switch
        {
            SortField.DateModified => (a, b) =>
            {
                int c = a.ModifiedTicks.CompareTo(b.ModifiedTicks);
                return dir * (c != 0 ? c : natural.Compare(a.Name, b.Name));
            },
            SortField.Size => (a, b) =>
            {
                int c = a.Size.CompareTo(b.Size);
                return dir * (c != 0 ? c : natural.Compare(a.Name, b.Name));
            },
            _ => (a, b) => dir * natural.Compare(a.Name, b.Name),
        };
    }

    /// <summary>The nav-list index of <paramref name="path"/> (case-insensitive), or -1 when it is not in the open folder.</summary>
    public int IndexOf(string path) => IndexOfPath(path);

    /// <summary>The <see cref="PositionLabel"/> ranking for any file in the folder, supported or not (the explorer
    /// card previews unsupported files with a placeholder and the counter should still say where they sit).
    /// Empty when the path is not in the folder.</summary>
    public string PositionLabelFor(string path)
    {
        FileEntry target;
        int index = IndexOfPath(path);
        if (index >= 0)
        {
            target = _entries[index];
        }
        else
        {
            int other = IndexOfOther(path);
            if (other < 0)
                return string.Empty;
            target = _others[other];
        }
        Comparison<FileEntry> comparison = BuildComparison(_sort);
        int before = 0;
        foreach (FileEntry entry in _entries)
        {
            if (comparison(entry, target) < 0)
                before++;
        }
        foreach (FileEntry entry in _others)
        {
            if (comparison(entry, target) < 0)
                before++;
        }
        return $"{before + 1} / {TotalFileCount}";
    }

    private int IndexOfOther(string path)
    {
        for (int i = 0; i < _others.Count; i++)
        {
            if (string.Equals(_others[i].Path, path, StringComparison.OrdinalIgnoreCase))
                return i;
        }
        return -1;
    }

    private int IndexOfPath(string path)
    {
        for (int i = 0; i < _entries.Count; i++)
        {
            if (string.Equals(_entries[i].Path, path, StringComparison.OrdinalIgnoreCase))
                return i;
        }
        return -1;
    }
}
