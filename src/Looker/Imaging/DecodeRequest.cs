using System.Threading;

namespace Looker.Imaging;

/// <summary>
/// Everything the decode pipeline needs to produce — and later cache-key — one image.
/// <see cref="TargetWidth"/>/<see cref="TargetHeight"/> are physical pixels the viewport wants
/// to fill; the decoder scales down to fit and never upscales. <see cref="FullResolution"/>
/// bypasses the target for zoom-to-100% (M3). <see cref="FirstFrameOnly"/> forces a static decode
/// (skips the animated decoder) — used for the fast low-res placeholder of an animation.
/// </summary>
public readonly record struct DecodeRequest(
    string Path,
    long ModifiedTicks,
    int TargetWidth,
    int TargetHeight,
    bool FullResolution,
    bool FirstFrameOnly,
    CancellationToken Cancellation);
