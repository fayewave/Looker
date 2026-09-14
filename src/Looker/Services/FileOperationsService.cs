using System;
using System.Collections.Generic;
using System.Diagnostics;
using System.IO;
using System.Threading.Tasks;
using Looker.Helpers;
using Looker.Imaging;
using MetadataExtractor;
using MetadataExtractor.Formats.Exif;
using Windows.ApplicationModel.DataTransfer;
using Windows.Foundation;
using Windows.Graphics.Imaging;
using Windows.Storage;
using Windows.Storage.Streams;
using Windows.System;
using Directory = MetadataExtractor.Directory;

namespace Looker.Services;

/// <summary>
/// The M7 file-management actions, all off the UI thread where they touch disk. Deliberately built on
/// managed/WinRT APIs rather than shell P/Invoke (the one exception is setting the wallpaper — see
/// <see cref="NativeMethods"/>): recycle via <see cref="StorageFile.DeleteAsync(StorageDeleteOption)"/>,
/// clipboard via the WinRT <see cref="Clipboard"/>, reveal via Explorer's <c>/select</c>, and rotate via
/// a <see cref="Windows.Graphics.Imaging"/> transcode. The app is runFullTrust, so path-based file access
/// works everywhere. Every method is failure-tolerant: a denied or impossible operation returns false/null
/// rather than throwing, so the viewer never dies on a bad file.
/// </summary>
public sealed class FileOperationsService
{
    /// <summary>Move the file to the Recycle Bin (no confirm dialog). <see cref="StorageDeleteOption.Default"/>
    /// recycles rather than permanently deleting.</summary>
    public async Task<bool> RecycleAsync(string path)
    {
        try
        {
            StorageFile file = await StorageFile.GetFileFromPathAsync(path);
            await file.DeleteAsync(StorageDeleteOption.Default);
            return true;
        }
        catch
        {
            return false;
        }
    }

    /// <summary>Rename in place. If the new name has no extension the original one is kept, so the rename
    /// flyout can pre-select just the base name. Returns the new full path, or null on collision/invalid
    /// name/failure.</summary>
    public async Task<string?> RenameAsync(string path, string newName)
    {
        try
        {
            newName = (newName ?? string.Empty).Trim();
            if (newName.Length == 0 || newName.IndexOfAny(Path.GetInvalidFileNameChars()) >= 0)
                return null;

            string dir = Path.GetDirectoryName(path) ?? string.Empty;
            if (!Path.HasExtension(newName))
                newName += Path.GetExtension(path);

            string newPath = Path.Combine(dir, newName);
            if (string.Equals(newPath, path, StringComparison.OrdinalIgnoreCase))
                return path; // unchanged (or case-only; treat as no-op)
            if (File.Exists(newPath) || System.IO.Directory.Exists(newPath))
                return null; // collision

            await Task.Run(() => File.Move(path, newPath));
            return newPath;
        }
        catch
        {
            return null;
        }
    }

    /// <summary>
    /// Rotate-and-save in 90° steps for the encodable raster formats (JPEG/PNG/TIFF/BMP). For an image with
    /// no meaningful EXIF orientation (the common case) this transcodes in place, preserving all metadata;
    /// for an EXIF-oriented image it bakes the existing orientation plus the requested turn into fresh pixels
    /// and drops the (now-neutralised) orientation tag, so the result is always visually correct. Writes to a
    /// sibling temp file and swaps atomically; the mtime bump invalidates the decode cache. Returns false for
    /// non-encodable formats (HEIC/RAW/SVG/PSD/animated GIF/…) or on any failure.
    /// </summary>
    public Task<bool> RotateAsync(string path, bool clockwise) => RotateAsync(path, clockwise ? 90 : 270);

    /// <summary>Rotate-and-save by <paramref name="degrees"/> clockwise (90/180/270); other multiples are
    /// normalized. A no-op multiple of 360 returns false. Used to bake in an accumulated rotation preview.</summary>
    public async Task<bool> RotateAsync(string path, int degrees)
    {
        int delta = ((degrees % 360) + 360) % 360;
        if (delta == 0)
            return false;

        Guid? encoderId = EncoderIdFor(Path.GetExtension(path));
        if (encoderId is null)
            return false;

        string temp = Path.Combine(
            Path.GetDirectoryName(path) ?? string.Empty,
            "." + Path.GetFileName(path) + ".flebrot.tmp");

        try
        {
            int orientation = ReadExifOrientation(path);
            try
            {
                if (orientation == 1)
                    await TranscodeRotateAsync(path, temp, delta);
                else
                    await BakeRotateAsync(path, temp, encoderId.Value, delta);

                ReplaceFile(temp, path);
            }
            finally
            {
                TryDelete(temp);
            }

            // Guarantee a new cache key even if the filesystem preserved the write time through the swap.
            try { File.SetLastWriteTimeUtc(path, DateTime.UtcNow); } catch { }
            return true;
        }
        catch
        {
            return false;
        }
    }

    /// <summary>Put the image on the clipboard as both a file (StorageItems) and a bitmap (so it pastes into
    /// Paint). Common raster formats are referenced directly; exotic formats are rendered to a PNG first via
    /// <paramref name="renderPng"/> (the viewport's current display).</summary>
    public async Task CopyImageAsync(string path, ImageFormat format, Func<string, Task<bool>> renderPng)
    {
        try
        {
            StorageFile file = await StorageFile.GetFileFromPathAsync(path);
            var package = new DataPackage { RequestedOperation = DataPackageOperation.Copy };
            package.SetStorageItems(new IStorageItem[] { file });

            if (IsClipboardRaster(format))
            {
                package.SetBitmap(RandomAccessStreamReference.CreateFromFile(file));
            }
            else
            {
                string tmp = Path.Combine(Path.GetTempPath(), "fleb-clip-" + Guid.NewGuid().ToString("N") + ".png");
                if (await renderPng(tmp))
                {
                    StorageFile rendered = await StorageFile.GetFileFromPathAsync(tmp);
                    package.SetBitmap(RandomAccessStreamReference.CreateFromFile(rendered));
                }
            }

            Clipboard.SetContent(package);
            TryFlushClipboard();
        }
        catch
        {
            // clipboard denied or file gone — nothing to surface
        }
    }

    /// <summary>Copy the full file path as text.</summary>
    public void CopyPath(string path)
    {
        try
        {
            var package = new DataPackage();
            package.SetText(path);
            Clipboard.SetContent(package);
            TryFlushClipboard();
        }
        catch
        {
            // ignore
        }
    }

    /// <summary>Open Explorer on the item's folder with the item highlighted. Fire-and-forget wrapper over
    /// <see cref="RevealAsync"/>.</summary>
    public void Reveal(string path) => _ = RevealAsync(path);

    /// <summary>Open Explorer on the item's folder with the item highlighted. Goes through the WinRT
    /// <see cref="Launcher.LaunchFolderPathAsync(string, FolderLauncherOptions)"/> with the item in
    /// <see cref="FolderLauncherOptions.ItemsToSelect"/>, which selects reliably; the classic
    /// <c>explorer.exe /select,"path"</c> is only the fallback because on Windows 11 it often opens the folder
    /// without highlighting anything (notably when Explorer opens folders in tabs), which is what the user saw.
    /// A drive root (no parent) just opens.</summary>
    public async Task RevealAsync(string path)
    {
        try
        {
            IStorageItem item = System.IO.Directory.Exists(path)
                ? await StorageFolder.GetFolderFromPathAsync(path)
                : await StorageFile.GetFileFromPathAsync(path);
            string? parent = Path.GetDirectoryName(path);
            if (string.IsNullOrEmpty(parent))
            {
                await Launcher.LaunchFolderPathAsync(path);
                return;
            }
            var options = new FolderLauncherOptions();
            options.ItemsToSelect.Add(item);
            if (await Launcher.LaunchFolderPathAsync(parent, options))
                return;
        }
        catch
        {
            // fall through to the classic form
        }

        try
        {
            Process.Start(new ProcessStartInfo
            {
                FileName = "explorer.exe",
                Arguments = $"/select,\"{path}\"",
                UseShellExecute = true,
            });
        }
        catch
        {
            // ignore
        }
    }

    /// <summary>Set the desktop wallpaper. jpg/png/bmp are used as-is; anything else is rendered to a PNG in
    /// the app's local folder first (so exotic formats still work).</summary>
    public async Task<bool> SetWallpaperAsync(string path, ImageFormat format, Func<string, Task<bool>> renderPng)
    {
        try
        {
            string imagePath = path;
            if (!IsWallpaperFriendly(format))
            {
                string dest = Path.Combine(ApplicationData.Current.LocalFolder.Path, "wallpaper.png");
                if (!await renderPng(dest))
                    return false;
                imagePath = dest;
            }

            return await Task.Run(() => NativeMethods.SetDesktopWallpaper(imagePath));
        }
        catch
        {
            return false;
        }
    }

    // --- Rotate internals ---

    private static async Task TranscodeRotateAsync(string path, string temp, int delta)
    {
        using FileStream srcFs = File.OpenRead(path);
        using IRandomAccessStream srcRas = srcFs.AsRandomAccessStream();
        BitmapDecoder decoder = await BitmapDecoder.CreateAsync(srcRas);

        using var dstFs = new FileStream(temp, FileMode.Create, FileAccess.ReadWrite);
        using IRandomAccessStream dstRas = dstFs.AsRandomAccessStream();

        // Transcoding copies all metadata; the transform rotates the raster. Correct because a
        // (near-)orientation-1 image has no orientation tag to compose with.
        BitmapEncoder encoder = await BitmapEncoder.CreateForTranscodingAsync(dstRas, decoder);
        encoder.BitmapTransform.Rotation = ToRotation(delta);
        await encoder.FlushAsync();
    }

    private static async Task BakeRotateAsync(string path, string temp, Guid encoderId, int delta)
    {
        using FileStream srcFs = File.OpenRead(path);
        using IRandomAccessStream srcRas = srcFs.AsRandomAccessStream();
        BitmapDecoder decoder = await BitmapDecoder.CreateAsync(srcRas);

        // Get the display-oriented pixels (EXIF applied, no rotation transform to avoid pipeline-order
        // ambiguity), then rotate the buffer ourselves — unambiguous and always correct.
        PixelDataProvider provider = await decoder.GetPixelDataAsync(
            BitmapPixelFormat.Bgra8,
            BitmapAlphaMode.Premultiplied,
            new BitmapTransform(),
            ExifOrientationMode.RespectExifOrientation,
            ColorManagementMode.ColorManageToSRgb);

        byte[] oriented = provider.DetachPixelData();
        int w = (int)decoder.OrientedPixelWidth;
        int h = (int)decoder.OrientedPixelHeight;
        byte[] rotated = RotateBgra(oriented, w, h, delta, out int nw, out int nh);

        using var dstFs = new FileStream(temp, FileMode.Create, FileAccess.ReadWrite);
        using IRandomAccessStream dstRas = dstFs.AsRandomAccessStream();

        BitmapEncoder encoder = encoderId == BitmapEncoder.JpegEncoderId
            ? await BitmapEncoder.CreateAsync(encoderId, dstRas, new BitmapPropertySet
            {
                { "ImageQuality", new BitmapTypedValue(0.95, PropertyType.Single) },
            })
            : await BitmapEncoder.CreateAsync(encoderId, dstRas);

        encoder.SetPixelData(BitmapPixelFormat.Bgra8, BitmapAlphaMode.Premultiplied, (uint)nw, (uint)nh, 96, 96, rotated);
        await encoder.FlushAsync();
    }

    /// <summary>Rotate a tightly-packed BGRA8 buffer by 90/180/270° clockwise (gather form).</summary>
    private static byte[] RotateBgra(byte[] src, int w, int h, int delta, out int nw, out int nh)
    {
        var dst = new byte[src.Length];
        switch (delta)
        {
            case 180:
                nw = w; nh = h;
                for (int y = 0; y < h; y++)
                    for (int x = 0; x < w; x++)
                        Copy4(src, ((h - 1 - y) * w + (w - 1 - x)) * 4, dst, (y * w + x) * 4);
                break;

            case 270: // 90° counter-clockwise
                nw = h; nh = w;
                for (int dy = 0; dy < w; dy++)
                    for (int dx = 0; dx < h; dx++)
                        Copy4(src, (dx * w + (w - 1 - dy)) * 4, dst, (dy * nw + dx) * 4);
                break;

            default: // 90° clockwise
                nw = h; nh = w;
                for (int dy = 0; dy < w; dy++)
                    for (int dx = 0; dx < h; dx++)
                        Copy4(src, ((h - 1 - dx) * w + dy) * 4, dst, (dy * nw + dx) * 4);
                break;
        }
        return dst;
    }

    private static void Copy4(byte[] src, int si, byte[] dst, int di)
    {
        dst[di] = src[si];
        dst[di + 1] = src[si + 1];
        dst[di + 2] = src[si + 2];
        dst[di + 3] = src[si + 3];
    }

    private static BitmapRotation ToRotation(int delta) => delta switch
    {
        90 => BitmapRotation.Clockwise90Degrees,
        180 => BitmapRotation.Clockwise180Degrees,
        270 => BitmapRotation.Clockwise270Degrees,
        _ => BitmapRotation.None,
    };

    private static int ReadExifOrientation(string path)
    {
        try
        {
            IReadOnlyList<Directory> dirs = ImageMetadataReader.ReadMetadata(path);
            foreach (Directory d in dirs)
            {
                if (d is ExifIfd0Directory ifd0 && ifd0.TryGetInt32(ExifDirectoryBase.TagOrientation, out int o))
                    return o;
            }
        }
        catch
        {
            // no/broken metadata — treat as upright
        }
        return 1;
    }

    private static Guid? EncoderIdFor(string ext) => ext.ToLowerInvariant() switch
    {
        ".jpg" or ".jpeg" or ".jpe" or ".jfif" => BitmapEncoder.JpegEncoderId,
        ".png" => BitmapEncoder.PngEncoderId,
        ".tif" or ".tiff" => BitmapEncoder.TiffEncoderId,
        ".bmp" or ".dib" => BitmapEncoder.BmpEncoderId,
        _ => null,
    };

    private static void ReplaceFile(string temp, string dest)
    {
        try
        {
            File.Replace(temp, dest, null);
        }
        catch (Exception ex) when (ex is IOException or UnauthorizedAccessException or PlatformNotSupportedException)
        {
            // File.Replace can reject some filesystems (network shares, FAT) — fall back to an overwrite copy.
            File.Copy(temp, dest, overwrite: true);
        }
    }

    private static void TryDelete(string path)
    {
        try { if (File.Exists(path)) File.Delete(path); } catch { }
    }

    private static void TryFlushClipboard()
    {
        // Flush so the content survives app close; can throw for delay-rendered streams, which is harmless.
        try { Clipboard.Flush(); } catch { }
    }

    private static bool IsClipboardRaster(ImageFormat format) => format switch
    {
        ImageFormat.Jpeg or ImageFormat.Png or ImageFormat.Bmp
            or ImageFormat.Gif or ImageFormat.Tiff or ImageFormat.Ico => true,
        _ => false,
    };

    private static bool IsWallpaperFriendly(ImageFormat format) => format switch
    {
        ImageFormat.Jpeg or ImageFormat.Png or ImageFormat.Bmp => true,
        _ => false,
    };
}
