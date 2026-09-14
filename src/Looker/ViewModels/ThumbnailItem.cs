using System.ComponentModel;
using System.IO;
using Microsoft.UI.Xaml.Media;

namespace Looker.ViewModels;

/// <summary>
/// One cell in the thumbnail strip. The <see cref="Thumbnail"/> is filled lazily by the strip's
/// phased loader (only for realized containers) and cached on the item so scrolling back doesn't
/// re-fetch. Notifies so the compiled binding updates when the image arrives or is invalidated.
/// </summary>
public sealed class ThumbnailItem : INotifyPropertyChanged
{
    public string Path { get; }
    public string FileName { get; }
    public long ModifiedTicks { get; private set; }

    private ImageSource? _thumbnail;

    public ThumbnailItem(string path, long modifiedTicks)
    {
        Path = path;
        FileName = System.IO.Path.GetFileName(path);
        ModifiedTicks = modifiedTicks;
    }

    public ImageSource? Thumbnail
    {
        get => _thumbnail;
        set
        {
            if (ReferenceEquals(_thumbnail, value))
                return;
            _thumbnail = value;
            PropertyChanged?.Invoke(this, ThumbnailChangedArgs);
        }
    }

    /// <summary>The file changed on disk: drop the cached image so the strip reloads it, and bump the
    /// mtime the loader keys against.</summary>
    public void Invalidate(long modifiedTicks)
    {
        ModifiedTicks = modifiedTicks;
        Thumbnail = null;
    }

    private static readonly PropertyChangedEventArgs ThumbnailChangedArgs = new(nameof(Thumbnail));
    public event PropertyChangedEventHandler? PropertyChanged;
}
