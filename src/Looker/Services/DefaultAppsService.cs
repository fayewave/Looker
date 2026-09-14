using System;
using System.Threading.Tasks;
using Windows.ApplicationModel;
using Windows.System;

namespace Looker.Services;

/// <summary>
/// Opens Settings &gt; Apps &gt; Default apps on Looker's own page, where the user can pick Looker for each
/// image type (or press "Set default" on builds that offer it). A packaged app cannot change file
/// associations itself — Windows only lets the user do that through Settings — so this is the whole
/// "set as default" flow. The per-app deep link (<c>registeredAUMID=</c>) is Windows 11 only; if the
/// shell refuses it we fall back to the generic Default apps page.
/// </summary>
public static class DefaultAppsService
{
    private const string DefaultAppsUri = "ms-settings:defaultapps";

    public static async Task OpenAsync()
    {
        string aumid = AppInfo.Current.AppUserModelId;
        if (!string.IsNullOrEmpty(aumid))
        {
            var perApp = new Uri($"{DefaultAppsUri}?registeredAUMID={Uri.EscapeDataString(aumid)}");
            if (await TryLaunchAsync(perApp))
                return;
        }
        await TryLaunchAsync(new Uri(DefaultAppsUri));
    }

    private static async Task<bool> TryLaunchAsync(Uri uri)
    {
        try
        {
            return await Launcher.LaunchUriAsync(uri);
        }
        catch (Exception)
        {
            return false;
        }
    }
}
