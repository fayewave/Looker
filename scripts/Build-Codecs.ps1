<#
.SYNOPSIS
    Builds the codec DLLs Looker loads on demand (libheif + libde265 for HEIC, dav1d for AVIF, libavif for animated
    AVIF, LibRaw for camera RAW) with vcpkg, and collects them with their licences in %LOCALAPPDATA%\Looker\codecs.

.DESCRIPTION
    They are only used when Windows itself can't decode a file (no HEVC Video Extension, which is a paid Store
    add-on, or no AV1 Video Extension), except libavif, which plays animated AVIF (image sequences) everywhere:
    neither WIC nor libheif decodes those. Built as DLLs so the LGPL libraries stay replaceable, with the C runtime
    linked in (native/codecs/triplets/x64-windows-looker.cmake), release only. libheif comes from an overlay port
    (native/codecs/ports/libheif) that adds a dav1d feature and is built without its default x265 *encoder* (GPL);
    libavif from one (native/codecs/ports/libavif) built without libyuv, which would bring libjpeg-turbo along.
    Only the five DLLs Looker loads are collected, whatever else the dependency graph left in bin\.

    vcpkg itself lives in %LOCALAPPDATA%\Looker\vcpkg (cloned and bootstrapped on first run). NativeLayout.ps1
    copies the result into the package's codecs\ folder; this script also copies it next to the dev build
    (native-target\release\codecs) so unpackaged runs find it. The first build takes a while (~30-60 min).

.EXAMPLE
    pwsh scripts/Build-Codecs.ps1
#>
[CmdletBinding()]
param(
    [string]$Vcpkg = (Join-Path $env:LOCALAPPDATA 'Looker\vcpkg'),
    [string]$OutDir = (Join-Path $env:LOCALAPPDATA 'Looker\codecs')
)
$ErrorActionPreference = 'Stop'
$root = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
$codecs = Join-Path $root 'native\codecs'
$triplet = 'x64-windows-looker'
$installRoot = Join-Path $env:LOCALAPPDATA 'Looker\vcpkg-installed'

if (-not (Test-Path (Join-Path $Vcpkg 'vcpkg.exe'))) {
    if (-not (Test-Path $Vcpkg)) { git clone --depth 1 https://github.com/microsoft/vcpkg.git $Vcpkg }
    & (Join-Path $Vcpkg 'bootstrap-vcpkg.bat') -disableMetrics
    if ($LASTEXITCODE -ne 0) { throw "vcpkg bootstrap failed ($LASTEXITCODE)" }
}

# `install` never rebuilds an installed package, even when its overlay port changed; removing it first lets the
# binary cache (keyed on the port's contents) decide whether a rebuild is needed.
& (Join-Path $Vcpkg 'vcpkg.exe') remove "libheif:$triplet" "libavif:$triplet" --x-install-root $installRoot --overlay-ports (Join-Path $codecs 'ports') --overlay-triplets (Join-Path $codecs 'triplets') | Out-Null
& (Join-Path $Vcpkg 'vcpkg.exe') install 'libheif[core,dav1d]' --triplet $triplet `
    --overlay-ports (Join-Path $codecs 'ports') --overlay-triplets (Join-Path $codecs 'triplets') `
    --x-install-root $installRoot --clean-after-build
if ($LASTEXITCODE -ne 0) { throw "vcpkg install failed ($LASTEXITCODE)" }
# libavif shares the dav1d DLL above (its own default build has no decoder at all).
& (Join-Path $Vcpkg 'vcpkg.exe') install 'libavif[dav1d]' --triplet $triplet `
    --overlay-ports (Join-Path $codecs 'ports') --overlay-triplets (Join-Path $codecs 'triplets') `
    --x-install-root $installRoot --clean-after-build
if ($LASTEXITCODE -ne 0) { throw "vcpkg install libavif failed ($LASTEXITCODE)" }
# LibRaw (camera RAW without the Raw Image Extension) on a triplet of its own: raw_r.dll with jasper, lcms and zlib
# linked in, and Looker's entry point compiled into it (native/codecs/ports/libraw/looker_raw.cpp).
$rawTriplet = 'x64-windows-looker-raw'
& (Join-Path $Vcpkg 'vcpkg.exe') remove "libraw:$rawTriplet" --x-install-root $installRoot --overlay-ports (Join-Path $codecs 'ports') --overlay-triplets (Join-Path $codecs 'triplets') | Out-Null
& (Join-Path $Vcpkg 'vcpkg.exe') install 'libraw' --triplet $rawTriplet `
    --overlay-ports (Join-Path $codecs 'ports') --overlay-triplets (Join-Path $codecs 'triplets') `
    --x-install-root $installRoot --clean-after-build
if ($LASTEXITCODE -ne 0) { throw "vcpkg install libraw failed ($LASTEXITCODE)" }

$installed = Join-Path $installRoot $triplet
$rawInstalled = Join-Path $installRoot $rawTriplet
if (Test-Path $OutDir) { Remove-Item $OutDir -Recurse -Force }
New-Item -ItemType Directory (Join-Path $OutDir 'licenses') | Out-Null
foreach ($dll in 'heif.dll', 'libde265.dll', 'dav1d.dll', 'avif.dll') {
    Copy-Item (Join-Path $installed "bin\$dll") $OutDir
}
Copy-Item (Join-Path $rawInstalled 'bin\raw_r.dll') $OutDir
foreach ($port in 'libheif', 'libde265', 'dav1d', 'libavif') {
    Copy-Item (Join-Path $installed "share\$port\copyright") (Join-Path $OutDir "licenses\$port.txt")
}
foreach ($port in 'libraw', 'lcms', 'jasper', 'zlib') {
    Copy-Item (Join-Path $rawInstalled "share\$port\copyright") (Join-Path $OutDir "licenses\$port.txt")
}

$dev = Join-Path $env:LOCALAPPDATA 'Looker\native-target\release\codecs'
if (Test-Path (Split-Path $dev)) {
    if (Test-Path $dev) { Remove-Item $dev -Recurse -Force }
    Copy-Item $OutDir $dev -Recurse
}
Get-ChildItem $OutDir -Recurse -File | Select-Object @{ n = 'File'; e = { $_.FullName.Substring($OutDir.Length + 1) } }, Length | Format-Table -AutoSize
