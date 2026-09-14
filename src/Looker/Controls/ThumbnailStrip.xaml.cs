using System;
using System.Collections.Generic;
using System.Threading;
using System.Threading.Tasks;
using Looker.ViewModels;
using Microsoft.UI;
using Microsoft.UI.Dispatching;
using Microsoft.UI.Input;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Input;
using Microsoft.UI.Xaml.Media;
using Microsoft.UI.Xaml.Shapes;
using Windows.UI;

namespace Looker.Controls;

/// <summary>
/// The filmstrip: a horizontally virtualized <see cref="ItemsRepeater"/> in a ScrollViewer we own.
/// Owning the ScrollViewer is the whole point — centring the current cell is a supported
/// <see cref="ScrollViewer.ChangeView(double?,double?,float?)"/> call, whereas poking a ListView's
/// internal ScrollViewer during virtualization crashes XAML. The host wires <see cref="SetSource"/>
/// once, calls <see cref="SelectIndex"/> on each navigation, and listens to
/// <see cref="SelectionActivated"/> for clicks.
/// <para>The strip is resizable: dragging the grip along its top edge changes <see cref="StripHeight"/>
/// (cells scale with it, keeping 4:3) and <see cref="StripHeightChanged"/> fires when the drag ends so
/// the host can persist the value. Double-tapping the grip restores <see cref="DefaultStripHeight"/>.</para>
/// </summary>
public sealed partial class ThumbnailStrip : UserControl
{
    /// <summary>Colour the edge fades bleed from: the themed window colour (the strip has no fill of its own).</summary>
    public void SetFadeColor(Color color)
    {
        Color clear = Color.FromArgb(0, color.R, color.G, color.B);
        LeftFadeBrush.GradientStops[0].Color = color;
        LeftFadeBrush.GradientStops[1].Color = clear;
        RightFadeBrush.GradientStops[0].Color = clear;
        RightFadeBrush.GradientStops[1].Color = color;
    }

    public const double DefaultStripHeight = 96.0;
    public const double MinStripHeight = 56.0;
    public const double MaxStripHeight = 480.0;

    // Cell geometry derives from the strip height: 8px of breathing room above the cells only (they sit flush
    // on the strip's bottom edge, against the status row; the resize grip lives in that top band), 4:3 cells,
    // 4px StackLayout spacing (must match the XAML), 8px ItemsRepeater left margin. Everything is computed from
    // these so the centred offset stays exact at any size.
    private const double CellVerticalPadding = 8.0;
    private const double CellAspect = 4.0 / 3.0;
    private const double Spacing = 4.0;
    private const double LeftMargin = 8.0;

    // Only start decoding a cell that has stayed on-screen this long: a fast fling realizes and
    // recycles cells in a few ms each, so debouncing means we never decode the ones we blow past.
    private const int LoadDebounceMs = 120;

    // Cells this close to the selection skip the debounce: they are what the user is looking at (or about to
    // arrow onto), so they go to the loader ahead of the rest of the freshly realized row.
    private const int NearSelectionCells = 2;

    // Thumbnail decode size is quantized so a small resize does not re-fetch every visible thumbnail;
    // only crossing a step (at drag end) reloads them at the new size.
    private const uint ThumbSizeStep = 64;
    private const uint MinThumbSize = 96;
    private const uint MaxThumbSize = 1024;

    // One cancellation source per in-flight thumbnail load, keyed by item, cancelled when its element
    // recycles (scrolled off) so we never keep decoding for cells that are gone.
    private readonly Dictionary<ThumbnailItem, CancellationTokenSource> _loads = new();
    private readonly Dictionary<Border, ThumbnailItem> _bound = new(); // realized element → its item
    private readonly Brush _selectedBrush;
    private readonly Brush _hoverBrush = new SolidColorBrush(Color.FromArgb(0x99, 0xFF, 0xFF, 0xFF));
    private readonly Brush _unselectedBrush = new SolidColorBrush(Colors.Transparent);
    private readonly Brush _gripIdleBrush = new SolidColorBrush(Color.FromArgb(0x66, 0xFF, 0xFF, 0xFF));
    private readonly InputSystemCursor _resizeCursor = InputSystemCursor.Create(InputSystemCursorShape.SizeNorthSouth);
    private IReadOnlyList<ThumbnailItem>? _source;
    private int _selectedIndex = -1;
    private Border? _hovered;
    private double _stripHeight = DefaultStripHeight;
    private uint _loadedThumbSize; // decode size the current thumbnails were fetched at (0 = none yet)

    // Grip drag state. Positions are read relative to the XamlRoot (GetCurrentPoint(null)), not to this
    // control: the strip is bottom-anchored, so its own top edge moves under the pointer as it resizes.
    private bool _dragging;
    private bool _gripHovered;
    private double _dragStartY;
    private double _dragStartHeight;

    /// <summary>Raised when the user picks a thumbnail (never for programmatic selection).</summary>
    public event EventHandler<int>? SelectionActivated;

    /// <summary>Raised when a resize gesture ends (or the grip is double-tapped to reset), with the new
    /// height — the host persists it. Not raised for programmatic <see cref="StripHeight"/> sets.</summary>
    public event EventHandler<double>? StripHeightChanged;

    public ThumbnailStrip()
    {
        InitializeComponent();
        _selectedBrush = Application.Current.Resources["AccentFillColorDefaultBrush"] as Brush
            ?? new SolidColorBrush(Colors.DodgerBlue);
        ApplyHeight(DefaultStripHeight);
    }

    /// <summary>Total height of the strip in DIPs, clamped to [<see cref="MinStripHeight"/>, <see cref="MaxStripHeight"/>].</summary>
    public double StripHeight
    {
        get => _stripHeight;
        set
        {
            ApplyHeight(value);
            ReloadThumbnailsIfSizeChanged();
        }
    }

    private double CellHeight => Math.Max(32.0, _stripHeight - CellVerticalPadding);
    private double CellWidth => Math.Round(CellHeight * CellAspect);
    private double Pitch => CellWidth + Spacing;

    /// <summary>Pixel size to ask the loader for: the cell width at the current DPI, rounded up to a step.</summary>
    private uint ThumbnailPixelSize
    {
        get
        {
            double scale = XamlRoot?.RasterizationScale ?? 1.0;
            double px = CellWidth * scale;
            uint stepped = (uint)Math.Ceiling(px / ThumbSizeStep) * ThumbSizeStep;
            return Math.Clamp(stepped, MinThumbSize, MaxThumbSize);
        }
    }

    public void SetSource(IReadOnlyList<ThumbnailItem> items)
    {
        _source = items;
        Repeater.ItemsSource = items;
    }

    /// <summary>Reflect the current image: move the highlight and centre the cell, without raising
    /// <see cref="SelectionActivated"/>.</summary>
    public void SelectIndex(int index)
    {
        int previous = _selectedIndex;
        _selectedIndex = index;
        if (previous != index)
        {
            Highlight(previous, selected: false);
            Highlight(index, selected: true);
        }

        if (index < 0)
            return;

        // Defer so the ScrollViewer has valid extents right after the strip is shown or the list reset.
        DispatcherQueue.TryEnqueue(DispatcherQueuePriority.Low, () => CenterOn(index, animate: true));
    }

    private void CenterOn(int index, bool animate)
    {
        double viewport = Scroller.ViewportWidth;
        if (viewport <= 0)
            return;

        double cellCenter = LeftMargin + index * Pitch + CellWidth / 2.0;
        double target = cellCenter - viewport / 2.0;
        target = Math.Max(0, Math.Min(target, Scroller.ScrollableWidth));
        // Glide only for short moves (the next/previous cell). A target more than a viewport away - wrapping
        // from the last image to the first, Home/End, a click far off in the explorer - jumps instantly rather
        // than flying the whole strip past.
        bool jump = !animate || Math.Abs(target - Scroller.HorizontalOffset) > viewport;
        Scroller.ChangeView(target, null, null, disableAnimation: jump); // our own ScrollViewer — supported and safe
        UpdateEdgeFades(); // the extent can change (folder load) without a ViewChanged, so refresh here too
    }

    private void Highlight(int index, bool selected)
    {
        if (index < 0)
            return;
        if (Repeater.TryGetElement(index) is Border border)
            border.BorderBrush = BrushFor(border, selected);
    }

    private Brush BrushFor(Border border, bool selected)
        => selected ? _selectedBrush : border == _hovered ? _hoverBrush : _unselectedBrush;

    // --- Sizing ---

    private void ApplyHeight(double height)
    {
        double clamped = Math.Clamp(height, MinStripHeight, MaxStripHeight);
        _stripHeight = clamped;
        Scroller.Height = clamped;

        double w = CellWidth, h = CellHeight;
        foreach (Border border in _bound.Keys)
        {
            border.Width = w;
            border.Height = h;
        }
    }

    /// <summary>After a size change settles, re-fetch the visible thumbnails when the required decode
    /// size crossed a step (bigger cells would otherwise show upscaled, soft thumbnails).</summary>
    private void ReloadThumbnailsIfSizeChanged()
    {
        uint size = ThumbnailPixelSize;
        if (size == _loadedThumbSize || _loadedThumbSize == 0)
            return;
        _loadedThumbSize = size;
        foreach ((Border border, ThumbnailItem item) in _bound)
        {
            CancelLoad(item);
            item.Thumbnail = null;
            BeginLoad(item, Repeater.GetElementIndex(border));
        }
    }

    // --- Edge fades ---

    private void OnScrollerViewChanged(object? sender, ScrollViewerViewChangedEventArgs e) => UpdateEdgeFades();

    private void OnScrollerSizeChanged(object sender, SizeChangedEventArgs e) => UpdateEdgeFades();

    /// <summary>Show a fade only where there is content beyond that edge to scroll to.</summary>
    private void UpdateEdgeFades()
    {
        double offset = Scroller.HorizontalOffset;
        double max = Scroller.ScrollableWidth;
        LeftFade.Opacity = offset > 0.5 ? 1 : 0;
        RightFade.Opacity = max - offset > 0.5 ? 1 : 0;
    }

    // --- Resize grip ---

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
        _dragStartY = e.GetCurrentPoint(null).Position.Y;
        _dragStartHeight = _stripHeight;
        GripLine.Fill = _selectedBrush;
        GripLine.Opacity = 1;
        ProtectedCursor = _resizeCursor;
        e.Handled = true;
    }

    private void OnGripPointerMoved(object sender, PointerRoutedEventArgs e)
    {
        if (!_dragging)
            return;

        double y = e.GetCurrentPoint(null).Position.Y;
        double wanted = _dragStartHeight + (_dragStartY - y); // dragging up = taller
        // Never let the strip swallow the window: leave room for the chrome and a usable viewport.
        double cap = XamlRoot is { } root ? Math.Max(MinStripHeight, root.Size.Height - 260) : MaxStripHeight;
        ApplyHeight(Math.Min(wanted, cap));
        if (_selectedIndex >= 0)
            DispatcherQueue.TryEnqueue(DispatcherQueuePriority.Low, () => CenterOn(_selectedIndex, animate: false));
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
        ApplyHeight(DefaultStripHeight);
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
        ReloadThumbnailsIfSizeChanged();
        if (_selectedIndex >= 0)
            DispatcherQueue.TryEnqueue(DispatcherQueuePriority.Low, () => CenterOn(_selectedIndex, animate: false));
        StripHeightChanged?.Invoke(this, _stripHeight);
    }

    // --- Element lifecycle: highlight + hover + click + phased thumbnail load ---

    private void OnElementPrepared(ItemsRepeater sender, ItemsRepeaterElementPreparedEventArgs args)
    {
        if (args.Element is not Border border)
            return;

        border.Width = CellWidth;
        border.Height = CellHeight;
        border.BorderBrush = args.Index == _selectedIndex ? _selectedBrush : _unselectedBrush;
        SetVeil(border, visible: false);
        border.Tapped += OnItemTapped;
        border.PointerEntered += OnItemPointerEntered;
        border.PointerExited += OnItemPointerExited;

        if (_source is { } source && args.Index >= 0 && args.Index < source.Count)
        {
            ThumbnailItem item = source[args.Index];
            _bound[border] = item;
            BeginLoad(item, args.Index);
        }
    }

    private void OnElementClearing(ItemsRepeater sender, ItemsRepeaterElementClearingEventArgs args)
    {
        if (args.Element is not Border border)
            return;

        border.Tapped -= OnItemTapped;
        border.PointerEntered -= OnItemPointerEntered;
        border.PointerExited -= OnItemPointerExited;
        if (_hovered == border)
            _hovered = null;
        if (_bound.Remove(border, out ThumbnailItem? item))
        {
            CancelLoad(item);
            // Release the decoded bitmap so live thumbnails stay bounded to the visible window instead
            // of accumulating one per scrolled-past image (hundreds of surfaces crash XAML). Scrolling
            // back re-decodes from the warm shell cache, which is cheap.
            item.Thumbnail = null;
        }
    }

    private void OnItemPointerEntered(object sender, PointerRoutedEventArgs e)
    {
        var border = (Border)sender;
        _hovered = border;
        SetVeil(border, visible: true);
        int index = Repeater.GetElementIndex(border);
        border.BorderBrush = BrushFor(border, selected: index == _selectedIndex);
    }

    private void OnItemPointerExited(object sender, PointerRoutedEventArgs e)
    {
        var border = (Border)sender;
        if (_hovered == border)
            _hovered = null;
        SetVeil(border, visible: false);
        int index = Repeater.GetElementIndex(border);
        border.BorderBrush = BrushFor(border, selected: index == _selectedIndex);
    }

    /// <summary>The translucent white veil over the image (second child of the cell's inner Grid).</summary>
    private static void SetVeil(Border border, bool visible)
    {
        if (border.Child is Grid { Children.Count: >= 2 } grid && grid.Children[1] is Rectangle veil)
            veil.Opacity = visible ? 1 : 0;
    }

    private void OnItemTapped(object sender, TappedRoutedEventArgs e)
    {
        int index = Repeater.GetElementIndex((UIElement)sender);
        if (index < 0)
            return;

        // Move the highlight immediately so the click feels instant, rather than waiting for the host to
        // navigate and decode before echoing the selection back via SelectIndex. The later SelectIndex for
        // the same index is then a no-op for the highlight.
        SelectIndex(index);
        SelectionActivated?.Invoke(this, index);
    }

    private async void BeginLoad(ThumbnailItem item, int index)
    {
        if (item.Thumbnail is not null)
            return;

        CancelLoad(item); // supersede any stale load for this item
        var cts = new CancellationTokenSource();
        _loads[item] = cts;
        uint size = ThumbnailPixelSize;
        _loadedThumbSize = size;
        bool near = _selectedIndex >= 0 && index >= 0 && Math.Abs(index - _selectedIndex) <= NearSelectionCells;

        try
        {
            if (!near)
                await Task.Delay(LoadDebounceMs, cts.Token); // skip cells flung past before they settle
            await ThumbnailLoader.LoadAsync(item, size, cts.Token);
        }
        catch (OperationCanceledException)
        {
            // recycled or superseded
        }
        catch
        {
            // one bad file must not tear down the strip
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
}
