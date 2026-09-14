using Looker.Navigation;

namespace Looker.Tests;

/// <summary>
/// The file explorer's model: flattening an expanded folder tree into rows, keeping the flat list in step
/// through expand/collapse/insert/remove, and one sort for folders and files at every level.
/// Disk-backed tests build a small temp folder tree; the rest works on nodes created from it.
/// </summary>
public sealed class ExplorerTreeTests : IDisposable
{
    private readonly string _root = Path.Combine(Path.GetTempPath(), "looker-explorer-" + Guid.NewGuid().ToString("N"));

    public ExplorerTreeTests()
    {
        // root/
        //   Alpha/    (folder)  → holds inner.png
        //   beta/     (folder, empty)
        //   img10.jpg, img2.jpg, notes.txt, .hidden.jpg (Hidden attribute)
        Directory.CreateDirectory(Path.Combine(_root, "Alpha"));
        Directory.CreateDirectory(Path.Combine(_root, "beta"));
        Touch("Alpha/inner.png", 100);
        Touch("img10.jpg", 300);
        Touch("img2.jpg", 200);
        Touch("notes.txt", 50);
        string hidden = Touch(".hidden.jpg", 10);
        File.SetAttributes(hidden, FileAttributes.Hidden);
    }

    public void Dispose()
    {
        try { Directory.Delete(_root, recursive: true); } catch { }
    }

    private string Touch(string relative, int bytes)
    {
        string path = Path.Combine(_root, relative.Replace('/', Path.DirectorySeparatorChar));
        File.WriteAllBytes(path, new byte[bytes]);
        return path;
    }

    private static ExplorerTree OpenRoot(string root, SortMode? sort = null)
    {
        var tree = new ExplorerTree();
        if (sort is { } s)
            tree.SetSort(s);
        ExplorerNode node = tree.SetRoot(root);
        tree.SetChildren(node, ExplorerTree.Enumerate(node));
        tree.Expand(node);
        return tree;
    }

    private static string[] Names(ExplorerTree tree) => tree.Rows.Select(r => r.Name).ToArray();

    [Fact]
    public void Enumerate_ClassifiesEntries_AndSkipsHidden()
    {
        ExplorerTree tree = OpenRoot(_root);

        Assert.Equal(new[] { "Alpha", "beta", "img2.jpg", "img10.jpg", "notes.txt" }, Names(tree));
        Assert.Equal(ExplorerNodeKind.Folder, tree.Rows[0].Kind);
        Assert.Equal(ExplorerNodeKind.Image, tree.Rows[2].Kind);
        Assert.Equal(ExplorerNodeKind.Other, tree.Rows[4].Kind);
        Assert.All(tree.Rows, r => Assert.Equal(0, r.Depth));
        Assert.Equal(300, tree.Rows[3].Size);
    }

    [Fact]
    public void Descending_ReversesWithinGroups_FoldersStayFirst()
    {
        ExplorerTree tree = OpenRoot(_root, new SortMode(SortField.Name, SortDirection.Descending));

        Assert.Equal(new[] { "beta", "Alpha", "notes.txt", "img10.jpg", "img2.jpg" }, Names(tree));
    }

    [Fact]
    public void SizeSort_OrdersFilesBySize_FoldersByName()
    {
        ExplorerTree tree = OpenRoot(_root, new SortMode(SortField.Size, SortDirection.Ascending));

        Assert.Equal(new[] { "Alpha", "beta", "notes.txt", "img2.jpg", "img10.jpg" }, Names(tree));
    }

    [Fact]
    public void ExpandAndCollapse_InsertAndRemoveDescendantRows()
    {
        ExplorerTree tree = OpenRoot(_root);
        var changes = new List<ExplorerRowChange>();
        tree.RowsChanged += changes.Add;

        ExplorerNode alpha = tree.Rows[0];
        tree.SetChildren(alpha, ExplorerTree.Enumerate(alpha));
        Assert.True(tree.Expand(alpha));

        Assert.Equal(new[] { "Alpha", "inner.png", "beta", "img2.jpg", "img10.jpg", "notes.txt" }, Names(tree));
        Assert.Equal(1, tree.Rows[1].Depth);
        Assert.Equal(new ExplorerRowChange(ExplorerRowChangeKind.Inserted, 1, 1), changes.Single());
        Assert.Equal(1, tree.VisibleDescendantCount(alpha));

        changes.Clear();
        tree.Collapse(alpha);
        Assert.Equal(new[] { "Alpha", "beta", "img2.jpg", "img10.jpg", "notes.txt" }, Names(tree));
        Assert.Equal(new ExplorerRowChange(ExplorerRowChangeKind.Removed, 1, 1), changes.Single());
        Assert.False(alpha.IsExpanded);
        Assert.Null(alpha.Children); // re-expanding re-reads the disk
    }

    [Fact]
    public void Expand_RequiresLoadedChildren_AndIsIdempotent()
    {
        ExplorerTree tree = OpenRoot(_root);
        ExplorerNode beta = tree.Rows[1];

        Assert.False(tree.Expand(beta)); // never enumerated
        tree.SetChildren(beta, ExplorerTree.Enumerate(beta));
        Assert.True(tree.Expand(beta));
        Assert.False(tree.Expand(beta)); // already open
        Assert.Equal(5, tree.Rows.Count); // empty folder adds no rows
    }

    [Fact]
    public void Insert_PlacesNewFileInSortedPosition_ReportingRowIndex()
    {
        ExplorerTree tree = OpenRoot(_root);
        ExplorerNode alpha = tree.Rows[0];
        tree.SetChildren(alpha, ExplorerTree.Enumerate(alpha));
        tree.Expand(alpha); // rows: Alpha, inner.png, beta, img2, img10, notes
        var changes = new List<ExplorerRowChange>();
        tree.RowsChanged += changes.Add;

        string created = Touch("img5.jpg", 10);
        ExplorerNode node = ExplorerTree.TryCreateNode(tree.Root!, created)!;
        int row = tree.Insert(tree.Root!, node);

        Assert.Equal(4, row); // after Alpha(+inner), beta, img2 — the expanded folder's child counts
        Assert.Equal(new[] { "Alpha", "inner.png", "beta", "img2.jpg", "img5.jpg", "img10.jpg", "notes.txt" }, Names(tree));
        Assert.Equal(new ExplorerRowChange(ExplorerRowChangeKind.Inserted, 4, 1), changes.Single());
    }

    [Fact]
    public void Insert_IntoCollapsedFolder_RecordsButShowsNothing()
    {
        ExplorerTree tree = OpenRoot(_root);
        ExplorerNode alpha = tree.Rows[0];
        tree.SetChildren(alpha, ExplorerTree.Enumerate(alpha)); // loaded, not expanded

        string created = Touch("Alpha/added.jpg", 10);
        int row = tree.Insert(alpha, ExplorerTree.TryCreateNode(alpha, created)!);

        Assert.Equal(-1, row);
        Assert.Equal(5, tree.Rows.Count);
        Assert.Equal(2, alpha.Children!.Count);
    }

    [Fact]
    public void Remove_DropsTheRowAndItsVisibleDescendants()
    {
        ExplorerTree tree = OpenRoot(_root);
        ExplorerNode alpha = tree.Rows[0];
        tree.SetChildren(alpha, ExplorerTree.Enumerate(alpha));
        tree.Expand(alpha);
        var changes = new List<ExplorerRowChange>();
        tree.RowsChanged += changes.Add;

        tree.Remove(alpha);

        Assert.Equal(new[] { "beta", "img2.jpg", "img10.jpg", "notes.txt" }, Names(tree));
        Assert.Equal(new ExplorerRowChange(ExplorerRowChangeKind.Removed, 0, 2), changes.Single());
        Assert.Null(tree.Find(alpha.Path));
    }

    [Fact]
    public void Refresh_MovesAFileWhoseSizeChangedUnderSizeSort()
    {
        ExplorerTree tree = OpenRoot(_root, new SortMode(SortField.Size, SortDirection.Ascending));
        // files: notes(50), img2(200), img10(300)
        ExplorerNode notes = tree.Rows[2];

        tree.Refresh(notes, notes.ModifiedTicks + 1, 1000);

        Assert.Equal(new[] { "Alpha", "beta", "img2.jpg", "img10.jpg", "notes.txt" }, Names(tree));
    }

    [Fact]
    public void Refresh_LeavesRowAloneWhenOrderIsUnchanged()
    {
        ExplorerTree tree = OpenRoot(_root, new SortMode(SortField.Size, SortDirection.Ascending));
        ExplorerNode notes = tree.Rows[2];
        var changes = new List<ExplorerRowChange>();
        tree.RowsChanged += changes.Add;

        tree.Refresh(notes, notes.ModifiedTicks + 1, 60);

        Assert.Empty(changes);
        Assert.Equal(60, notes.Size);
    }

    [Fact]
    public void SetSort_ReordersEveryLoadedLevel()
    {
        ExplorerTree tree = OpenRoot(_root);
        ExplorerNode alpha = tree.Rows[0];
        Touch("Alpha/zed.jpg", 1);
        tree.SetChildren(alpha, ExplorerTree.Enumerate(alpha));
        tree.Expand(alpha);

        tree.SetSort(new SortMode(SortField.Name, SortDirection.Descending));

        Assert.Equal(new[] { "beta", "Alpha", "zed.jpg", "inner.png", "notes.txt", "img10.jpg", "img2.jpg" }, Names(tree));
    }

    [Fact]
    public void Find_LocatesNestedNodes_CaseInsensitively()
    {
        ExplorerTree tree = OpenRoot(_root);
        ExplorerNode alpha = tree.Rows[0];
        tree.SetChildren(alpha, ExplorerTree.Enumerate(alpha));

        ExplorerNode? inner = tree.Find(Path.Combine(_root, "ALPHA", "INNER.PNG"));

        Assert.NotNull(inner);
        Assert.Same(alpha, inner!.Parent);
        Assert.Same(tree.Root, tree.Find(_root + Path.DirectorySeparatorChar));
        Assert.Null(tree.Find(Path.Combine(_root, "beta", "nothing.jpg")));
    }

    [Fact]
    public void SetRoot_NamesDriveRootsByTheirPath()
    {
        var tree = new ExplorerTree();
        ExplorerNode root = tree.SetRoot(@"C:\");
        Assert.Equal(@"C:\", root.Name);
        Assert.Equal(-1, root.Depth);
        Assert.Equal("looker", tree.SetRoot(@"C:\photos\looker\").Name);
    }

    [Fact]
    public void ComputerRoot_ListsReadyDrivesByLetter()
    {
        ExplorerTree tree = OpenRoot(ExplorerTree.ComputerPath, new SortMode(SortField.Name, SortDirection.Descending));

        Assert.Equal(ExplorerTree.ComputerName, tree.Root!.Name);
        Assert.True(tree.Root.IsComputer);
        Assert.NotEmpty(tree.Rows);
        Assert.All(tree.Rows, r => Assert.True(r.IsDrive && r.IsFolder && r.Depth == 0));
        string[] letters = tree.Rows.Select(r => r.Path).ToArray();
        // By letter, ascending, whatever the sort says — like Explorer's This PC (labels and direction are ignored).
        Assert.Equal(letters.OrderBy(p => p, StringComparer.OrdinalIgnoreCase).ToArray(), letters);
        Assert.Contains(tree.Rows, r => r.Name.EndsWith("(C:)"));
        Assert.Same(tree.Rows.First(r => r.Path.StartsWith("C:")), ExplorerTree.FindChild(tree.Root, @"C:\"));
    }

    [Theory]
    [InlineData("", @"C:\", true)]
    [InlineData("", @"C:\photos\a.jpg", true)]
    [InlineData(@"C:\photos", @"C:\photos\a.jpg", true)]
    [InlineData(@"C:\photos\", @"C:\photos\sub\a.jpg", true)]
    [InlineData(@"C:\photos", @"C:\photos", false)]
    [InlineData(@"C:\photos", @"C:\photos2\a.jpg", false)]
    [InlineData(@"C:\", @"C:\a.jpg", true)]
    public void IsUnder_MatchesOnlyDescendants(string folder, string path, bool expected)
        => Assert.Equal(expected, ExplorerTree.IsUnder(folder, path));
}
