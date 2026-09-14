using System;
using System.Collections.Generic;
using System.IO;
using System.Threading;
using System.Threading.Tasks;
using Looker.Navigation;
using Looker.ViewModels;
using Microsoft.UI;
using Microsoft.UI.Dispatching;
using Microsoft.UI.Input;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Input;
using Microsoft.UI.Xaml.Media;
using Windows.UI;

namespace Looker.Controls;

/// <summary>A context-menu action the explorer asks its host to perform on a path.</summary>
public enum ExplorerAction
{
    /// <summary>Open the file (or the folder as the browsing folder).</summary>
    Open,
    Rename,
    Delete,
    CopyPath,
    Reveal,
}

/// <summary>
/// The file explorer card (left side of the viewport). Works like an Explorer window: a flat listing of one
/// folder (sub-folders first, then images with thumbnails and other files greyed out), named by the breadcrumb
/// trail. That folder is the open photo's folder until the user navigates: double-clicking a folder lists it
/// instead, a crumb lists an ancestor, and the photo on screen stays put meanwhile. Opening a photo from a
/// different folder moves the explorer into that folder. Every folder listed goes into a browser-style history
/// that the header's Back / Forward buttons (and, through the host, mouse buttons 4 / 5) walk:
/// <see cref="GoBack"/> / <see cref="GoForward"/>. The host calls <see cref="ShowAsync"/> on every navigation
/// (cheap within one folder: it only moves the highlight), <see cref="SetSort"/> when the toolbar sort changes,
/// and listens to <see cref="FileActivated"/> / <see cref="ActionRequested"/>.
/// <para>Single click: an image opens, a folder takes the keyboard cursor. Double click: a folder is listed.
/// Right click: file actions ("Open folder" opens a folder in the viewer as the browsing folder). Up/Down
/// (routed by the host to <see cref="MoveCursor"/>) walk the rows, opening images on the way; Enter
/// (<see cref="ActivateCursor"/>) lists the folder under the cursor. The trail starts at "This PC", one step
/// above the drives, so another drive is two clicks away.</para>
/// <para>The listed folder is watched with a <see cref="FileSystemWatcher"/> so files and folders appearing,
/// vanishing, renaming or changing on disk are folded in live; navigating away drops the listing and the
/// watcher. Thumbnails load through the strip's <see cref="ThumbnailLoader"/> with the same debounce and
/// release-on-recycle discipline. The model (<see cref="ExplorerTree"/>) can hold a deeper tree; this control
/// only ever expands its root.</para>
/// <para>Resizable like the info card: an 8px grip straddles the RIGHT border, drag sets
/// <see cref="PanelWidth"/>, double-tap resets it, and <see cref="PanelWidthChanged"/> fires at gesture end
/// for the host to persist.</para>
/// </summary>
public sealed partial class FileExplorer : UserControl
{
    /// <summary>Card fill = the themed window colour (see InfoPanel.SetCardBackground).</summary>
    public void SetCardBackground(Color color) => Card.Background = new SolidColorBrush(color);

    /// <summary>Default and bounds of <see cref="PanelWidth"/> in DIPs; the range mirrors
    /// <c>SettingsService.ExplorerWidth</c>.</summary>
    public const double DefaultPanelWidth = 320;
    public const double MinPanelWidth = 260;
    public const double MaxPanelWidth = 640;

    // Row geometry: the DataTemplate's Border height and the StackLayout spacing (both must match the XAML),
    // from which the scroll offset of any row index follows without measuring.
    private const double RowHeight = 28;
    private const double RowSpacing = 1;
    private const double RowPitch = RowHeight + RowSpacing;
    private const double ThumbBoxWidth = 28; // the icon box in the template

    // Thumbnail loading mirrors the strip: only decode a row that has stayed realized this long, and quantize the
    // requested pixel size so DPI changes don't re-fetch for nothing.
    private const int LoadDebounceMs = 120;
    private const uint ThumbSizeStep = 64;
    private const uint MinThumbSize = 64;
    private const uint MaxThumbSize = 256;

    // Above this many rows in one change, replace the bound list wholesale instead of raising one
    // CollectionChanged per row (expanding a folder of thousands of files).
    private const int BulkChangeThreshold = 64;

    private readonly ExplorerTree _tree = new();
    private readonly RangeObservableCollection<ExplorerNode> _rows = new();
    private readonly Dictionary<Border, ExplorerNode> _bound = new();     // realized row → its node
    private readonly Dictionary<ExplorerNode, Border> _realized = new();  // and back
    private readonly Dictionary<ExplorerNode, ThumbnailItem> _thumbs = new();
    private readonly Dictionary<ThumbnailItem, CancellationTokenSource> _loads = new();
    private readonly Dictionary<ExplorerNode, FileSystemWatcher> _watchers = new();
    private readonly Dictionary<ExplorerNode, Task> _loading = new();
    private readonly DispatcherQueue _dispatcher = DispatcherQueue.GetForCurrentThread(); // watcher callbacks marshal here

    private readonly Brush _accentBrush;
    private readonly Brush _transparentBrush = new SolidColorBrush(Colors.Transparent);
    private readonly Brush _hoverBorderBrush = new SolidColorBrush(Color.FromArgb(0x55, 0xFF, 0xFF, 0xFF));
    private readonly Brush _hoverFillBrush = new SolidColorBrush(Color.FromArgb(0x14, 0xFF, 0xFF, 0xFF));
    private readonly Brush _selectedFillBrush = new SolidColorBrush(Color.FromArgb(0x1F, 0xFF, 0xFF, 0xFF));
    private readonly Brush _thumbBoxBrush = new SolidColorBrush(Color.FromArgb(0x1F, 0xFF, 0xFF, 0xFF));
    private readonly Brush _gripIdleBrush = new SolidColorBrush(Color.FromArgb(0x66, 0xFF, 0xFF, 0xFF));
    private readonly InputSystemCursor _resizeCursor = InputSystemCursor.Create(InputSystemCursorShape.SizeWestEast);

    private readonly List<string> _crumbPaths = new(); // index-aligned with Crumbs.ItemsSource (the names)
    private readonly List<string> _history = new();    // every folder listed, oldest first; Back/Forward walk it
    private int _historyIndex = -1;                    // the listed folder's slot in _history
    private Border? _hovered;
    private ExplorerNode? _cursor; // keyboard cursor (Up/Down); follows the selection whenever that moves
    private string? _selectedPath;
    private string? _folder; // what the host last asked us to show (the open photo's folder)
    private string? _file;
    private int _showSerial;  // a newer ShowAsync/Up supersedes the async steps of an older one

    private double _panelWidth = DefaultPanelWidth;
    private bool _dragging;
    private bool _gripHovered;
    private double _dragStartX;
    private double _dragStartWidth;

    /// <summary>An image row was clicked: open this file.</summary>
    public event Action<string>? FileActivated;

    /// <summary>A context-menu action on a path.</summary>
    public event Action<ExplorerAction, string>? ActionRequested;

    /// <summary>Raised when a resize gesture ends (or the grip is double-tapped to reset), with the new width —
    /// the host persists it. Not raised for programmatic <see cref="PanelWidth"/> sets; the host follows the width
    /// live through the control's own SizeChanged.</summary>
    public event EventHandler<double>? PanelWidthChanged;

    public FileExplorer()
    {
        InitializeComponent();
        _accentBrush = Application.Current.Resources["AccentFillColorDefaultBrush"] as Brush
            ?? new SolidColorBrush(Colors.DodgerBlue);
        ApplyWidth(DefaultPanelWidth);
        _tree.RowsChanged += OnRowsChanged;
        Repeater.ItemsSource = _rows;
    }

    /// <summary>Total width of the control in DIPs (card + its 12px margins), which is what the host reserves on
    /// the left of the viewport for fit/zoom. Clamped to [<see cref="MinPanelWidth"/>, <see cref="MaxPanelWidth"/>].</summary>
    public double PanelWidth
    {
        get => _panelWidth;
        set => ApplyWidth(value);
    }

    private void ApplyWidth(double width)
    {
        _panelWidth = Math.Clamp(width, MinPanelWidth, MaxPanelWidth);
        Width = _panelWidth;
    }

    // --- Host API ---

    /// <summary>Show the photo's <paramref name="folder"/> with <paramref name="file"/> highlighted and scrolled into
    /// view. Works like Explorer's address bar: entering a *different* folder lists that folder (the trail ends at
    /// it); moving between photos of the same folder keeps whatever the user has navigated to (the file is only
    /// highlighted when it is in the listed folder). Null clears.</summary>
    public async Task ShowAsync(string? folder, string? file)
    {
        if (folder is not null)
            folder = Path.TrimEndingDirectorySeparator(folder); // "C:\Photos\" and "C:\Photos" are one folder
        bool folderChanged = _folder is null || folder is null || !ExplorerTree.SamePath(_folder, folder);
        _folder = folder;
        _file = file;
        int serial = ++_showSerial;
        if (folder is null)
        {
            ClearTree();
            ResetHistory(); // nothing open (landing page): the trail of folders ends with it
            return;
        }

        // A photo opened from the listed folder (a click on its row) must not reload the listing under the click.
        ExplorerNode? root = _tree.Root;
        if (root is null || (folderChanged && !ExplorerTree.SamePath(root.Path, folder)))
        {
            await SetRootAsync(folder);
            if (serial != _showSerial)
                return;
        }
        SelectPath(file);
    }

    /// <summary>Re-order every folder (files and sub-folders alike) under the toolbar's sort.</summary>
    public void SetSort(SortMode sort)
    {
        if (sort == _tree.Sort)
            return;
        _tree.SetSort(sort);
        SelectPath(_selectedPath); // the list was rebuilt: put the highlight back in view
    }

    // --- Root / expansion ---

    /// <summary>List <paramref name="path"/>. <paramref name="record"/> appends it to the history (a new place the
    /// user went, which forgets any Forward entries); Back/Forward pass false because they move within it.</summary>
    private async Task SetRootAsync(string path, bool record = true)
    {
        ClearTree();
        ExplorerNode root = _tree.SetRoot(path);
        BuildCrumbs(root.Path);
        if (record)
            RecordHistory(root.Path);
        UpdateHistoryButtons();
        await EnsureExpandedAsync(root);
    }

    // --- History (browser-style: Back / Forward through the folders listed so far) ---

    public bool CanGoBack => _historyIndex > 0;
    public bool CanGoForward => _historyIndex >= 0 && _historyIndex < _history.Count - 1;

    /// <summary>List the folder shown before this one (the header's Back button, mouse button 4).</summary>
    public void GoBack()
    {
        if (CanGoBack)
            _ = ReRootAsync(_history[--_historyIndex], record: false);
    }

    /// <summary>Return to the folder Back came from (the header's Forward button, mouse button 5).</summary>
    public void GoForward()
    {
        if (CanGoForward)
            _ = ReRootAsync(_history[++_historyIndex], record: false);
    }

    private void RecordHistory(string path)
    {
        if (_historyIndex >= 0 && ExplorerTree.SamePath(_history[_historyIndex], path))
            return; // re-listing the current folder is not a move
        if (_historyIndex < _history.Count - 1)
            _history.RemoveRange(_historyIndex + 1, _history.Count - _historyIndex - 1); // a new branch drops Forward
        _history.Add(path);
        _historyIndex = _history.Count - 1;
    }

    private void ResetHistory()
    {
        _history.Clear();
        _historyIndex = -1;
        UpdateHistoryButtons();
    }

    private void UpdateHistoryButtons()
    {
        BackButton.IsEnabled = CanGoBack;
        ForwardButton.IsEnabled = CanGoForward;
    }

    private void OnBackClicked(object sender, RoutedEventArgs e) => GoBack();

    private void OnForwardClicked(object sender, RoutedEventArgs e) => GoForward();

    /// <summary>The breadcrumb trail: every ancestor from the drive root down to the listed folder, exactly like
    /// Explorer's address bar. The last crumb is "where you are".</summary>
    private void BuildCrumbs(string folder)
    {
        _crumbPaths.Clear();
        var names = new List<string>();
        string? cursor = folder.Length == 0 ? null : folder;
        while (cursor is not null)
        {
            _crumbPaths.Insert(0, cursor);
            names.Insert(0, ExplorerTree.NameOf(cursor));
            cursor = Path.GetDirectoryName(cursor);
        }
        // "This PC" always heads the trail: one more step up from a drive shows every drive on the machine.
        _crumbPaths.Insert(0, ExplorerTree.ComputerPath);
        names.Insert(0, ExplorerTree.ComputerName);
        Crumbs.ItemsSource = names;
        ToolTipService.SetToolTip(Crumbs, folder.Length == 0 ? ExplorerTree.ComputerName : folder);
    }

    private void OnCrumbClicked(BreadcrumbBar sender, BreadcrumbBarItemClickedEventArgs args)
    {
        if (args.Index < 0 || args.Index >= _crumbPaths.Count)
            return;
        string target = _crumbPaths[args.Index];
        if (_tree.Root is { } root && !ExplorerTree.SamePath(root.Path, target))
            _ = ReRootAsync(target);
    }

    /// <summary>Navigate the listing to <paramref name="path"/> (a crumb, Back/Forward, or a double-clicked folder).
    /// The open photo is highlighted when it lives there; going up puts the keyboard cursor on the folder we came
    /// from, like Explorer does, so Enter goes straight back down.</summary>
    private async Task ReRootAsync(string path, bool record = true)
    {
        string? previousRoot = _tree.Root?.Path;
        int serial = ++_showSerial;
        await SetRootAsync(path, record);
        if (serial != _showSerial || _tree.Root is not { } root)
            return;
        SelectPath(_file);
        if (_selectedPath is null && previousRoot is not null && ExplorerTree.FindChild(root, previousRoot) is { } cameFrom)
        {
            SetCursor(cameFrom);
            int index = _tree.IndexOf(cameFrom);
            if (index >= 0)
                DispatcherQueue.TryEnqueue(DispatcherQueuePriority.Low, () => ScrollIntoView(index));
        }
    }

    private async Task EnsureExpandedAsync(ExplorerNode node)
    {
        if (node.IsExpanded)
            return;
        await EnsureLoadedAsync(node);
        if (!IsAttached(node) || !_tree.Expand(node))
            return;
        StartWatching(node);
        RefreshRow(node);
    }

    /// <summary>Enumerate a folder's children off the UI thread, once: concurrent callers share the same read.</summary>
    private async Task EnsureLoadedAsync(ExplorerNode node)
    {
        if (node.IsLoaded)
            return;
        if (_loading.TryGetValue(node, out Task? pending))
        {
            await pending;
            return;
        }

        var done = new TaskCompletionSource();
        _loading[node] = done.Task;
        try
        {
            List<ExplorerNode> children = await Task.Run(() => ExplorerTree.Enumerate(node));
            if (!node.IsLoaded && IsAttached(node))
                _tree.SetChildren(node, children);
        }
        finally
        {
            _loading.Remove(node);
            done.SetResult();
        }
    }

    /// <summary>Still part of the tree (not removed or collapsed away while an await was pending)?</summary>
    private bool IsAttached(ExplorerNode node)
    {
        ExplorerNode n = node;
        while (n.Parent is { } parent)
        {
            if (parent.ChildList is null || !parent.ChildList.Contains(n))
                return false;
            n = parent;
        }
        return ReferenceEquals(n, _tree.Root);
    }

    /// <summary>Forget everything tied to a node that is leaving the tree: its watcher, thumbnail and load.</summary>
    private void Detach(ExplorerNode node)
    {
        StopWatching(node);
        if (_thumbs.Remove(node, out ThumbnailItem? item))
        {
            CancelLoad(item);
            item.Thumbnail = null;
        }
    }

    private void ClearTree()
    {
        foreach (FileSystemWatcher watcher in _watchers.Values)
            DisposeWatcher(watcher);
        _watchers.Clear();
        foreach (CancellationTokenSource cts in _loads.Values)
        {
            cts.Cancel();
            cts.Dispose();
        }
        _loads.Clear();
        foreach (ThumbnailItem item in _thumbs.Values)
            item.Thumbnail = null;
        _thumbs.Clear();
        _selectedPath = null;
        _cursor = null;
        _tree.Clear();
        _crumbPaths.Clear();
        Crumbs.ItemsSource = null;
        ToolTipService.SetToolTip(Crumbs, null);
    }

    // --- Flat list mirroring (ExplorerTree → the bound collection) ---

    private void OnRowsChanged(ExplorerRowChange change)
    {
        switch (change.Kind)
        {
            case ExplorerRowChangeKind.Inserted when change.Count <= BulkChangeThreshold:
                for (int i = 0; i < change.Count; i++)
                    _rows.Insert(change.Index + i, _tree.Rows[change.Index + i]);
                break;
            case ExplorerRowChangeKind.Removed when change.Count <= BulkChangeThreshold:
                for (int i = 0; i < change.Count; i++)
                    _rows.RemoveAt(change.Index);
                break;
            default:
                _rows.Reset(_tree.Rows);
                break;
        }
    }

    // --- Selection ---

    private void SelectPath(string? path)
    {
        ExplorerNode? previous = _selectedPath is null ? null : _tree.Find(_selectedPath);
        _selectedPath = path;
        if (previous is not null)
            RefreshRow(previous);
        if (path is null || _tree.Find(path) is not { } node)
            return;
        SetCursor(node); // the keyboard cursor rides along with the selection
        RefreshRow(node);
        int index = _tree.IndexOf(node);
        if (index >= 0)
        {
            // Defer so the ScrollViewer has valid extents right after rows were inserted or the list reset.
            DispatcherQueue.TryEnqueue(DispatcherQueuePriority.Low, () => ScrollIntoView(index));
        }
    }

    // --- Keyboard cursor (host routes Up/Down/Enter here while the card is visible) ---

    /// <summary>Move the cursor <paramref name="delta"/> rows (clamped). Landing on a file activates it, exactly like
    /// clicking it (an image opens; an unsupported file gets the host's placeholder preview); landing on a folder
    /// only moves the cursor (Enter lists it).</summary>
    public void MoveCursor(int delta)
    {
        if (_rows.Count == 0)
            return;
        int index = _cursor is not null ? _tree.IndexOf(_cursor) : -1;
        index = index < 0
            ? (delta > 0 ? 0 : _rows.Count - 1)
            : Math.Clamp(index + delta, 0, _rows.Count - 1);
        ExplorerNode node = _rows[index];
        SetCursor(node);
        DispatcherQueue.TryEnqueue(DispatcherQueuePriority.Low, () => ScrollIntoView(index));
        if (!node.IsFolder)
        {
            SelectPath(node.Path);
            FileActivated?.Invoke(node.Path);
        }
    }

    /// <summary>Enter on the cursor row: list a folder, (re)activate a file. False when there is nothing to do, so
    /// the host lets the key through to whatever else wants it.</summary>
    public bool ActivateCursor()
    {
        if (_cursor is not { } node || _tree.IndexOf(node) < 0)
            return false;
        if (node.IsFolder)
        {
            _ = ReRootAsync(node.Path);
            return true;
        }
        FileActivated?.Invoke(node.Path);
        return true;
    }

    private void SetCursor(ExplorerNode? node)
    {
        if (ReferenceEquals(_cursor, node))
            return;
        ExplorerNode? previous = _cursor;
        _cursor = node;
        if (previous is not null && _realized.TryGetValue(previous, out Border? prevRow))
            RenderRowState(prevRow, previous);
        if (node is not null && _realized.TryGetValue(node, out Border? row))
            RenderRowState(row, node);
    }

    /// <summary>Bring row <paramref name="index"/> into the viewport, instantly: ChangeView animates by default, and
    /// after a listing loads (Back/Forward, a crumb, a new folder) that played a long scroll from the top of the
    /// fresh list down to the highlighted image every time.</summary>
    private void ScrollIntoView(int index)
    {
        double viewport = Scroller.ViewportHeight;
        if (viewport <= 0)
            return;
        double top = index * RowPitch;
        double bottom = top + RowHeight;
        double offset = Scroller.VerticalOffset;
        if (top < offset)
            Scroller.ChangeView(null, top, null, disableAnimation: true);
        else if (bottom > offset + viewport)
            Scroller.ChangeView(null, bottom - viewport, null, disableAnimation: true);
    }

    private bool IsSelected(ExplorerNode node)
        => _selectedPath is { } selected && ExplorerTree.SamePath(node.Path, selected);

    // --- Row rendering (all imperative: the template carries no bindings) ---

    private readonly record struct RowParts(Grid IconBox, Image Image, FontIcon Glyph, TextBlock Name);

    private static RowParts Parts(Border row)
    {
        var grid = (Grid)row.Child;
        var iconBox = (Grid)grid.Children[0];
        return new RowParts(
            iconBox,
            (Image)iconBox.Children[0],
            (FontIcon)iconBox.Children[1],
            (TextBlock)grid.Children[1]);
    }

    private void RenderRow(Border row, ExplorerNode node)
    {
        RowParts p = Parts(row);
        p.Name.Text = node.Name;
        ((Grid)row.Child).Opacity = node.Kind == ExplorerNodeKind.Other ? 0.45 : 1.0;
        ToolTipService.SetToolTip(row, node.Kind == ExplorerNodeKind.Other ? $"{node.Name}: not an image Looker can open" : node.Name);

        switch (node.Kind)
        {
            case ExplorerNodeKind.Folder:
                p.IconBox.Background = _transparentBrush;
                p.Glyph.Glyph = node.IsDrive ? "\uEDA2" : "\uE8B7"; // HardDrive / Folder
                p.Glyph.Visibility = Visibility.Visible;
                p.Image.Source = null;
                break;
            case ExplorerNodeKind.Image:
                p.IconBox.Background = _thumbBoxBrush;
                p.Glyph.Glyph = "\uE91B"; // Photo, until the thumbnail lands
                ApplyThumbnail(row, _thumbs.TryGetValue(node, out ThumbnailItem? item) ? item.Thumbnail : null);
                break;
            default:
                p.IconBox.Background = _transparentBrush;
                p.Glyph.Glyph = "\uE7C3"; // Page
                p.Glyph.Visibility = Visibility.Visible;
                p.Image.Source = null;
                break;
        }
        RenderRowState(row, node);
    }

    private void RenderRowState(Border row, ExplorerNode node)
    {
        bool selected = IsSelected(node);
        bool hovered = row == _hovered || ReferenceEquals(node, _cursor); // the keyboard cursor reads like a hover
        row.BorderBrush = selected ? _accentBrush : hovered ? _hoverBorderBrush : _transparentBrush;
        row.Background = selected ? _selectedFillBrush : hovered ? _hoverFillBrush : _transparentBrush;
    }

    private static void ApplyThumbnail(Border row, ImageSource? source)
    {
        RowParts p = Parts(row);
        p.Image.Source = source;
        p.Glyph.Visibility = source is null ? Visibility.Visible : Visibility.Collapsed;
    }

    private void RefreshRow(ExplorerNode node)
    {
        if (_realized.TryGetValue(node, out Border? row))
            RenderRow(row, node);
    }

    // --- Element lifecycle ---

    private void OnElementPrepared(ItemsRepeater sender, ItemsRepeaterElementPreparedEventArgs args)
    {
        if (args.Element is not Border row || args.Index < 0 || args.Index >= _rows.Count)
            return;
        ExplorerNode node = _rows[args.Index];
        _bound[row] = node;
        _realized[node] = row;
        row.Tapped += OnRowTapped;
        row.DoubleTapped += OnRowDoubleTapped;
        row.RightTapped += OnRowRightTapped;
        row.PointerEntered += OnRowPointerEntered;
        row.PointerExited += OnRowPointerExited;
        RenderRow(row, node);
        if (node.Kind == ExplorerNodeKind.Image)
            BeginLoad(node);
    }

    private void OnElementClearing(ItemsRepeater sender, ItemsRepeaterElementClearingEventArgs args)
    {
        if (args.Element is not Border row)
            return;
        row.Tapped -= OnRowTapped;
        row.DoubleTapped -= OnRowDoubleTapped;
        row.RightTapped -= OnRowRightTapped;
        row.PointerEntered -= OnRowPointerEntered;
        row.PointerExited -= OnRowPointerExited;
        if (_hovered == row)
            _hovered = null;
        if (_bound.Remove(row, out ExplorerNode? node))
        {
            if (_realized.TryGetValue(node, out Border? current) && current == row)
                _realized.Remove(node);
            if (_thumbs.TryGetValue(node, out ThumbnailItem? item))
            {
                CancelLoad(item);
                item.Thumbnail = null; // keep live thumbnails bounded to the realized rows (see ThumbnailStrip)
            }
        }
    }

    private void OnRowPointerEntered(object sender, PointerRoutedEventArgs e)
    {
        var row = (Border)sender;
        _hovered = row;
        if (_bound.TryGetValue(row, out ExplorerNode? node))
            RenderRowState(row, node);
    }

    private void OnRowPointerExited(object sender, PointerRoutedEventArgs e)
    {
        var row = (Border)sender;
        if (_hovered == row)
            _hovered = null;
        if (_bound.TryGetValue(row, out ExplorerNode? node))
            RenderRowState(row, node);
    }

    private void OnRowTapped(object sender, TappedRoutedEventArgs e)
    {
        if (!_bound.TryGetValue((Border)sender, out ExplorerNode? node))
            return;
        e.Handled = true;
        if (node.IsFolder)
        {
            SetCursor(node); // a double-click (or Enter) lists it
        }
        else
        {
            // Images and unsupported files alike: the host opens the former and shows a placeholder for the latter.
            SelectPath(node.Path); // highlight at once; the host echoes it back through ShowAsync
            FileActivated?.Invoke(node.Path);
        }
    }

    private void OnRowDoubleTapped(object sender, DoubleTappedRoutedEventArgs e)
    {
        if (!_bound.TryGetValue((Border)sender, out ExplorerNode? node) || !node.IsFolder)
            return;
        e.Handled = true;
        _ = ReRootAsync(node.Path);
    }

    private void OnRowRightTapped(object sender, RightTappedRoutedEventArgs e)
    {
        var row = (Border)sender;
        if (!_bound.TryGetValue(row, out ExplorerNode? node))
            return;
        e.Handled = true;

        var menu = new MenuFlyout();
        void Add(string text, string glyph, ExplorerAction action)
        {
            var item = new MenuFlyoutItem { Text = text, Icon = new FontIcon { Glyph = glyph } };
            string path = node.Path;
            item.Click += (_, _) => ActionRequested?.Invoke(action, path);
            menu.Items.Add(item);
        }

        switch (node.Kind)
        {
            case ExplorerNodeKind.Folder:
                Add("Open folder", "\uE8B7", ExplorerAction.Open);
                menu.Items.Add(new MenuFlyoutSeparator());
                break;
            case ExplorerNodeKind.Image:
                Add("Open", "\uE8A7", ExplorerAction.Open);
                Add("Rename", "\uE8AC", ExplorerAction.Rename);
                Add("Delete", "\uE74D", ExplorerAction.Delete);
                menu.Items.Add(new MenuFlyoutSeparator());
                break;
        }
        Add("Copy path", "\uE71B", ExplorerAction.CopyPath);
        Add("Reveal in File Explorer", "\uEC50", ExplorerAction.Reveal);
        menu.ShowAt(row, e.GetPosition(row));
    }

    // --- Thumbnails ---

    /// <summary>Pixel size to ask the loader for: the icon box width at the current DPI, rounded up to a step.</summary>
    private uint ThumbnailPixelSize
    {
        get
        {
            double scale = XamlRoot?.RasterizationScale ?? 1.0;
            uint stepped = (uint)Math.Ceiling(ThumbBoxWidth * scale / ThumbSizeStep) * ThumbSizeStep;
            return Math.Clamp(stepped, MinThumbSize, MaxThumbSize);
        }
    }

    private ThumbnailItem ThumbFor(ExplorerNode node)
    {
        if (!_thumbs.TryGetValue(node, out ThumbnailItem? item))
        {
            item = new ThumbnailItem(node.Path, node.ModifiedTicks);
            ThumbnailItem captured = item;
            item.PropertyChanged += (_, _) =>
            {
                if (_realized.TryGetValue(node, out Border? row))
                    ApplyThumbnail(row, captured.Thumbnail);
            };
            _thumbs[node] = item;
        }
        return item;
    }

    private async void BeginLoad(ExplorerNode node)
    {
        ThumbnailItem item = ThumbFor(node);
        if (item.Thumbnail is not null)
            return;

        CancelLoad(item);
        var cts = new CancellationTokenSource();
        _loads[item] = cts;
        try
        {
            if (!IsSelected(node))
                await Task.Delay(LoadDebounceMs, cts.Token); // rows scrolled past never decode
            await ThumbnailLoader.LoadAsync(item, ThumbnailPixelSize, cts.Token);
        }
        catch (OperationCanceledException)
        {
            // recycled or superseded
        }
        catch
        {
            // one bad file must not tear down the panel
        }
        finally
        {
            if (_loads.TryGetValue(item, out CancellationTokenSource? current) && current == cts)
                _loads.Remove(item);
            cts.Dispose();
        }
    }

    private void CancelLoad(ThumbnailItem item)
    {
        if (_loads.Remove(item, out CancellationTokenSource? cts))
        {
            cts.Cancel();
            cts.Dispose();
        }
    }

    // --- Live folder changes: one non-recursive watcher on the listed folder, marshalled to the UI thread ---

    private void StartWatching(ExplorerNode node)
    {
        if (_watchers.ContainsKey(node) || node.IsComputer) // drives coming and going are not watched
            return;
        try
        {
            var watcher = new FileSystemWatcher(node.Path)
            {
                NotifyFilter = NotifyFilters.FileName | NotifyFilters.DirectoryName | NotifyFilters.LastWrite | NotifyFilters.Size,
                IncludeSubdirectories = false,
            };
            watcher.Created += (_, e) => Post(() => OnDiskCreated(node, e.FullPath));
            watcher.Deleted += (_, e) => Post(() => OnDiskDeleted(node, e.FullPath));
            watcher.Renamed += (_, e) => Post(() =>
            {
                OnDiskDeleted(node, e.OldFullPath);
                OnDiskCreated(node, e.FullPath);
            });
            watcher.Changed += (_, e) =>
            {
                if (e.ChangeType == WatcherChangeTypes.Changed)
                    Post(() => OnDiskChanged(node, e.FullPath));
            };
            watcher.EnableRaisingEvents = true;
            _watchers[node] = watcher;
        }
        catch (Exception ex) when (ex is IOException or UnauthorizedAccessException or ArgumentException)
        {
            // Watching is best-effort; the listing still works without live updates.
        }
    }

    private void StopWatching(ExplorerNode node)
    {
        if (_watchers.Remove(node, out FileSystemWatcher? watcher))
            DisposeWatcher(watcher);
    }

    private static void DisposeWatcher(FileSystemWatcher watcher)
    {
        try
        {
            watcher.EnableRaisingEvents = false;
            watcher.Dispose();
        }
        catch
        {
            // already gone
        }
    }

    private void Post(Action action) => _dispatcher.TryEnqueue(() => action()); // raised on a threadpool thread

    private void OnDiskCreated(ExplorerNode parent, string path)
    {
        if (!_watchers.ContainsKey(parent) || !parent.IsLoaded) // collapsed since the event was raised
            return;
        if (ExplorerTree.FindChild(parent, path) is not null)
            return;
        if (ExplorerTree.TryCreateNode(parent, path) is { } node)
            _tree.Insert(parent, node);
        if (_selectedPath is { } selected && ExplorerTree.SamePath(selected, path))
            SelectPath(selected); // the current file was renamed into existence: keep it highlighted
    }

    private void OnDiskDeleted(ExplorerNode parent, string path)
    {
        if (!_watchers.ContainsKey(parent))
            return;
        if (ExplorerTree.FindChild(parent, path) is not { } node)
            return;
        foreach (ExplorerNode descendant in ExplorerTree.Descendants(node))
            Detach(descendant);
        Detach(node);
        _tree.Remove(node);
    }

    private void OnDiskChanged(ExplorerNode parent, string path)
    {
        if (!_watchers.ContainsKey(parent))
            return;
        if (ExplorerTree.FindChild(parent, path) is not { } node)
            return;

        long ticks, size;
        try
        {
            FileSystemInfo info = node.IsFolder ? new DirectoryInfo(path) : new FileInfo(path);
            if (!info.Exists)
                return;
            ticks = info.LastWriteTimeUtc.Ticks;
            size = info is FileInfo fi ? fi.Length : 0;
        }
        catch (Exception ex) when (ex is IOException or UnauthorizedAccessException)
        {
            return;
        }
        if (ticks == node.ModifiedTicks && size == node.Size)
            return; // spurious event: nothing observable changed

        _tree.Refresh(node, ticks, size); // may move the row under a date/size sort
        if (node.Kind == ExplorerNodeKind.Image && _thumbs.TryGetValue(node, out ThumbnailItem? item))
        {
            CancelLoad(item);
            item.Invalidate(ticks);
            if (_realized.ContainsKey(node))
            {
                RefreshRow(node);
                BeginLoad(node);
            }
        }
    }

    // --- Resize grip (mirrors InfoPanel's, on the right edge: dragging right = wider) ---

    private void OnGripPointerEntered(object sender, PointerRoutedEventArgs e)
    {
        _gripHovered = true;
        GripLine.Opacity = 1;
        ProtectedCursor = _resizeCursor;
    }

    private void OnGripPointerExited(object sender, PointerRoutedEventArgs e)
    {
        _gripHovered = false;
        if (_dragging)
            return; // keep the line and cursor until the drag ends
        GripLine.Opacity = 0;
        ProtectedCursor = null;
    }

    private void OnGripPointerPressed(object sender, PointerRoutedEventArgs e)
    {
        if (!e.GetCurrentPoint(Grip).Properties.IsLeftButtonPressed)
            return;
        if (!Grip.CapturePointer(e.Pointer))
            return;

        _dragging = true;
        _dragStartX = e.GetCurrentPoint(null).Position.X;
        _dragStartWidth = _panelWidth;
        GripLine.Fill = _accentBrush;
        GripLine.Opacity = 1;
        ProtectedCursor = _resizeCursor;
        e.Handled = true;
    }

    private void OnGripPointerMoved(object sender, PointerRoutedEventArgs e)
    {
        if (!_dragging)
            return;

        double x = e.GetCurrentPoint(null).Position.X;
        double wanted = _dragStartWidth + (x - _dragStartX); // dragging right = wider
        // Never let the card swallow the window: leave a usable viewport to its right.
        double cap = XamlRoot is { } root ? Math.Max(MinPanelWidth, root.Size.Width - 480) : MaxPanelWidth;
        ApplyWidth(Math.Min(wanted, cap));
        e.Handled = true;
    }

    private void OnGripPointerReleased(object sender, PointerRoutedEventArgs e)
    {
        if (!_dragging)
            return;
        _dragging = false;
        Grip.ReleasePointerCaptures(); // PointerCaptureLost re-enters here; the flag above makes it a no-op
        EndResizeGesture();
    }

    private void OnGripDoubleTapped(object sender, DoubleTappedRoutedEventArgs e)
    {
        if (_dragging)
            return;
        ApplyWidth(DefaultPanelWidth);
        EndResizeGesture();
        e.Handled = true;
    }

    private void EndResizeGesture()
    {
        GripLine.Fill = _gripIdleBrush;
        if (!_gripHovered)
        {
            GripLine.Opacity = 0;
            ProtectedCursor = null;
        }
        PanelWidthChanged?.Invoke(this, _panelWidth);
    }
}
