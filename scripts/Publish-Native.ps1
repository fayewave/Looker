<#
.SYNOPSIS
    Builds the native Looker (native/, Rust) and registers it as the dev-mode package under the Store identity,
    replacing whatever Looker layout is registered now (the C# app's Debug or Publish-Dev layout).

.DESCRIPTION
      1. cargo build --release (target dir is %LOCALAPPDATA%\Looker\native-target, outside Dropbox);
      2. lays the package out in %LOCALAPPDATA%\Looker\native-pkg-<Platform>: Looker.exe, the logo PNGs from
         src/Looker/Assets and native/Package.appxmanifest with a unique revision stamped in (registering the
         same version from another folder is a silent no-op);
      3. indexes the scale/targetsize logo variants into resources.pri with the SDK's makepri;
      4. registers it with -ForceUpdateFromAnyVersion. LocalSettings survive (never Remove-AppxPackage: that
         wipes them), so the first packaged launch migrates the C# app's preferences.

    Afterwards Looker runs from the Start menu, "Open with", `Looker photo.jpg` (the alias) or
    shell:AppsFolder\fayewave.Looker-PhotoViewer_tp8spdf6gttjc!App. Go back to the C# app with
    `pwsh scripts/Publish-Dev.ps1` or the Debug layout's Add-AppxPackage -Register.

.EXAMPLE
    pwsh scripts/Publish-Native.ps1
#>
[CmdletBinding()]
param(
    [ValidateSet('x64')]
    [string]$Platform = 'x64',
    [string]$OutDir = (Join-Path $env:LOCALAPPDATA "Looker\native-pkg-$Platform")
)
$ErrorActionPreference = 'Stop'
$root = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
$native = Join-Path $root 'native'
$exe = Join-Path $env:LOCALAPPDATA 'Looker\native-target\release\looker.exe'

if (Get-Process Looker -ErrorAction SilentlyContinue) {
    throw 'Looker is running; close it first (re-registration replaces the files it runs from).'
}

Push-Location $native
try {
    & cargo build --release
    if ($LASTEXITCODE -ne 0) { throw "cargo build failed ($LASTEXITCODE)" }
}
finally { Pop-Location }

# A fresh layout every time, so nothing stale (an old exe, a removed asset) lingers in the package.
if (Test-Path $OutDir) { Remove-Item $OutDir -Recurse -Force }
New-Item -ItemType Directory $OutDir | Out-Null
New-Item -ItemType Directory (Join-Path $OutDir 'Assets') | Out-Null
Copy-Item $exe (Join-Path $OutDir 'Looker.exe')
Copy-Item (Join-Path $root 'src\Looker\Assets\*.png') (Join-Path $OutDir 'Assets')

$manifest = Get-Content (Join-Path $native 'Package.appxmanifest') -Raw
# Unique revision per publish (16-bit field): minutes since midnight, so consecutive runs differ.
$revision = [int]((Get-Date) - (Get-Date).Date).TotalMinutes
$manifest = [regex]::Replace($manifest, '(<Identity[^>]*\sVersion=")(\d+)\.(\d+)\.(\d+)\.\d+(")', { param($m) "$($m.Groups[1].Value)$($m.Groups[2].Value).$($m.Groups[3].Value).$($m.Groups[4].Value).$revision$($m.Groups[5].Value)" })
$manifestPath = Join-Path $OutDir 'AppxManifest.xml'
Set-Content $manifestPath $manifest -NoNewline

# resources.pri: without it the shell can't pick Square44x44Logo.targetsize-24.png for "Square44x44Logo.png".
$makepri = Get-ChildItem 'C:\Program Files (x86)\Windows Kits\10\bin\*\x64\makepri.exe' | Sort-Object FullName -Descending | Select-Object -First 1
if (-not $makepri) { throw 'makepri.exe not found (Windows SDK).' }
$work = Join-Path ([IO.Path]::GetTempPath()) 'looker-pri'
New-Item -ItemType Directory $work -Force | Out-Null
$config = Join-Path $work 'priconfig.xml'
& $makepri.FullName createconfig /cf $config /dq en-US /pv 10.0.0 /o | Out-Null
if ($LASTEXITCODE -ne 0) { throw "makepri createconfig failed ($LASTEXITCODE)" }
& $makepri.FullName new /pr $OutDir /cf $config /mn $manifestPath /of (Join-Path $OutDir 'resources.pri') /o | Out-Null
if ($LASTEXITCODE -ne 0) { throw "makepri new failed ($LASTEXITCODE)" }

Write-Host "Registering $OutDir ..."
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
