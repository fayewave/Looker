using System;
using System.Collections.Generic;
using System.IO;
using System.Text;
using Microsoft.Windows.AppLifecycle;
using Windows.ApplicationModel.Activation;

namespace Looker.Services;

/// <summary>
/// Turns an activation (initial or redirected from a second instance) into the single path the viewer
/// should open. Explorer / "Open with" / the default-app flow arrive as <see cref="ExtendedActivationKind.File"/>;
/// a command line (app-execution alias, direct exe launch during dev) arrives as
/// <see cref="ExtendedActivationKind.Launch"/> with the raw argument string.
/// </summary>
public static class ActivationService
{
    public static string? GetPathToOpen(AppActivationArguments activation)
    {
        try
        {
            switch (activation.Kind)
            {
                case ExtendedActivationKind.File when activation.Data is IFileActivatedEventArgs file:
                    foreach (var item in file.Files)
                    {
                        if (!string.IsNullOrEmpty(item.Path))
                            return item.Path;
                    }
                    return null;

                case ExtendedActivationKind.Launch when activation.Data is ILaunchActivatedEventArgs launch:
                    return CommandLineArgs.FirstExistingPath(CommandLineArgs.Split(launch.Arguments));

                default:
                    return null;
            }
        }
        catch
        {
            return null; // malformed activation data: behave like a plain launch
        }
    }
}
