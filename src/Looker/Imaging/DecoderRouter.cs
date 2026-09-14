using System;
using System.Collections.Generic;
using System.Linq;
using System.Threading.Tasks;
using Microsoft.Graphics.Canvas;

namespace Looker.Imaging;

/// <summary>
/// The decoder the viewport talks to. For a given sniffed format it builds an ordered chain
/// (via <see cref="DecoderChain"/>) and tries each decoder until one produces an image; a decoder
/// that throws or declines is skipped. If the whole chain fails, the last error propagates so the
/// viewport can show its error state.
/// </summary>
public sealed class DecoderRouter : IImageDecoder
{
    private readonly CodecInventory _inventory;
    private readonly AnimatedImageDecoder _animated = new();
    private readonly WicDecoder _wic;
    private readonly SvgDecoder _svg = new();
    private readonly MagickDecoder _magick = new();
    private readonly PdfDecoder _pdf = new();

    public DecoderRouter(CodecInventory inventory)
    {
        _inventory = inventory;
        _wic = new WicDecoder(inventory);
    }

    /// <summary>Debug switch: force every decode down the Magick fallback to exercise that path.</summary>
    public static bool ForceMagickFallback { get; set; }

    public bool CanDecode(ImageFormat format) => true;

    public async Task<DecodedImage?> DecodeAsync(DecodeRequest request, ImageFormat format, CanvasDevice device)
    {
        IReadOnlyList<DecoderKind> chain = ForceMagickFallback
            ? new[] { DecoderKind.Magick }
            : DecoderChain.Plan(format, _inventory.CanWicDecode);

        // The low-res placeholder wants a static first frame — skip the (heavy) animated decoder.
        if (request.FirstFrameOnly)
            chain = chain.Where(kind => kind != DecoderKind.Animated).ToList();

        Exception? lastError = null;
        foreach (DecoderKind kind in chain)
        {
            request.Cancellation.ThrowIfCancellationRequested();
            try
            {
                DecodedImage? result = await Resolve(kind).DecodeAsync(request, format, device);
                if (result is not null)
                    return result;
            }
            catch (OperationCanceledException)
            {
                throw;
            }
            catch (Exception ex)
            {
                lastError = ex; // try the next decoder in the chain
            }
        }

        if (lastError is not null)
            throw lastError;
        return null;
    }

    private IImageDecoder Resolve(DecoderKind kind) => kind switch
    {
        DecoderKind.Animated => _animated,
        DecoderKind.Wic => _wic,
        DecoderKind.Svg => _svg,
        DecoderKind.Magick => _magick,
        DecoderKind.Pdf => _pdf,
        _ => _wic,
    };
}
