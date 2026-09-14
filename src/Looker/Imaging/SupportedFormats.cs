using System;
using System.Collections.Generic;
using System.IO;

namespace Looker.Imaging;

/// <summary>
/// The set of file extensions Looker will navigate to within a folder. M2 ships the
/// WIC-native set; M4 expands this as the decoder router gains HEIC/AVIF/RAW/SVG/PSD/JXL/etc.
/// (extension list is intentionally the single place to widen coverage).
/// </summary>
public static class SupportedFormats
{
    public static readonly IReadOnlySet<string> Extensions = new HashSet<string>(StringComparer.OrdinalIgnoreCase)
    {
        // WIC-native (M2)
        ".jpg", ".jpeg", ".jpe", ".jfif",
        ".png", ".apng",
        ".bmp", ".dib",
        ".gif",
        ".tif", ".tiff",
        ".webp",
        ".ico", ".cur",
        ".jxr", ".wdp", ".hdp",
        // M4: WIC-with-codec-pack or Magick fallback
        ".heic", ".heif", ".hif", ".heics",
        ".avif", ".avifs",
        ".jxl",
        ".jp2", ".j2k", ".jpf", ".jpx", ".j2c",
        ".svg",
        ".pdf",
        ".psd", ".psb",
        ".tga", ".icb", ".vda", ".vst",
        ".dds",
        ".pcx",
        ".pnm", ".ppm", ".pgm", ".pbm",
        ".xcf",
        ".qoi",
        // Camera RAW (WIC RawImageExtension or Magick/libraw)
        ".cr2", ".cr3", ".crw", ".nef", ".nrw", ".arw", ".sr2", ".srf", ".dng", ".orf", ".raf",
        // (No Sigma .x3f: libraw dropped Foveon support and no WIC codec reads it, so nothing here could decode one.)
        ".rw2", ".pef", ".srw", ".3fr", ".fff", ".rwl", ".iiq", ".mrw", ".dcr", ".kdc", ".erf", ".mef",
    };

    public static bool IsSupported(string path)
        => Extensions.Contains(Path.GetExtension(path));
}
