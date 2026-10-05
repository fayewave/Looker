<#
.SYNOPSIS
    Builds the native Looker (native/, Rust) and registers it as the dev-mode package under the Store identity,
    replacing whatever Looker layout is registered now (the C# app's Debug or Publish-Dev layout).

.DESCRIPTION
      1. cargo build --release (target dir is %LOCALAPPDATA%\Looker\native-target, outside Dropbox);
      2. lays the package out (NativeLayout.ps1): Looker.exe, the logo PNGs from native/assets,
         native/Package.appxmanifest with a unique revision stamped in (registering the same version from another
         folder is a silent no-op) and resources.pri. The layout alternates between
         %LOCALAPPDATA%\Looker\native-pkg-<Platform>-a and -b, always the one the registered package is NOT
         installed from: deleting and recreating the folder under a registered dev-mode package breaks its file
         activation for good (Explorer: "The parameter is incorrect", AppModel-Runtime event 203, 0x80070057,
         while Start-menu launches still work), and re-registering from the same folder doesn't repair it.
         Registering from the other folder does;
      3. registers it with -ForceUpdateFromAnyVersion. LocalSettings survive (never Remove-AppxPackage: that
         wipes them), so the first packaged launch migrates the C# app's preferences.

    Afterwards Looker runs from the Start menu, "Open with", `Looker photo.jpg` (the alias) or
    shell:AppsFolder\fayewave.Looker-PhotoViewer_tp8spdf6gttjc!App. For a Store or sideload package use
    Build-NativePackage.ps1. (The retired C# app, with its own Publish-Dev.ps1, is at the csharp-final tag.)

.EXAMPLE
    pwsh scripts/Publish-Native.ps1
#>
[CmdletBinding()]
param(
    [ValidateSet('x64')]
    [string]$Platform = 'x64',
    [string]$OutDir
)
$ErrorActionPreference = 'Stop'
$root = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
. (Join-Path $PSScriptRoot 'NativeLayout.ps1')

if (Get-Process Looker -ErrorAction SilentlyContinue) {
    throw 'Looker is running; close it first (re-registration replaces the package it runs from).'
}

# Never rebuild the folder the package is registered from (see .DESCRIPTION): use the other one.
$installed = (Get-AppxPackage -Name 'fayewave.Looker-PhotoViewer').InstallLocation
if (-not $OutDir) {
    $a = Join-Path $env:LOCALAPPDATA "Looker\native-pkg-$Platform-a"
    $b = Join-Path $env:LOCALAPPDATA "Looker\native-pkg-$Platform-b"
    $OutDir = if ($installed -and [IO.Path]::GetFullPath($installed) -eq [IO.Path]::GetFullPath($a)) { $b } else { $a }
}
elseif ($installed -and [IO.Path]::GetFullPath($installed).TrimEnd('\') -eq [IO.Path]::GetFullPath($OutDir).TrimEnd('\')) {
    throw "$OutDir is the folder Looker is registered from; rebuilding it would break opening files from Explorer. Use another folder."
}

# Unique revision per publish (16-bit field): minutes since midnight, so consecutive runs differ.
$revision = [int]((Get-Date) - (Get-Date).Date).TotalMinutes
$version = New-NativeLayout -Root $root -OutDir $OutDir -Revision $revision
$manifestPath = Join-Path $OutDir 'AppxManifest.xml'

Write-Host "Registering $OutDir ($version) ..."
$attempt = 0
while ($true) {
    try {
        Add-AppxPackage -Register $manifestPath -ForceUpdateFromAnyVersion -ErrorAction Stop
        break
    }
    catch {
        if (++$attempt -ge 4) { throw }
        Write-Host "  registration busy, retrying ($attempt)..."
        Start-Sleep -Seconds 3
    }
}
$pkg = Get-AppxPackage -Name 'fayewave.Looker-PhotoViewer'
Write-Host "Registered $($pkg.PackageFullName)"
Write-Host "InstallLocation: $($pkg.InstallLocation)"
