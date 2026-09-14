using System.Collections.Concurrent;
using Looker.Imaging;

namespace Looker.Tests;

public class PreloadSchedulerTests
{
    private static CacheKey K(string path) => new(path, 0, 1536);

    [Fact]
    public async Task AdoptedLoadSurvivesRescheduleAndCompletes()
    {
        var started = new ConcurrentDictionary<CacheKey, TaskCompletionSource>();
        var release = new TaskCompletionSource();
        var cancelled = new ConcurrentBag<CacheKey>();

        var scheduler = new PreloadScheduler(2, _ => false, async (key, ct) =>
        {
            started.GetOrAdd(key, _ => new TaskCompletionSource()).TrySetResult();
            try { await release.Task.WaitAsync(ct); }
            catch (OperationCanceledException) { cancelled.Add(key); throw; }
        });

        scheduler.Schedule(new[] { K("a") });
        await started.GetOrAdd(K("a"), _ => new TaskCompletionSource()).Task.WaitAsync(TimeSpan.FromSeconds(5));

        Assert.True(scheduler.TryAdopt(K("a"), out Task adopted));
        Assert.False(adopted.IsCompleted);

        // A new window without "a" would normally cancel it — adopted loads are no longer the scheduler's.
        scheduler.Schedule(new[] { K("b") });
        release.SetResult();
        await adopted.WaitAsync(TimeSpan.FromSeconds(5));
        Assert.DoesNotContain(K("a"), cancelled);
    }

    [Fact]
    public void QueuedLoadIsCancelledRatherThanAdopted()
    {
        var gate = new TaskCompletionSource();
        var scheduler = new PreloadScheduler(1, _ => false, (_, ct) => gate.Task.WaitAsync(ct));

        scheduler.Schedule(new[] { K("a"), K("b") }); // concurrency 1: "b" waits behind the gate, never started

        Assert.False(scheduler.TryAdopt(K("b"), out _));
        Assert.False(scheduler.TryAdopt(K("missing"), out _));
        gate.SetResult();
    }
}
