using Looker.Navigation;

namespace Looker.Tests;

public class NaturalSortComparerTests
{
    private static List<string> Sorted(params string[] items)
    {
        var list = items.ToList();
        list.Sort(NaturalSortComparer.Instance);
        return list;
    }

    [Fact]
    public void EmbeddedNumbersCompareNumericallyNotLexically()
    {
        // The canonical case: lexical sort would put img10 before img2.
        Assert.Equal(
            new[] { "img1.jpg", "img2.jpg", "img10.jpg", "img100.jpg" },
            Sorted("img10.jpg", "img100.jpg", "img2.jpg", "img1.jpg"));
    }

    [Fact]
    public void MatchesExplorerOrderingForMixedNames()
    {
        Assert.Equal(
            new[] { "IMG_1.jpg", "IMG_2.jpg", "IMG_10.jpg", "IMG_20.jpg", "pic.jpg", "Pic1.jpg" },
            Sorted("Pic1.jpg", "IMG_20.jpg", "IMG_2.jpg", "pic.jpg", "IMG_10.jpg", "IMG_1.jpg"));
    }

    [Fact]
    public void LeadingZerosDoNotChangeNumericValue()
    {
        Assert.Equal(
            new[] { "z001", "z2", "z03", "z10", "z21" },
            Sorted("z10", "z21", "z2", "z001", "z03"));
    }

    [Fact]
    public void IsCaseInsensitive()
    {
        // apple/APPLE differ only by case → treated as equal by the shell comparer.
        Assert.Equal(0, NaturalSortComparer.Instance.Compare("apple.png", "APPLE.PNG"));
    }

    [Theory]
    [InlineData("file9.txt", "file10.txt", -1)]
    [InlineData("file10.txt", "file9.txt", 1)]
    [InlineData("a.jpg", "a.jpg", 0)]
    public void CompareReturnsExpectedSign(string x, string y, int expectedSign)
    {
        Assert.Equal(expectedSign, Math.Sign(NaturalSortComparer.Instance.Compare(x, y)));
    }

    [Fact]
    public void NullsSortBeforeValues()
    {
        Assert.True(NaturalSortComparer.Instance.Compare(null, "a") < 0);
        Assert.True(NaturalSortComparer.Instance.Compare("a", null) > 0);
        Assert.Equal(0, NaturalSortComparer.Instance.Compare(null, null));
    }
}
