using Looker.Imaging;

namespace Looker.Tests;

public class PreloadPlanTests
{
    private static CacheKey K(string path) => new(path, 0, 1536);

    [Fact]
    public void StartsUncachedUnscheduledDesiredItems()
    {
        var diff = PreloadPlan.Compute(
            desired: new[] { K("a"), K("b"), K("c") },
            inFlight: new[] { K("c") },          // c already loading
            isCached: k => k == K("a"));         // a already cached

        Assert.Equal(new[] { K("b") }, diff.Start);   // only b needs starting
        Assert.Empty(diff.Cancel);                    // c is still desired
    }

    [Fact]
    public void CancelsInFlightItemsThatLeftTheWindow()
    {
        var diff = PreloadPlan.Compute(
            desired: new[] { K("a"), K("b") },
            inFlight: new[] { K("b"), K("z") },  // z no longer wanted
            isCached: _ => false);

        Assert.Equal(new[] { K("a") }, diff.Start);
        Assert.Equal(new[] { K("z") }, diff.Cancel);
    }

    [Fact]
    public void DeduplicatesDesiredKeys()
    {
        var diff = PreloadPlan.Compute(
            desired: new[] { K("a"), K("a"), K("b") },
            inFlight: System.Array.Empty<CacheKey>(),
            isCached: _ => false);

        Assert.Equal(new[] { K("a"), K("b") }, diff.Start);
    }

    [Fact]
    public void EmptyDesiredCancelsAllInFlight()
    {
        var diff = PreloadPlan.Compute(
            desired: System.Array.Empty<CacheKey>(),
            inFlight: new[] { K("a"), K("b") },
            isCached: _ => false);

        Assert.Empty(diff.Start);
        Assert.Equal(new[] { K("a"), K("b") }, diff.Cancel);
    }

    [Fact]
    public void NothingToDoWhenAllCachedAndNoneInFlight()
    {
        var diff = PreloadPlan.Compute(
            desired: new[] { K("a"), K("b") },
            inFlight: System.Array.Empty<CacheKey>(),
            isCached: _ => true);

        Assert.Empty(diff.Start);
        Assert.Empty(diff.Cancel);
    }
}
