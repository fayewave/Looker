using System;
using System.Collections.Generic;
using System.Linq;

namespace Looker.Imaging;

/// <summary>A reference-counted value the <see cref="ImageCache"/> can size and release.</summary>
public interface ICacheValue
{
    long ByteSize { get; }
    void Retain();
    void Release();
}

/// <summary>
/// Cache identity for a decode. <see cref="Bucket"/> quantizes the requested target size (max
/// dimension, in 256 px steps) so small window resizes reuse the same entry instead of thrashing
/// the cache, while low-res and screen-res decodes of the same file get distinct entries.
/// </summary>
public readonly record struct CacheKey(string Path, long ModifiedTicks, int Bucket)
{
    public static int BucketFor(int maxTargetDimension) => Math.Max(1, (maxTargetDimension + 255) / 256) * 256;
}

/// <summary>
/// Byte-budgeted LRU of decoded images — the core of "revisiting is instant". Thread-safe because
/// the preload scheduler inserts from worker threads while the UI thread reads. Eviction releases
/// the cache's reference; refcounting keeps any still-displayed bitmap alive past eviction.
/// </summary>
public sealed class ImageCache
{
    private sealed class Entry
    {
        public required CacheKey Key { get; init; }
        public required ICacheValue Value { get; init; }
        public required long Size { get; init; }
    }

    private readonly object _lock = new();
    private readonly Dictionary<CacheKey, LinkedListNode<Entry>> _map = new();
    private readonly LinkedList<Entry> _lru = new(); // first = most recently used
    private long _bytes;

    public ImageCache(long budgetBytes)
    {
        BudgetBytes = budgetBytes;
    }

    public long BudgetBytes { get; set; }

    public long CurrentBytes
    {
        get { lock (_lock) { return _bytes; } }
    }

    public int Count
    {
        get { lock (_lock) { return _map.Count; } }
    }

    /// <summary>On a hit, promotes to most-recently-used and hands the caller its own reference.</summary>
    public bool TryGet(CacheKey key, out ICacheValue value)
    {
        lock (_lock)
        {
            if (_map.TryGetValue(key, out LinkedListNode<Entry>? node))
            {
                _lru.Remove(node);
                _lru.AddFirst(node);
                value = node.Value.Value;
                value.Retain();
                return true;
            }
        }

        value = null!;
        return false;
    }

    public bool Contains(CacheKey key)
    {
        lock (_lock) { return _map.ContainsKey(key); }
    }

    /// <summary>Insert (or replace), taking a cache reference, then evict LRU until within budget.</summary>
    public void Insert(CacheKey key, ICacheValue value)
    {
        List<ICacheValue>? released = null;
        lock (_lock)
        {
            if (_map.TryGetValue(key, out LinkedListNode<Entry>? existing))
            {
                _bytes -= existing.Value.Size;
                (released ??= new()).Add(existing.Value.Value);
                _lru.Remove(existing);
                _map.Remove(key);
            }

            value.Retain();
            long size = Math.Max(0, value.ByteSize);
            var node = new LinkedListNode<Entry>(new Entry { Key = key, Value = value, Size = size });
            _lru.AddFirst(node);
            _map[key] = node;
            _bytes += size;

            // Evict from the LRU tail, but never the entry we just inserted (keep at least it).
            while (_bytes > BudgetBytes && _lru.Count > 1)
            {
                LinkedListNode<Entry> tail = _lru.Last!;
                _lru.RemoveLast();
                _map.Remove(tail.Value.Key);
                _bytes -= tail.Value.Size;
                (released ??= new()).Add(tail.Value.Value);
            }
        }

        if (released is not null)
        {
            foreach (ICacheValue v in released)
                v.Release();
        }
    }

    /// <summary>Drop everything — e.g. on device-lost, when every cached GPU bitmap is invalid.</summary>
    public void Clear()
    {
        List<ICacheValue> all;
        lock (_lock)
        {
            all = _lru.Select(e => e.Value).ToList();
            _map.Clear();
            _lru.Clear();
            _bytes = 0;
        }

        foreach (ICacheValue v in all)
            v.Release();
    }
}
