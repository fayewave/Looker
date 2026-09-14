using System;
using System.Collections.Generic;
using System.Linq;
using System.Threading;
using System.Threading.Tasks;

namespace Looker.Imaging;

/// <summary>The diff between what is desired in the preload window and what is cached/in-flight.</summary>
public readonly record struct PreloadDiff(IReadOnlyList<CacheKey> Start, IReadOnlyList<CacheKey> Cancel);

/// <summary>
/// Pure window-diff logic, split out so it can be unit-tested without any async/threading. Given
/// the desired keys (in priority order), what is already cached, and what is currently in flight,
/// decide which decodes to start and which to cancel.
/// </summary>
public static class PreloadPlan
{
    public static PreloadDiff Compute(
        IReadOnlyList<CacheKey> desired,
        IReadOnlyCollection<CacheKey> inFlight,
        Func<CacheKey, bool> isCached)
    {
        var desiredSet = new HashSet<CacheKey>(desired);
        var inFlightSet = new HashSet<CacheKey>(inFlight);

        List<CacheKey> cancel = inFlight.Where(k => !desiredSet.Contains(k)).ToList();

        var seen = new HashSet<CacheKey>();
        List<CacheKey> start = desired
            .Where(k => seen.Add(k) && !isCached(k) && !inFlightSet.Contains(k))
            .ToList();

        return new PreloadDiff(start, cancel);
    }
}

/// <summary>
/// Keeps the cache warm around the current image so next/prev feels instant. On each navigation it
/// diffs the desired window against in-flight work, cancels decodes that fell out of the window,
/// and starts new ones under a small concurrency cap. Fire-and-forget: results land in the cache.
/// The viewport can also <see cref="TryAdopt"/> a running load when that image becomes current, so
/// arrowing onto a neighbour mid-decode waits for the work already done instead of restarting it.
/// </summary>
public sealed class PreloadScheduler
{
    private sealed class Entry
    {
        public readonly CancellationTokenSource Cts = new();
        public Task Task = System.Threading.Tasks.Task.CompletedTask;
        public volatile bool Started; // the decode itself is running (past the concurrency gate)
    }

    private readonly object _lock = new();
    private readonly Dictionary<CacheKey, Entry> _inFlight = new();
    private readonly SemaphoreSlim _gate;
    private readonly Func<CacheKey, bool> _isCached;
    private readonly Func<CacheKey, CancellationToken, Task> _load;

    public PreloadScheduler(int maxParallel, Func<CacheKey, bool> isCached, Func<CacheKey, CancellationToken, Task> load)
    {
        _gate = new SemaphoreSlim(Math.Max(1, maxParallel));
        _isCached = isCached;
        _load = load;
    }

    /// <summary>Recommended concurrency: half the cores, clamped to [2, 4] (plan §Cache &amp; preload).</summary>
    public static int DefaultConcurrency => Math.Clamp(Environment.ProcessorCount / 2, 2, 4);

    public void Schedule(IReadOnlyList<CacheKey> desired)
    {
        lock (_lock)
        {
            PreloadDiff diff = PreloadPlan.Compute(desired, _inFlight.Keys.ToList(), _isCached);

            foreach (CacheKey key in diff.Cancel)
            {
                if (_inFlight.Remove(key, out Entry? entry))
                {
                    entry.Cts.Cancel();
                    entry.Cts.Dispose();
                }
            }

            foreach (CacheKey key in diff.Start)
            {
                var entry = new Entry();
                _inFlight[key] = entry;
                entry.Task = RunAsync(key, entry);
            }
        }
    }

    /// <summary>
    /// Take over a load that is already decoding: it leaves the scheduler's bookkeeping (so a later
    /// <see cref="Schedule"/> cannot cancel it) and the caller awaits <paramref name="task"/>, after which
    /// the result is in the cache. A load still queued behind the gate is cancelled instead and false is
    /// returned — the caller decodes it itself right away rather than waiting its turn in the queue.
    /// </summary>
    public bool TryAdopt(CacheKey key, out Task task)
    {
        lock (_lock)
        {
            if (_inFlight.Remove(key, out Entry? entry))
            {
                if (entry.Started)
                {
                    task = entry.Task;
                    task.ContinueWith(static (_, state) => ((Entry)state!).Cts.Dispose(), entry, TaskScheduler.Default);
                    return true;
                }
                entry.Cts.Cancel();
                entry.Cts.Dispose();
            }
        }
        task = Task.CompletedTask;
        return false;
    }

    public void CancelAll()
    {
        lock (_lock)
        {
            foreach (Entry entry in _inFlight.Values)
            {
                entry.Cts.Cancel();
                entry.Cts.Dispose();
            }
            _inFlight.Clear();
        }
    }

    private async Task RunAsync(CacheKey key, Entry entry)
    {
        CancellationTokenSource cts = entry.Cts;
        try
        {
            await _gate.WaitAsync(cts.Token).ConfigureAwait(false);
            try
            {
                // Task.Run so the load never starts on the caller's (UI) thread: a free semaphore completes
                // synchronously, and the load's own awaits would then resume on the UI dispatcher.
                if (!cts.Token.IsCancellationRequested)
                {
                    entry.Started = true;
                    await Task.Run(() => _load(key, cts.Token), cts.Token).ConfigureAwait(false);
                }
            }
            finally
            {
                _gate.Release();
            }
        }
        catch
        {
            // Cancelled or failed — the item simply stays uncached; nothing to surface.
        }
        finally
        {
            lock (_lock)
            {
                if (_inFlight.TryGetValue(key, out Entry? current) && ReferenceEquals(current, entry))
                {
                    _inFlight.Remove(key);
                    cts.Dispose();
                }
            }
        }
    }
}
