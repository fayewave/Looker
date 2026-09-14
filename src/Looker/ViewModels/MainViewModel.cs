using System;
using System.Collections.Generic;
using System.IO;
using System.Threading;
using System.Threading.Tasks;
using Looker.Controls;
using Looker.Imaging;
using Looker.Navigation;
using Looker.Services;
using Microsoft.UI.Dispatching;

namespace Looker.ViewModels;

/// <summary>
/// Orchestrator for the viewer loop: owns the <see cref="FolderContext"/> and drives the
/// <see cref="ImageViewport"/> imperatively. Each navigation hands the viewport the current image
/// plus a travel-direction-biased list of neighbours to preload, so next/prev is warm before the
/// key is pressed. Also publishes the <see cref="Thumbnails"/> collection for the strip, applies the
/// persisted sort, and folds live folder changes into both the nav list and the strip.
/// </summary>
public sealed class MainViewModel
{
    // Sharp (screen-res) preload: the immediate neighbours in travel direction, so steady
    // arrowing lands on an already-crisp image.
    private static readonly int[] HighForward = { +1, +2, -1 };
    private static readonly int[] HighBackward = { -1, -2, +1 };

    // Low-res preload: a wide, cheap ring (~0.5 MP each) so jumping to — or outrunning the sharp
    // preloader toward — any nearby image still shows something instantly, regardless of file size.
    private static readonly int[] LowForward = { +1, +2, +3, +4, +5, -1, -2, -3 };
    private static readonly int[] LowBackward = { -1, -2, -3, -4, -5, +1, +2, +3 };

    private readonly ImageViewport _viewport;
    private readonly SettingsService _settings;
    private readonly FolderContext _folder = new();
    private readonly FolderWatcher _watcher;
    private readonly FileOperationsService _fileOps = new();
    private SortMode _sort;
    private bool _forward = true;

    private bool _infoVisible;
    private CancellationTokenSource? _infoCts;

    /// <summary>Raised with the window/title-bar caption, e.g. "beach.jpg — 3 / 128".</summary>
    public event Action<string>? TitleChanged;

    /// <summary>Raised with the current index (or -1 when nothing is shown) so the strip can sync
    /// its selection. Fired after every navigation and live list change.</summary>
    public event Action<int>? CurrentChanged;

    /// <summary>Raised (on the UI thread) with a fresh EXIF/histogram snapshot for the info panel, or null
    /// when nothing is shown or the panel was just hidden. Only fires while <see cref="InfoVisible"/>.</summary>
    public event Action<InfoSnapshot?>? InfoUpdated;

    /// <summary>Raised with true when an image is on screen and false when nothing is (folder emptied,
    /// nothing opened yet) so the host can show/hide the empty state.</summary>
    public event Action<bool>? ContentPresenceChanged;

    /// <summary>Raised with a short line the host should flash in the OSD (nav position, sort change,
    /// "Copied", …).</summary>
    public event Action<string>? OsdRequested;

    /// <summary>Raised (on the UI thread) for the always-on status row under the strip: native size, file size
    /// and capture time of the current image, or null when nothing is shown. Fires once as soon as the image
    /// is up and again when the EXIF date has been read.</summary>
    public event Action<StatusInfo?>? StatusUpdated;
    private CancellationTokenSource? _statusCts;
    private string? _statusPath; // what the status row currently describes (re-raised when its frame lands)
    private long _statusSize;
    private DateTime? _statusTaken;
    private DateTime? _statusModified;

    private bool _hasContent;

    /// <summary>The filmstrip's items, index-aligned with the folder's nav order.</summary>
    public RangeObservableCollection<ThumbnailItem> Thumbnails { get; } = new();

    public SortMode Sort => _sort;

    /// <summary>The current image's index in the nav list (-1 when nothing is open).</summary>
    public int CurrentIndex => _folder.CurrentIndex;

    /// <summary>Number of images in the current folder.</summary>
    public int Count => _folder.Count;

    /// <summary>The path of the currently displayed file, or null when nothing is open.</summary>
    public string? CurrentPath => _folder.CurrentPath;

    /// <summary>The folder being browsed, or null when nothing is open.</summary>
    public string? FolderPath => _folder.FolderPath;

    /// <summary>Human position for the OSD, e.g. "3 / 128" (empty when no folder is loaded).</summary>
    public string PositionLabel => _folder.PositionLabel;

    /// <summary>Position counter text for any file in the open folder (see <see cref="FolderContext.PositionLabelFor"/>).</summary>
    public string PositionLabelFor(string path) => _folder.PositionLabelFor(path);

    public MainViewModel(ImageViewport viewport, SettingsService settings)
    {
        _viewport = viewport;
        _settings = settings;
        _sort = settings.Sort;
        _infoVisible = settings.InfoVisible;
        _viewport.SetCacheBudgetBytes((long)settings.CacheBudgetMB * 1024 * 1024);

        _watcher = new FolderWatcher(DispatcherQueue.GetForCurrentThread());
        _watcher.Created += OnFileCreated;
        _watcher.Deleted += OnFileDeleted;
        _watcher.Renamed += OnFileRenamed;
        _watcher.Modified += OnFileModified;
        _viewport.DisplayChanged += OnDisplayChanged;
        _viewport.PageChanged += OnPageChanged;
    }

    public async Task OpenFileAsync(string path)
    {
        // Start decoding at once and enumerate the folder alongside it (not after it, as before) so nav
        // lights up as early as possible. The re-show below finds that decode still in flight for the same
        // ref and only refreshes the preload window around it.
        var initial = new ImageRef(path, SafeModifiedTicks(path));
        _ = _viewport.ShowAsync(initial, Array.Empty<PreloadItem>());
        SetContentPresence(true);
        ScheduleStatusRefresh(path);
        TitleChanged?.Invoke(Path.GetFileName(path));
        _settings.PushRecent(path);

        await _folder.LoadFileAsync(path, _sort);
        OnFolderLoaded();
        await ShowCurrentAsync();
    }

    /// <summary>Home: drop the folder and the image and return to the landing page.</summary>
    public async Task CloseAsync()
    {
        _folder.Clear();
        OnFolderLoaded(); // empties the strip, stops watching
        await ShowCurrentAsync(); // null ref: viewport clears, presence goes false ⇒ empty state shows
    }

    public async Task OpenFolderAsync(string folderPath)
    {
        await _folder.LoadFolderAsync(folderPath, _sort);
        OnFolderLoaded();
        await ShowCurrentAsync();
    }

    /// <summary>Show a file (file explorer click): a jump within the open folder when it is there, otherwise a
    /// full open of the file and its folder. <paramref name="reshow"/> re-announces the current image (title,
    /// <see cref="CurrentChanged"/>, status) instead of doing nothing when it is the one asked for — the host
    /// uses that to take down the unsupported-file placeholder that was covering it.</summary>
    public Task ShowFileAsync(string path, bool reshow = false)
    {
        int index = _folder.IndexOf(path);
        if (index < 0)
            return OpenFileAsync(path);
        if (index == _folder.CurrentIndex)
            return reshow ? ShowCurrentAsync() : Task.CompletedTask;
        return GoToIndexAsync(index);
    }

    /// <summary>Navigate to an absolute index (thumbnail click).</summary>
    public async Task GoToIndexAsync(int index)
    {
        if (index == _folder.CurrentIndex)
            return;
        _forward = index >= _folder.CurrentIndex;
        if (_folder.MoveTo(index))
        {
            await ShowCurrentAsync();
        }
    }

    /// <summary>Apply and persist a new sort; the current file stays selected under the new order.</summary>
    public async Task SetSortAsync(SortMode sort)
    {
        if (sort == _sort)
            return;
        _sort = sort;
        _settings.Sort = sort;
        _folder.SetSort(sort);
        RebuildThumbnails();
        await ShowCurrentAsync();
        OsdRequested?.Invoke(DescribeSort(sort));
    }

    private static string DescribeSort(SortMode sort)
    {
        string field = sort.Field switch
        {
            SortField.DateModified => "Date modified",
            SortField.Size => "Size",
            _ => "Name",
        };
        string arrow = sort.Direction == SortDirection.Ascending ? "↑" : "↓";
        return $"Sort: {field} {arrow}";
    }

    private void OnFolderLoaded()
    {
        _forward = true;
        RebuildThumbnails();
        _watcher.Watch(_folder.FolderPath);
    }

    private void RebuildThumbnails()
    {
        var items = new List<ThumbnailItem>(_folder.Count);
        for (int i = 0; i < _folder.Count; i++)
        {
            if (_folder.RefAt(i) is { } reference)
                items.Add(new ThumbnailItem(reference.Path, reference.ModifiedTicks));
        }
        Thumbnails.Reset(items);
    }

    // --- Live folder changes (marshaled to the UI thread by FolderWatcher) ---

    private void OnFileCreated(string path)
    {
        FolderMutation change = _folder.OnFileCreated(path);
        if (change.CountChanged)
            NotifyPosition(); // an unsupported file: nothing for the strip, but the "3 / 128" total moved
        if (!change.Changed)
            return;
        if (_folder.RefAt(change.Index) is { } reference)
            Thumbnails.Insert(change.Index, new ThumbnailItem(reference.Path, reference.ModifiedTicks));
        NotifyPosition();
    }

    private async void OnFileDeleted(string path) => await ApplyRemovalAsync(path);

    /// <summary>Fold a deletion (external or app-initiated) into the nav list + strip. Idempotent: a path
    /// already gone returns <see cref="FolderMutation.None"/>, so the watcher event that follows our own
    /// delete is a harmless no-op.</summary>
    private async Task ApplyRemovalAsync(string path)
    {
        FolderMutation change = _folder.OnFileDeleted(path);
        if (change.CountChanged)
            NotifyPosition();
        if (!change.Changed)
            return;
        if (change.Index < Thumbnails.Count)
            Thumbnails.RemoveAt(change.Index);

        if (change.AffectsCurrent)
            await ShowCurrentAsync(); // the current file vanished — show whatever slid into its place
        else
            NotifyPosition();
    }

    private void OnFileRenamed(string oldPath, string newPath) => ApplyRename(oldPath, newPath);

    private void ApplyRename(string oldPath, string newPath)
    {
        bool wasCurrent = string.Equals(_folder.CurrentPath, oldPath, StringComparison.OrdinalIgnoreCase);

        FolderMutation removed = _folder.OnFileDeleted(oldPath);
        if (removed.Changed && removed.Index < Thumbnails.Count)
            Thumbnails.RemoveAt(removed.Index);

        FolderMutation added = _folder.OnFileCreated(newPath);
        if (added.Changed && _folder.RefAt(added.Index) is { } reference)
            Thumbnails.Insert(added.Index, new ThumbnailItem(reference.Path, reference.ModifiedTicks));

        // A rename leaves the pixels on screen untouched; just keep the selection on the same file.
        if (wasCurrent && added.Changed)
            _folder.MoveTo(added.Index);

        NotifyPosition();
    }

    private void OnFileModified(string path)
    {
        FolderMutation change = _folder.OnFileModified(path);
        if (!change.Changed)
            return;
        if (change.Index >= 0 && change.Index < Thumbnails.Count && _folder.RefAt(change.Index) is { } reference)
            Thumbnails[change.Index].Invalidate(reference.ModifiedTicks);
    }

    private void NotifyPosition()
    {
        UpdateTitle();
        CurrentChanged?.Invoke(_folder.CurrentIndex);
    }

    public Task NextAsync() => MoveAsync(_folder.MoveNext, forward: true);

    public Task PreviousAsync() => MoveAsync(_folder.MovePrevious, forward: false);

    public Task FirstAsync() => MoveAsync(_folder.MoveFirst, forward: true);

    public Task LastAsync() => MoveAsync(_folder.MoveLast, forward: false);

    private async Task MoveAsync(Func<bool> move, bool forward)
    {
        _forward = forward;
        if (move())
        {
            await ShowCurrentAsync();
        }
    }

    /// <summary>Everything the host shows about the position (strip highlight and scroll, title, counter,
    /// status row) moves on the keypress itself; the viewport then loads the image behind it. Anything derived
    /// from the pixels (native size, info panel) follows via <see cref="OnDisplayChanged"/> as frames land,
    /// so a slow decode never holds the strip back.</summary>
    private async Task ShowCurrentAsync()
    {
        ImageRef? current = _folder.CurrentRef;
        SetContentPresence(current is not null);
        UpdateTitle();
        CurrentChanged?.Invoke(_folder.CurrentIndex);
        if (_folder.CurrentPath is { } shown)
            _settings.PushRecent(shown); // browsing counts as "recent", not only explicit opens

        Task firstFrame = _viewport.ShowAsync(current, BuildPreloadWindow());
        ScheduleStatusRefresh(_folder.CurrentPath); // after ShowAsync: a cached image is already up ⇒ size known
        await firstFrame;
    }

    /// <summary>A frame of the current image landed (placeholder, sharp or an upgrade), or the viewport
    /// cleared/failed: fill in the native size and refresh the info panel from what is actually on screen.</summary>
    private void OnDisplayChanged()
    {
        if (_statusPath is not null)
            RaiseStatus();
        ScheduleInfoRefresh();
    }

    // Turning a PDF page changes what the status row and info card describe (that page's size and pixels).
    private void OnPageChanged() => OnDisplayChanged();

    private void SetContentPresence(bool present)
    {
        if (_hasContent == present)
            return;
        _hasContent = present;
        ContentPresenceChanged?.Invoke(present);
    }

    // --- File management commands (M7); each is a no-op when nothing is open ---

    /// <summary>Recycle the current file (no confirm) and advance. The later watcher event is idempotent.</summary>
    public Task DeleteCurrentAsync() => _folder.CurrentPath is { } path ? DeleteAsync(path) : Task.CompletedTask;

    /// <summary>Recycle any file (no confirm) — the file explorer's context menu reaches files outside the open
    /// folder. One in the open folder is folded into the nav list at once (advancing if it was current); the
    /// explorer picks the change up from its own watcher.</summary>
    public async Task DeleteAsync(string path)
    {
        if (await _fileOps.RecycleAsync(path))
            await ApplyRemovalAsync(path); // a path not in the list is a no-op
    }

    /// <summary>Rename the current file, keeping it selected. No-op on collision/invalid name/failure.</summary>
    public Task RenameCurrentAsync(string newName)
        => _folder.CurrentPath is { } path ? RenameAsync(path, newName) : Task.CompletedTask;

    /// <summary>Rename any file (see <see cref="DeleteAsync"/>). Only a file in the open folder touches the nav
    /// list: <see cref="ApplyRename"/> would otherwise insert a foreign folder's file into it.</summary>
    public async Task RenameAsync(string path, string newName)
    {
        string? newPath = await _fileOps.RenameAsync(path, newName);
        if (newPath is null || string.Equals(newPath, path, StringComparison.OrdinalIgnoreCase))
            return;
        if (IsInOpenFolder(path))
            ApplyRename(path, newPath);
    }

    private bool IsInOpenFolder(string path)
        => _folder.FolderPath is { } folder
           && string.Equals(Path.TrimEndingDirectorySeparator(Path.GetDirectoryName(path) ?? string.Empty),
                            Path.TrimEndingDirectorySeparator(folder), StringComparison.OrdinalIgnoreCase);

    /// <summary>Rotate the on-screen image 90° (right = clockwise) as a non-destructive preview. The file is
    /// left untouched until <see cref="SaveRotationAsync"/>, so rotating no longer re-sorts the folder or makes
    /// the current image jump out from under the user.</summary>
    public void RotateCurrent(bool clockwise) => _viewport.PreviewRotate(clockwise);

    /// <summary>Commit the current rotation preview to the file, then re-decode so the saved orientation shows.
    /// No-op when there's no preview; ignored (with an OSD) for formats that can't be re-encoded.</summary>
    public async Task SaveRotationAsync()
    {
        string? path = _folder.CurrentPath;
        int turns = _viewport.PreviewQuarterTurns;
        if (path is null || turns == 0)
            return;

        if (!await _fileOps.RotateAsync(path, turns * 90))
        {
            OsdRequested?.Invoke("Couldn't save rotation");
            return;
        }

        _viewport.ClearRotationPreview();

        // The file's mtime changed under it: refresh the list entry (new cache key) and invalidate the
        // strip thumbnail, then re-decode. The watcher's later Modified event finds nothing new → no-op.
        FolderMutation change = _folder.OnFileModified(path);
        if (change.Changed && change.Index >= 0 && change.Index < Thumbnails.Count && _folder.RefAt(change.Index) is { } reference)
            Thumbnails[change.Index].Invalidate(reference.ModifiedTicks);
        await ShowCurrentAsync();
        OsdRequested?.Invoke("Rotation saved");
    }

    /// <summary>Copy the current image to the clipboard (file + bitmap for paste-into-Paint).</summary>
    public async Task CopyImageAsync()
    {
        string? path = _folder.CurrentPath;
        if (path is null)
            return;
        await _fileOps.CopyImageAsync(path, _viewport.CurrentFormat ?? ImageFormat.Unknown, _viewport.SaveCurrentPngAsync);
        OsdRequested?.Invoke("Copied");
    }

    /// <summary>Copy the current file's full path as text.</summary>
    public void CopyPath()
    {
        if (_folder.CurrentPath is { } path)
            CopyPath(path);
    }

    /// <summary>Copy any path as text (file explorer context menu).</summary>
    public void CopyPath(string path)
    {
        _fileOps.CopyPath(path);
        OsdRequested?.Invoke("Copied path");
    }

    /// <summary>Open Explorer with the current file selected.</summary>
    public void Reveal()
    {
        if (_folder.CurrentPath is { } path)
            Reveal(path);
    }

    /// <summary>Open Explorer with any file or folder selected (file explorer context menu).</summary>
    public void Reveal(string path) => _fileOps.Reveal(path);

    /// <summary>Set the current image as the desktop wallpaper.</summary>
    public async Task SetWallpaperAsync()
    {
        string? path = _folder.CurrentPath;
        if (path is null)
            return;
        if (await _fileOps.SetWallpaperAsync(path, _viewport.CurrentFormat ?? ImageFormat.Unknown, _viewport.SaveCurrentPngAsync))
            OsdRequested?.Invoke("Wallpaper set");
    }

    // --- Info panel (EXIF + histogram) ---

    /// <summary>Whether the info panel is showing. Setting it persists the choice and, when turned on,
    /// kicks a refresh; when turned off, cancels any pending read and clears the panel.</summary>
    public bool InfoVisible
    {
        get => _infoVisible;
        set
        {
            if (_infoVisible == value)
                return;
            _infoVisible = value;
            _settings.InfoVisible = value;
            if (value)
                ScheduleInfoRefresh();
            else
            {
                _infoCts?.Cancel();
                InfoUpdated?.Invoke(null);
            }
        }
    }

    /// <summary>Debounced EXIF read + histogram compute for the current image, fired only while the panel is
    /// visible. The short delay both coalesces rapid navigation and lets the sharp decode land so the
    /// histogram reflects the full-resolution buffer. Continuation returns to the UI thread (WinUI sync
    /// context), where <see cref="InfoUpdated"/> is raised.</summary>
    private async void ScheduleInfoRefresh()
    {
        if (!_infoVisible)
            return;

        _infoCts?.Cancel();
        var cts = new CancellationTokenSource();
        _infoCts = cts;
        CancellationToken ct = cts.Token;

        try
        {
            await Task.Delay(140, ct);

            string? path = _folder.CurrentPath;
            if (path is null)
            {
                InfoUpdated?.Invoke(null);
                return;
            }

            // Histogram uses the on-screen (downscaled) buffer; the reported Dimensions must be the file's
            // TRUE native size, not that buffer — otherwise a 20 MP photo displays as its ~2 MP decode.
            byte[]? pixels = null;
            int displayW = 0, displayH = 0;
            if (_viewport.TryGetDisplayPixels(out byte[] px, out int pw, out int ph))
            {
                pixels = px;
                displayW = pw;
                displayH = ph;
            }

            int nativeW = displayW, nativeH = displayH;
            if (_viewport.CurrentNativeSize is { } native)
            {
                nativeW = (int)native.Width;
                nativeH = (int)native.Height;
            }

            long sizeBytes = SafeSize(path);
            IReadOnlyList<MetadataGroup> groups = await MetadataService.ReadAsync(path, nativeW, nativeH, sizeBytes, ct);
            HistogramData? histogram = pixels is null
                ? null
                : await Task.Run(() => MetadataService.ComputeHistogram(pixels, displayW, displayH), ct);

            if (ct.IsCancellationRequested)
                return;
            Looker.Helpers.StartupTrace.Mark($"info panel: {groups.Count} metadata groups, histogram={(histogram is not null)}");
            InfoUpdated?.Invoke(new InfoSnapshot(groups, histogram));
        }
        catch (OperationCanceledException)
        {
            // superseded by a newer refresh or the panel closed
        }
        catch (Exception ex)
        {
            Looker.Helpers.StartupTrace.Mark($"info panel FAILED: {ex.GetType().Name}: {ex.Message}");
            InfoUpdated?.Invoke(null);
        }
    }

    /// <summary>Feed the status row under the strip (always on, unlike the info panel). Native size and file
    /// size are known synchronously once the image is showing; the EXIF capture time follows from a header
    /// parse off the UI thread. Continuations return to the UI thread (WinUI sync context).</summary>
    private async void ScheduleStatusRefresh(string? path)
    {
        _statusCts?.Cancel();
        _statusPath = path;
        if (path is null)
        {
            StatusUpdated?.Invoke(null);
            return;
        }

        var cts = new CancellationTokenSource();
        _statusCts = cts;
        _statusSize = SafeSize(path);
        _statusModified = SafeModified(path);
        _statusTaken = null;
        RaiseStatus();

        try
        {
            DateTime? taken = await MetadataService.ReadDateTakenAsync(path, cts.Token);
            if (!cts.IsCancellationRequested)
            {
                _statusTaken = taken;
                RaiseStatus(); // native size re-read: the sharp decode may have landed
            }
        }
        catch (OperationCanceledException)
        {
            // superseded by a newer navigation
        }
        catch
        {
            // no metadata is not an error; the first raise already showed what we know
        }
    }

    private void RaiseStatus()
    {
        int w = 0, h = 0;
        if (_viewport.CurrentNativeSize is { } native) // null until a frame of *this* image is up
        {
            w = (int)native.Width;
            h = (int)native.Height;
        }
        // Type and native size are known only once a frame of this image is up (CurrentFormat/CurrentNativeSize
        // are null until then); OnDisplayChanged re-raises to fill them in.
        string? type = _viewport.CurrentFormat is { } format ? ImageFormatNames.DisplayName(format, _statusPath) : null;
        StatusUpdated?.Invoke(new StatusInfo(type, w, h, _statusSize, _statusTaken, _statusModified, _viewport.PageCount));
    }

    private static DateTime? SafeModified(string path)
    {
        try { return File.GetLastWriteTime(path); }
        catch { return null; }
    }

    private IReadOnlyList<PreloadItem> BuildPreloadWindow()
    {
        if (_folder.Count <= 1)
            return Array.Empty<PreloadItem>();

        int current = _folder.CurrentIndex;
        int[] high = _forward ? HighForward : HighBackward;
        int[] low = _forward ? LowForward : LowBackward;

        var items = new List<PreloadItem>();
        var seenHigh = new HashSet<int>();
        var seenLow = new HashSet<int>();

        void AddHigh(int offset)
        {
            int index = _folder.WrapIndex(current + offset);
            if (index < 0 || index == current || !seenHigh.Add(index))
                return;
            if (_folder.RefAt(index) is { } reference)
                items.Add(new PreloadItem(reference, PreloadTier.High));
        }

        void AddLow(int offset)
        {
            int index = _folder.WrapIndex(current + offset);
            if (index < 0 || index == current || !seenLow.Add(index))
                return;
            if (_folder.RefAt(index) is { } reference)
                items.Add(new PreloadItem(reference, PreloadTier.Low));
        }

        // Priority order: sharpen the immediate next, then queue its fast low-res fallback,
        // then the rest of the sharp neighbours, then the wide low-res ring.
        if (high.Length > 0) AddHigh(high[0]);
        if (low.Length > 0) AddLow(low[0]);
        for (int k = 1; k < high.Length; k++) AddHigh(high[k]);
        for (int k = 1; k < low.Length; k++) AddLow(low[k]);

        return items;
    }

    private void UpdateTitle()
    {
        string? path = _folder.CurrentPath;
        if (path is null)
        {
            TitleChanged?.Invoke("Looker");
            return;
        }

        // Filename only — the "3 / 128" position lives in the OSD (on navigation), not the title bar.
        TitleChanged?.Invoke(Path.GetFileName(path));
    }

    private static long SafeModifiedTicks(string path)
    {
        try { return File.GetLastWriteTimeUtc(path).Ticks; }
        catch { return 0; }
    }

    private static long SafeSize(string path)
    {
        try { return new FileInfo(path).Length; }
        catch { return 0; }
    }
}
