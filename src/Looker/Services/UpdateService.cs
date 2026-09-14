using System;
using System.Threading.Tasks;
using Windows.ApplicationModel;
using Windows.System;

namespace Looker.Services;

public enum UpdateStatus
{
    /// <summary>No check has completed, or the Store could not answer (dev-registered loose layouts, offline).</summary>
    Unknown,
    Checking,
    UpToDate,
    /// <summary>A newer version is published in the Microsoft Store.</summary>
    Available,
}

/// <summary>
/// Asks the Store whether a newer version of this package is published. Updates themselves are installed by
/// Windows in the background (Store apps need no updater); this only lets the UI say "an update is waiting"
/// and deep-link to the Store's downloads page so the user can pull it early. The check is a network call
/// to the Store, so it runs once after startup settles and on demand from Settings, never on the cold-start path.
/// </summary>
public static class UpdateService
{
    private const string StoreUpdatesUri = "ms-windows-store://downloadsandupdates";
    public const string StoreProductId = "9NV130N4C2GZ";
    private const string StoreListingUri = "ms-windows-store://pdp/?productid=" + StoreProductId;
    private const string StoreListingWebUrl = "https://apps.microsoft.com/detail/" + StoreProductId;
    public const string GitHubUrl = "https://github.com/fayewave/Looker";

    public static UpdateStatus Status { get; private set; } = UpdateStatus.Unknown;

    /// <summary>Raised on the thread that ran the check (the UI thread for every caller in the app).</summary>
    public static event Action<UpdateStatus>? StatusChanged;

    private static Task<UpdateStatus>? _inFlight;

    public static Task<UpdateStatus> CheckAsync()
    {
        if (_inFlight is { IsCompleted: false })
            return _inFlight;
        _inFlight = RunCheckAsync();
        return _inFlight;
    }

    private static async Task<UpdateStatus> RunCheckAsync()
    {
        Set(UpdateStatus.Checking);
        UpdateStatus result;
        try
        {
            var r = await Package.Current.CheckUpdateAvailabilityAsync();
            result = r.Availability switch
            {
                PackageUpdateAvailability.Available or PackageUpdateAvailability.Required => UpdateStatus.Available,
                PackageUpdateAvailability.NoUpdates => UpdateStatus.UpToDate,
                _ => UpdateStatus.Unknown,
            };
        }
        catch (Exception)
        {
            result = UpdateStatus.Unknown;
        }
        Set(result);
        return result;
    }

    private static void Set(UpdateStatus status)
    {
        Status = status;
        StatusChanged?.Invoke(status);
    }

    /// <summary>Opens Microsoft Store on its Downloads &amp; updates page, where "Get updates" pulls the new version now.</summary>
    public static Task<bool> OpenStoreAsync() => TryLaunchAsync(StoreUpdatesUri);

    /// <summary>Opens Looker's Store listing in the Store app, or its web page if the protocol is refused.</summary>
    public static async Task<bool> OpenStoreListingAsync()
        => await TryLaunchAsync(StoreListingUri) || await TryLaunchAsync(StoreListingWebUrl);

    public static Task<bool> OpenGitHubAsync() => TryLaunchAsync(GitHubUrl);

    private static async Task<bool> TryLaunchAsync(string uri)
    {
        try
        {
            return await Launcher.LaunchUriAsync(new Uri(uri));
        }
        catch (Exception)
        {
            return false;
        }
    }
}
