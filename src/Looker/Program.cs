using System;
using System.Diagnostics;
using System.IO;
using System.Threading;
using System.Threading.Tasks;
using Looker.Helpers;
using Looker.Services;
using Microsoft.UI.Dispatching;
using Microsoft.UI.Xaml;
using Microsoft.Windows.AppLifecycle;

namespace Looker;

/// <summary>
/// Custom entry point (the XAML-generated Main is disabled via <c>DISABLE_XAML_GENERATED_MAIN</c>) so
/// single-instancing can be decided *before* any window or XAML runtime is created. A second launch
/// (Explorer double-click, alias, MRU) redirects its activation to the instance that owns the
/// <see cref="InstanceKey"/> and exits immediately; the surviving instance handles it in
/// <see cref="App.OnRedirectedActivation"/>.
/// <para>
/// Cold start: the moment we know this process is the survivor, the launch path is resolved here and
/// <see cref="StartupWarmup"/> begins decoding it on the thread pool — in parallel with the XAML runtime
/// and window construction that follow — instead of waiting for the Win2D control to be loaded.
/// </para>
/// </summary>
public static class Program
{
    private const string InstanceKey = "looker-main";

    /// <summary>The file or folder this instance was launched with (resolved once, before XAML), or null.</summary>
    public static string? InitialPath { get; private set; }

    [STAThread]
    private static int Main(string[] args)
    {
        StartupTrace.Mark("Main entered");
        WinRT.ComWrappersSupport.InitializeComWrappers();

        // Light warm-ups (WIC inventory, settings store) run on the pool from here on; if this turns out to be a
        // redirecting second instance they are simply abandoned with the process.
        StartupWarmup.Begin();

        if (TryRedirectToExistingInstance(out AppActivationArguments activation))
            return 0;
        StartupTrace.Mark("single-instance check done (we are main)");

        InitialPath = ActivationService.GetPathToOpen(activation);
        if (InitialPath is not null && File.Exists(InitialPath))
            StartupWarmup.BeginInitialDecode(InitialPath);

        Application.Start(callbackParams =>
        {
            StartupTrace.Mark("Application.Start callback (XAML runtime up)");
            var context = new DispatcherQueueSynchronizationContext(DispatcherQueue.GetForCurrentThread());
            SynchronizationContext.SetSynchronizationContext(context);
            _ = new App();
        });
        return 0;
    }

    /// <summary>True when this process handed its activation to an already-running instance and should exit.</summary>
    private static bool TryRedirectToExistingInstance(out AppActivationArguments activation)
    {
        activation = AppInstance.GetCurrent().GetActivatedEventArgs();
        StartupTrace.Mark("GetActivatedEventArgs (WinAppSDK runtime up)");
        AppInstance main = AppInstance.FindOrRegisterForKey(InstanceKey);
        if (main.IsCurrent)
            return false;

        // Documented pattern: RedirectActivationToAsync must not be awaited on the STA thread with a
        // blocking wait (the cross-process COM call needs the message pump), so run it off-thread and
        // pump with CoWaitForMultipleObjects until it signals.
        IntPtr redirectDone = NativeMethods.CreateEvent(IntPtr.Zero, true, false, null);
        try
        {
            AppActivationArguments toRedirect = activation;
            Task.Run(async () =>
            {
                try { await main.RedirectActivationToAsync(toRedirect); }
                catch { /* the target died mid-redirect; nothing better to do than exit */ }
                finally { NativeMethods.SetEvent(redirectDone); }
            });
            NativeMethods.CoWaitForMultipleObjects(0, NativeMethods.INFINITE, 1, new[] { redirectDone }, out _);
        }
        finally
        {
            NativeMethods.CloseHandle(redirectDone);
        }

        // This process has foreground rights (the user just launched it); hand them to the survivor.
        try
        {
            using Process target = Process.GetProcessById((int)main.ProcessId);
            if (target.MainWindowHandle != IntPtr.Zero)
                NativeMethods.SetForegroundWindow(target.MainWindowHandle);
        }
        catch { /* best effort */ }
        return true;
    }
}
