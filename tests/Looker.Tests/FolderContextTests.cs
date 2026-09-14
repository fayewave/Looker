using System;
using System.IO;
using System.Threading.Tasks;
using Looker.Navigation;

namespace Looker.Tests;

/// <summary>
/// Exercises the M6 live-mutation logic (create/delete/rename/modify) on a real temp folder, since
/// the mutations key off <see cref="FileInfo"/>. Ordering is the natural sort verified elsewhere;
/// these focus on the index bookkeeping that keeps the current selection pinned.
/// </summary>
public sealed class FolderContextTests : IDisposable
{
    private readonly string _dir;

    public FolderContextTests()
    {
        _dir = Path.Combine(Path.GetTempPath(), "looker-fctx-" + Guid.NewGuid().ToString("N"));
        Directory.CreateDirectory(_dir);
    }

    public void Dispose()
    {
        try { Directory.Delete(_dir, recursive: true); } catch { /* best effort */ }
    }

    private string Touch(string name)
    {
        string path = Path.Combine(_dir, name);
        File.WriteAllBytes(path, new byte[] { 0xFF });
        return path;
    }

    private async Task<FolderContext> LoadedAsync(params string[] names)
    {
        foreach (string n in names)
            Touch(n);
        var ctx = new FolderContext();
        await ctx.LoadFolderAsync(_dir, SortMode.Default);
        return ctx;
    }

    [Fact]
    public async Task LoadFolder_SortsNaturally_AndSelectsFirst()
    {
        FolderContext ctx = await LoadedAsync("img10.jpg", "img2.jpg", "img1.jpg");

        Assert.Equal(3, ctx.Count);
        Assert.Equal(0, ctx.CurrentIndex);
        Assert.EndsWith("img1.jpg", ctx.CurrentPath);
        Assert.EndsWith("img2.jpg", ctx.RefAt(1)!.Value.Path);
        Assert.EndsWith("img10.jpg", ctx.RefAt(2)!.Value.Path);
    }

    [Fact]
    public async Task OnFileCreated_InsertsSorted_AndShiftsCurrentWhenBefore()
    {
        FolderContext ctx = await LoadedAsync("b.jpg", "d.jpg");
        ctx.MoveTo(1); // current = d.jpg
        Assert.EndsWith("d.jpg", ctx.CurrentPath);

        string created = Touch("a.jpg");
        FolderMutation change = ctx.OnFileCreated(created);

        Assert.True(change.Changed);
        Assert.Equal(0, change.Index);
        Assert.False(change.AffectsCurrent);
        Assert.Equal(3, ctx.Count);
        Assert.Equal(2, ctx.CurrentIndex);      // shifted right to stay on d.jpg
        Assert.EndsWith("d.jpg", ctx.CurrentPath);
    }

    [Fact]
    public async Task OnFileCreated_AfterCurrent_LeavesCurrentIndex()
    {
        FolderContext ctx = await LoadedAsync("a.jpg", "b.jpg");
        // current = a.jpg (index 0)
        string created = Touch("z.jpg");
        FolderMutation change = ctx.OnFileCreated(created);

        Assert.Equal(2, change.Index);
        Assert.Equal(0, ctx.CurrentIndex);
        Assert.EndsWith("a.jpg", ctx.CurrentPath);
    }

    [Fact]
    public async Task OnFileCreated_IgnoresUnsupportedAndDuplicates()
    {
        FolderContext ctx = await LoadedAsync("a.jpg");
        string txt = Touch("notes.txt");
        FolderMutation unsupported = ctx.OnFileCreated(txt);
        Assert.False(unsupported.Changed);
        Assert.True(unsupported.CountChanged); // counted, not listed
        Assert.False(ctx.OnFileCreated(Path.Combine(_dir, "a.jpg")).Changed);
        Assert.False(ctx.OnFileCreated(txt).CountChanged); // already counted
        Assert.Equal(1, ctx.Count);
        Assert.Equal(2, ctx.TotalFileCount);
    }

    [Fact]
    public async Task PositionLabel_CountsEveryFile_AndRanksAmongThem()
    {
        // Sorted by name: a.jpg, b.txt, c.jpg, d.txt — the counter counts all four and ranks the image among them.
        FolderContext ctx = await LoadedAsync("c.jpg", "b.txt", "d.txt", "a.jpg");

        Assert.Equal(2, ctx.Count);
        Assert.Equal(4, ctx.TotalFileCount);
        Assert.Equal("1 / 4", ctx.PositionLabel);
        ctx.MoveTo(1);
        Assert.Equal("3 / 4", ctx.PositionLabel);

        ctx.SetSort(new SortMode(SortField.Name, SortDirection.Descending)); // d.txt, c.jpg, b.txt, a.jpg
        Assert.Equal("2 / 4", ctx.PositionLabel);
    }

    [Fact]
    public async Task PositionLabelFor_RanksImagesAndUnsupportedFilesAlike()
    {
        // Sorted by name: a.jpg, b.txt, c.jpg, d.txt.
        FolderContext ctx = await LoadedAsync("c.jpg", "b.txt", "d.txt", "a.jpg");

        Assert.Equal("2 / 4", ctx.PositionLabelFor(Path.Combine(_dir, "b.txt")));
        Assert.Equal("4 / 4", ctx.PositionLabelFor(Path.Combine(_dir, "d.txt")));
        Assert.Equal("3 / 4", ctx.PositionLabelFor(Path.Combine(_dir, "C.JPG"))); // case-insensitive, same as IndexOf
        Assert.Equal(ctx.PositionLabel, ctx.PositionLabelFor(ctx.CurrentPath!));
        Assert.Equal(string.Empty, ctx.PositionLabelFor(Path.Combine(_dir, "missing.txt")));

        ctx.SetSort(new SortMode(SortField.Name, SortDirection.Descending)); // d.txt, c.jpg, b.txt, a.jpg
        Assert.Equal("3 / 4", ctx.PositionLabelFor(Path.Combine(_dir, "b.txt")));
    }

    [Fact]
    public async Task PositionLabel_FollowsUnsupportedFilesComingAndGoing()
    {
        FolderContext ctx = await LoadedAsync("a.jpg", "b.txt", "c.jpg");
        ctx.MoveTo(1); // c.jpg = "3 / 3"
        Assert.Equal("3 / 3", ctx.PositionLabel);

        string aa = Touch("aa.txt");
        Assert.True(ctx.OnFileCreated(aa).CountChanged);
        Assert.Equal("4 / 4", ctx.PositionLabel);

        string z = Touch("z.txt");
        Assert.True(ctx.OnFileCreated(z).CountChanged);
        Assert.Equal("4 / 5", ctx.PositionLabel);

        File.Delete(aa);
        FolderMutation gone = ctx.OnFileDeleted(aa);
        Assert.False(gone.Changed);
        Assert.True(gone.CountChanged);
        Assert.Equal("3 / 4", ctx.PositionLabel);
        Assert.Equal(2, ctx.Count); // the nav list never saw any of this
    }

    [Fact]
    public async Task PositionLabel_RenamedUnsupportedFile_MovesRank()
    {
        FolderContext ctx = await LoadedAsync("01 JPEG.jpg", "02 PNG.png", "b.txt", "zz.txt");
        ctx.MoveTo(1); // 02 PNG.png = "2 / 4"
        Assert.Equal("2 / 4", ctx.PositionLabel);

        string b = Path.Combine(_dir, "b.txt");
        string renamed = Path.Combine(_dir, "00.txt");
        File.Move(b, renamed);
        Assert.True(ctx.OnFileDeleted(b).CountChanged);
        Assert.True(ctx.OnFileCreated(renamed).CountChanged);
        Assert.Equal("3 / 4", ctx.PositionLabel); // 00.txt now sorts ahead of it
    }

    [Fact]
    public async Task PositionLabel_SkipsHiddenFiles()
    {
        FolderContext ctx = await LoadedAsync("a.jpg", "b.txt");
        string hidden = Touch("Thumbs.db");
        File.SetAttributes(hidden, FileAttributes.Hidden);
        await ctx.LoadFolderAsync(_dir, SortMode.Default);

        Assert.Equal(2, ctx.TotalFileCount);
        Assert.Equal("1 / 2", ctx.PositionLabel);
        Assert.False(ctx.OnFileCreated(hidden).CountChanged);
    }

    [Fact]
    public async Task OnFileDeleted_BeforeCurrent_DecrementsIndex()
    {
        FolderContext ctx = await LoadedAsync("a.jpg", "b.jpg", "c.jpg");
        ctx.MoveTo(2); // current = c.jpg

        FolderMutation change = ctx.OnFileDeleted(Path.Combine(_dir, "a.jpg"));

        Assert.True(change.Changed);
        Assert.Equal(0, change.Index);
        Assert.False(change.AffectsCurrent);
        Assert.Equal(1, ctx.CurrentIndex);
        Assert.EndsWith("c.jpg", ctx.CurrentPath);
    }

    [Fact]
    public async Task OnFileDeleted_Current_KeepsIndexOnNextFile()
    {
        FolderContext ctx = await LoadedAsync("a.jpg", "b.jpg", "c.jpg");
        ctx.MoveTo(1); // current = b.jpg

        FolderMutation change = ctx.OnFileDeleted(Path.Combine(_dir, "b.jpg"));

        Assert.True(change.AffectsCurrent);
        Assert.Equal(1, ctx.CurrentIndex);       // same slot now holds c.jpg
        Assert.EndsWith("c.jpg", ctx.CurrentPath);
    }

    [Fact]
    public async Task OnFileDeleted_LastAndCurrent_ClampsToNewLast()
    {
        FolderContext ctx = await LoadedAsync("a.jpg", "b.jpg");
        ctx.MoveTo(1); // current = b.jpg (last)

        ctx.OnFileDeleted(Path.Combine(_dir, "b.jpg"));

        Assert.Equal(1, ctx.Count);
        Assert.Equal(0, ctx.CurrentIndex);
        Assert.EndsWith("a.jpg", ctx.CurrentPath);
    }

    [Fact]
    public async Task OnFileDeleted_OnlyFile_LeavesEmptySelection()
    {
        FolderContext ctx = await LoadedAsync("a.jpg");
        ctx.OnFileDeleted(Path.Combine(_dir, "a.jpg"));

        Assert.Equal(0, ctx.Count);
        Assert.Equal(-1, ctx.CurrentIndex);
        Assert.Null(ctx.CurrentPath);
    }

    [Fact]
    public async Task OnFileModified_UpdatesMtime_WhenChanged()
    {
        FolderContext ctx = await LoadedAsync("a.jpg");
        long before = ctx.CurrentModifiedTicks;

        // Rewrite with a distinctly later timestamp so the mtime observably changes.
        string path = Path.Combine(_dir, "a.jpg");
        File.WriteAllBytes(path, new byte[] { 1, 2, 3 });
        File.SetLastWriteTimeUtc(path, DateTime.UtcNow.AddMinutes(5));

        FolderMutation change = ctx.OnFileModified(path);

        Assert.True(change.Changed);
        Assert.True(change.AffectsCurrent);
        Assert.NotEqual(before, ctx.CurrentModifiedTicks);
    }

    [Fact]
    public async Task OnFileModified_NoRealChange_IsNoOp()
    {
        FolderContext ctx = await LoadedAsync("a.jpg");
        FolderMutation change = ctx.OnFileModified(Path.Combine(_dir, "a.jpg"));
        Assert.False(change.Changed);
    }
}

public class FolderContextClearTests
{
    [Fact]
    public void Clear_DropsEntriesAndSelection()
    {
        var ctx = new FolderContext();
        ctx.Clear();
        Assert.Equal(0, ctx.Count);
        Assert.Equal(-1, ctx.CurrentIndex);
        Assert.Null(ctx.FolderPath);
        Assert.Null(ctx.CurrentRef);
        Assert.False(ctx.MoveNext());
    }
}
