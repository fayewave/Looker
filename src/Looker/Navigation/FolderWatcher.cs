using System;
using System.IO;
using Microsoft.UI.Dispatching;

namespace Looker.Navigation;

/// <summary>
/// Watches the open folder and republishes create/delete/rename/modify events on the UI thread so
/// all list mutations happen on one queue (plan: "list mutations through one UI-thread queue").
/// Every file event is passed through, unsupported files included: <see cref="FolderContext"/> keeps those
/// out of the nav list but counts them for the "3 / 128" position counter (a rename is folded there into a
/// delete + create, which covers one crossing the supported/unsupported boundary). The raw <see cref="FileSystemWatcher"/> raises on a
/// threadpool thread, hence the <see cref="DispatcherQueue"/> marshal.
/// </summary>
public sealed class FolderWatcher : IDisposable
{
    private readonly DispatcherQueue _dispatcher;
    private FileSystemWatcher? _watcher;

    public FolderWatcher(DispatcherQueue dispatcher) => _dispatcher = dispatcher;

    public event Action<string>? Created;
    public event Action<string>? Deleted;
    public event Action<string>? Modified;
    public event Action<string, string>? Renamed; // (oldPath, newPath)

    /// <summary>Start watching <paramref name="folder"/> (stops any previous watch). Null/missing clears.</summary>
    public void Watch(string? folder)
    {
        Stop();
        if (string.IsNullOrEmpty(folder) || !Directory.Exists(folder))
            return;

        try
        {
            _watcher = new FileSystemWatcher(folder)
            {
                NotifyFilter = NotifyFilters.FileName | NotifyFilters.LastWrite | NotifyFilters.Size,
                IncludeSubdirectories = false,
            };
            _watcher.Created += OnCreated;
            _watcher.Deleted += OnDeleted;
            _watcher.Renamed += OnRenamed;
            _watcher.Changed += OnChanged;
            _watcher.EnableRaisingEvents = true;
        }
        catch (Exception ex) when (ex is IOException or UnauthorizedAccessException or ArgumentException)
        {
            Stop(); // watching is best-effort; the viewer still works without live updates
        }
    }

    private void OnCreated(object sender, FileSystemEventArgs e)
        => Marshal(() => Created?.Invoke(e.FullPath));

    private void OnDeleted(object sender, FileSystemEventArgs e)
        => Marshal(() => Deleted?.Invoke(e.FullPath));

    private void OnChanged(object sender, FileSystemEventArgs e)
    {
        if (e.ChangeType == WatcherChangeTypes.Changed)
            Marshal(() => Modified?.Invoke(e.FullPath));
    }

    private void OnRenamed(object sender, RenamedEventArgs e)
        => Marshal(() => Renamed?.Invoke(e.OldFullPath, e.FullPath));

    private void Marshal(Action action)
    {
        if (!_dispatcher.TryEnqueue(() => action()))
        {
            // Queue is shutting down (window closing) — safe to drop.
        }
    }

    public void Stop()
    {
        if (_watcher is null)
            return;

        _watcher.EnableRaisingEvents = false;
        _watcher.Created -= OnCreated;
        _watcher.Deleted -= OnDeleted;
        _watcher.Renamed -= OnRenamed;
        _watcher.Changed -= OnChanged;
        _watcher.Dispose();
        _watcher = null;
    }

    public void Dispose() => Stop();
}
