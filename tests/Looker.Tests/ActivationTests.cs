using System.Xml.Linq;
using Looker.Imaging;
using Looker.Services;

namespace Looker.Tests;

public class CommandLineArgsTests
{
    [Fact]
    public void Split_HandlesQuotesAndWhitespace()
    {
        var parts = CommandLineArgs.Split("  \"C:\\My Photos\\a b.jpg\"  plain   \"x\"\"y\" ");
        Assert.Equal(new[] { @"C:\My Photos\a b.jpg", "plain", "x\"y" }, parts);
    }

    [Fact]
    public void Split_EmptyOrNull_IsEmpty()
    {
        Assert.Empty(CommandLineArgs.Split(null));
        Assert.Empty(CommandLineArgs.Split("   "));
    }

    [Fact]
    public void FirstExistingPath_SkipsExeAndMissing_ReturnsFullPath()
    {
        string dir = Path.Combine(Path.GetTempPath(), "looker-tests-" + Guid.NewGuid().ToString("N"));
        Directory.CreateDirectory(dir);
        string file = Path.Combine(dir, "photo.jpg");
        File.WriteAllBytes(file, new byte[] { 0xFF, 0xD8 });
        try
        {
            string? found = CommandLineArgs.FirstExistingPath(new[] { @"C:\app\Looker.exe", @"Z:\nope\missing.png", file });
            Assert.Equal(file, found);

            Assert.Equal(dir, CommandLineArgs.FirstExistingPath(new[] { dir }));
            Assert.Null(CommandLineArgs.FirstExistingPath(new[] { @"Z:\nope\missing.png", "" }));
        }
        finally
        {
            Directory.Delete(dir, recursive: true);
        }
    }
}

public class RecentFilesTests
{
    [Fact]
    public void Push_MovesExistingToFront_CaseInsensitive()
    {
        var list = RecentFiles.Push(new[] { @"C:\a.jpg", @"C:\b.jpg", @"C:\c.jpg" }, @"c:\B.JPG");
        Assert.Equal(new[] { @"c:\B.JPG", @"C:\a.jpg", @"C:\c.jpg" }, list);
    }

    [Fact]
    public void Push_TrimsToCapacity()
    {
        var existing = Enumerable.Range(0, RecentFiles.Capacity).Select(i => $@"C:\{i}.png").ToList();
        var list = RecentFiles.Push(existing, @"C:\new.png");
        Assert.Equal(RecentFiles.Capacity, list.Count);
        Assert.Equal(@"C:\new.png", list[0]);
        Assert.DoesNotContain($@"C:\{RecentFiles.Capacity - 1}.png", list);
    }

    [Fact]
    public void Serialize_Parse_RoundTrips()
    {
        var paths = new[] { @"C:\Photos\one.jpg", @"D:\two two.png" };
        Assert.Equal(paths, RecentFiles.Parse(RecentFiles.Serialize(paths)));
        Assert.Empty(RecentFiles.Parse(null));
        Assert.Empty(RecentFiles.Parse(""));
    }
}

public class ManifestAssociationTests
{
    [Fact]
    public void FileTypeAssociation_MatchesSupportedFormats()
    {
        string manifestPath = Path.Combine(AppContext.BaseDirectory, "Package.appxmanifest");
        Assert.True(File.Exists(manifestPath), $"manifest not copied next to tests: {manifestPath}");

        XNamespace uap = "http://schemas.microsoft.com/appx/manifest/uap/windows10";
        var declared = XDocument.Load(manifestPath)
            .Descendants(uap + "FileType")
            .Select(e => e.Value.Trim())
            .ToHashSet(StringComparer.OrdinalIgnoreCase);

        var expected = SupportedFormats.Extensions.ToHashSet(StringComparer.OrdinalIgnoreCase);
        Assert.Empty(expected.Except(declared)); // supported but not associated
        Assert.Empty(declared.Except(expected)); // associated but not supported
    }
}

public class RecentFilesRemoveTests
{
    [Fact]
    public void Remove_DropsMatchingEntryCaseInsensitive_KeepsOrder()
    {
        var list = new[] { @"C:\a.jpg", @"C:\B.png", @"C:\c.gif" };
        var result = RecentFiles.Remove(list, @"c:\b.PNG");
        Assert.Equal(new[] { @"C:\a.jpg", @"C:\c.gif" }, result);
    }

    [Fact]
    public void Remove_UnknownPath_IsNoOp()
    {
        var list = new[] { @"C:\a.jpg" };
        Assert.Equal(list, RecentFiles.Remove(list, @"C:\zzz.jpg"));
    }
}
