<#
.SYNOPSIS
    Publishes a ReadyToRun (precompiled) Release build of Looker to a local folder and registers it as the
    dev-mode package, so day-to-day launches run the fast layout instead of the JIT-everything Debug build.

.DESCRIPTION
    A plain `dotnet build` never runs ReadyToRun (crossgen) - it is a publish-time step - so the Debug loose
    layout that `Add-AppxPackage -Register` normally points at JIT-compiles the 7 MB WinUI projection, the
    26 MB Windows SDK projection, Win2D interop and the app itself on every cold start (~60-70 ms of the
    startup budget on a fast machine, far more on a laptop). This script:

      1. `dotnet publish -c Release` with PublishReadyToRun (and, by default, PublishTrimmed) into
         %LOCALAPPDATA%\Looker\dev-<Platform> - outside Dropbox, so registration never hits a sync lock;
      2. copies the generated AppxManifest.xml next to it (publish does not) and stamps a unique
         revision, because dev-mode registration of the *same* version from a *different* folder
         silently keeps the old folder;
      3. registers it with -ForceUpdateFromAnyVersion (LocalSettings are preserved).

    Run the app afterwards exactly as before (Start menu, `Looker photo.jpg`, or the AppsFolder shell link).
    To go back to the Debug layout, run the usual `Add-AppxPackage -Register .../Debug/.../AppxManifest.xml`.

.EXAMPLE
    pwsh scripts/Publish-Dev.ps1              # ReadyToRun + trimmed
    pwsh scripts/Publish-Dev.ps1 -NoTrim      # ReadyToRun only, untrimmed (if a trimmed build misbehaves)
    pwsh scripts/Publish-Dev.ps1 -Aot         # Native AOT (fastest cold start; needs VS Build Tools C++)
#>
[CmdletBinding()]
param(
    [ValidateSet('x64', 'ARM64')]
    [string]$Platform = 'x64',
    [switch]$NoTrim,
    [switch]$Aot,
    [string]$OutDir = (Join-Path $env:LOCALAPPDATA "Looker\dev-$Platform")
)
$ErrorActionPreference = 'Stop'
$root = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
$project = Join-Path $root 'src\Looker\Looker.csproj'
$rid = if ($Platform -eq 'ARM64') { 'win-arm64' } else { 'win-x64' }
$binDir = Join-Path $root "src\Looker\bin\$Platform\Release\net8.0-windows10.0.26100.0\$rid"

if (Get-Process Looker -ErrorAction SilentlyContinue) {
    throw 'Looker is running; close it first (it locks Looker.exe and blocks re-registration).'
}

$props = @("-p:Platform=$Platform")
if ($Aot) {
    # Native AOT needs the MSVC linker; import the VS developer environment so link.exe and the SDK libs resolve
    # (the SDK's own vswhere lookup fails on this BuildTools-only install), then tell ILC to use tools from PATH.
    if (-not (Get-Command link.exe -ErrorAction SilentlyContinue)) {
        $devshell = Get-ChildItem 'C:\Program Files (x86)\Microsoft Visual Studio\*\BuildTools\Common7\Tools\Launch-VsDevShell.ps1',
                                  'C:\Program Files\Microsoft Visual Studio\*\*\Common7\Tools\Launch-VsDevShell.ps1' -ErrorAction SilentlyContinue | Select-Object -First 1
        if (-not $devshell) { throw 'Native AOT needs the MSVC toolchain (VS Build Tools with C++); Launch-VsDevShell.ps1 not found.' }
        & $devshell.FullName -Arch amd64 -HostArch amd64 -SkipAutomaticLocation *> $null
    }
    $props += '-p:PublishAot=true', '-p:IlcUseEnvironmentalTools=true'
}
else {
    $props += '-p:PublishReadyToRun=true'
    if ($NoTrim) { $props += '-p:PublishTrimmed=false' }
}

Write-Host "dotnet publish $project -c Release -o $OutDir $($props -join ' ')"
& dotnet publish $project -c Release -o $OutDir @props
if ($LASTEXITCODE -ne 0) { throw "publish failed ($LASTEXITCODE)" }

$manifestSrc = Join-Path $binDir 'AppxManifest.xml'
if (-not (Test-Path $manifestSrc)) { throw "generated manifest not found: $manifestSrc" }
$manifest = Get-Content $manifestSrc -Raw
# Unique revision per publish (16-bit field): minutes since midnight, so consecutive runs differ.
$revision = [int]((Get-Date) - (Get-Date).Date).TotalMinutes
$manifest = [regex]::Replace($manifest, '(<Identity[^>]*\sVersion=")(\d+)\.(\d+)\.(\d+)\.\d+(")', { param($m) "$($m.Groups[1].Value)$($m.Groups[2].Value).$($m.Groups[3].Value).$($m.Groups[4].Value).$revision$($m.Groups[5].Value)" })
Set-Content (Join-Path $OutDir 'AppxManifest.xml') $manifest -NoNewline

Write-Host "Registering $OutDir ..."
$attempt = 0
while ($true) {
    try {
        Add-AppxPackage -Register (Join-Path $OutDir 'AppxManifest.xml') -ForceUpdateFromAnyVersion -ErrorAction Stop
        break
    }
    catch {
        # 0x80070020: the previous location is still being touched (Dropbox sync); retry a few times.
        if (++$attempt -ge 6) { throw }
        Write-Host "  registration busy, retrying ($attempt)..."
        Start-Sleep -Seconds 3
    }
}
$pkg = Get-AppxPackage -Name 'fayewave.Looker-PhotoViewer'
Write-Host "Registered $($pkg.PackageFullName)"
Write-Host "InstallLocation: $($pkg.InstallLocation)"
