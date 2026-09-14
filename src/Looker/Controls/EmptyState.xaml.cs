using System;
using System.Collections.Generic;
using System.IO;
using System.Linq;
using System.Threading;
using System.Threading.Tasks;
using Looker.Services;
using Looker.ViewModels;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Media.Imaging;

namespace Looker.Controls;

/// <summary>One row of the recent-files list: display name, parent folder, the full path to open, and the
/// <see cref="ThumbnailItem"/> the row's picture binds to (filled asynchronously by <see cref="ThumbnailLoader"/>).</summary>
public sealed class RecentEntry
{
    public RecentEntry(string path)
    {
        Path = path;
        Name = System.IO.Path.GetFileName(path);
        Folder = System.IO.Path.GetDirectoryName(path) ?? string.Empty;
        Item = new ThumbnailItem(path, 0);
    }

    public string Path { get; }
    public string Name { get; }
    public string Folder { get; }
    public ThumbnailItem Item { get; }
}

/// <summary>
/// The empty state shown when no image is open. The two buttons raise events the host handles with the
/// same file/folder pickers as the Ctrl+O / Ctrl+Shift+O accelerators; the default-apps hint opens
/// Settings on Looker's Default apps page via <see cref="DefaultAppsService"/>. The recent list is
/// pushed in by the host (<see cref="SetRecent"/>) and filtered to files that still exist.
/// </summary>
public sealed partial class EmptyState : UserControl
{
    public event EventHandler? OpenPhotoRequested;
    public event EventHandler? OpenFolderRequested;
    public event EventHandler<string>? RecentRequested;
    public event EventHandler? ClearRecentRequested;
    /// <summary>Right-click "Remove from recent" on one entry.</summary>
    public event EventHandler<string>? RemoveRecentRequested;
    /// <summary>The X next to the default-app button: hide it forever.</summary>
    public event EventHandler? DefaultHintDismissed;

    // Wordmark viewBox is 3216x819; rasterize it for the actual DPI so it stays crisp.
    private const double WordmarkAspect = 3216.0 / 819.0;
    private const double RecentThumbWidthDips = 48;

    public EmptyState()
    {
        InitializeComponent();
        SizeChanged += (_, _) => UpdateContentCap();
        Loaded += (_, _) =>
        {
            double scale = XamlRoot?.RasterizationScale ?? 1.0;
            Wordmark.Source = new SvgImageSource(new Uri("ms-appx:///Assets/Brand/looker_wordmark.svg"))
            {
                RasterizePixelHeight = Wordmark.Height * scale,
                RasterizePixelWidth = Wordmark.Height * WordmarkAspect * scale,
            };
        };
    }

    private int _recentVersion;
    private CancellationTokenSource? _thumbCts;

    /// <summary>Show the given recent paths (most recent first); entries whose file is gone are dropped.
    /// An empty result hides the whole section. The existence checks run off the UI thread (they can stall
    /// on a sleeping drive or a cloud placeholder); the newest call wins if several overlap.</summary>
    public void SetRecent(IEnumerable<string> paths)
    {
        string[] snapshot = paths.ToArray();
        int version = ++_recentVersion;
        _ = ApplyRecentAsync(snapshot, version);
    }

    private async Task ApplyRecentAsync(string[] paths, int version)
    {
        List<RecentEntry> entries = await Task.Run(() =>
        {
            var list = new List<RecentEntry>(paths.Length);
            foreach (string path in paths)
            {
                if (File.Exists(path))
                    list.Add(new RecentEntry(path));
            }
            return list;
        });

        if (version != _recentVersion)
            return; // superseded
        RecentList.ItemsSource = entries;
        RecentPanel.Visibility = entries.Count > 0 ? Visibility.Visible : Visibility.Collapsed;

        // Thumbnails land one by one through ThumbnailItem.PropertyChanged; a newer list cancels the old loads.
        _thumbCts?.Cancel();
        _thumbCts = new CancellationTokenSource();
        CancellationToken ct = _thumbCts.Token;
        // The DPI may not be known yet (the control is built before the window shows), so request at least 2x.
        double scale = Math.Max(XamlRoot?.RasterizationScale ?? 1.0, 2.0);
        uint pixelSize = (uint)Math.Ceiling(RecentThumbWidthDips * scale);
        foreach (RecentEntry entry in entries)
            _ = LoadThumbnailAsync(entry, pixelSize, ct);
    }

    private static async Task LoadThumbnailAsync(RecentEntry entry, uint pixelSize, CancellationToken ct)
    {
        try
        {
            await ThumbnailLoader.LoadAsync(entry.Item, pixelSize, ct);
        }
        catch (OperationCanceledException)
        {
        }
        catch
        {
            // undecodable / unreachable: the row keeps its blank tile
        }
    }

    private void OnOpenPhoto(object sender, RoutedEventArgs e)
        => OpenPhotoRequested?.Invoke(this, EventArgs.Empty);

    private void OnOpenFolder(object sender, RoutedEventArgs e)
        => OpenFolderRequested?.Invoke(this, EventArgs.Empty);

    private void OnRecentClick(object sender, RoutedEventArgs e)
    {
        if (sender is Button { Tag: string path })
            RecentRequested?.Invoke(this, path);
    }

    private void OnClearRecent(object sender, RoutedEventArgs e)
        => ClearRecentRequested?.Invoke(this, EventArgs.Empty);

    private void OnRemoveRecent(object sender, RoutedEventArgs e)
    {
        if (sender is MenuFlyoutItem { Tag: string path })
            RemoveRecentRequested?.Invoke(this, path);
    }

    public void SetDefaultHintVisible(bool visible)
    {
        DefaultHintPanel.Visibility = visible ? Visibility.Visible : Visibility.Collapsed;
        UpdateContentCap();
    }

    /// <summary>Cap the centred block to the space left after the bottom row so it scrolls in short windows
    /// (an Auto grid row would let it grow past the page and be clipped).</summary>
    private void UpdateContentCap()
    {
        if (ActualHeight <= 0)
            return;
        double hint = 0;
        if (DefaultHintPanel.Visibility == Visibility.Visible)
        {
            DefaultHintPanel.Measure(new Windows.Foundation.Size(double.PositiveInfinity, double.PositiveInfinity));
            hint = DefaultHintPanel.DesiredSize.Height; // includes its margins
        }
        ContentScroller.MaxHeight = Math.Max(0, ActualHeight - hint);
    }

    private void OnDismissDefaultHint(object sender, RoutedEventArgs e)
    {
        SetDefaultHintVisible(false);
        DefaultHintDismissed?.Invoke(this, EventArgs.Empty);
    }

    private void OnSetDefault(object sender, RoutedEventArgs e)
        => _ = DefaultAppsService.OpenAsync();
}
