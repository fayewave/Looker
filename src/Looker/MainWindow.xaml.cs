using System;
using System.Collections.Generic;
using System.IO;
using System.Threading.Tasks;
using Looker.Helpers;
using Looker.Imaging;
using Looker.Navigation;
using Looker.Services;
using Looker.ViewModels;
using Microsoft.UI.Dispatching;
using Microsoft.UI.Windowing;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Input;
using Windows.ApplicationModel.DataTransfer;
using Windows.Storage;
using Windows.Storage.Pickers;
using WinRT.Interop;

namespace Looker;

/// <summary>
/// The application window: chrome (Mica + extended TitleBar) hosting the Win2D viewport, plus the
/// keyboard map wired to the <see cref="MainViewModel"/> (navigation/open) and the viewport
/// (zoom). Also hosts the thumbnail strip and sort menu (M6). Pickers are initialized with the
/// window HWND as WinUI 3 requires.
/// </summary>
public sealed partial class MainWindow : Window
{
    private readonly SettingsService _settings = new();
    private ThemeColors _theme = ThemeColors.For(AppTheme.Black); // what the XAML defaults are drawn for
    private readonly MainViewModel _viewModel;
    private readonly WindowStateService _windowState;
    private readonly SlideshowService _slideshow;

    // True while the rename flyout owns the keyboard: all root accelerators are disabled so typing
    // (including single-letter and Delete keys) reaches the TextBox instead of firing a shortcut.
    private bool _isEditing;

    private DispatcherQueueTimer? _flushTimer;
    private bool _hasContent;
    private bool _isFullscreen;
    private bool _enteredFullscreenForSlideshow;

    // Middle-ellipsis of the title-bar filename: WinUI's TextBlock only trims at the end, so we measure and
    // truncate ourselves whenever the title or the available width changes. _fullTitle is the untruncated
    // name (also the taskbar/alt-tab title); _titleMeasure is an off-tree TextBlock used only for measuring.
    private readonly TextBlock _titleMeasure = new() { FontSize = 14 };
    private string _fullTitle = "Looker";
    private string? _pageTitle; // while a page (Settings) is open the window is titled after it, not the photo

    /// <param name="showEmptyState">False when the app is opening a file straight away: the empty state is
    /// then never created unless the folder later turns out to be empty (see <see cref="ShowEmptyState"/>).</param>
    public MainWindow(bool showEmptyState)
    {
        StartupTrace.Mark("MainWindow ctor entered (Window created)");
        InitializeComponent();
        StartupTrace.Mark("MainWindow.InitializeComponent (XAML tree)");
        // Mouse buttons 4/5 = explorer Back/Forward anywhere in the window. handledEventsToo: a press over a
        // control that handles pointer input (buttons, the strip) must still reach us.
        RootGrid.AddHandler(UIElement.PointerPressedEvent, new PointerEventHandler(OnRootPointerPressed), handledEventsToo: true);

        // Build the panels this session will show BEFORE extending the title bar. Measured: creating them
        // here costs ~25 ms but the ExtendsContentIntoTitleBar step below then runs ~40 ms faster than when the
        // tree holds only the viewport (it evidently shares one-time work with the panels' construction), so
        // this order is a net win — and panels a launch-with-file never shows are still never built.
        _viewModel = new MainViewModel(Viewport, _settings);
        if (_settings.Theme != AppTheme.Black)
            ApplyTheme(_settings.Theme); // RootGrid + viewport now; panels pick the colour up when built (Ensure*)
        Viewport.WheelNavigates = _settings.WheelMode == WheelMode.Navigate;
        Viewport.ZoomAtPointer = _settings.ZoomAnchor == ZoomAnchor.Pointer;
        if (showEmptyState)
            ShowEmptyState();
        if (_settings.InfoVisible)
            EnsureInfoPanel();
        if (_settings.StripVisible)
            EnsureStrip();
        if (_settings.ExplorerVisible)
            EnsureExplorer();
        StartupTrace.Mark("panels for this session created");

        ExtendsContentIntoTitleBar = true;
        StartupTrace.Mark("ExtendsContentIntoTitleBar");
        SetTitleBar(AppTitleBar);
        AppWindow.SetIcon("Assets/AppIcon.ico");
        StartupTrace.Mark("SetTitleBar + AppWindow.SetIcon");
        StartupWarmup.StartDevice(); // Win2D device on the pool from here (see StartupWarmup.StartDevice)

        // No system backdrop (Mica tints from the desktop wallpaper): RootGrid paints a solid black instead, and
        // App.xaml forces the Dark theme + app accent so the look never follows the user's Windows theme colour.

        // Restore placement before the window is activated (App.OnLaunched activates after the ctor).
        _windowState = new WindowStateService(AppWindow, _settings);
        _windowState.Restore();
        StartupTrace.Mark("window placement restored");
        Closed += OnWindowClosed;

        _viewModel.TitleChanged += OnTitleChanged;
        _viewModel.CurrentChanged += OnCurrentChanged;
        _viewModel.InfoUpdated += OnInfoUpdated;
        _viewModel.ContentPresenceChanged += OnContentPresenceChanged;
        _viewModel.OsdRequested += OnOsdRequested;
        _viewModel.StatusUpdated += OnStatusUpdated;
        StyleStatusRevealLink();
        Viewport.ZoomChanged += OnViewportZoomChanged;
        Viewport.ViewChanged += RenderStatus; // zoom readout in the status row follows fit/resize/gestures
        Viewport.RotationPreviewChanged += OnRotationPreviewChanged;
        Viewport.DisplayChanged += UpdatePageBar; // a PDF landing (or leaving) shows/hides the page bar
        Viewport.PageChanged += UpdatePageBar;
        Viewport.WheelStepRequested += (_, direction) => _ = direction > 0 ? _viewModel.NextAsync() : _viewModel.PreviousAsync();

        _slideshow = new SlideshowService(DispatcherQueue) { Interval = TimeSpan.FromSeconds(_settings.SlideshowSeconds) };
        _slideshow.Advance += OnSlideshowAdvance;

        // Ctrl+, (comma) and Ctrl+S have no named VirtualKey enum members we use in XAML, so wire in code.
        AddCodeAccelerator((Windows.System.VirtualKey)0xBC, OnAccelSettings);       // Ctrl+, → settings
        AddCodeAccelerator(Windows.System.VirtualKey.S, OnAccelSaveRotation);       // Ctrl+S → save rotation

        SyncSortMenus();
        SetStripVisible(_settings.StripVisible, persist: false);
        SetExplorerVisible(_settings.ExplorerVisible, persist: false);
        InitializeInfoPanel();
        if (showEmptyState)
            ApplyPanelPresence(false); // panels stay built (cheap to show later) but hidden behind the landing page

        // Re-truncate the filename when the title bar resizes.
        AppTitleBar.SizeChanged += (_, _) => UpdateTitleBarText();
        AppTitleBar.Loaded += (_, _) => ApplyAppFontToTitleBar();
        StartupTrace.Mark("MainWindow ctor end");
    }

    private void OnWindowClosed(object sender, WindowEventArgs e)
    {
        if (!_skipPlacementSave && _settings.RememberWindowPlacement)
            _windowState.Save();
    }

    private void AddCodeAccelerator(Windows.System.VirtualKey key, Windows.Foundation.TypedEventHandler<KeyboardAccelerator, KeyboardAcceleratorInvokedEventArgs> handler)
    {
        var accelerator = new KeyboardAccelerator { Key = key, Modifiers = Windows.System.VirtualKeyModifiers.Control };
        accelerator.Invoked += handler;
        RootGrid.KeyboardAccelerators.Add(accelerator);
    }

    private void InitializeInfoPanel()
    {
        bool visible = _viewModel.InfoVisible;
        InfoToggle.IsChecked = visible;
        if (visible)
        {
            Controls.InfoPanel panel = EnsureInfoPanel();
            panel.Visibility = Visibility.Visible;
            panel.Render(null); // empty state until an image is open
            ApplyInfoInset(panel.PanelWidth);
        }
    }

    /// <summary>Reserve the card's footprint (its width, margins included) for fit/zoom and keep the OSD toast
    /// clear of it; 0 when the card is hidden. Also called live while the card's grip is dragged.</summary>
    private void ApplyInfoInset(double width)
    {
        Viewport.RightInset = width;
        Osd.Margin = new Thickness(16, 16, 16 + width, 16);
        PositionPageBar();
        PositionPlaceholder();
    }

    /// <summary>Reserve the file explorer card's footprint on the left for fit/zoom; 0 when hidden. Also called
    /// live while its grip is dragged. The OSD sits bottom-right, so it needs no shift.</summary>
    private void ApplyExplorerInset(double width)
    {
        Viewport.LeftInset = width;
        PositionPageBar();
        PositionPlaceholder();
    }

    // --- Unsupported-file placeholder (explorer cursor/click on a file Looker can't open) ---

    private Grid? Placeholder;              // opaque cover over the viewport: a file icon and the name
    private StackPanel? _placeholderBody;   // centred in the *inner* viewport (between the two cards)
    private TextBlock? _placeholderName;
    private bool _placeholderShowing;

    private Grid EnsurePlaceholder()
    {
        if (Placeholder is null)
        {
            var secondary = (Microsoft.UI.Xaml.Media.Brush)Application.Current.Resources["TextFillColorSecondaryBrush"];
            var icon = new FontIcon
            {
                Glyph = "\uE7C3", // Page — the same glyph the explorer row shows for these files
                FontSize = 96,
                Foreground = secondary,
                HorizontalAlignment = HorizontalAlignment.Center,
            };
            var name = new TextBlock
            {
                FontSize = 16,
                TextAlignment = TextAlignment.Center,
                TextWrapping = TextWrapping.Wrap,
                MaxWidth = 520,
                HorizontalAlignment = HorizontalAlignment.Center,
                Margin = new Thickness(0, 20, 0, 0),
            };
            var body = new StackPanel { HorizontalAlignment = HorizontalAlignment.Center, VerticalAlignment = VerticalAlignment.Center };
            body.Children.Add(icon);
            body.Children.Add(name);
            // Opaque (window colour) so the photo it stands in for is fully covered, but not hit-testable: wheel
            // navigation, double-click and the context menu keep reaching the viewport underneath.
            var host = new Grid
            {
                Background = new Microsoft.UI.Xaml.Media.SolidColorBrush(_theme.Window),
                IsHitTestVisible = false,
                Visibility = Visibility.Collapsed,
            };
            host.Children.Add(body);
            Grid.SetColumn(host, 0);
            // Right above the viewport, below the cards, the landing page (never shown together) and the OSD.
            ViewportGrid.Children.Insert(ViewportGrid.Children.IndexOf(Viewport) + 1, host);
            Placeholder = host;
            _placeholderBody = body;
            _placeholderName = name;
        }
        PositionPlaceholder();
        return Placeholder;
    }

    private void PositionPlaceholder()
    {
        if (_placeholderBody is { } body)
            body.Margin = new Thickness(Viewport.LeftInset, 0, Viewport.RightInset, 0);
    }

    /// <summary>The explorer landed on a file Looker can't open: cover the photo with a file icon and the name,
    /// and describe the file in the title, status row and position counter. The open image stays current (←/→
    /// continue from it), and any real navigation (<see cref="OnCurrentChanged"/>) takes the cover down.</summary>
    private void ShowFilePlaceholder(string path)
    {
        Grid host = EnsurePlaceholder();
        _placeholderName!.Text = Path.GetFileName(path);
        host.Visibility = Visibility.Visible;
        _placeholderShowing = true;
        Strip?.SelectIndex(-1); // the highlighted thumbnail is not what is on screen any more
        OnTitleChanged(Path.GetFileName(path));
        PositionCounter.Text = _viewModel.PositionLabelFor(path);
        OnStatusUpdated(DescribeFile(path));
    }

    private void HideFilePlaceholder()
    {
        if (!_placeholderShowing)
            return;
        _placeholderShowing = false;
        Placeholder!.Visibility = Visibility.Collapsed;
    }

    /// <summary>Status-row facts for a non-image: "PS1 file · 2.1 KB · Modified …" (no size or zoom).</summary>
    private static StatusInfo DescribeFile(string path)
    {
        string ext = Path.GetExtension(path).TrimStart('.');
        string type = ext.Length > 0 ? ext.ToUpperInvariant() + " file" : "File";
        long size = 0;
        DateTime? modified = null;
        try
        {
            var info = new FileInfo(path);
            if (info.Exists)
            {
                size = info.Length;
                modified = info.LastWriteTime;
            }
        }
        catch (Exception)
        {
            // A vanished or unreadable file still gets its name and type.
        }
        return new StatusInfo(type, 0, 0, size, null, modified);
    }

    // --- Page bar (PDF) ---

    private Controls.PageBar? PageBar;

    /// <summary>The page bar floats bottom-centre of the *inner* viewport (between the two cards), like the
    /// image it belongs to.</summary>
    private void PositionPageBar()
    {
        if (PageBar is { } bar)
            bar.Margin = new Thickness(16 + Viewport.LeftInset, 16, 16 + Viewport.RightInset, 16);
    }

    private Controls.PageBar EnsurePageBar()
    {
        if (PageBar is null)
        {
            var bar = new Controls.PageBar { HorizontalAlignment = HorizontalAlignment.Center, VerticalAlignment = VerticalAlignment.Bottom };
            bar.SetBackground(_theme.Window);
            bar.PreviousRequested += (_, _) => { Viewport.PreviousPage(); Viewport.Focus(FocusState.Programmatic); };
            bar.NextRequested += (_, _) => { Viewport.NextPage(); Viewport.Focus(FocusState.Programmatic); };
            Grid.SetColumn(bar, 0);
            ViewportGrid.Children.Insert(ViewportGrid.Children.IndexOf(Osd), bar); // over the cards, under the toast
            PageBar = bar;
            PositionPageBar();
        }
        return PageBar;
    }

    /// <summary>Show the bar for a multi-page document, hide it for anything else (or behind a page / the
    /// landing page). A single-page PDF gets no bar: there is nothing to turn.</summary>
    private void UpdatePageBar()
    {
        int count = Viewport.PageCount;
        bool show = _panelsPresent && count > 1;
        if (!show)
        {
            if (PageBar is { } hidden)
                hidden.Visibility = Visibility.Collapsed;
            return;
        }
        Controls.PageBar bar = EnsurePageBar();
        bar.SetPage(Viewport.CurrentPage, count);
        bar.Visibility = Visibility.Visible;
    }

    // --- Lazily created panels: the empty state, info panel and thumbnail strip all start hidden (or, for the
    //     empty state, are never seen on a launch-with-file), so they are built on first use instead of being
    //     parsed and constructed on every cold start. Each lives where its XAML declaration used to be.

    private Controls.EmptyState? Empty;
    private Controls.InfoPanel? InfoPanel;
    private Controls.ThumbnailStrip? Strip;
    private Controls.FileExplorer? Explorer;

    /// <summary>Create the empty state (first time only) and show it with a fresh MRU list.</summary>
    private void ShowEmptyState()
    {
        if (Empty is null)
        {
            var empty = new Controls.EmptyState();
            empty.OpenPhotoRequested += (_, _) => _ = OpenFilePickerAsync();
            empty.OpenFolderRequested += (_, _) => _ = OpenFolderPickerAsync();
            empty.RecentRequested += (_, path) => _ = OpenPathAsync(path);
            empty.ClearRecentRequested += (_, _) =>
            {
                _settings.ClearRecent();
                empty.SetRecent(RecentForDisplay());
            };
            empty.RemoveRecentRequested += (_, path) =>
            {
                _settings.RemoveRecent(path);
                empty.SetRecent(RecentForDisplay());
            };
            empty.DefaultHintDismissed += (_, _) => _settings.DefaultHintDismissed = true;
            empty.SetDefaultHintVisible(!_settings.DefaultHintDismissed);
            Grid.SetColumn(empty, 0);
            // Overlays the viewport (column 0) but stays under the OSD toast: insert right after the viewport.
            ViewportGrid.Children.Insert(ViewportGrid.Children.IndexOf(Viewport) + 1, empty);
            Empty = empty;
        }
        Empty.Visibility = Visibility.Visible;
        Empty.SetRecent(RecentForDisplay()); // refresh the MRU list every time the empty state shows
    }

    /// <summary>The recent list the landing page should show: empty (panel hidden) when the feature is off.</summary>
    private IReadOnlyList<string> RecentForDisplay()
        => _settings.RecentsEnabled ? _settings.RecentFiles : Array.Empty<string>();

    private Controls.InfoPanel EnsureInfoPanel()
    {
        if (InfoPanel is null)
        {
            var panel = new Controls.InfoPanel { HorizontalAlignment = HorizontalAlignment.Right, PanelWidth = _settings.InfoWidth };
            panel.SetCardBackground(_theme.Window);
            panel.RevealRequested += (_, _) => _viewModel.Reveal(); // the path row: show the file in Explorer
            // The image fit and the OSD follow the width live during a grip drag; the width the user settles on is
            // persisted when the gesture ends (like the strip's height).
            panel.SizeChanged += (_, e) =>
            {
                if (panel.Visibility == Visibility.Visible)
                    ApplyInfoInset(e.NewSize.Width);
            };
            panel.PanelWidthChanged += (_, w) => _settings.InfoWidth = (int)Math.Round(w);
            // Floats over the viewport (column 0) so the checkerboard shows around its card; the viewport's
            // RightInset keeps the image fitting in the space to its left. Inserted below the OSD toast.
            Grid.SetColumn(panel, 0);
            ViewportGrid.Children.Insert(ViewportGrid.Children.IndexOf(Osd), panel);
            InfoPanel = panel;
        }
        return InfoPanel;
    }

    private Controls.FileExplorer EnsureExplorer()
    {
        if (Explorer is null)
        {
            var explorer = new Controls.FileExplorer { HorizontalAlignment = HorizontalAlignment.Left, PanelWidth = _settings.ExplorerWidth };
            explorer.SetCardBackground(_theme.Window);
            explorer.SetSort(_viewModel.Sort);
            // The image fit follows the width live during a grip drag; the width the user settles on is persisted
            // when the gesture ends (like the info card).
            explorer.SizeChanged += (_, e) =>
            {
                if (explorer.Visibility == Visibility.Visible)
                    ApplyExplorerInset(e.NewSize.Width);
            };
            explorer.PanelWidthChanged += (_, w) => _settings.ExplorerWidth = (int)Math.Round(w);
            explorer.FileActivated += path => _ = ShowFileFromExplorerAsync(path);
            explorer.ActionRequested += OnExplorerAction;
            // Floats over the viewport (column 0), left-aligned, mirroring the info card on the right; the
            // viewport's LeftInset keeps the image fitting in the space to its right. Inserted below the OSD toast.
            Grid.SetColumn(explorer, 0);
            ViewportGrid.Children.Insert(ViewportGrid.Children.IndexOf(Osd), explorer);
            Explorer = explorer;
        }
        return Explorer;
    }

    private Controls.ThumbnailStrip EnsureStrip()
    {
        if (Strip is null)
        {
            var strip = new Controls.ThumbnailStrip { StripHeight = _settings.StripHeight };
            strip.SetFadeColor(_theme.Window);
            // Persist the height the user dragged the strip to (raised only when a resize gesture ends).
            strip.StripHeightChanged += (_, h) => _settings.StripHeight = (int)Math.Round(h);
            strip.Transitions = new Microsoft.UI.Xaml.Media.Animation.TransitionCollection
            {
                new Microsoft.UI.Xaml.Media.Animation.EntranceThemeTransition { FromVerticalOffset = strip.StripHeight },
            };
            strip.SelectionActivated += OnStripSelectionActivated;
            strip.SetSource(_viewModel.Thumbnails);
            Grid.SetRow(strip, 3);
            RootGrid.Children.Add(strip);
            Strip = strip;
        }
        return Strip;
    }

    // --- Full-window pages (Settings, About). They take the landing page's slot over the viewport rather than
    //     being modal dialogs, so they read as places in the app; one AppPage instance is reused for both. ---

    private Controls.AppPage? Page;
    private bool _pageOpen;
    private Action? _pageOnClose;      // e.g. detach the About page's update-status subscription
    private bool _statusRowWasVisible; // the status line is hidden behind a page and restored after

    private Controls.AppPage EnsurePage()
    {
        if (Page is null)
        {
            var page = new Controls.AppPage { Visibility = Visibility.Collapsed };
            // Opaque: a page covers the photo behind it (the landing page can stay transparent because the
            // viewport paints the window colour itself when nothing is open).
            page.SetBackground(_theme.Window);
            page.BackRequested += (_, _) => ClosePage();
            Grid.SetColumn(page, 0);
            // Above the info card and the landing page, below the OSD toast.
            ViewportGrid.Children.Insert(ViewportGrid.Children.IndexOf(Osd), page);
            Page = page;
        }
        return Page;
    }

    /// <summary>Show <paramref name="content"/> as a page. Called again while one is open (More ▸ About from
    /// Settings), it swaps the content and runs the previous page's <paramref name="onClose"/>.</summary>
    private void ShowPage(string title, UIElement content, Action? onClose = null)
    {
        _pageOnClose?.Invoke();
        _pageOnClose = onClose;

        Controls.AppPage page = EnsurePage();
        page.Show(title, content);
        page.Visibility = Visibility.Visible;
        _pageTitle = title;
        ApplyWindowTitle();

        if (!_pageOpen)
        {
            _pageOpen = true;
            SettingsToggle.IsChecked = true;
            _statusRowWasVisible = StatusRow.Visibility == Visibility.Visible;
            // Everything that frames a photo steps aside: landing page, strip, info card, status line, and the
            // image toolbar is disabled (the More menu stays live so About/Settings reach each other).
            if (Empty is { } empty)
                empty.Visibility = Visibility.Collapsed;
            StatusRow.Visibility = Visibility.Collapsed;
            UpdatePanelPresence();
            // Single-key accelerators (T, I, Del, Space…) must not fire while the user types in a settings
            // field. AppPage handles Escape itself for this reason.
            SetAcceleratorsEnabled(false);
        }
        page.FocusBack();
    }

    private void ClosePage()
    {
        if (!_pageOpen)
            return;
        _pageOpen = false;
        _pageOnClose?.Invoke();
        _pageOnClose = null;
        _pageTitle = null;
        ApplyWindowTitle();
        SettingsToggle.IsChecked = false;
        if (Page is { } page)
        {
            page.Visibility = Visibility.Collapsed;
            page.Clear(); // don't retain the body (and its handlers) while hidden
        }
        SetAcceleratorsEnabled(true);
        if (_hasContent)
            StatusRow.Visibility = _statusRowWasVisible ? Visibility.Visible : Visibility.Collapsed;
        else
            ShowEmptyState();
        UpdatePanelPresence();
        Viewport.Focus(FocusState.Programmatic);
    }

    private void OnRootLoaded(object sender, RoutedEventArgs e)
    {
        StartupTrace.Mark("RootGrid Loaded");
        _flushTimer = DispatcherQueue.CreateTimer();
        _flushTimer.Interval = TimeSpan.FromSeconds(1.2);
        _flushTimer.IsRepeating = false;
        _flushTimer.Tick += (_, _) =>
        {
            StartupTrace.Flush("1.2 s after Loaded", final: true);
            // Startup has settled: ask the Store (network) whether a newer version is published.
            UpdateService.StatusChanged += OnUpdateStatusChanged;
            _ = UpdateService.CheckAsync();
        };
        _flushTimer.Start();
        // Park focus on the viewport so the accelerators fire without a first click.
        Viewport.Focus(FocusState.Programmatic);
    }

    private void OnCurrentChanged(int index)
    {
        HideFilePlaceholder();
        Strip?.SelectIndex(index);
        SyncExplorer();
        PositionCounter.Text = _viewModel.PositionLabel; // "3 / 128" at the right end of the toolbar row
    }

    // --- File explorer (E) ---

    /// <summary>Point the explorer at the open folder and current file (a highlight move when the folder is
    /// already in its tree). Skipped while hidden: showing it re-syncs.</summary>
    private void SyncExplorer()
    {
        if (Explorer is { Visibility: Visibility.Visible } explorer)
            _ = explorer.ShowAsync(_viewModel.FolderPath, _viewModel.CurrentPath);
    }

    private void OnAccelToggleExplorer(KeyboardAccelerator sender, KeyboardAcceleratorInvokedEventArgs args)
    {
        args.Handled = true;
        if (_panelsPresent)
            SetExplorerVisible(!IsExplorerVisible, persist: true);
    }

    private bool IsExplorerVisible => Explorer is { Visibility: Visibility.Visible };

    private void OnAccelExplorerUp(KeyboardAccelerator sender, KeyboardAcceleratorInvokedEventArgs args)
        => args.Handled = MoveExplorerCursor(-1);

    private void OnAccelExplorerDown(KeyboardAccelerator sender, KeyboardAcceleratorInvokedEventArgs args)
        => args.Handled = MoveExplorerCursor(+1);

    private void OnAccelExplorerEnter(KeyboardAccelerator sender, KeyboardAcceleratorInvokedEventArgs args)
        => args.Handled = IsExplorerVisible && Explorer!.ActivateCursor();

    /// <summary>Up/Down walk the explorer rows while the card shows; an image row opens as the cursor lands on it.</summary>
    private bool MoveExplorerCursor(int delta)
    {
        if (!IsExplorerVisible)
            return false;
        Explorer!.MoveCursor(delta);
        ResetSlideshowIfRunning();
        return true;
    }

    private void SetExplorerVisible(bool visible, bool persist)
    {
        if (visible)
        {
            Controls.FileExplorer explorer = EnsureExplorer();
            explorer.Visibility = Visibility.Visible;
            _ = explorer.ShowAsync(_viewModel.FolderPath, _viewModel.CurrentPath);
        }
        else if (Explorer is { } explorer)
        {
            explorer.Visibility = Visibility.Collapsed; // never realized yet ⇒ already hidden
        }
        ExplorerToggle.IsChecked = visible; // keep the toolbar toggle's "selected" state in sync
        ApplyExplorerInset(visible && Explorer is { } shown ? shown.PanelWidth : 0);
        if (persist)
            _settings.ExplorerVisible = visible;
    }

    /// <summary>A file row clicked (or arrowed onto) in the explorer: an image is a jump within the open folder or
    /// an open of another folder; anything else gets the placeholder preview.</summary>
    private async Task ShowFileFromExplorerAsync(string path)
    {
        if (!SupportedFormats.IsSupported(path))
        {
            ShowFilePlaceholder(path);
            ResetSlideshowIfRunning();
            Viewport.Focus(FocusState.Programmatic);
            return;
        }
        try
        {
            // Asking for the image that is already current while the placeholder covers it must re-announce
            // it (CurrentChanged/title/status) rather than no-op, or the cover would stay up.
            await _viewModel.ShowFileAsync(path, reshow: _placeholderShowing);
        }
        catch (Exception ex)
        {
            Osd.Show($"Can't open {Path.GetFileName(path)}: {ex.Message}");
            return;
        }
        ResetSlideshowIfRunning();
        Viewport.Focus(FocusState.Programmatic);
    }

    /// <summary>The explorer's context menu. Rename and delete reuse the viewer's dialogs on the chosen path.</summary>
    private void OnExplorerAction(Controls.ExplorerAction action, string path)
    {
        switch (action)
        {
            case Controls.ExplorerAction.Open:
                _ = Directory.Exists(path) ? OpenPathAsync(path) : ShowFileFromExplorerAsync(path);
                break;
            case Controls.ExplorerAction.Rename:
                _ = BeginRenameAsync(path);
                break;
            case Controls.ExplorerAction.Delete:
                _ = ConfirmAndDeleteAsync(path);
                break;
            case Controls.ExplorerAction.CopyPath:
                _viewModel.CopyPath(path);
                break;
            case Controls.ExplorerAction.Reveal:
                _viewModel.Reveal(path);
                break;
        }
    }

    // --- Status row (always on, under the strip) ---

    private StatusInfo? _status; // last status from the view model; re-rendered when the zoom changes

    private void OnStatusUpdated(StatusInfo? status)
    {
        _status = status;
        RenderStatus();
    }

    /// <summary>The status row's "Open in Explorer" link: the current file, selected, in File Explorer.</summary>
    private void OnStatusReveal(Microsoft.UI.Xaml.Documents.Hyperlink sender, Microsoft.UI.Xaml.Documents.HyperlinkClickEventArgs args)
        => _viewModel.Reveal();

    /// <summary>The link reads as the rest of the status row (secondary grey) and brightens under the pointer.
    /// Hyperlink takes its state brushes from element-scope resources, so they are set on the TextBlock.</summary>
    private void StyleStatusRevealLink()
    {
        StatusReveal.Resources["HyperlinkForeground"] = Application.Current.Resources["TextFillColorSecondaryBrush"];
        StatusReveal.Resources["HyperlinkForegroundPointerOver"] = Application.Current.Resources["TextFillColorPrimaryBrush"];
        StatusReveal.Resources["HyperlinkForegroundPressed"] = Application.Current.Resources["TextFillColorTertiaryBrush"];
    }

    private void RenderStatus()
    {
        StatusInfo? status = _status;
        if (status is null)
        {
            StatusText.Text = string.Empty;
            StatusRow.Visibility = Visibility.Collapsed;
            return;
        }

        var parts = new List<string>(6);
        if (status.Type is { } type)
            parts.Add(type);
        if (status.PageCount > 0)
            parts.Add(status.PageCount == 1 ? "1 page" : $"{status.PageCount} pages");
        if (status.Width > 0 && status.Height > 0)
        {
            parts.Add($"{status.Width} × {status.Height}");
            if (status.PageCount == 0) // a PDF page's 96-dpi size is not a pixel count
                parts.Add($"{status.Width * (long)status.Height / 1_000_000.0:0.0} MP");
        }
        if (status.SizeBytes > 0)
            parts.Add(MetadataService.FormatBytes(status.SizeBytes));
        if (status.Taken is { } taken)
            parts.Add($"Taken {taken:g}");
        else if (status.Modified is { } modified)
            parts.Add($"Modified {modified:g}");
        if (status.Width > 0)
            parts.Add($"{Viewport.ZoomPercent:0}%"); // only once the image is up: before that the zoom is stale

        StatusText.Text = string.Join("   ·   ", parts);
        if (!_pageOpen) // a page (Settings) hides the row and restores it itself on close
            StatusRow.Visibility = Visibility.Visible;
    }

    // --- Thumbnail strip ---

    private void OnStripSelectionActivated(object? sender, int index)
    {
        _ = _viewModel.GoToIndexAsync(index);
        Viewport.Focus(FocusState.Programmatic);
    }

    private void OnAccelToggleStrip(KeyboardAccelerator sender, KeyboardAcceleratorInvokedEventArgs args)
    {
        args.Handled = true;
        if (_panelsPresent)
            SetStripVisible(!IsStripVisible, persist: true);
    }

    private bool IsStripVisible => Strip is { Visibility: Visibility.Visible };

    private void SetStripVisible(bool visible, bool persist)
    {
        if (visible)
        {
            Controls.ThumbnailStrip strip = EnsureStrip();
            strip.Visibility = Visibility.Visible;
            strip.SelectIndex(_viewModel.CurrentIndex);
        }
        else if (Strip is { } strip)
        {
            strip.Visibility = Visibility.Collapsed; // never realized yet ⇒ already hidden
        }
        StripToggle.IsChecked = visible; // keep the toolbar toggle's "selected" state in sync
        if (persist)
            _settings.StripVisible = visible;
    }

    // --- Sort menu (title-bar dropdown) ---

    private void SyncSortMenus()
    {
        SortMode sort = _viewModel.Sort;
        SortName.IsChecked = sort.Field == SortField.Name;
        SortDate.IsChecked = sort.Field == SortField.DateModified;
        SortSize.IsChecked = sort.Field == SortField.Size;
        SortAsc.IsChecked = sort.Direction == SortDirection.Ascending;
        SortDesc.IsChecked = sort.Direction == SortDirection.Descending;
    }

    private void OnSortName(object sender, RoutedEventArgs e) => ApplySortField(SortField.Name);
    private void OnSortDate(object sender, RoutedEventArgs e) => ApplySortField(SortField.DateModified);
    private void OnSortSize(object sender, RoutedEventArgs e) => ApplySortField(SortField.Size);
    private void OnSortAscending(object sender, RoutedEventArgs e) => ApplySortDirection(SortDirection.Ascending);
    private void OnSortDescending(object sender, RoutedEventArgs e) => ApplySortDirection(SortDirection.Descending);

    private void ApplySortField(SortField field) => ApplySort(_viewModel.Sort with { Field = field });

    private void ApplySortDirection(SortDirection direction) => ApplySort(_viewModel.Sort with { Direction = direction });

    /// <summary>One sort for the nav order, the strip and the explorer's folders and files alike.</summary>
    private void ApplySort(SortMode sort)
    {
        _ = _viewModel.SetSortAsync(sort);
        Explorer?.SetSort(sort);
        SyncSortMenus();
        Viewport.Focus(FocusState.Programmatic);
    }

    private void OnTitleChanged(string title)
    {
        _fullTitle = title;
        ApplyWindowTitle();
    }

    private void ApplyWindowTitle()
    {
        Title = _pageTitle ?? _fullTitle; // taskbar / alt-tab keep the full, untruncated name
        UpdateTitleBarText();
    }

    // Set the visible title bar text to the filename (or the open page's title), middle-ellipsized to whatever
    // width is left after the icon and the toolbar. Keeping the first and last characters lets the user still
    // read the name + extension.
    private void UpdateTitleBarText()
    {
        AppTitleBar.Title = MiddleEllipsize(_pageTitle ?? _fullTitle, AvailableTitleWidth());
    }

    // The WinUI TitleBar template styles its title/subtitle TextBlocks with CaptionTextBlockStyle resolved from
    // inside generic.xaml, which hard-codes the system font and bypasses both ContentControlThemeFontFamily and
    // the app-level style overrides in App.xaml. Set the app font on those TextBlocks directly once the template
    // has loaded (a local value outranks the style setter, and it persists across later Title changes).
    private void ApplyAppFontToTitleBar()
    {
        if (Application.Current.Resources["ContentControlThemeFontFamily"] is not Microsoft.UI.Xaml.Media.FontFamily font)
            return;
        ApplyFont(AppTitleBar);

        void ApplyFont(DependencyObject node)
        {
            int count = Microsoft.UI.Xaml.Media.VisualTreeHelper.GetChildrenCount(node);
            for (int i = 0; i < count; i++)
            {
                var child = Microsoft.UI.Xaml.Media.VisualTreeHelper.GetChild(node, i);
                if (child is TextBlock text)
                    text.FontFamily = font;
                else
                    ApplyFont(child);
            }
        }
    }

    // Lightweight-styling overrides for white text on accent-filled surfaces (the Dark theme defaults to
    // black). RootGrid.Resources carries the same six brushes for everything in the window tree, but popups
    // (ContentDialog, Flyout) render in their own root and never see RootGrid, so their content gets the
    // overrides on its own Resources. App.xaml-level overrides of these keys do not work (measured).
    /// <summary>A grey button with a Segoe Fluent glyph before its label (About / Updates rows).</summary>
    private static Button IconButton(string glyph, string text)
    {
        var content = new StackPanel { Orientation = Orientation.Horizontal, Spacing = 8 };
        content.Children.Add(new FontIcon { Glyph = glyph, FontSize = 14, VerticalAlignment = VerticalAlignment.Center });
        content.Children.Add(new TextBlock { Text = text, VerticalAlignment = VerticalAlignment.Center });
        return new Button { Content = content };
    }

    private void WhiteOnAccent(FrameworkElement element)
    {
        static Microsoft.UI.Xaml.Media.SolidColorBrush Rgba(byte a, byte r, byte g, byte b)
            => new(Windows.UI.Color.FromArgb(a, r, g, b));
        var white = Rgba(0xFF, 0xFF, 0xFF, 0xFF);
        var pressed = Rgba(0xCC, 0xFF, 0xFF, 0xFF);
        element.Resources["ToggleButtonForegroundChecked"] = white;
        element.Resources["ToggleButtonForegroundCheckedPointerOver"] = white;
        element.Resources["ToggleButtonForegroundCheckedPressed"] = pressed;
        element.Resources["AccentButtonForeground"] = white;
        element.Resources["AccentButtonForegroundPointerOver"] = white;
        element.Resources["AccentButtonForegroundPressed"] = pressed;

        // Same grey button fill as the toolbar (RootGrid.Resources carries these for the window tree; popups
        // have their own root so they need their own copy).
        element.Resources["ButtonBackground"] = Rgba(0x24, 0xFF, 0xFF, 0xFF);
        element.Resources["ButtonBackgroundPointerOver"] = Rgba(0x33, 0xFF, 0xFF, 0xFF);
        element.Resources["ButtonBackgroundPressed"] = Rgba(0x1C, 0xFF, 0xFF, 0xFF);
        element.Resources["ToggleButtonBackground"] = Rgba(0x24, 0xFF, 0xFF, 0xFF);
        element.Resources["ToggleButtonBackgroundPointerOver"] = Rgba(0x33, 0xFF, 0xFF, 0xFF);
        element.Resources["ToggleButtonBackgroundPressed"] = Rgba(0x1C, 0xFF, 0xFF, 0xFF);

        // Dialog body a step darker than the stock #202020 layer (per theme): content area over a darker button strip.
        if (element is ContentDialog dialog)
        {
            var strip = new Microsoft.UI.Xaml.Media.SolidColorBrush(_theme.DialogStrip);
            dialog.Background = strip;
            element.Resources["ContentDialogBackground"] = strip;
            element.Resources["ContentDialogTopOverlay"] = new Microsoft.UI.Xaml.Media.SolidColorBrush(_theme.DialogOverlay);
        }
    }

    private double AvailableTitleWidth()
    {
        double total = AppTitleBar.ActualWidth;
        if (total <= 0)
            return double.PositiveInfinity; // not laid out yet — show full; a SizeChanged will refine it

        // Reserve the left icon/title padding and the right-hand caption buttons + min-drag region.
        // Deliberately a little conservative so our truncation lands before WinUI's own end-ellipsis would.
        const double leftInset = 52;
        const double captionInset = 160;
        return total - leftInset - captionInset;
    }

    private string MiddleEllipsize(string text, double maxWidth)
    {
        if (double.IsPositiveInfinity(maxWidth) || MeasureText(text) <= maxWidth)
            return text;

        // Shrink the kept character count (split across front/back) until the ellipsized form fits.
        for (int keep = text.Length - 1; keep >= 2; keep--)
        {
            int front = (keep + 1) / 2;
            int back = keep - front;
            string candidate = $"{text[..front]}…{text[^back..]}";
            if (MeasureText(candidate) <= maxWidth)
                return candidate;
        }
        return text.Length > 0 ? $"{text[..1]}…" : text;
    }

    private double MeasureText(string text)
    {
        _titleMeasure.Text = text;
        _titleMeasure.Measure(new Windows.Foundation.Size(double.PositiveInfinity, double.PositiveInfinity));
        return _titleMeasure.DesiredSize.Width;
    }

    // --- Navigation ---

    private void OnAccelPrevious(KeyboardAccelerator sender, KeyboardAcceleratorInvokedEventArgs args)
    {
        args.Handled = true;
        _ = _viewModel.PreviousAsync();
        ResetSlideshowIfRunning();
    }

    private void OnAccelNext(KeyboardAccelerator sender, KeyboardAcceleratorInvokedEventArgs args)
    {
        args.Handled = true;
        _ = _viewModel.NextAsync();
        ResetSlideshowIfRunning();
    }

    // Page Up / Page Down turn PDF pages; for anything else they stay unhandled so they route on normally.
    private void OnAccelPreviousPage(KeyboardAccelerator sender, KeyboardAcceleratorInvokedEventArgs args)
    {
        if (Viewport.PageCount <= 1)
            return;
        args.Handled = true;
        Viewport.PreviousPage();
    }

    private void OnAccelNextPage(KeyboardAccelerator sender, KeyboardAcceleratorInvokedEventArgs args)
    {
        if (Viewport.PageCount <= 1)
            return;
        args.Handled = true;
        Viewport.NextPage();
    }

    private void OnAccelFirst(KeyboardAccelerator sender, KeyboardAcceleratorInvokedEventArgs args)
    {
        args.Handled = true;
        _ = _viewModel.FirstAsync();
        ResetSlideshowIfRunning();
    }

    private void OnAccelLast(KeyboardAccelerator sender, KeyboardAcceleratorInvokedEventArgs args)
    {
        args.Handled = true;
        _ = _viewModel.LastAsync();
        ResetSlideshowIfRunning();
    }

    private void ResetSlideshowIfRunning()
    {
        if (_slideshow.IsRunning)
            _slideshow.Reset();
    }

    // --- Zoom (straight to the viewport) ---

    private void OnAccelZoomIn(KeyboardAccelerator sender, KeyboardAcceleratorInvokedEventArgs args)
    {
        args.Handled = true;
        Viewport.ZoomInStep();
    }

    private void OnAccelZoomOut(KeyboardAccelerator sender, KeyboardAcceleratorInvokedEventArgs args)
    {
        args.Handled = true;
        Viewport.ZoomOutStep();
    }

    private void OnAccelFit(KeyboardAccelerator sender, KeyboardAcceleratorInvokedEventArgs args)
    {
        args.Handled = true;
        Viewport.FitToWindow();
    }

    private void OnAccelPlayPause(KeyboardAccelerator sender, KeyboardAcceleratorInvokedEventArgs args)
    {
        args.Handled = true;
        if (_slideshow.IsRunning)
        {
            if (_slideshow.IsPaused)
            {
                _slideshow.Resume();
                Osd.Show("Playing");
            }
            else
            {
                _slideshow.Pause();
                Osd.Show("Paused");
            }
        }
        else
        {
            Viewport.ToggleAnimationPause();
        }
    }

    private void OnAccelActualSize(KeyboardAccelerator sender, KeyboardAcceleratorInvokedEventArgs args)
    {
        args.Handled = true;
        Viewport.ZoomActualSize();
    }

    // --- Info panel (I) ---

    private void OnAccelToggleInfo(KeyboardAccelerator sender, KeyboardAcceleratorInvokedEventArgs args)
    {
        args.Handled = true;
        if (_panelsPresent)
            SetInfoVisible(!_viewModel.InfoVisible);
    }

    private void SetInfoVisible(bool visible)
    {
        _viewModel.InfoVisible = visible; // setter persists and drives the refresh/clear
        ApplyInfoVisible(visible);
    }

    /// <summary>Show or hide the info card without touching the saved preference (the landing page hides it).</summary>
    private void ApplyInfoVisible(bool visible)
    {
        if (visible)
        {
            Controls.InfoPanel panel = EnsureInfoPanel();
            panel.Visibility = Visibility.Visible;
            panel.Render(null); // show the empty state immediately; the snapshot follows via InfoUpdated
        }
        else if (InfoPanel is { } panel)
        {
            panel.Visibility = Visibility.Collapsed;
        }
        InfoToggle.IsChecked = visible; // keep the toolbar toggle's "selected" state in sync
        ApplyInfoInset(visible && InfoPanel is { } shown ? shown.PanelWidth : 0);
        Viewport.Focus(FocusState.Programmatic);
    }

    private void OnInfoUpdated(InfoSnapshot? snapshot) => InfoPanel?.Render(snapshot);

    // --- File management (M7) ---

    private void OnAccelDelete(KeyboardAccelerator sender, KeyboardAcceleratorInvokedEventArgs args)
    {
        args.Handled = true;
        _ = ConfirmAndDeleteAsync();
    }

    // Confirm before recycling. The dialog is modal and accelerators are disabled while it's open, so the
    // Delete key can't re-trigger it. Only on confirmation does the view model recycle + advance.
    /// <param name="target">A file chosen in the explorer; null = the current image.</param>
    private async Task ConfirmAndDeleteAsync(string? target = null)
    {
        string? path = target ?? _viewModel.CurrentPath;
        if (path is null)
            return;

        var dialog = new ContentDialog
        {
            Title = "Delete photo",
            Content = $"Move \"{Path.GetFileName(path)}\" to the Recycle Bin?",
            PrimaryButtonText = "Delete",
            CloseButtonText = "Cancel",
            DefaultButton = ContentDialogButton.Close,
            XamlRoot = Content.XamlRoot,
        };
        WhiteOnAccent(dialog);

        SetAcceleratorsEnabled(false);
        try
        {
            if (await dialog.ShowAsync() == ContentDialogResult.Primary)
                await _viewModel.DeleteAsync(path);
        }
        finally
        {
            SetAcceleratorsEnabled(true);
            Viewport.Focus(FocusState.Programmatic);
        }
    }

    private void OnAccelRotateRight(KeyboardAccelerator sender, KeyboardAcceleratorInvokedEventArgs args)
    {
        args.Handled = true;
        _viewModel.RotateCurrent(clockwise: true); // preview only; commit with Ctrl+S / Save
    }

    private void OnAccelRotateLeft(KeyboardAccelerator sender, KeyboardAcceleratorInvokedEventArgs args)
    {
        args.Handled = true;
        _viewModel.RotateCurrent(clockwise: false);
    }

    private void OnAccelSaveRotation(KeyboardAccelerator sender, KeyboardAcceleratorInvokedEventArgs args)
    {
        args.Handled = true;
        _ = _viewModel.SaveRotationAsync();
    }

    private void OnAccelCopyImage(KeyboardAccelerator sender, KeyboardAcceleratorInvokedEventArgs args)
    {
        args.Handled = true;
        _ = _viewModel.CopyImageAsync();
    }

    private void OnAccelCopyPath(KeyboardAccelerator sender, KeyboardAcceleratorInvokedEventArgs args)
    {
        args.Handled = true;
        _viewModel.CopyPath();
    }

    private void OnAccelReveal(KeyboardAccelerator sender, KeyboardAcceleratorInvokedEventArgs args)
    {
        args.Handled = true;
        _viewModel.Reveal();
    }

    // --- Context menu (right-click). Each mirrors an accelerator; wallpaper is menu-only. ---

    private void OnMenuRotateRight(object sender, RoutedEventArgs e) => _viewModel.RotateCurrent(clockwise: true);
    private void OnMenuRotateLeft(object sender, RoutedEventArgs e) => _viewModel.RotateCurrent(clockwise: false);
    private void OnMenuCopyImage(object sender, RoutedEventArgs e) => _ = _viewModel.CopyImageAsync();
    private void OnMenuCopyPath(object sender, RoutedEventArgs e) => _viewModel.CopyPath();
    private void OnMenuRename(object sender, RoutedEventArgs e) => BeginRename();
    private void OnMenuDelete(object sender, RoutedEventArgs e) => _ = ConfirmAndDeleteAsync();
    private void OnMenuWallpaper(object sender, RoutedEventArgs e) => _ = _viewModel.SetWallpaperAsync();
    private void OnMenuReveal(object sender, RoutedEventArgs e) => _viewModel.Reveal();
    private void OnMenuToggleInfo(object sender, RoutedEventArgs e)
    {
        if (_panelsPresent)
            SetInfoVisible(!_viewModel.InfoVisible);
    }

    private void OnMenuToggleStrip(object sender, RoutedEventArgs e)
    {
        if (_panelsPresent)
            SetStripVisible(!IsStripVisible, persist: true);
    }

    private void OnMenuToggleExplorer(object sender, RoutedEventArgs e)
    {
        if (_panelsPresent)
            SetExplorerVisible(!IsExplorerVisible, persist: true);
    }

    // --- Drag and drop: files or folders from Explorer (or any shell source) opened by dropping them anywhere
    //     on the window. RootGrid carries AllowDrop, so the viewport, landing page, strip and status row all
    //     funnel here through routed-event bubbling. ---

    /// <summary>Mouse buttons 4 / 5 (the side "back"/"forward" buttons) step the file explorer's folder history
    /// while it is showing; they are ignored otherwise.</summary>
    private void OnRootPointerPressed(object sender, PointerRoutedEventArgs e)
    {
        if (!IsExplorerVisible || Page is not null)
            return;
        var props = e.GetCurrentPoint(null).Properties;
        if (props.IsXButton1Pressed)
            Explorer!.GoBack();
        else if (props.IsXButton2Pressed)
            Explorer!.GoForward();
        else
            return;
        e.Handled = true;
    }

    private void OnRootDragOver(object sender, DragEventArgs e)
    {
        if (!e.DataView.Contains(StandardDataFormats.StorageItems))
            return; // AcceptedOperation stays None: the shell shows the "can't drop here" cursor

        e.AcceptedOperation = DataPackageOperation.Copy; // Copy = "open a reference"; never moves the file
        if (e.DragUIOverride is { } ui)
        {
            ui.Caption = "Open in Looker";
            ui.IsCaptionVisible = true;
            ui.IsGlyphVisible = true;
        }
        e.Handled = true;
    }

    private async void OnRootDrop(object sender, DragEventArgs e)
    {
        if (!e.DataView.Contains(StandardDataFormats.StorageItems))
            return;
        e.Handled = true;

        // The data view is only readable until the handler returns; awaiting without a deferral loses it.
        var deferral = e.GetDeferral();
        try
        {
            IReadOnlyList<IStorageItem> items = await e.DataView.GetStorageItemsAsync();
            if (items.Count == 0)
                return;

            // A folder wins over loose files, then the first image Looker can decode. Dropping several files
            // opens the first supported one; the rest of the folder is a keypress away anyway.
            IStorageItem? target = null;
            foreach (IStorageItem item in items)
            {
                if (item is StorageFolder)
                {
                    target = item;
                    break;
                }
                if (target is null && item is StorageFile file && SupportedFormats.IsSupported(file.Path))
                    target = file;
            }

            if (target is null)
            {
                Osd.Show(items.Count == 1
                    ? $"Can't open {Path.GetFileName(items[0].Path)}"
                    : "Nothing droppable there");
                return;
            }

            await OpenPathAsync(target.Path);
        }
        catch (Exception ex)
        {
            Osd.Show($"Can't open that: {ex.Message}");
        }
        finally
        {
            deferral.Complete();
        }
    }

    // --- Rename (F2). A centered ContentDialog: as a Flyout it was anchored on the full-window viewport,
    //     which WinUI placed at the bottom edge of the window. Accelerators are disabled while it is open. ---

    private void OnAccelRename(KeyboardAccelerator sender, KeyboardAcceleratorInvokedEventArgs args)
    {
        args.Handled = true;
        BeginRename();
    }

    private void BeginRename() => _ = BeginRenameAsync();

    /// <param name="target">A file chosen in the explorer; null = the current image.</param>
    private async Task BeginRenameAsync(string? target = null)
    {
        string? path = target ?? _viewModel.CurrentPath;
        if (path is null || _isEditing)
            return;

        string name = Path.GetFileName(path);
        var textBox = new TextBox { Text = name, MinWidth = 320 };

        var dialog = new ContentDialog
        {
            Title = "Rename file",
            Content = textBox,
            PrimaryButtonText = "Rename",
            CloseButtonText = "Cancel",
            DefaultButton = ContentDialogButton.Primary,
            XamlRoot = Content.XamlRoot,
        };
        WhiteOnAccent(dialog);

        // Select just the base name so the user can retype without clobbering the extension.
        textBox.Loaded += (_, _) =>
        {
            int dot = name.LastIndexOf('.');
            textBox.Select(0, dot > 0 ? dot : name.Length);
            textBox.Focus(FocusState.Programmatic);
        };
        // A single-line TextBox doesn't handle Enter, so it bubbles to the dialog's default button; Escape is
        // the dialog's own close key. Empty input can't commit.
        textBox.TextChanged += (_, _) => dialog.IsPrimaryButtonEnabled = !string.IsNullOrWhiteSpace(textBox.Text);

        _isEditing = true;
        SetAcceleratorsEnabled(false);
        try
        {
            if (await dialog.ShowAsync() == ContentDialogResult.Primary && !string.IsNullOrWhiteSpace(textBox.Text))
                await _viewModel.RenameAsync(path, textBox.Text);
        }
        finally
        {
            EndRename();
        }
    }

    private void EndRename()
    {
        if (!_isEditing)
            return;
        _isEditing = false;
        SetAcceleratorsEnabled(true);
        Viewport.Focus(FocusState.Programmatic);
    }

    private void SetAcceleratorsEnabled(bool enabled)
    {
        foreach (KeyboardAccelerator accelerator in RootGrid.KeyboardAccelerators)
            accelerator.IsEnabled = enabled;
    }

    // --- Open ---

    private void OnAccelOpenFile(KeyboardAccelerator sender, KeyboardAcceleratorInvokedEventArgs args)
    {
        args.Handled = true;
        _ = OpenFilePickerAsync();
    }

    private void OnAccelOpenFolder(KeyboardAccelerator sender, KeyboardAcceleratorInvokedEventArgs args)
    {
        args.Handled = true;
        _ = OpenFolderPickerAsync();
    }

    private async Task OpenFilePickerAsync()
    {
        var picker = new FileOpenPicker
        {
            ViewMode = PickerViewMode.Thumbnail,
            SuggestedStartLocation = PickerLocationId.PicturesLibrary,
        };
        InitializeWithWindow.Initialize(picker, WindowNative.GetWindowHandle(this));
        foreach (string ext in SupportedFormats.Extensions)
            picker.FileTypeFilter.Add(ext);

        StorageFile? file = await picker.PickSingleFileAsync();
        if (file is not null)
            await _viewModel.OpenFileAsync(file.Path);

        Viewport.Focus(FocusState.Programmatic);
    }

    private async Task OpenFolderPickerAsync()
    {
        var picker = new FolderPicker
        {
            SuggestedStartLocation = PickerLocationId.PicturesLibrary,
        };
        picker.FileTypeFilter.Add("*");
        InitializeWithWindow.Initialize(picker, WindowNative.GetWindowHandle(this));

        StorageFolder? folder = await picker.PickSingleFolderAsync();
        if (folder is not null)
            await _viewModel.OpenFolderAsync(folder.Path);

        Viewport.Focus(FocusState.Programmatic);
    }

    // --- Empty state / OSD / content presence (M8) ---

    private void OnContentPresenceChanged(bool present)
    {
        _hasContent = present;
        if (present && _pageOpen)
            ClosePage(); // opening a photo (picker, drop, MRU) returns from Settings/About to the viewer
        if (present)
        {
            if (Empty is { } empty)
                empty.Visibility = Visibility.Collapsed;
        }
        else if (!_pageOpen)
        {
            HideFilePlaceholder();
            ShowEmptyState();
        }
        UpdatePanelPresence();
    }

    /// <summary>The toolbar and the two panels are live only when there is a photo to act on and no page is
    /// covering it.</summary>
    private void UpdatePanelPresence() => ApplyPanelPresence(_hasContent && !_pageOpen);

    // The landing page shows neither the strip nor the info card (they would frame an empty viewport) and
    // disables their toggles plus the whole left toolbar group (only the More menu stays live); the user's
    // preference is untouched, so whatever was on comes back the moment content is present. Starts true so
    // the first "nothing open" transition actually hides them.
    private bool _panelsPresent = true;

    private void ApplyPanelPresence(bool present)
    {
        if (present == _panelsPresent)
            return;
        _panelsPresent = present;
        // Image actions + sort: nothing to act on while the landing page shows (panels can't be disabled as one).
        foreach (UIElement child in ToolbarPanel.Children)
        {
            if (child is Control control)
                control.IsEnabled = present;
        }
        HomeButton.IsEnabled = present;
        ExplorerToggle.IsEnabled = present;
        StripToggle.IsEnabled = present;
        InfoToggle.IsEnabled = present;
        SetStripVisible(present && _settings.StripVisible, persist: false);
        SetExplorerVisible(present && _settings.ExplorerVisible, persist: false);
        if (!present)
            _ = Explorer?.ShowAsync(null, null); // drop the tree and its folder watchers behind the landing page
        ApplyInfoVisible(present && _viewModel.InfoVisible);
        UpdatePageBar();
    }

    private void OnOsdRequested(string text) => Osd.Show(text);

    private void OnViewportZoomChanged(double percent) => Osd.ShowQuick($"{percent:0}%");

    // Show the Save button only while an unsaved rotation preview exists.
    private void OnRotationPreviewChanged(bool hasPreview)
        => SaveRotationButton.Visibility = hasPreview ? Visibility.Visible : Visibility.Collapsed;

    // --- Toolbar button handlers (mirror the accelerators; refocus so shortcuts stay live) ---

    private void OnToolPrevious(object sender, RoutedEventArgs e)
    {
        _ = _viewModel.PreviousAsync();
        ResetSlideshowIfRunning();
        Viewport.Focus(FocusState.Programmatic);
    }

    private void OnToolNext(object sender, RoutedEventArgs e)
    {
        _ = _viewModel.NextAsync();
        ResetSlideshowIfRunning();
        Viewport.Focus(FocusState.Programmatic);
    }

    private void OnToolZoomOut(object sender, RoutedEventArgs e)
    {
        Viewport.ZoomOutStep();
        Viewport.Focus(FocusState.Programmatic);
    }

    private void OnToolFit(object sender, RoutedEventArgs e)
    {
        Viewport.FitToWindow();
        Viewport.Focus(FocusState.Programmatic);
    }

    private void OnToolZoomIn(object sender, RoutedEventArgs e)
    {
        Viewport.ZoomInStep();
        Viewport.Focus(FocusState.Programmatic);
    }

    private void OnToolRotate(object sender, RoutedEventArgs e)
    {
        _viewModel.RotateCurrent(clockwise: true); // preview only; commit with the Save button
        Viewport.Focus(FocusState.Programmatic);
    }

    private void OnToolSaveRotation(object sender, RoutedEventArgs e)
    {
        _ = _viewModel.SaveRotationAsync();
        Viewport.Focus(FocusState.Programmatic);
    }

    private void OnToolDelete(object sender, RoutedEventArgs e) => _ = ConfirmAndDeleteAsync();

    private void OnToolSlideshow(object sender, RoutedEventArgs e)
    {
        ToggleSlideshow();
        Viewport.Focus(FocusState.Programmatic);
    }

    private void OnToolFullscreen(object sender, RoutedEventArgs e)
    {
        ToggleFullscreen();
        Viewport.Focus(FocusState.Programmatic);
    }

    private void OnToolHome(object sender, RoutedEventArgs e) => _ = _viewModel.CloseAsync();

    private void OnToolStrip(object sender, RoutedEventArgs e)
    {
        SetStripVisible(!IsStripVisible, persist: true);
        Viewport.Focus(FocusState.Programmatic);
    }

    private void OnToolInfo(object sender, RoutedEventArgs e) => SetInfoVisible(!_viewModel.InfoVisible);

    private void OnToolExplorer(object sender, RoutedEventArgs e)
    {
        SetExplorerVisible(!IsExplorerVisible, persist: true);
        Viewport.Focus(FocusState.Programmatic);
    }

    /// <summary>The toolbar Settings toggle: open the page, or close it when it is already showing. ShowPage and
    /// ClosePage own IsChecked, so the toggle also tracks pages opened by Ctrl+, and closed by Escape/Back.</summary>
    private void OnToolSettings(object sender, RoutedEventArgs e)
    {
        if (_pageOpen)
            ClosePage();
        else
            ShowSettingsPage();
    }

    // --- Updates (Store-delivered; we only surface "a newer version is waiting") ---

    private void OnUpdateStatusChanged(UpdateStatus status)
    {
        if (!DispatcherQueue.HasThreadAccess)
        {
            DispatcherQueue.TryEnqueue(() => OnUpdateStatusChanged(status));
            return;
        }
        var visibility = status == UpdateStatus.Available ? Visibility.Visible : Visibility.Collapsed;
        UpdateDot.Visibility = visibility;
    }

    /// <summary>
    /// "Updates" row of the About dialog (status line + Store button). Triggers a fresh check when opened
    /// (unless one is already running or an update is already known) and follows status changes until the
    /// dialog closes.
    /// </summary>
    /// <summary>The About page's Updates block. <paramref name="detach"/> unsubscribes from the service and must
    /// be run when the page closes.</summary>
    private StackPanel CreateUpdatePanel(out Action detach)
    {
        var secondary = (Microsoft.UI.Xaml.Media.Brush)Application.Current.Resources["TextFillColorSecondaryBrush"];
        var status = new TextBlock { TextWrapping = TextWrapping.Wrap, Foreground = secondary };
        Button openStore = IconButton("\uE896", "Update now in Microsoft Store"); // Download
        openStore.Visibility = Visibility.Collapsed;
        openStore.Click += (_, _) => _ = UpdateService.OpenStoreAsync();
        Button checkNow = IconButton("\uE72C", "Check for updates");              // Refresh
        checkNow.Click += (_, _) => _ = UpdateService.CheckAsync();

        void Apply(UpdateStatus s)
        {
            if (!DispatcherQueue.HasThreadAccess)
            {
                DispatcherQueue.TryEnqueue(() => Apply(s));
                return;
            }
            status.Text = s switch
            {
                UpdateStatus.Checking => "Checking for updates\u2026",
                UpdateStatus.UpToDate => "Looker is up to date.",
                UpdateStatus.Available => "A newer version of Looker is available. Windows installs Store updates automatically; open the Store to get it now.",
                _ => "Couldn't check for updates. Updates are delivered automatically through the Microsoft Store.",
            };
            openStore.Visibility = s == UpdateStatus.Available ? Visibility.Visible : Visibility.Collapsed;
            checkNow.IsEnabled = s != UpdateStatus.Checking;
        }

        Apply(UpdateService.Status);
        UpdateService.StatusChanged += Apply;
        detach = () => UpdateService.StatusChanged -= Apply;
        if (UpdateService.Status is UpdateStatus.Unknown or UpdateStatus.UpToDate)
            _ = UpdateService.CheckAsync();

        var links = new StackPanel { Orientation = Orientation.Horizontal, Spacing = 8, Margin = new Thickness(0, 6, 0, 0) };
        links.Children.Add(openStore);
        links.Children.Add(checkNow);
        var panel = new StackPanel { Spacing = 2 };
        panel.Children.Add(new TextBlock { Text = "Updates", Style = (Style)Application.Current.Resources["BodyStrongTextBlockStyle"] });
        panel.Children.Add(status);
        panel.Children.Add(links);
        return panel;
    }

    // --- Activation (M9): file/folder handed in by Explorer, the command line, the MRU list or a
    //     redirected second instance ---

    /// <summary>Open a file (and its folder) or a folder. Unknown paths are ignored.</summary>
    public async Task OpenPathAsync(string path)
    {
        try
        {
            if (Directory.Exists(path))
                await _viewModel.OpenFolderAsync(path);
            else if (File.Exists(path))
                await _viewModel.OpenFileAsync(path);
            else
                return;
        }
        catch (Exception ex)
        {
            Osd.Show($"Can't open {Path.GetFileName(path)}: {ex.Message}");
            return;
        }
        ResetSlideshowIfRunning();
        Viewport.Focus(FocusState.Programmatic);
    }

    /// <summary>Un-minimize and activate; the redirecting process hands over foreground rights.</summary>
    public void BringToForeground()
    {
        if (AppWindow.Presenter is OverlappedPresenter { State: OverlappedPresenterState.Minimized } presenter)
            presenter.Restore();
        Activate();
    }

    // --- About: the tail section of the Settings page (there is no separate About entry any more) ---

    /// <summary>Logo, version, links, the default-app and Reset buttons, and the log path.</summary>
    private StackPanel BuildAboutSection()
    {
        var version = Windows.ApplicationModel.Package.Current.Id.Version;
        string arch = System.Runtime.InteropServices.RuntimeInformation.ProcessArchitecture.ToString().ToLowerInvariant();
        var secondary = (Microsoft.UI.Xaml.Media.Brush)Application.Current.Resources["TextFillColorSecondaryBrush"];

        // The full logo as a vector (Assets/Brand/*.svg are cleaned copies of brand/slices, written by
        // scripts/Generate-Logos.ps1) so it stays crisp at any DPI; the version sits under it.
        double scale = Content.XamlRoot?.RasterizationScale ?? 1.0;
        var panel = new StackPanel { Spacing = 16 };
        const double logoHeight = 44; // 3216x819 viewBox → ~173 px wide at this height
        panel.Children.Add(new Image
        {
            Source = new Microsoft.UI.Xaml.Media.Imaging.SvgImageSource(new Uri("ms-appx:///Assets/Brand/looker_wordmark.svg"))
            {
                RasterizePixelHeight = logoHeight * scale,
                RasterizePixelWidth = logoHeight * 3216.0 / 819.0 * scale,
            },
            Height = logoHeight,
            HorizontalAlignment = HorizontalAlignment.Left,
            Stretch = Microsoft.UI.Xaml.Media.Stretch.Uniform,
        });
        panel.Children.Add(new TextBlock
        {
            Text = "A fast, native photo viewer for Windows 11.",
            TextWrapping = TextWrapping.Wrap,
            Foreground = secondary,
        });
        panel.Children.Add(new TextBlock
        {
            Text = $"Version {version.Major}.{version.Minor}.{version.Build}.{version.Revision} · {arch}",
            Foreground = secondary,
            Style = (Style)Application.Current.Resources["CaptionTextBlockStyle"],
        });

        Button github = IconButton("\uE943", "GitHub");           // Code
        github.Click += (_, _) => _ = UpdateService.OpenGitHubAsync();
        Button store = IconButton("\uE719", "Microsoft Store");   // Shop
        store.Click += (_, _) => _ = UpdateService.OpenStoreListingAsync();
        var links = new StackPanel { Orientation = Orientation.Horizontal, Spacing = 8 };
        links.Children.Add(github);
        links.Children.Add(store);
        panel.Children.Add(links);

        Button setDefault = IconButton("\uE71D", "Set as default photo viewer"); // AllApps
        setDefault.Click += (_, _) => _ = DefaultAppsService.OpenAsync();
        panel.Children.Add(setDefault);

        Button reset = IconButton("\uE7A7", "Reset Looker"); // Undo
        reset.Click += (_, _) => _ = ResetAppAsync();
        panel.Children.Add(reset);
        panel.Children.Add(new TextBlock
        {
            Text = "Puts every setting back to its default, clears the recent list and the decode cache, and forgets the saved window size. Your photos are not touched.",
            TextWrapping = TextWrapping.Wrap,
            Foreground = secondary,
            Style = (Style)Application.Current.Resources["CaptionTextBlockStyle"],
            Margin = new Thickness(0, -8, 0, 0),
        });

        panel.Children.Add(new TextBlock
        {
            Text = $"Logs: {Path.GetTempPath()}looker*.log",
            TextWrapping = TextWrapping.Wrap,
            Foreground = secondary,
            Opacity = 0.6, // quieter than the other captions: a maintainer detail, not something to read
            Style = (Style)Application.Current.Resources["CaptionTextBlockStyle"],
        });

        return panel;
    }

    // --- Fullscreen (F11) ---

    private void OnAccelFullscreen(KeyboardAccelerator sender, KeyboardAcceleratorInvokedEventArgs args)
    {
        args.Handled = true;
        ToggleFullscreen();
    }

    private void ToggleFullscreen()
    {
        if (_isFullscreen)
            ExitFullscreen();
        else
            EnterFullscreen();
    }

    private void EnterFullscreen()
    {
        if (_isFullscreen)
            return;
        AppWindow.SetPresenter(AppWindowPresenterKind.FullScreen);
        _isFullscreen = true;
    }

    private void ExitFullscreen()
    {
        if (!_isFullscreen)
            return;
        AppWindow.SetPresenter(AppWindowPresenterKind.Overlapped);
        _isFullscreen = false;
    }

    // --- Slideshow (F5) ---

    private void OnAccelSlideshow(KeyboardAccelerator sender, KeyboardAcceleratorInvokedEventArgs args)
    {
        args.Handled = true;
        ToggleSlideshow();
    }

    private void ToggleSlideshow()
    {
        if (_slideshow.IsRunning)
            StopSlideshow();
        else
            StartSlideshow();
    }

    private void StartSlideshow()
    {
        if (!_hasContent)
            return;
        _slideshow.Interval = TimeSpan.FromSeconds(_settings.SlideshowSeconds);
        if (!_isFullscreen)
        {
            EnterFullscreen();
            _enteredFullscreenForSlideshow = true;
        }
        _slideshow.Start();
        Osd.Show("Slideshow");
    }

    private void StopSlideshow()
    {
        if (!_slideshow.IsRunning)
            return;
        _slideshow.Stop();
        if (_enteredFullscreenForSlideshow)
        {
            ExitFullscreen();
            _enteredFullscreenForSlideshow = false;
        }
        Osd.Show("Slideshow ended");
    }

    private async void OnSlideshowAdvance()
    {
        Viewport.CrossFadeNext = true; // 300ms dissolve between slides
        await _viewModel.NextAsync();
    }

    // --- Escape: exit slideshow → exit fullscreen → close ---

    private void OnAccelEscape(KeyboardAccelerator sender, KeyboardAcceleratorInvokedEventArgs args)
    {
        args.Handled = true;
        if (_pageOpen)
            ClosePage();
        else if (_slideshow.IsRunning)
            StopSlideshow();
        else if (_isFullscreen)
            ExitFullscreen();
        else
            Close();
    }

    // --- Theme ---

    /// <summary>Paint the window chrome for <paramref name="theme"/>: RootGrid, the viewport's nothing-open fill, the
    /// info card and the strip's edge fades (when built), plus the dialog layers via <see cref="WhiteOnAccent"/>.
    /// The preview checkerboard is deliberately untouched.</summary>
    private void ApplyTheme(AppTheme theme)
    {
        _theme = ThemeColors.For(theme);
        RootGrid.Background = new Microsoft.UI.Xaml.Media.SolidColorBrush(_theme.Window);
        Viewport.SetEmptyBackground(_theme.Window);
        InfoPanel?.SetCardBackground(_theme.Window);
        Explorer?.SetCardBackground(_theme.Window);
        Strip?.SetFadeColor(_theme.Window);
        Page?.SetBackground(_theme.Window);
        PageBar?.SetBackground(_theme.Window);
        if (Placeholder is { } placeholder)
            placeholder.Background = new Microsoft.UI.Xaml.Media.SolidColorBrush(_theme.Window);
    }

    // --- Settings (Ctrl+,) — a full-window page. Every control applies on change; there is no Save button. ---

    private void OnAccelSettings(KeyboardAccelerator sender, KeyboardAcceleratorInvokedEventArgs args)
    {
        args.Handled = true;
        ShowSettingsPage();
    }

    private void ShowSettingsPage()
    {
        var secondary = (Microsoft.UI.Xaml.Media.Brush)Application.Current.Resources["TextFillColorSecondaryBrush"];
        var caption = (Style)Application.Current.Resources["CaptionTextBlockStyle"];
        TextBlock Hint(string text) => new()
        {
            Text = text,
            TextWrapping = TextWrapping.Wrap,
            Foreground = secondary,
            Style = caption,
            Margin = new Thickness(0, -10, 0, 0), // tuck under the control it explains
        };

        // Items first: a Selector rejects a SelectedIndex it has no item for. Handlers are wired *after* the
        // initial SelectedIndex so restoring the saved value doesn't count as a change.
        var wheelChoice = new ComboBox { Header = "Mouse wheel over the image", HorizontalAlignment = HorizontalAlignment.Stretch };
        wheelChoice.Items.Add("Zoom in and out");
        wheelChoice.Items.Add("Next and previous image");
        wheelChoice.SelectedIndex = (int)_settings.WheelMode;
        wheelChoice.SelectionChanged += (_, _) =>
        {
            if (wheelChoice.SelectedIndex < 0)
                return;
            _settings.WheelMode = (WheelMode)wheelChoice.SelectedIndex;
            Viewport.WheelNavigates = _settings.WheelMode == WheelMode.Navigate;
        };

        var zoomChoice = new ComboBox { Header = "Zoom towards", HorizontalAlignment = HorizontalAlignment.Stretch };
        zoomChoice.Items.Add("The mouse pointer");
        zoomChoice.Items.Add("The middle of the view");
        zoomChoice.SelectedIndex = (int)_settings.ZoomAnchor;
        zoomChoice.SelectionChanged += (_, _) =>
        {
            if (zoomChoice.SelectedIndex < 0)
                return;
            _settings.ZoomAnchor = (ZoomAnchor)zoomChoice.SelectedIndex;
            Viewport.ZoomAtPointer = _settings.ZoomAnchor == ZoomAnchor.Pointer;
        };

        var themeChoice = new ComboBox { Header = "Theme", HorizontalAlignment = HorizontalAlignment.Stretch };
        themeChoice.Items.Add("Black");
        themeChoice.Items.Add("Dark grey");
        themeChoice.SelectedIndex = (int)_settings.Theme;
        themeChoice.SelectionChanged += (_, _) =>
        {
            if (themeChoice.SelectedIndex < 0)
                return;
            _settings.Theme = (AppTheme)themeChoice.SelectedIndex;
            ApplyTheme(_settings.Theme);
        };

        var recentsSwitch = new ToggleSwitch
        {
            Header = "Recent photos",
            OnContent = "On",
            OffContent = "Off",
            IsOn = _settings.RecentsEnabled,
        };
        recentsSwitch.Toggled += (_, _) =>
        {
            _settings.RecentsEnabled = recentsSwitch.IsOn;
            if (!recentsSwitch.IsOn)
                _settings.ClearRecent(); // off means off: nothing lingers in the store either
            Empty?.SetRecent(RecentForDisplay());
        };

        var windowSwitch = new ToggleSwitch
        {
            Header = "Remember window size and position",
            OnContent = "On",
            OffContent = "Off",
            IsOn = _settings.RememberWindowPlacement,
        };
        windowSwitch.Toggled += (_, _) =>
        {
            _settings.RememberWindowPlacement = windowSwitch.IsOn;
            if (!windowSwitch.IsOn)
                _settings.ClearWindowPlacement(); // off means off: the next launch opens the default window
        };

        var cacheBox = new NumberBox
        {
            Header = "Decode cache budget (MB)",
            Value = _settings.CacheBudgetMB,
            Minimum = 128,
            Maximum = 2048,
            SpinButtonPlacementMode = NumberBoxSpinButtonPlacementMode.Inline,
            HorizontalAlignment = HorizontalAlignment.Stretch,
        };
        cacheBox.ValueChanged += (_, _) =>
        {
            if (double.IsNaN(cacheBox.Value)) // the box was cleared mid-edit
                return;
            _settings.CacheBudgetMB = (int)cacheBox.Value;
            Viewport.SetCacheBudgetBytes((long)_settings.CacheBudgetMB * 1024 * 1024);
        };

        var preferences = new StackPanel { Spacing = 20 };
        preferences.Children.Add(CreateUpdatePanel(out Action detachUpdates)); // first: the one thing that may need action
        preferences.Children.Add(Divider());
        preferences.Children.Add(wheelChoice);
        preferences.Children.Add(zoomChoice);
        preferences.Children.Add(Hint("Where the wheel and a double-click zoom into. Ctrl + and Ctrl - always zoom into the middle."));
        preferences.Children.Add(themeChoice);
        preferences.Children.Add(Hint("The checkerboard behind a photo stays the same in both themes."));
        preferences.Children.Add(recentsSwitch);
        preferences.Children.Add(Hint("Shows the photos you looked at last on the landing page. Turning it off also forgets the current list."));
        preferences.Children.Add(windowSwitch);
        preferences.Children.Add(Hint("Looker opens where you left it. Off: every launch opens a default-sized window in the middle of the screen."));
        preferences.Children.Add(cacheBox);
        preferences.Children.Add(Hint("How much memory Looker keeps decoded photos in, so going back to one is instant. "
            + "Raise it for large RAW or HEIC files; lower it on a machine short on memory."));

        // About lives on the Settings page rather than on its own: beside the preferences when the page is wide
        // enough for two columns, under them (behind a divider) otherwise.
        var subtitle = (Style)Application.Current.Resources["SubtitleTextBlockStyle"];
        Microsoft.UI.Xaml.Shapes.Rectangle aboutDivider = Divider();
        var about = new StackPanel { Spacing = 20 };
        about.Children.Add(aboutDivider);
        about.Children.Add(new TextBlock { Text = "About", Style = subtitle });
        about.Children.Add(BuildAboutSection());
        // In two-column mode the page's large header title is hidden and this heading tops the preferences
        // column instead, on the same row and in the same style as "About", so the two columns read as peers.
        var settingsHeading = new TextBlock { Text = "Settings", Style = subtitle };

        // Responsive layout at one breakpoint — what a XAML AdaptiveTrigger/VisualState would do, done in code
        // because this page is built in code. Column 1 is only given width in two-column mode.
        const double columnWidth = 480, columnGap = 48, twoColumnWidth = 2 * columnWidth + columnGap;
        var layout = new Grid { ColumnSpacing = columnGap, RowSpacing = 20 };
        layout.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        layout.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(0) });
        layout.RowDefinitions.Add(new RowDefinition { Height = GridLength.Auto });
        layout.RowDefinitions.Add(new RowDefinition { Height = GridLength.Auto });
        layout.Children.Add(preferences);
        layout.Children.Add(about);
        about.VerticalAlignment = VerticalAlignment.Top;

        Controls.AppPage page = EnsurePage();
        bool? twoColumns = null;
        void Arrange(double available)
        {
            bool wide = available >= twoColumnWidth;
            if (twoColumns == wide)
                return;
            twoColumns = wide;
            layout.ColumnDefinitions[1].Width = wide ? new GridLength(1, GridUnitType.Star) : new GridLength(0);
            Grid.SetColumn(about, wide ? 1 : 0);
            Grid.SetRow(about, wide ? 0 : 1);
            aboutDivider.Visibility = wide ? Visibility.Collapsed : Visibility.Visible;
            page.ContentMaxWidth = wide ? twoColumnWidth : Controls.AppPage.DefaultContentMaxWidth;
            page.TitleVisible = !wide;
            if (wide && !preferences.Children.Contains(settingsHeading))
                preferences.Children.Insert(0, settingsHeading);
            else if (!wide)
                preferences.Children.Remove(settingsHeading);
        }
        page.BodyWidthChanged += Arrange;

        ShowPage("Settings", layout, () =>
        {
            detachUpdates();
            page.BodyWidthChanged -= Arrange;
        });
        // Before the page's first layout BodyWidth is 0; the window width minus the page padding is the same
        // number, and using it avoids a one-frame flip from one column to two.
        Arrange(page.BodyWidth > 0 ? page.BodyWidth : RootGrid.ActualWidth - 48);
    }

    private static Microsoft.UI.Xaml.Shapes.Rectangle Divider() => new()
    {
        Height = 1,
        Margin = new Thickness(0, 8, 0, 0),
        Fill = (Microsoft.UI.Xaml.Media.Brush)Application.Current.Resources["DividerStrokeColorDefaultBrush"],
    };

    // --- Reset (Settings > About) ---

    private bool _skipPlacementSave; // after a reset the window placement must not be re-saved on close

    private async Task ResetAppAsync()
    {
        var dialog = new ContentDialog
        {
            Title = "Reset Looker?",
            Content = new TextBlock
            {
                Text = "Every setting goes back to its default, the recent list and the decode cache are cleared, and the saved window size and position are forgotten. Your photos are not touched.",
                TextWrapping = TextWrapping.Wrap,
            },
            PrimaryButtonText = "Reset",
            CloseButtonText = "Cancel",
            DefaultButton = ContentDialogButton.Close,
            XamlRoot = Content.XamlRoot,
        };
        WhiteOnAccent(dialog);
        if (await dialog.ShowAsync() != ContentDialogResult.Primary)
            return;

        // Stored state: LocalSettings, the app-data folders (wallpaper.png copy, temp/cache), the decode cache.
        _settings.ResetAll();
        _skipPlacementSave = true;
        Viewport.ClearCache();
        foreach (string folder in new[]
        {
            ApplicationData.Current.LocalFolder.Path,
            ApplicationData.Current.TemporaryFolder.Path,
            ApplicationData.Current.LocalCacheFolder.Path,
        })
        {
            await Task.Run(() => TryEmptyFolder(folder));
        }

        // Live state, so the reset is visible without a restart. Strip/info visibility and the recent list follow
        // the cleared settings when the page closes (UpdatePanelPresence / ShowEmptyState).
        ApplyTheme(_settings.Theme);
        Viewport.WheelNavigates = _settings.WheelMode == WheelMode.Navigate;
        Viewport.ZoomAtPointer = _settings.ZoomAnchor == ZoomAnchor.Pointer;
        Viewport.SetCacheBudgetBytes((long)_settings.CacheBudgetMB * 1024 * 1024);
        _slideshow.Interval = TimeSpan.FromSeconds(_settings.SlideshowSeconds);
        _viewModel.InfoVisible = _settings.InfoVisible;
        if (Strip is { } strip)
            strip.StripHeight = _settings.StripHeight;
        if (InfoPanel is { } info)
            info.PanelWidth = _settings.InfoWidth;
        if (Explorer is { } explorer)
            explorer.PanelWidth = _settings.ExplorerWidth;
        Empty?.SetDefaultHintVisible(!_settings.DefaultHintDismissed);
        _ = _viewModel.SetSortAsync(_settings.Sort);
        Explorer?.SetSort(_settings.Sort);
        SyncSortMenus();

        Osd.Show("Looker reset to defaults");
        ShowSettingsPage(); // rebuild so every control shows its default
    }

    private static void TryEmptyFolder(string folder)
    {
        try
        {
            if (!Directory.Exists(folder))
                return;
            foreach (string file in Directory.EnumerateFiles(folder))
                File.Delete(file);
            foreach (string dir in Directory.EnumerateDirectories(folder))
                Directory.Delete(dir, recursive: true);
        }
        catch
        {
            // A locked file (e.g. a wallpaper the shell is still reading) just stays; nothing here is precious.
        }
    }
}
