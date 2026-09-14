using System;
using Looker.Services;
using Microsoft.UI.Text;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Input;
using Microsoft.UI.Xaml.Documents;
using Microsoft.UI.Xaml.Input;
using Microsoft.UI.Xaml.Media;
using Microsoft.UI.Xaml.Shapes;
using Windows.Foundation;
using Windows.UI;

namespace Looker.Controls;

/// <summary>
/// The EXIF + histogram panel. Host code calls <see cref="Render"/> with a fresh
/// <see cref="InfoSnapshot"/> on each navigation (while visible); the panel rebuilds its rows and redraws
/// the histogram imperatively. Passing null (nothing open / panel hidden) shows the empty state.
/// </summary>
public sealed partial class InfoPanel : UserControl
{
    /// <summary>Card fill = the themed window colour, so the card reads as a piece of chrome floating over the
    /// checkerboard under either theme.</summary>
    public void SetCardBackground(Color color) => Card.Background = new SolidColorBrush(color);

    /// <summary>Default and bounds of <see cref="PanelWidth"/> in DIPs; the range mirrors
    /// <c>SettingsService.InfoWidth</c>. The lower bound keeps the 96px label column plus a readable value.</summary>
    public const double DefaultPanelWidth = 320;
    public const double MinPanelWidth = 260;
    public const double MaxPanelWidth = 640;

    // The histogram is drawn at this fixed size and scaled by a Viewbox to the card's width.
    private const double HistogramWidth = 256;
    private const double HistogramHeight = 120;

    private readonly Brush _accentBrush;
    private readonly Brush _gripIdleBrush = new SolidColorBrush(Color.FromArgb(0x66, 0xFF, 0xFF, 0xFF));
    private readonly InputSystemCursor _resizeCursor = InputSystemCursor.Create(InputSystemCursorShape.SizeWestEast);
    private double _panelWidth = DefaultPanelWidth;

    // Grip drag state. Positions are read relative to the XamlRoot (GetCurrentPoint(null)), not to this control:
    // the card is right-anchored, so its own left edge moves under the pointer as it resizes.
    private bool _dragging;
    private bool _gripHovered;
    private double _dragStartX;
    private double _dragStartWidth;

    /// <summary>The path row was clicked: the host reveals the current file in File Explorer.</summary>
    public event EventHandler? RevealRequested;

    /// <summary>Raised when a resize gesture ends (or the grip is double-tapped to reset), with the new width —
    /// the host persists it. Not raised for programmatic <see cref="PanelWidth"/> sets. The host follows the
    /// width *live* through the control's own SizeChanged (the image fit and the OSD margin track the drag).</summary>
    public event EventHandler<double>? PanelWidthChanged;

    public InfoPanel()
    {
        InitializeComponent();
        _accentBrush = Application.Current.Resources["AccentFillColorDefaultBrush"] as Brush
            ?? new SolidColorBrush(Microsoft.UI.Colors.DodgerBlue);
        ApplyWidth(DefaultPanelWidth);
    }

    /// <summary>Total width of the control in DIPs — the card plus its 12px margins, which is exactly what the
    /// host reserves on the right of the viewport for fit/zoom (a fit image then ends 12px from the card, the
    /// same gap the card keeps from the window edge). Clamped to [<see cref="MinPanelWidth"/>, <see cref="MaxPanelWidth"/>].</summary>
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

    // --- Resize grip (mirrors ThumbnailStrip's top-edge grip) ---

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
        double wanted = _dragStartWidth + (_dragStartX - x); // dragging left = wider
        // Never let the card swallow the window: leave a usable viewport to its left.
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

    public void Render(InfoSnapshot? snapshot)
    {
        GroupsHost.Children.Clear();
        HistogramCanvas.Children.Clear();

        if (snapshot is null || snapshot.Groups.Count == 0)
        {
            EmptyText.Visibility = Visibility.Visible;
            HistogramCard.Visibility = Visibility.Collapsed;
            return;
        }

        EmptyText.Visibility = Visibility.Collapsed;

        if (snapshot.Histogram is { } histogram)
        {
            HistogramCard.Visibility = Visibility.Visible;
            DrawHistogram(histogram);
        }
        else
        {
            HistogramCard.Visibility = Visibility.Collapsed;
        }

        foreach (MetadataGroup group in snapshot.Groups)
            GroupsHost.Children.Add(BuildGroup(group));
    }

    private FrameworkElement BuildGroup(MetadataGroup group)
    {
        var panel = new StackPanel { Spacing = 2 };
        panel.Children.Add(new TextBlock
        {
            Text = group.Title.ToUpperInvariant(),
            FontSize = 11,
            FontWeight = FontWeights.SemiBold,
            Foreground = Brush("TextFillColorTertiaryBrush"),
            Margin = new Thickness(0, 0, 0, 2),
        });

        foreach (MetadataEntry entry in group.Entries)
            panel.Children.Add(BuildRow(entry));

        return panel;
    }

    private FrameworkElement BuildRow(MetadataEntry entry)
    {
        var grid = new Grid { Margin = new Thickness(0, 1, 0, 1) };
        grid.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(96) });
        grid.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });

        var label = new TextBlock
        {
            Text = entry.Label,
            FontSize = 12,
            Foreground = Brush("TextFillColorSecondaryBrush"),
            TextWrapping = TextWrapping.Wrap,
            VerticalAlignment = VerticalAlignment.Top,
        };

        var value = new TextBlock
        {
            FontSize = 12,
            Foreground = Brush("TextFillColorPrimaryBrush"),
            TextWrapping = TextWrapping.Wrap,
        };
        if (entry.IsPath)
        {
            // The file-path row is an inline Hyperlink (hand cursor, wraps like any other value) that reveals the
            // file in Explorer, but reads as plain white text like the other values: the explicit Foreground covers
            // the resting state and the element-scope resources the pointer-over/pressed states.
            Brush plain = Brush("TextFillColorPrimaryBrush");
            var link = new Hyperlink { UnderlineStyle = UnderlineStyle.None, Foreground = plain };
            value.Resources["HyperlinkForeground"] = plain;
            value.Resources["HyperlinkForegroundPointerOver"] = plain;
            value.Resources["HyperlinkForegroundPressed"] = plain;
            link.Inlines.Add(new Run { Text = entry.Value });
            link.Click += (_, _) => RevealRequested?.Invoke(this, EventArgs.Empty);
            value.Inlines.Add(link);
            ToolTipService.SetToolTip(value, "Show in File Explorer");
        }
        else
        {
            value.Text = entry.Value;
            value.IsTextSelectionEnabled = true;
        }
        Grid.SetColumn(value, 1);

        grid.Children.Add(label);
        grid.Children.Add(value);
        return grid;
    }

    private void DrawHistogram(HistogramData histogram)
    {
        double max = Math.Max(1, histogram.Max);

        // Luma as a filled area behind, R/G/B as translucent curves over it.
        HistogramCanvas.Children.Add(MakeArea(histogram.Luma, max, Color.FromArgb(0x55, 0x88, 0x88, 0x88)));
        HistogramCanvas.Children.Add(MakeLine(histogram.Red, max, Color.FromArgb(0xC0, 0xE0, 0x57, 0x4B)));
        HistogramCanvas.Children.Add(MakeLine(histogram.Green, max, Color.FromArgb(0xC0, 0x4C, 0xAF, 0x50)));
        HistogramCanvas.Children.Add(MakeLine(histogram.Blue, max, Color.FromArgb(0xC0, 0x5B, 0x9B, 0xD5)));
    }

    private static Polyline MakeLine(int[] bins, double max, Color color)
    {
        var points = new PointCollection();
        for (int i = 0; i < 256; i++)
            points.Add(new Point(i / 255.0 * HistogramWidth, HistogramHeight - Math.Min(1.0, bins[i] / max) * HistogramHeight));

        return new Polyline
        {
            Points = points,
            Stroke = new SolidColorBrush(color),
            StrokeThickness = 1,
        };
    }

    private static Polygon MakeArea(int[] bins, double max, Color color)
    {
        var points = new PointCollection { new Point(0, HistogramHeight) };
        for (int i = 0; i < 256; i++)
            points.Add(new Point(i / 255.0 * HistogramWidth, HistogramHeight - Math.Min(1.0, bins[i] / max) * HistogramHeight));
        points.Add(new Point(HistogramWidth, HistogramHeight));

        return new Polygon { Points = points, Fill = new SolidColorBrush(color) };
    }

    private static Brush Brush(string themeKey)
        => (Brush)Application.Current.Resources[themeKey];
}
