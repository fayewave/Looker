using System;
using System.Collections.Generic;
using Windows.Graphics.Imaging;

namespace Looker.Imaging;

/// <summary>
/// What Windows Imaging Component can decode on <em>this</em> machine — which depends on installed
/// Store codec packs (HEVC/AV1/RAW/WebP extensions). Built once by enumerating the registered WIC
/// decoders; the router uses it to decide whether to try the fast WIC path before Magick.
/// </summary>
public sealed class CodecInventory
{
    /// <summary>
    /// CLSID of the Microsoft Raw Image Extension decoder (Store codec pack, libraw-based). Windows
    /// also ships an inbox "DNG Decoder" that only returns the embedded preview (IFD0, e.g. 960x720
    /// for a 12 MP DJI file) and is registered ahead of this one for ".dng", so for camera RAW we
    /// must ask for this codec by id rather than trust WIC's own selection.
    /// </summary>
    public static readonly Guid RawImageDecoderId = new("41945702-8302-44a6-9445-ac98e8afa086");

    private readonly HashSet<string> _wicExtensions;
    private readonly Dictionary<string, List<Guid>> _decodersByExtension;

    private CodecInventory(HashSet<string> wicExtensions, Dictionary<string, List<Guid>> decodersByExtension)
    {
        _wicExtensions = wicExtensions;
        _decodersByExtension = decodersByExtension;
    }

    public static CodecInventory Create()
    {
        var extensions = new HashSet<string>(StringComparer.OrdinalIgnoreCase);
        var decoders = new Dictionary<string, List<Guid>>(StringComparer.OrdinalIgnoreCase);
        try
        {
            foreach (BitmapCodecInformation info in BitmapDecoder.GetDecoderInformationEnumerator())
            {
                // FileExtensions is a list like [".jpg", ".jpeg", ".jpe", ...].
                foreach (string raw in info.FileExtensions)
                {
                    string ext = raw.Trim();
                    extensions.Add(ext);
                    if (!decoders.TryGetValue(ext, out List<Guid>? ids))
                        decoders[ext] = ids = new List<Guid>();
                    ids.Add(info.CodecId);
                }
            }
        }
        catch
        {
            // If enumeration fails, treat every format as non-WIC and let Magick handle everything.
        }

        return new CodecInventory(extensions, decoders);
    }

    /// <summary>
    /// A WIC decoder registered for a file extension, for callers that must not rely on WIC content
    /// sniffing. If <paramref name="preferred"/> is among the registered codecs it wins; otherwise the
    /// first registration does. Camera RAW is the case that matters (see <see cref="RawImageDecoderId"/>).
    /// </summary>
    public bool TryGetDecoderId(string? extension, Guid? preferred, out Guid decoderId)
    {
        decoderId = Guid.Empty;
        if (string.IsNullOrEmpty(extension) || !_decodersByExtension.TryGetValue(extension, out List<Guid>? ids) || ids.Count == 0)
            return false;

        decoderId = preferred is Guid p && ids.Contains(p) ? p : ids[0];
        return true;
    }

    public IReadOnlyCollection<string> WicExtensions => _wicExtensions;

    /// <summary>Whether a WIC codec for this format is installed (so the fast path is worth trying).</summary>
    public bool CanWicDecode(ImageFormat format)
    {
        foreach (string ext in ExtensionsFor(format))
        {
            if (_wicExtensions.Contains(ext))
                return true;
        }
        return false;
    }

    private static IReadOnlyList<string> ExtensionsFor(ImageFormat format) => format switch
    {
        ImageFormat.Jpeg => new[] { ".jpg", ".jpeg" },
        ImageFormat.Png => new[] { ".png" },
        ImageFormat.Bmp => new[] { ".bmp" },
        ImageFormat.Gif => new[] { ".gif" },
        ImageFormat.Tiff => new[] { ".tif", ".tiff" },
        ImageFormat.Ico => new[] { ".ico" },
        ImageFormat.Cur => new[] { ".cur" },
        ImageFormat.JpegXr => new[] { ".jxr", ".wdp", ".hdp" },
        ImageFormat.Jpeg2000 => new[] { ".jp2", ".j2k" },
        ImageFormat.Webp => new[] { ".webp" },
        ImageFormat.Heif => new[] { ".heic", ".heif", ".hif" },
        ImageFormat.Avif => new[] { ".avif", ".avifs" },
        ImageFormat.JpegXl => new[] { ".jxl" },
        ImageFormat.Psd => new[] { ".psd" },
        ImageFormat.Tga => new[] { ".tga" },
        ImageFormat.Dds => new[] { ".dds" },
        ImageFormat.Pcx => new[] { ".pcx" },
        ImageFormat.Pnm => new[] { ".pnm", ".ppm", ".pgm", ".pbm" },
        ImageFormat.Xcf => new[] { ".xcf" },
        ImageFormat.Qoi => new[] { ".qoi" },
        ImageFormat.Raw => new[] { ".cr2", ".cr3", ".crw", ".nef", ".nrw", ".arw", ".sr2", ".srf", ".dng", ".orf", ".raf",
            ".rw2", ".pef", ".srw", ".3fr", ".fff", ".rwl", ".iiq", ".mrw", ".dcr", ".kdc", ".erf", ".mef" },
        _ => Array.Empty<string>(),
    };
}
