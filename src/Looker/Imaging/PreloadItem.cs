namespace Looker.Imaging;

/// <summary>Which resolution tier to warm the cache with.</summary>
public enum PreloadTier
{
    /// <summary>Screen-resolution (sharp) — for the immediate next/prev so steady nav is crisp.</summary>
    High,

    /// <summary>Small (~0.5 MP) — cheap, so it can blanket a wide ring for instant-on-arrival display.</summary>
    Low,
}

/// <summary>One entry in the preload window: a file plus the tier to decode it at.</summary>
public readonly record struct PreloadItem(ImageRef Image, PreloadTier Tier);
