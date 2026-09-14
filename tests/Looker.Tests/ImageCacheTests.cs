using Looker.Imaging;

namespace Looker.Tests;

public class ImageCacheTests
{
    // Test double for ICacheValue: a plain refcounted value with a chosen byte size.
    private sealed class FakeValue : ICacheValue
    {
        public FakeValue(long bytes) => ByteSize = bytes;
        public long ByteSize { get; }
        public int RefCount { get; private set; } = 1;
        public bool Disposed => RefCount <= 0;
        public void Retain() => RefCount++;
        public void Release() => RefCount--;
    }

    private static CacheKey Key(string path) => new(path, 0, 1536);

    [Fact]
    public void InsertTracksBytesAndCount()
    {
        var cache = new ImageCache(budgetBytes: 1000);
        var a = new FakeValue(100);
        cache.Insert(Key("a"), a);
        a.Release(); // creator hands off; cache holds the only reference

        Assert.Equal(1, cache.Count);
        Assert.Equal(100, cache.CurrentBytes);
        Assert.Equal(1, a.RefCount); // still alive, held by the cache
    }

    [Fact]
    public void EvictsLeastRecentlyUsedWhenOverBudgetAndReleasesIt()
    {
        var cache = new ImageCache(budgetBytes: 100);
        var a = new FakeValue(60);
        var b = new FakeValue(60);
        cache.Insert(Key("a"), a); a.Release();
        cache.Insert(Key("b"), b); b.Release(); // 120 > 100 → evict LRU (a)

        Assert.False(cache.Contains(Key("a")));
        Assert.True(cache.Contains(Key("b")));
        Assert.Equal(60, cache.CurrentBytes);
        Assert.True(a.Disposed);   // evicted → cache released its reference → disposed
        Assert.False(b.Disposed);
    }

    [Fact]
    public void DisplayedValueSurvivesEvictionViaRefcount()
    {
        var cache = new ImageCache(budgetBytes: 100);
        var a = new FakeValue(60);
        cache.Insert(Key("a"), a); // creator keeps its reference (simulates "on screen")
        var b = new FakeValue(60);
        cache.Insert(Key("b"), b); b.Release(); // evicts a from the cache

        Assert.False(cache.Contains(Key("a")));
        Assert.Equal(1, a.RefCount);  // cache's ref dropped, creator's remains
        Assert.False(a.Disposed);     // NOT disposed while still displayed
    }

    [Fact]
    public void TryGetPromotesToMostRecentlyUsedAndRetains()
    {
        var cache = new ImageCache(budgetBytes: 200);
        var a = new FakeValue(60);
        var b = new FakeValue(60);
        cache.Insert(Key("a"), a); a.Release();
        cache.Insert(Key("b"), b); b.Release();

        Assert.True(cache.TryGet(Key("a"), out ICacheValue got)); // touches a → MRU
        Assert.Same(a, got);
        Assert.Equal(2, a.RefCount); // cache + returned reference
        got.Release();

        var c = new FakeValue(90);
        cache.Insert(Key("c"), c); c.Release(); // 210 > 200 → evict LRU, which is now b

        Assert.True(cache.Contains(Key("a")));
        Assert.False(cache.Contains(Key("b")));
    }

    [Fact]
    public void ReplacingSameKeyUpdatesBytesAndReleasesOldValue()
    {
        var cache = new ImageCache(budgetBytes: 1000);
        var a = new FakeValue(100);
        cache.Insert(Key("a"), a); a.Release();
        var a2 = new FakeValue(250);
        cache.Insert(Key("a"), a2); a2.Release();

        Assert.Equal(1, cache.Count);
        Assert.Equal(250, cache.CurrentBytes);
        Assert.True(a.Disposed);
        Assert.False(a2.Disposed);
    }

    [Fact]
    public void ClearReleasesEverything()
    {
        var cache = new ImageCache(budgetBytes: 1000);
        var a = new FakeValue(100);
        var b = new FakeValue(100);
        cache.Insert(Key("a"), a); a.Release();
        cache.Insert(Key("b"), b); b.Release();

        cache.Clear();

        Assert.Equal(0, cache.Count);
        Assert.Equal(0, cache.CurrentBytes);
        Assert.True(a.Disposed);
        Assert.True(b.Disposed);
    }

    [Fact]
    public void SingleEntryLargerThanBudgetIsRetained()
    {
        var cache = new ImageCache(budgetBytes: 100);
        var big = new FakeValue(500);
        cache.Insert(Key("big"), big); big.Release();

        Assert.True(cache.Contains(Key("big"))); // never evict the only entry
        Assert.Equal(1, cache.Count);
    }

    [Fact]
    public void DifferentBucketsAreDistinctEntries()
    {
        var cache = new ImageCache(budgetBytes: 10_000);
        var low = new FakeValue(50);
        var high = new FakeValue(500);
        cache.Insert(new CacheKey("a", 0, 512), low); low.Release();
        cache.Insert(new CacheKey("a", 0, 1536), high); high.Release();

        Assert.Equal(2, cache.Count); // low-res and screen-res coexist
        Assert.True(cache.Contains(new CacheKey("a", 0, 512)));
        Assert.True(cache.Contains(new CacheKey("a", 0, 1536)));
    }

    [Theory]
    [InlineData(1, 256)]
    [InlineData(256, 256)]
    [InlineData(257, 512)]
    [InlineData(1440, 1536)]
    [InlineData(2560, 2560)]
    public void BucketForQuantizesTo256Steps(int maxDimension, int expectedBucket)
    {
        Assert.Equal(expectedBucket, CacheKey.BucketFor(maxDimension));
    }
}
