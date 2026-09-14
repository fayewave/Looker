using System;
using System.IO;
using Looker.Helpers;
using Looker.Services;
using Microsoft.UI.Xaml;
using Microsoft.Windows.AppLifecycle;

namespace Looker;

/// <summary>
/// Provides application-specific behavior to supplement the default Application class. Activation
/// (file / command line) is read from <see cref="AppInstance"/>, never from the XAML launch args: those
/// always report Kind == Launch regardless of how the app was started. The initial activation is resolved
/// once in <see cref="Program.Main"/> (so its decode can start before XAML exists) and read from
/// <see cref="Program.InitialPath"/> here.
/// </summary>
public partial class App : Application
{
    private static readonly string LogPath = Path.Combine(Path.GetTempPath(), "looker-crash.log");
    private MainWindow? _window;

    /// <summary>
    /// Initializes the singleton application object.  This is the first line of authored code
    /// executed, and as such is the logical equivalent of main() or WinMain().
    /// </summary>
    public App()
    {
        InitializeComponent();
        StartupTrace.Mark("App.InitializeComponent (XamlControlsResources)");
        UnhandledException += OnUnhandledException;
    }

    /// <summary>
    /// Invoked when the application is launched.
    /// </summary>
    /// <param name="args">Details about the launch request and process.</param>
    protected override void OnLaunched(Microsoft.UI.Xaml.LaunchActivatedEventArgs args)
    {
        StartupTrace.Mark("OnLaunched");
        string? path = Program.InitialPath;

        // The empty state is only built when it will actually be seen: a launch-with-file never shows it.
        _window = new MainWindow(showEmptyState: path is null);
        StartupTrace.Mark("MainWindow constructed");
        _window.Activate();
        StartupTrace.Mark("Window.Activate returned");

        if (path is not null)
            _ = _window.OpenPathAsync(path);

        // Subsequent launches (Program redirects them here) arrive on a worker thread.
        AppInstance.GetCurrent().Activated += OnRedirectedActivation;
    }

    private void OnRedirectedActivation(object? sender, AppActivationArguments e)
    {
        string? path = ActivationService.GetPathToOpen(e);
        MainWindow? window = _window;
        if (window is null)
            return;

        window.DispatcherQueue.TryEnqueue(() =>
        {
            window.BringToForeground();
            if (path is not null)
                _ = window.OpenPathAsync(path);
        });
    }

    // A single stray exception (e.g. from a shell thumbnail provider on one odd file) should not take
    // the whole viewer down. Log it and keep running; genuine bugs still surface in the log.
    private static void OnUnhandledException(object sender, Microsoft.UI.Xaml.UnhandledExceptionEventArgs e)
    {
        try
        {
            File.AppendAllText(LogPath, $"{DateTime.Now:O}  UNHANDLED {e.Message}\n{e.Exception}\n\n");
        }
        catch
        {
            // logging is best-effort
        }
        e.Handled = true;
    }
}
