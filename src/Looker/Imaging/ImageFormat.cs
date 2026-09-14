namespace Looker.Imaging;

/// <summary>
/// Logical image format resolved by <see cref="FormatSniffer"/> from magic bytes (extension
/// only as a tiebreaker). Drives the decoder router in M4; before then it is informational.
/// </summary>
public enum ImageFormat
{
    Unknown = 0,

    // Decodable via WIC from M2:
    Jpeg,
    Png,
    Bmp,
    Gif,
    Tiff,
    Webp,
    Ico,

    // Recognized by the sniffer now, decodable once the router + fallbacks land (M4+):
    Heif,
    Avif,
    Svg,
    Psd,
    JpegXl,
    Tga,
    Dds,
    Raw,

    // Sept 2026 widening: JPEG XR is WIC-native (inbox codec, no Magick coder); the rest are Magick only.
    JpegXr,
    Jpeg2000,
    Cur,
    Pcx,
    Pnm,
    Xcf,
    Qoi,

    // Rendered page by page through the inbox Windows.Data.Pdf renderer (no WIC codec, no Magick).
    Pdf,
}

public static class ImageFormatNames
{
    /// <summary>User-facing name of a sniffed format for the info panel and status row, e.g. "JPEG", "HEIC",
    /// "RAW (NEF)". The container is what the bytes say; the extension only refines the label where one
    /// container has several common names. Null for <see cref="ImageFormat.Unknown"/>.</summary>
    public static string? DisplayName(ImageFormat format, string? path)
    {
        string ext = System.IO.Path.GetExtension(path ?? string.Empty).TrimStart('.').ToUpperInvariant();
        return format switch
        {
            ImageFormat.Jpeg => "JPEG",
            ImageFormat.Png => ext == "APNG" ? "APNG" : "PNG",
            ImageFormat.Bmp => "BMP",
            ImageFormat.Gif => "GIF",
            ImageFormat.Tiff => "TIFF",
            ImageFormat.Webp => "WebP",
            ImageFormat.Ico => "ICO",
            ImageFormat.Cur => "CUR",
            ImageFormat.JpegXr => "JPEG XR",
            ImageFormat.Jpeg2000 => "JPEG 2000",
            ImageFormat.Heif => ext is "HEIC" or "HEICS" ? "HEIC" : ext is "HIF" ? "HIF" : "HEIF",
            ImageFormat.Avif => "AVIF",
            ImageFormat.Svg => "SVG",
            ImageFormat.Psd => ext == "PSB" ? "PSB" : "PSD",
            ImageFormat.JpegXl => "JPEG XL",
            ImageFormat.Tga => "TGA",
            ImageFormat.Dds => "DDS",
            ImageFormat.Pcx => "PCX",
            ImageFormat.Pnm => ext is "PPM" or "PGM" or "PBM" ? ext : "PNM",
            ImageFormat.Xcf => "XCF",
            ImageFormat.Qoi => "QOI",
            ImageFormat.Raw => ext.Length > 0 ? $"RAW ({ext})" : "RAW",
            ImageFormat.Pdf => "PDF",
            _ => null,
        };
    }
}
