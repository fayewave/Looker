using System;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Media;
using Windows.UI;

namespace Looker.Controls;

/// <summary>
/// The page bar for multi-page documents: previous / "Page 3 of 12" / next. The host shows it only while a
/// PDF is up and moves the viewport's page from its two events.
/// </summary>
public sealed partial class PageBar : UserControl
{
    public event EventHandler? PreviousRequested;
    public event EventHandler? NextRequested;

    /// <summary>The pill takes the window colour of the current theme (black, or the dark grey), like the cards,
    /// instead of the stock theme grey.</summary>
    public void SetBackground(Color color) => Pill.Background = new SolidColorBrush(color);

    public PageBar()
    {
        InitializeComponent();
    }

    /// <summary>Show page <paramref name="index"/> (zero-based) of <paramref name="count"/>, greying out the
    /// button that has nowhere to go.</summary>
    public void SetPage(int index, int count)
    {
        Label.Text = $"Page {index + 1} of {count}";
        PreviousButton.IsEnabled = index > 0;
        NextButton.IsEnabled = index < count - 1;
    }

    private void OnPrevious(object sender, RoutedEventArgs e) => PreviousRequested?.Invoke(this, EventArgs.Empty);

    private void OnNext(object sender, RoutedEventArgs e) => NextRequested?.Invoke(this, EventArgs.Empty);
}
