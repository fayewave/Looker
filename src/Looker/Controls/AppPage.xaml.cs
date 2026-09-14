using System;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Input;

namespace Looker.Controls;

/// <summary>
/// Shell for the app's full-window pages (Settings, About). It occupies the same overlay slot as the landing
/// page, over the viewport, so those screens read as places in the app rather than modal dialogs. The host
/// supplies the title and body (<see cref="Show"/>) and handles <see cref="BackRequested"/>.
/// </summary>
public sealed partial class AppPage : UserControl
{
    /// <summary>The back button, or Escape while focus is anywhere inside the page.</summary>
    public event EventHandler? BackRequested;

    /// <summary>Raised with the width available to the body whenever it changes, so a page can switch between
    /// one and two columns at a breakpoint. <see cref="BodyWidth"/> reads the current value.</summary>
    public event Action<double>? BodyWidthChanged;

    public const double DefaultContentMaxWidth = 560;

    public AppPage()
    {
        InitializeComponent();
        BackButton.Click += (_, _) => BackRequested?.Invoke(this, EventArgs.Empty);

        // The host disables the root KeyboardAccelerators while a page is open (single-key ones like T/I/Del
        // must not fire while the user types in a settings field), so Escape is handled here instead. An open
        // ComboBox swallows it first, which is what you want: the drop-down closes, not the page.
        KeyDown += (_, e) =>
        {
            if (e.Key != Windows.System.VirtualKey.Escape)
                return;
            e.Handled = true;
            BackRequested?.Invoke(this, EventArgs.Empty);
        };
        Scroller.SizeChanged += (_, e) => BodyWidthChanged?.Invoke(e.NewSize.Width);
    }

    /// <summary>Width the body can use right now (0 before the first layout).</summary>
    public double BodyWidth => Scroller.ActualWidth;

    /// <summary>Cap on the centred header + body column. Reset to <see cref="DefaultContentMaxWidth"/> by
    /// <see cref="Clear"/> so a wide layout never leaks into the next page.</summary>
    public double ContentMaxWidth
    {
        get => Body.MaxWidth;
        set
        {
            Body.MaxWidth = value;
            Header.MaxWidth = value;
        }
    }

    /// <summary>Fill the page. Replaces whatever was shown, so switching Settings → About reuses one instance.</summary>
    public void Show(string title, UIElement content)
    {
        TitleText.Text = title;
        Body.Content = content;
    }

    /// <summary>Drop the body so its controls (and their event handlers) are not retained while hidden.</summary>
    public void Clear()
    {
        Body.Content = null;
        ContentMaxWidth = DefaultContentMaxWidth;
        TitleVisible = true;
    }

    /// <summary>Whether the large title sits in the header row beside the back button. A page that lays out in
    /// columns hides it and puts its own heading in the body so it lines up with the other column's heading.</summary>
    public bool TitleVisible
    {
        get => TitleText.Visibility == Visibility.Visible;
        set => TitleText.Visibility = value ? Visibility.Visible : Visibility.Collapsed;
    }

    /// <summary>Paint the page. Must go on the root Grid: a UserControl's own Background is not rendered by its
    /// default template, so setting it on the control leaves the page transparent and the photo shows through.</summary>
    public void SetBackground(Windows.UI.Color color)
        => Root.Background = new Microsoft.UI.Xaml.Media.SolidColorBrush(color);

    /// <summary>Park focus on the back button so Escape and Tab work the moment the page appears. On the very
    /// first show the button may not be loaded yet and Focus returns false; retry once it is.</summary>
    public void FocusBack()
    {
        if (BackButton.Focus(FocusState.Programmatic))
            return;
        BackButton.Loaded -= OnBackButtonLoaded;
        BackButton.Loaded += OnBackButtonLoaded;
    }

    private void OnBackButtonLoaded(object sender, RoutedEventArgs e)
    {
        BackButton.Loaded -= OnBackButtonLoaded;
        BackButton.Focus(FocusState.Programmatic);
    }
}
