using System;
using System.Collections.Generic;
using System.IO;
using Looker.Imaging;

namespace Looker.Navigation;

public enum ExplorerNodeKind
{
    Folder,
    /// <summary>A file Looker can open (<see cref="SupportedFormats"/>).</summary>
    Image,
    /// <summary>Any other file: listed greyed out so the folder reads like Explorer, but inert.</summary>
    Other,
}

/// <summary>
/// One entry of the file explorer tree: a folder or a file under the tree's root. Folders hold their
/// enumerated <see cref="Children"/> only while expanded (collapsing drops them, so re-expanding re-reads
/// the disk and never shows a stale listing). <see cref="Depth"/> is 0 for the root's own children.
/// </summary>
public sealed class ExplorerNode
{
    internal List<ExplorerNode>? ChildList;

    internal ExplorerNode(ExplorerNode? parent, string path, string name, ExplorerNodeKind kind, long modifiedTicks, long size)
    {
        Parent = parent;
        Path = path;
        Name = name;
        Kind = kind;
        ModifiedTicks = modifiedTicks;
        Size = size;
        Depth = parent is null ? -1 : parent.Depth + 1;
    }

    public ExplorerNode? Parent { get; }
    public string Path { get; }
    public string Name { get; }
    public ExplorerNodeKind Kind { get; }
    public int Depth { get; }
    public long ModifiedTicks { get; internal set; }
    public long Size { get; internal set; }
    public bool IsFolder => Kind == ExplorerNodeKind.Folder;
    public bool IsExpanded { get; internal set; }

    /// <summary>The virtual "This PC" node (<see cref="ExplorerTree.ComputerPath"/>) whose children are the drives.</summary>
    public bool IsComputer => Path.Length == 0;

    /// <summary>A drive root ("C:\"), i.e. a direct child of the computer node.</summary>
    public bool IsDrive => Parent is { IsComputer: true };

    /// <summary>The sorted children, or null while the folder has never been enumerated (or was collapsed).</summary>
    public IReadOnlyList<ExplorerNode>? Children => ChildList;
    public bool IsLoaded => ChildList is not null;
}

public enum ExplorerRowChangeKind
{
    Reset,
    Inserted,
    Removed,
}

/// <summary>One mutation of <see cref="ExplorerTree.Rows"/>: the host mirrors it into its bound collection.
/// For <see cref="ExplorerRowChangeKind.Inserted"/> the new rows are already in <see cref="ExplorerTree.Rows"/>
/// at [Index, Index+Count); for Removed they are already gone from there.</summary>
public readonly record struct ExplorerRowChange(ExplorerRowChangeKind Kind, int Index, int Count);

/// <summary>
/// The file explorer's model: a folder tree rooted at <see cref="Root"/>, flattened into the list of visible
/// <see cref="Rows"/> (a folder's children follow it while it is expanded). Every mutation keeps the flat list in
/// step and reports the exact row range that changed through <see cref="RowsChanged"/>, so the UI can insert
/// and remove single rows instead of rebuilding. Ordering follows one <see cref="SortMode"/> for every level:
/// folders always come before files; folders sort by name or date (there is no folder size), files by the
/// chosen field, both in the chosen direction, with Explorer's natural name order as the tiebreak.
/// Disk access is confined to <see cref="Enumerate"/> and <see cref="TryCreateNode"/> so the rest is testable.
/// </summary>
public sealed class ExplorerTree
{
    private readonly List<ExplorerNode> _rows = new();
    private Comparison<ExplorerNode> _comparison = BuildComparison(SortMode.Default);

    public ExplorerNode? Root { get; private set; }
    public SortMode Sort { get; private set; } = SortMode.Default;

    /// <summary>The visible rows, top to bottom.</summary>
    public IReadOnlyList<ExplorerNode> Rows => _rows;

    public event Action<ExplorerRowChange>? RowsChanged;

    /// <summary>The path of the virtual "This PC" level above the drives: an empty string, which no real folder has.
    /// <see cref="SetRoot"/> with it lists the ready drives; it contains every rooted path.</summary>
    public const string ComputerPath = "";
    public const string ComputerName = "This PC";

    /// <summary>Display name of a folder path: the last segment, the drive for "C:\", "This PC" for the computer.</summary>
    public static string NameOf(string path)
    {
        if (path.Length == 0)
            return ComputerName;
        string name = Path.GetFileName(Path.TrimEndingDirectorySeparator(path));
        return string.IsNullOrEmpty(name) ? path : name; // a drive root ("C:\") has no file name
    }

    /// <summary>Start over at <paramref name="path"/> (or <see cref="ComputerPath"/>): the new root is not itself a
    /// row; its children become the top-level rows once they are set and the root is expanded.</summary>
    public ExplorerNode SetRoot(string path)
    {
        Root = new ExplorerNode(null, path, NameOf(path), ExplorerNodeKind.Folder, 0, 0);
        _rows.Clear();
        RowsChanged?.Invoke(new ExplorerRowChange(ExplorerRowChangeKind.Reset, 0, 0));
        return Root;
    }

    public void Clear()
    {
        Root = null;
        _rows.Clear();
        RowsChanged?.Invoke(new ExplorerRowChange(ExplorerRowChangeKind.Reset, 0, 0));
    }

    // --- Disk access (call off the UI thread) ---

    /// <summary>Read <paramref name="parent"/>'s folder: sub-folders and every non-hidden file. Unsorted; feed the
    /// result to <see cref="SetChildren"/>. Inaccessible or vanished folders yield an empty list.</summary>
    public static List<ExplorerNode> Enumerate(ExplorerNode parent)
    {
        var list = new List<ExplorerNode>();
        if (parent.IsComputer)
        {
            foreach (DriveInfo drive in SafeDrives())
            {
                string label;
                try { label = drive.IsReady ? drive.VolumeLabel : string.Empty; }
                catch { label = string.Empty; }
                if (string.IsNullOrEmpty(label))
                    label = drive.DriveType == DriveType.Removable ? "Removable Disk" : "Local Disk";
                string letter = drive.Name.TrimEnd(Path.DirectorySeparatorChar); // "C:"
                list.Add(new ExplorerNode(parent, drive.RootDirectory.FullName, $"{label} ({letter})", ExplorerNodeKind.Folder, 0, 0));
            }
            return list;
        }
        try
        {
            foreach (FileSystemInfo info in new DirectoryInfo(parent.Path).EnumerateFileSystemInfos())
            {
                if ((info.Attributes & (FileAttributes.Hidden | FileAttributes.System)) != 0)
                    continue; // desktop.ini, Thumbs.db, $RECYCLE.BIN… — Explorer hides them too
                list.Add(Create(parent, info));
            }
        }
        catch (Exception ex) when (ex is UnauthorizedAccessException or DirectoryNotFoundException or IOException or System.Security.SecurityException)
        {
            // Show what we could read (possibly nothing); the folder simply looks empty.
        }
        return list;
    }

    /// <summary>A node for a path that just appeared under <paramref name="parent"/>, or null when it is gone
    /// again, hidden, or unreadable.</summary>
    public static ExplorerNode? TryCreateNode(ExplorerNode parent, string path)
    {
        try
        {
            FileSystemInfo info = Directory.Exists(path) ? new DirectoryInfo(path) : new FileInfo(path);
            if (!info.Exists || (info.Attributes & (FileAttributes.Hidden | FileAttributes.System)) != 0)
                return null;
            return Create(parent, info);
        }
        catch (Exception ex) when (ex is UnauthorizedAccessException or IOException or System.Security.SecurityException)
        {
            return null;
        }
    }

    private static DriveInfo[] SafeDrives()
    {
        try { return DriveInfo.GetDrives(); }
        catch { return Array.Empty<DriveInfo>(); }
    }

    private static ExplorerNode Create(ExplorerNode parent, FileSystemInfo info)
    {
        if (info is DirectoryInfo)
            return new ExplorerNode(parent, info.FullName, info.Name, ExplorerNodeKind.Folder, SafeTicks(info), 0);
        var kind = SupportedFormats.IsSupported(info.Name) ? ExplorerNodeKind.Image : ExplorerNodeKind.Other;
        long size = info is FileInfo fi ? fi.Length : 0;
        return new ExplorerNode(parent, info.FullName, info.Name, kind, SafeTicks(info), size);
    }

    private static long SafeTicks(FileSystemInfo info)
    {
        try { return info.LastWriteTimeUtc.Ticks; }
        catch { return 0; }
    }

    // --- Structure ---

    /// <summary>Store (and sort) a folder's enumerated children. If the folder is showing its children already,
    /// the visible rows are replaced in place.</summary>
    public void SetChildren(ExplorerNode parent, List<ExplorerNode> children)
    {
        bool wasExpanded = parent.IsExpanded;
        if (wasExpanded)
            RemoveDescendantRows(parent);
        children.Sort(_comparison);
        parent.ChildList = children;
        if (wasExpanded)
            InsertDescendantRows(parent);
    }

    /// <summary>Show a loaded folder's children under it. False when it has no children loaded yet (enumerate
    /// first) or is already expanded.</summary>
    public bool Expand(ExplorerNode node)
    {
        if (node.IsExpanded || node.ChildList is null)
            return false;
        node.IsExpanded = true;
        InsertDescendantRows(node);
        return true;
    }

    /// <summary>Hide a folder's rows and forget its listing (re-expanding re-reads the disk).</summary>
    public void Collapse(ExplorerNode node)
    {
        if (node.IsExpanded)
        {
            RemoveDescendantRows(node);
            node.IsExpanded = false;
        }
        if (!ReferenceEquals(node, Root))
            node.ChildList = null;
    }

    /// <summary>Re-order every loaded level under a new sort and rebuild the flat list.</summary>
    public void SetSort(SortMode sort)
    {
        Sort = sort;
        _comparison = BuildComparison(sort);
        if (Root is null)
            return;
        SortLoaded(Root);
        _rows.Clear();
        if (Root.IsExpanded)
            Flatten(Root, _rows);
        RowsChanged?.Invoke(new ExplorerRowChange(ExplorerRowChangeKind.Reset, 0, _rows.Count));
    }

    private void SortLoaded(ExplorerNode node)
    {
        if (node.ChildList is not { } children)
            return;
        children.Sort(_comparison);
        foreach (ExplorerNode child in children)
            SortLoaded(child);
    }

    /// <summary>Add a node that appeared on disk under a loaded folder, in sorted position. Returns its row index,
    /// or -1 when the parent is not showing its children (the node is still recorded in the parent's list).</summary>
    public int Insert(ExplorerNode parent, ExplorerNode node)
    {
        if (parent.ChildList is not { } children)
            return -1;
        int k = 0;
        while (k < children.Count && _comparison(children[k], node) <= 0)
            k++;
        children.Insert(k, node);
        if (!parent.IsExpanded)
            return -1;

        int row = FirstChildRow(parent);
        for (int i = 0; i < k; i++)
            row += 1 + VisibleDescendantCount(children[i]);
        _rows.Insert(row, node);
        RowsChanged?.Invoke(new ExplorerRowChange(ExplorerRowChangeKind.Inserted, row, 1));
        return row;
    }

    /// <summary>Remove a node (and any rows under it) after it vanished from disk.</summary>
    public void Remove(ExplorerNode node)
    {
        if (node.Parent is not { } parent || parent.ChildList is not { } siblings)
            return;
        int row = _rows.IndexOf(node);
        if (row >= 0)
        {
            int count = 1 + VisibleDescendantCount(node);
            _rows.RemoveRange(row, count);
            RowsChanged?.Invoke(new ExplorerRowChange(ExplorerRowChangeKind.Removed, row, count));
        }
        siblings.Remove(node);
        node.IsExpanded = false;
        node.ChildList = null;
    }

    /// <summary>A file's timestamp/size changed: update it and, when the active sort now puts it elsewhere among
    /// its siblings, move its row. Folders only take the new values (their rows never move: an expanded folder's
    /// mtime changes with every file written inside it, and re-shuffling it under the user would be hostile).</summary>
    public void Refresh(ExplorerNode node, long modifiedTicks, long size)
    {
        if (node.ModifiedTicks == modifiedTicks && node.Size == size)
            return;
        node.ModifiedTicks = modifiedTicks;
        node.Size = size;
        if (node.IsFolder || node.Parent is not { } parent || parent.ChildList is not { } siblings)
            return;

        int at = siblings.IndexOf(node);
        if (at < 0)
            return;
        bool inPlace = (at == 0 || _comparison(siblings[at - 1], node) <= 0)
                       && (at == siblings.Count - 1 || _comparison(node, siblings[at + 1]) <= 0);
        if (inPlace)
            return;
        Remove(node);
        Insert(parent, node);
    }

    // --- Lookup ---

    public int IndexOf(ExplorerNode node) => _rows.IndexOf(node);

    /// <summary>The loaded node at <paramref name="path"/> (case-insensitive), or null.</summary>
    public ExplorerNode? Find(string path)
    {
        if (Root is null)
            return null;
        if (SamePath(Root.Path, path))
            return Root;
        return FindUnder(Root, path);
    }

    private static ExplorerNode? FindUnder(ExplorerNode node, string path)
    {
        if (node.ChildList is not { } children)
            return null;
        foreach (ExplorerNode child in children)
        {
            if (SamePath(child.Path, path))
                return child;
            // Only descend into a folder that can contain the path — a prefix test keeps this linear.
            if (child.IsFolder && child.ChildList is not null && IsUnder(child.Path, path))
                return FindUnder(child, path);
        }
        return null;
    }

    /// <summary>The direct child of <paramref name="parent"/> at <paramref name="path"/>, or null.</summary>
    public static ExplorerNode? FindChild(ExplorerNode parent, string path)
    {
        if (parent.ChildList is not { } children)
            return null;
        foreach (ExplorerNode child in children)
        {
            if (SamePath(child.Path, path))
                return child;
        }
        return null;
    }

    /// <summary>Every loaded node below <paramref name="node"/>, parents before children.</summary>
    public static IEnumerable<ExplorerNode> Descendants(ExplorerNode node)
    {
        if (node.ChildList is not { } children)
            yield break;
        foreach (ExplorerNode child in children)
        {
            yield return child;
            foreach (ExplorerNode grandchild in Descendants(child))
                yield return grandchild;
        }
    }

    public int VisibleDescendantCount(ExplorerNode node)
    {
        if (!node.IsExpanded || node.ChildList is not { } children)
            return 0;
        int n = 0;
        foreach (ExplorerNode child in children)
            n += 1 + VisibleDescendantCount(child);
        return n;
    }

    public static bool SamePath(string a, string b)
        => string.Equals(Path.TrimEndingDirectorySeparator(a), Path.TrimEndingDirectorySeparator(b), StringComparison.OrdinalIgnoreCase);

    /// <summary>True when <paramref name="path"/> lies inside <paramref name="folder"/> (any depth), not equal to it.</summary>
    public static bool IsUnder(string folder, string path)
    {
        if (folder.Length == 0)
            return path.Length > 0; // "This PC" holds every drive and everything on them
        string prefix = Path.TrimEndingDirectorySeparator(folder);
        if (!path.StartsWith(prefix, StringComparison.OrdinalIgnoreCase))
            return false;
        if (prefix.Length > 0 && IsSeparator(prefix[^1]))
            return path.Length > prefix.Length; // a drive root keeps its separator ("C:\")
        return path.Length > prefix.Length + 1 && IsSeparator(path[prefix.Length]);
    }

    private static bool IsSeparator(char c) => c == Path.DirectorySeparatorChar || c == Path.AltDirectorySeparatorChar;

    // --- Flat list maintenance ---

    private int FirstChildRow(ExplorerNode parent)
        => ReferenceEquals(parent, Root) ? 0 : _rows.IndexOf(parent) + 1;

    private void InsertDescendantRows(ExplorerNode node)
    {
        var block = new List<ExplorerNode>();
        Flatten(node, block);
        if (block.Count == 0)
            return;
        int at = FirstChildRow(node);
        _rows.InsertRange(at, block);
        RowsChanged?.Invoke(new ExplorerRowChange(ExplorerRowChangeKind.Inserted, at, block.Count));
    }

    private void RemoveDescendantRows(ExplorerNode node)
    {
        int count = VisibleDescendantCount(node);
        if (count == 0)
            return;
        int at = FirstChildRow(node);
        _rows.RemoveRange(at, count);
        RowsChanged?.Invoke(new ExplorerRowChange(ExplorerRowChangeKind.Removed, at, count));
    }

    private static void Flatten(ExplorerNode node, List<ExplorerNode> into)
    {
        if (node.ChildList is not { } children)
            return;
        foreach (ExplorerNode child in children)
        {
            into.Add(child);
            if (child.IsExpanded)
                Flatten(child, into);
        }
    }

    private static Comparison<ExplorerNode> BuildComparison(SortMode sort)
    {
        int dir = sort.Direction == SortDirection.Ascending ? 1 : -1;
        IComparer<string> natural = NaturalSortComparer.Instance;
        return (a, b) =>
        {
            if (a.IsFolder != b.IsFolder)
                return a.IsFolder ? -1 : 1; // folders first, whichever direction
            if (a.IsDrive && b.IsDrive)
                return string.Compare(a.Path, b.Path, StringComparison.OrdinalIgnoreCase); // drives by letter, like Explorer
            int c = sort.Field switch
            {
                SortField.DateModified => a.ModifiedTicks.CompareTo(b.ModifiedTicks),
                SortField.Size when !a.IsFolder => a.Size.CompareTo(b.Size),
                _ => 0,
            };
            if (c == 0)
                c = natural.Compare(a.Name, b.Name);
            return dir * c;
        };
    }
}
