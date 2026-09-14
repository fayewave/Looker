namespace Looker.Imaging;

/// <summary>
/// Identifies a file to load, carrying the mtime so it participates in cache keys (an edit to a
/// file with the same path must not serve a stale decode).
/// </summary>
public readonly record struct ImageRef(string Path, long ModifiedTicks);
