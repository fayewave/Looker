using System;
using Microsoft.UI.Dispatching;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Media.Animation;

namespace Looker.Controls;

/// <summary>
/// A transient status toast. <see cref="Show"/> displays the given text, holds briefly, then fades out;
/// calling it again while one is up refreshes the text and restarts the hold, so rapid events (a wheel-zoom
/// burst, held-arrow navigation) coalesce into a single steady readout instead of flickering.
/// </summary>
public sealed partial class OsdOverlay : UserControl
{
    private static readonly TimeSpan HoldTime = TimeSpan.FromMilliseconds(1200);
    private static readonly TimeSpan FadeIn = TimeSpan.FromMilliseconds(120);
    private static readonly TimeSpan FadeOut = TimeSpan.FromMilliseconds(400);

    // Quick variant for continuous readouts (zoom %): the value is only interesting while it is changing,
    // so it lingers far less than a one-off toast like "Copied".
    private static readonly TimeSpan QuickHoldTime = TimeSpan.FromMilliseconds(350);
    private static readonly TimeSpan QuickFadeOut = TimeSpan.FromMilliseconds(180);

    private TimeSpan _fadeOut = FadeOut; // fade used when the current hold expires

    private DispatcherQueueTimer? _holdTimer;
    private Storyboard? _fade;

    public OsdOverlay()
    {
        InitializeComponent();
    }

    public void Show(string text) => Show(text, HoldTime, FadeOut);

    /// <summary>Like <see cref="Show"/> but gone almost as soon as the events stop (zoom readout).</summary>
    public void ShowQuick(string text) => Show(text, QuickHoldTime, QuickFadeOut);

    private void Show(string text, TimeSpan hold, TimeSpan fadeOut)
    {
        Label.Text = text;
        _fadeOut = fadeOut;

        _fade?.Stop();
        Pill.Opacity = 1; // fade-in animation replaces this, but a snap keeps it visible if animations are off
        _fade = Animate(from: null, to: 1.0, FadeIn);
        _fade.Begin();

        _holdTimer ??= CreateHoldTimer();
        _holdTimer.Stop();
        _holdTimer.Interval = hold;
        _holdTimer.Start();
    }

    private DispatcherQueueTimer CreateHoldTimer()
    {
        DispatcherQueueTimer timer = DispatcherQueue.CreateTimer();
        timer.IsRepeating = false;
        timer.Tick += (_, _) =>
        {
            _fade?.Stop();
            _fade = Animate(from: Pill.Opacity, to: 0.0, _fadeOut);
            _fade.Begin();
        };
        return timer;
    }

    private Storyboard Animate(double? from, double to, TimeSpan duration)
    {
        var animation = new DoubleAnimation
        {
            From = from,
            To = to,
            Duration = duration,
            EnableDependentAnimation = true,
        };
        Storyboard.SetTarget(animation, Pill);
        Storyboard.SetTargetProperty(animation, "Opacity");
        var storyboard = new Storyboard();
        storyboard.Children.Add(animation);
        return storyboard;
    }
}
