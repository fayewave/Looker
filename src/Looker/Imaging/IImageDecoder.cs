using System.Threading.Tasks;
using Microsoft.Graphics.Canvas;

namespace Looker.Imaging;

/// <summary>
/// Turns a file into a device-bound <see cref="DecodedImage"/>. Implementations run their heavy
/// work off the UI thread and must honor <see cref="DecodeRequest.Cancellation"/>. The router
/// (M4) chains these: WIC first, then Magick/ImageSharp fallbacks.
/// </summary>
public interface IImageDecoder
{
    /// <summary>Whether this decoder is willing to attempt the given sniffed format.</summary>
    bool CanDecode(ImageFormat format);

    /// <summary>
    /// Decodes onto <paramref name="device"/>. Throws <see cref="System.OperationCanceledException"/>
    /// if cancelled; returns null if this decoder declines the file (caller tries the next).
    /// </summary>
    Task<DecodedImage?> DecodeAsync(DecodeRequest request, ImageFormat format, CanvasDevice device);
}
