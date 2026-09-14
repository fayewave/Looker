using System;
using System.Collections.Generic;
using System.IO;
using System.Linq;
using System.Threading;
using System.Threading.Tasks;
using Looker.Imaging;
using MetadataExtractor;
using MetadataExtractor.Formats.Exif;
using Directory = MetadataExtractor.Directory;

namespace Looker.Services;

/// <summary>One "Label : Value" row in the info panel. <paramref name="IsPath"/> marks the file-location row,
/// which the panel renders as a button that reveals the file in Explorer.</summary>
public sealed record MetadataEntry(string Label, string Value, bool IsPath = false);

/// <summary>A titled cluster of <see cref="MetadataEntry"/> rows (e.g. "Camera", "Exposure").</summary>
public sealed record MetadataGroup(string Title, IReadOnlyList<MetadataEntry> Entries);

/// <summary>A full info-panel refresh: the curated EXIF groups plus the luma/RGB histogram (null when the
/// display pixels weren't available).</summary>
public sealed record InfoSnapshot(IReadOnlyList<MetadataGroup> Groups, HistogramData? Histogram);

/// <summary>The always-on status row under the strip: native pixel size, file size and the EXIF capture time
/// (null when the file has none). Cheap to produce; independent of the info panel.</summary>
public sealed record StatusInfo(string? Type, int Width, int Height, long SizeBytes, DateTime? Taken, DateTime? Modified, int PageCount = 0);

/// <summary>256-bucket per-channel counts plus a normalization ceiling (tallest interior bin, ignoring the
/// 0/255 spikes so a big flat/clipped area doesn't flatten the rest of the curve).</summary>
public sealed class HistogramData
{
    public required int[] Red { get; init; }
    public required int[] Green { get; init; }
    public required int[] Blue { get; init; }
    public required int[] Luma { get; init; }
    public int Max { get; init; }
}

/// <summary>
/// Reads EXIF/IPTC/XMP/GPS via MetadataExtractor (no pixel decode) into curated groups for the info panel,
/// and computes the display histogram from the screen-resolution BGRA8 buffer. Both run off the UI thread.
/// Everything is failure-tolerant: a file with no or broken metadata still yields the File group.
/// </summary>
public static class MetadataService
{
    public static Task<IReadOnlyList<MetadataGroup>> ReadAsync(string path, int width, int height, long sizeBytes, CancellationToken ct)
        => Task.Run(() => Read(path, width, height, sizeBytes), ct);

    /// <summary>The EXIF DateTimeOriginal, or null when the file has none or no readable metadata. Header parse
    /// only (no pixel decode), so it is cheap enough to run for every navigation.</summary>
    public static Task<DateTime?> ReadDateTakenAsync(string path, CancellationToken ct)
        => Task.Run(() => ReadDateTaken(path), ct);

    private static DateTime? ReadDateTaken(string path)
    {
        try
        {
            foreach (Directory dir in ImageMetadataReader.ReadMetadata(path))
            {
                if (dir is ExifSubIfdDirectory sub && sub.TryGetDateTime(ExifDirectoryBase.TagDateTimeOriginal, out DateTime taken))
                    return taken;
            }
        }
        catch { /* unsupported container or broken metadata: no date */ }
        return null;
    }

    /// <summary>The container format from the file's magic bytes (not its extension), named for display.</summary>
    private static string? SniffType(string path)
    {
        try
        {
            using FileStream fs = File.OpenRead(path);
            byte[] header = new byte[FormatSniffer.HeaderSize];
            int read = fs.Read(header, 0, header.Length);
            return ImageFormatNames.DisplayName(FormatSniffer.Sniff(header.AsSpan(0, read), Path.GetExtension(path)), path);
        }
        catch
        {
            return null;
        }
    }

    private static IReadOnlyList<MetadataGroup> Read(string path, int width, int height, long sizeBytes)
    {
        var groups = new List<MetadataGroup>();

        // File basics — always present, from the filesystem + the decoded (oriented) dimensions.
        var file = new List<MetadataEntry> { new("Name", Path.GetFileName(path)) };
        if (SniffType(path) is { } type)
            file.Add(new("Type", type));
        if (width > 0 && height > 0)
        {
            file.Add(new("Dimensions", $"{width} × {height}"));
            file.Add(new("Megapixels", $"{width * (long)height / 1_000_000.0:0.0} MP"));
        }
        if (sizeBytes > 0)
            file.Add(new("Size", FormatBytes(sizeBytes)));
        try { file.Add(new("Modified", File.GetLastWriteTime(path).ToString("g"))); } catch { }
        file.Add(new("File Path", path, IsPath: true));
        groups.Add(new("File", file));

        IReadOnlyList<Directory>? dirs = null;
        try { dirs = ImageMetadataReader.ReadMetadata(path); }
        catch { /* unsupported container or no metadata — File group is enough */ }

        if (dirs is not null)
        {
            ExifIfd0Directory? ifd0 = dirs.OfType<ExifIfd0Directory>().FirstOrDefault();
            ExifSubIfdDirectory? sub = dirs.OfType<ExifSubIfdDirectory>().FirstOrDefault();
            GpsDirectory? gps = dirs.OfType<GpsDirectory>().FirstOrDefault();

            var camera = new List<MetadataEntry>();
            AddDesc(camera, "Make", ifd0, ExifDirectoryBase.TagMake);
            AddDesc(camera, "Model", ifd0, ExifDirectoryBase.TagModel);
            AddDesc(camera, "Lens", sub, ExifDirectoryBase.TagLensModel);
            AddGroup(groups, "Camera", camera);

            var exposure = new List<MetadataEntry>();
            AddDesc(exposure, "Exposure", sub, ExifDirectoryBase.TagExposureTime);
            AddDesc(exposure, "Aperture", sub, ExifDirectoryBase.TagFNumber);
            AddDesc(exposure, "ISO", sub, ExifDirectoryBase.TagIsoEquivalent);
            AddDesc(exposure, "Focal length", sub, ExifDirectoryBase.TagFocalLength);
            AddDesc(exposure, "Exposure bias", sub, ExifDirectoryBase.TagExposureBias);
            AddDesc(exposure, "Metering", sub, ExifDirectoryBase.TagMeteringMode);
            AddDesc(exposure, "Flash", sub, ExifDirectoryBase.TagFlash);
            AddDesc(exposure, "White balance", sub, ExifDirectoryBase.TagWhiteBalance);
            AddGroup(groups, "Exposure", exposure);

            var date = new List<MetadataEntry>();
            AddDesc(date, "Taken", sub, ExifDirectoryBase.TagDateTimeOriginal);
            AddDesc(date, "Digitized", sub, ExifDirectoryBase.TagDateTimeDigitized);
            AddGroup(groups, "Date", date);

            if (gps is not null)
            {
                var location = new List<MetadataEntry>();
                GeoLocation? geo = gps.GetGeoLocation();
                if (geo is not null && !geo.IsZero)
                {
                    location.Add(new("Latitude", geo.Latitude.ToString("0.000000")));
                    location.Add(new("Longitude", geo.Longitude.ToString("0.000000")));
                }
                AddDesc(location, "Altitude", gps, GpsDirectory.TagAltitude);
                AddGroup(groups, "Location", location);
            }
        }

        return groups;
    }

    /// <summary>Bin the BGRA8 display buffer into per-channel + luma histograms.</summary>
    public static HistogramData ComputeHistogram(byte[] bgra, int width, int height)
    {
        var red = new int[256];
        var green = new int[256];
        var blue = new int[256];
        var luma = new int[256];

        int stride = width * 4;
        for (int y = 0; y < height; y++)
        {
            int row = y * stride;
            for (int x = 0; x < width; x++)
            {
                int i = row + (x << 2);
                byte b = bgra[i];
                byte g = bgra[i + 1];
                byte r = bgra[i + 2];
                red[r]++;
                green[g]++;
                blue[b]++;
                // Rec.709 luma with integer weights summing to 256.
                luma[(r * 54 + g * 183 + b * 19) >> 8]++;
            }
        }

        int max = 1;
        for (int i = 1; i < 255; i++)
        {
            if (red[i] > max) max = red[i];
            if (green[i] > max) max = green[i];
            if (blue[i] > max) max = blue[i];
            if (luma[i] > max) max = luma[i];
        }

        return new HistogramData { Red = red, Green = green, Blue = blue, Luma = luma, Max = max };
    }

    private static void AddGroup(List<MetadataGroup> groups, string title, List<MetadataEntry> entries)
    {
        if (entries.Count > 0)
            groups.Add(new MetadataGroup(title, entries));
    }

    private static void AddDesc(List<MetadataEntry> list, string label, Directory? directory, int tag)
    {
        if (directory is null)
            return;
        string? description = null;
        try { description = directory.GetDescription(tag); } catch { }
        if (!string.IsNullOrWhiteSpace(description))
            list.Add(new MetadataEntry(label, description!));
    }

    public static string FormatBytes(long bytes)
    {
        if (bytes >= 1L << 20)
            return $"{bytes / (double)(1 << 20):0.0} MB";
        if (bytes >= 1 << 10)
            return $"{bytes / (double)(1 << 10):0.0} KB";
        return $"{bytes} B";
    }
}
