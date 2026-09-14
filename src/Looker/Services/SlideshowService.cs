using System;
using Microsoft.UI.Dispatching;

namespace Looker.Services;

/// <summary>
/// Drives the timed slideshow: a repeating <see cref="DispatcherQueueTimer"/> that raises
/// <see cref="Advance"/> on the UI thread each interval. The window owns the visual side (entering
/// fullscreen, the cross-fade, exiting on Esc); this service is purely the clock, so a manual
/// navigation can <see cref="Reset"/> it to keep the dwell fair.
/// </summary>
public sealed class SlideshowService
{
    private readonly DispatcherQueueTimer _timer;

    /// <summary>Raised each interval while running — advance to the next image.</summary>
    public event Action? Advance;

    public SlideshowService(DispatcherQueue queue)
    {
        _timer = queue.CreateTimer();
        _timer.IsRepeating = true;
        _timer.Tick += (_, _) => Advance?.Invoke();
    }

    public bool IsRunning { get; private set; }

    /// <summary>Whether a running slideshow is paused (Space). Only meaningful while <see cref="IsRunning"/>.</summary>
    public bool IsPaused { get; private set; }

    /// <summary>Dwell time per image. Applied on the next <see cref="Start"/>/<see cref="Reset"/>.</summary>
    public TimeSpan Interval { get; set; } = TimeSpan.FromSeconds(4);

    public void Start()
    {
        _timer.Interval = Interval;
        _timer.Start();
        IsRunning = true;
        IsPaused = false;
    }

    public void Stop()
    {
        _timer.Stop();
        IsRunning = false;
        IsPaused = false;
    }

    /// <summary>Restart the current interval from now (e.g. after a manual next/prev during the show).</summary>
    public void Reset()
    {
        if (!IsRunning || IsPaused)
            return;
        _timer.Stop();
        _timer.Interval = Interval;
        _timer.Start();
    }

    public void Pause()
    {
        if (!IsRunning || IsPaused)
            return;
        _timer.Stop();
        IsPaused = true;
    }

    public void Resume()
    {
        if (!IsRunning || !IsPaused)
            return;
        _timer.Interval = Interval;
        _timer.Start();
        IsPaused = false;
    }
}
