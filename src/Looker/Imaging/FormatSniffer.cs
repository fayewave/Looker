using System;

namespace Looker.Imaging;

/// <summary>
/// Identifies image formats from a file's leading bytes (magic numbers). Content beats
/// extension because extensions lie (a ".jpg" that is really a PNG is common). Extension is
/// used only as a last resort for formats with no reliable signature (e.g. TGA).
/// </summary>
public static class FormatSniffer
{
    /// <summary>Bytes to read for a confident sniff (covers ISO-BMFF ftyp brands and APNG scan runway).</summary>
    public const int HeaderSize = 64;

    public static ImageFormat Sniff(ReadOnlySpan<byte> header, string? extension = null)
    {
        if (header.Length >= 3 && header[0] == 0xFF && header[1] == 0xD8 && header[2] == 0xFF)
            return ImageFormat.Jpeg;

        if (StartsWith(header, 0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A))
            return ImageFormat.Png;

        if (Ascii(header, 0, "GIF87a") || Ascii(header, 0, "GIF89a"))
            return ImageFormat.Gif;

        if (Ascii(header, 0, "BM"))
            return ImageFormat.Bmp;

        // Camera RAW with its own signature: Olympus ORF ("IIRO"/"IIRS"/"MMOR"), Fujifilm RAF, Panasonic/Leica
        // RW2/RWL ("IIU\0"), Minolta MRW ("\0MRM"), Canon CRW (CIFF: "II\x1a\0\0\0HEAPCCDR").
        if (Ascii(header, 0, "IIRO") || Ascii(header, 0, "IIRS") || Ascii(header, 0, "MMOR") ||
            Ascii(header, 0, "FUJIFILMCCD-RAW") ||
            StartsWith(header, 0x49, 0x49, 0x55, 0x00) ||
            StartsWith(header, 0x00, 0x4D, 0x52, 0x4D) ||
            (StartsWith(header, 0x49, 0x49, 0x1A, 0x00) && Ascii(header, 6, "HEAPCCDR")))
            return ImageFormat.Raw;

        // JPEG XR: "II" + 0xBC, distinct from TIFF's "II*".
        if (StartsWith(header, 0x49, 0x49, 0xBC))
            return ImageFormat.JpegXr;

        // TIFF: little-endian "II*\0" or big-endian "MM\0*". Most RAW containers (DNG, NEF, ARW, CR2,
        // PEF...) are TIFF underneath, and a TIFF decoder would hand back IFD0 - usually a small embedded
        // preview - instead of the sensor data. CR2 is identifiable by content ("CR" at offset 8); the
        // rest are told apart only by extension, which is the one place extension beats content.
        if (StartsWith(header, 0x49, 0x49, 0x2A, 0x00) || StartsWith(header, 0x4D, 0x4D, 0x00, 0x2A))
        {
            if (Ascii(header, 8, "CR") || IsRawExtension(extension))
                return ImageFormat.Raw;
            return ImageFormat.Tiff;
        }

        // RIFF container: "RIFF"<size>"WEBP".
        if (Ascii(header, 0, "RIFF") && Ascii(header, 8, "WEBP"))
            return ImageFormat.Webp;

        // ISO-BMFF: bytes 4..7 == "ftyp", major brand at 8..11 distinguishes HEIF vs AVIF vs Canon CR3.
        if (Ascii(header, 4, "ftyp"))
        {
            if (Ascii(header, 8, "crx "))
                return ImageFormat.Raw;
            if (Ascii(header, 8, "avif") || Ascii(header, 8, "avis"))
                return ImageFormat.Avif;
            if (Ascii(header, 8, "heic") || Ascii(header, 8, "heix") ||
                Ascii(header, 8, "heif") || Ascii(header, 8, "hevc") ||
                Ascii(header, 8, "mif1") || Ascii(header, 8, "msf1"))
                return ImageFormat.Heif;
        }

        if (Ascii(header, 0, "8BPS"))
            return ImageFormat.Psd;

        // JPEG XL: raw codestream (FF 0A) or ISO-BMFF container box.
        if (StartsWith(header, 0xFF, 0x0A) ||
            StartsWith(header, 0x00, 0x00, 0x00, 0x0C, 0x4A, 0x58, 0x4C, 0x20, 0x0D, 0x0A, 0x87, 0x0A))
            return ImageFormat.JpegXl;

        // DDS: "DDS ".
        if (Ascii(header, 0, "DDS "))
            return ImageFormat.Dds;

        // ICO / CUR: reserved(0) + type(1 == icon, 2 == cursor).
        if (StartsWith(header, 0x00, 0x00, 0x01, 0x00))
            return ImageFormat.Ico;
        if (StartsWith(header, 0x00, 0x00, 0x02, 0x00))
            return ImageFormat.Cur;

        // JPEG 2000: JP2 signature box ("jP  " where JPEG XL has "JXL ") or a bare J2K codestream (FF 4F FF 51).
        if (StartsWith(header, 0x00, 0x00, 0x00, 0x0C, 0x6A, 0x50, 0x20, 0x20, 0x0D, 0x0A, 0x87, 0x0A) ||
            StartsWith(header, 0xFF, 0x4F, 0xFF, 0x51))
            return ImageFormat.Jpeg2000;

        if (Ascii(header, 0, "gimp xcf "))
            return ImageFormat.Xcf;

        if (Ascii(header, 0, "qoif"))
            return ImageFormat.Qoi;

        // PCX: manufacturer 0x0A, version 0..5, RLE encoding flag 1.
        if (header.Length >= 3 && header[0] == 0x0A && header[1] <= 5 && header[2] == 1)
            return ImageFormat.Pcx;

        // Netpbm: "P1".."P6" followed by whitespace (ASCII or binary bitmap/graymap/pixmap).
        if (header.Length >= 3 && header[0] == (byte)'P' && header[1] >= (byte)'1' && header[1] <= (byte)'6' &&
            (header[2] == (byte)' ' || header[2] == (byte)'\n' || header[2] == (byte)'\r' || header[2] == (byte)'\t'))
            return ImageFormat.Pnm;

        if (Ascii(header, 0, "%PDF-"))
            return ImageFormat.Pdf;

        if (LooksLikeSvg(header))
            return ImageFormat.Svg;

        return FromExtension(extension);
    }

    private static bool IsRawExtension(string? extension)
        => !string.IsNullOrEmpty(extension) && FromExtension(extension) == ImageFormat.Raw;

    private static ImageFormat FromExtension(string? extension)
    {
        if (string.IsNullOrEmpty(extension))
            return ImageFormat.Unknown;

        return extension.ToLowerInvariant() switch
        {
            ".jpg" or ".jpeg" or ".jpe" or ".jfif" => ImageFormat.Jpeg,
            ".png" or ".apng" => ImageFormat.Png,
            ".gif" => ImageFormat.Gif,
            ".bmp" or ".dib" => ImageFormat.Bmp,
            ".tif" or ".tiff" => ImageFormat.Tiff,
            ".webp" => ImageFormat.Webp,
            ".ico" => ImageFormat.Ico,
            ".cur" => ImageFormat.Cur,
            ".jxr" or ".wdp" or ".hdp" => ImageFormat.JpegXr,
            ".jp2" or ".j2k" or ".jpf" or ".jpx" or ".j2c" => ImageFormat.Jpeg2000,
            ".heic" or ".heif" or ".hif" or ".heics" => ImageFormat.Heif,
            ".avif" or ".avifs" => ImageFormat.Avif,
            ".svg" => ImageFormat.Svg,
            ".pdf" => ImageFormat.Pdf,
            ".psd" or ".psb" => ImageFormat.Psd,
            ".jxl" => ImageFormat.JpegXl,
            ".dds" => ImageFormat.Dds,
            ".tga" or ".icb" or ".vda" or ".vst" => ImageFormat.Tga,
            ".pcx" => ImageFormat.Pcx,
            ".pnm" or ".ppm" or ".pgm" or ".pbm" => ImageFormat.Pnm,
            ".xcf" => ImageFormat.Xcf,
            ".qoi" => ImageFormat.Qoi,
            ".cr2" or ".cr3" or ".crw" or ".nef" or ".nrw" or ".arw" or ".sr2" or ".srf" or ".dng" or ".orf" or ".raf"
                or ".rw2" or ".pef" or ".srw" or ".3fr" or ".fff" or ".rwl" or ".iiq" or ".mrw" or ".dcr"
                or ".kdc" or ".erf" or ".mef" => ImageFormat.Raw,
            _ => ImageFormat.Unknown,
        };
    }

    private static bool LooksLikeSvg(ReadOnlySpan<byte> header)
    {
        // Skip a UTF-8 BOM, then look for "<?xml" or "<svg" near the start.
        int start = StartsWith(header, 0xEF, 0xBB, 0xBF) ? 3 : 0;
        ReadOnlySpan<byte> s = header[start..];
        return Ascii(s, 0, "<?xml") || Ascii(s, 0, "<svg") || Ascii(s, 0, "<!--");
    }

    private static bool StartsWith(ReadOnlySpan<byte> header, params byte[] prefix)
    {
        if (header.Length < prefix.Length)
            return false;
        for (int i = 0; i < prefix.Length; i++)
        {
            if (header[i] != prefix[i])
                return false;
        }
        return true;
    }

    /// <summary>Case-sensitive ASCII match of <paramref name="text"/> at <paramref name="offset"/>.</summary>
    private static bool Ascii(ReadOnlySpan<byte> header, int offset, string text)
    {
        if (offset < 0 || offset + text.Length > header.Length)
            return false;
        for (int i = 0; i < text.Length; i++)
        {
            if (header[offset + i] != (byte)text[i])
                return false;
        }
        return true;
    }
}
