using System;
using System.Collections.Generic;
using System.IO;
using System.Text;

namespace Looker.Services;

/// <summary>
/// Pure helpers for turning a launch argument string into a path to open. WinUI-free so the unit tests
/// link it directly (see tests/Looker.Tests).
/// </summary>
public static class CommandLineArgs
{
    /// <summary>The first argument that names an existing file or directory (skipping the exe itself,
    /// which some launch paths include in the argument string).</summary>
    public static string? FirstExistingPath(IEnumerable<string> args)
    {
        foreach (string arg in args)
        {
            if (arg.Length == 0)
                continue;
            if (arg.EndsWith(".exe", StringComparison.OrdinalIgnoreCase))
                continue;
            string full;
            try { full = Path.GetFullPath(arg); }
            catch { continue; }
            if (File.Exists(full) || Directory.Exists(full))
                return full;
        }
        return null;
    }

    /// <summary>Minimal Windows command-line tokenizer: whitespace-separated, double quotes group, a
    /// doubled quote inside quotes is a literal quote.</summary>
    public static List<string> Split(string? commandLine)
    {
        var result = new List<string>();
        if (string.IsNullOrWhiteSpace(commandLine))
            return result;

        var current = new StringBuilder();
        bool inQuotes = false;
        bool hasToken = false;
        for (int i = 0; i < commandLine.Length; i++)
        {
            char c = commandLine[i];
            if (c == '"')
            {
                if (inQuotes && i + 1 < commandLine.Length && commandLine[i + 1] == '"')
                {
                    current.Append('"');
                    i++;
                }
                else
                {
                    inQuotes = !inQuotes;
                }
                hasToken = true;
            }
            else if (char.IsWhiteSpace(c) && !inQuotes)
            {
                if (hasToken)
                {
                    result.Add(current.ToString());
                    current.Clear();
                    hasToken = false;
                }
            }
            else
            {
                current.Append(c);
                hasToken = true;
            }
        }
        if (hasToken)
            result.Add(current.ToString());
        return result;
    }
}
