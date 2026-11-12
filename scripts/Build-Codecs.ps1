<#
.SYNOPSIS
    Builds the codec DLLs Looker loads on demand (libheif + libde265 for HEIC, dav1d for AVIF, libavif for animated
    AVIF, LibRaw for camera RAW, FFmpeg + looker_video.dll for video) with vcpkg, and collects them with their
    licences in %LOCALAPPDATA%\Looker\codecs.

.DESCRIPTION
    They are only used when Windows itself can't decode a file (no HEVC Video Extension, which is a paid Store
    add-on, or no AV1 Video Extension), except libavif, which plays animated AVIF (image sequences) everywhere:
    neither WIC nor libheif decodes those. Built as DLLs so the LGPL libraries stay replaceable, with the C runtime
    linked in (native/codecs/triplets/x64-windows-looker.cmake), release only. libheif comes from an overlay port
    (native/codecs/ports/libheif) that adds a dav1d feature and is built without its default x265 *encoder* (GPL);
    libavif from one (native/codecs/ports/libavif) built without libyuv, which would bring libjpeg-turbo along.
    Video: FFmpeg from an overlay port (native/codecs/ports/ffmpeg, vcpkg's with --disable-everything and only the
    containers and codecs Looker plays, LGPL only) and looker_video.dll, Looker's own entry points into it
    (native/codecs/ports/looker-video), sharing dav1d. Subtitles: libass from an overlay port (native/codecs/ports/libass:
    FreeType without its optional zlib/PNG/brotli/bzip2, fonts from DirectWrite), with HarfBuzz and FriBidi, each a
    DLL (libass ISC, FreeType FTL, HarfBuzz MIT, FriBidi LGPL-2.1).
    Only the DLLs Looker loads (and theirs) are collected, whatever else the dependency graph left in bin\.

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
# Video: looker_video.dll pulls in the trimmed FFmpeg.
& (Join-Path $Vcpkg 'vcpkg.exe') remove "looker-video:$triplet" "ffmpeg:$triplet" "libass:$triplet" --x-install-root $installRoot --overlay-ports (Join-Path $codecs 'ports') --overlay-triplets (Join-Path $codecs 'triplets') | Out-Null
# FreeType and HarfBuzz named with [core]: classic mode would otherwise build their default features (zlib, PNG,
# brotli, bzip2 beside FreeType) whatever libass asks for.
& (Join-Path $Vcpkg 'vcpkg.exe') install 'looker-video' 'freetype[core]' 'harfbuzz[core,freetype]' --triplet $triplet `
    --overlay-ports (Join-Path $codecs 'ports') --overlay-triplets (Join-Path $codecs 'triplets') `
    --x-install-root $installRoot --clean-after-build
if ($LASTEXITCODE -ne 0) { throw "vcpkg install looker-video failed ($LASTEXITCODE)" }
# Archives (zip, 7z, RAR, tar, gz/bz2/xz/zst, cab, ISO...): libarchive (BSD) as archive.dll, read by Looker through
# imaging/archive.rs. Not its lzo feature (liblzo2 is GPL) nor OpenSSL (Windows' own CNG decrypts AES zips).
& (Join-Path $Vcpkg 'vcpkg.exe') install 'libarchive[core,bzip2,lz4,lzma,zstd]' --triplet $triplet `
    --overlay-ports (Join-Path $codecs 'ports') --overlay-triplets (Join-Path $codecs 'triplets') `
    --x-install-root $installRoot --clean-after-build
if ($LASTEXITCODE -ne 0) { throw "vcpkg install libarchive failed ($LASTEXITCODE)" }
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
# looker_video.dll and every DLL it loads, as far down as they go, from vcpkg's bin\ (FFmpeg's, libass with
# FreeType, HarfBuzz and FriBidi, zlib, FluidSynth, libopenmpt with mpg123 and Vorbis, libzvbi, liblc3,
# libaribcaption...): read from their import tables, so a library added to the build ships without a list here.
function Get-PeImports([string]$Path) {
    $b = [IO.File]::ReadAllBytes($Path)
    $pe = [BitConverter]::ToInt32($b, 0x3C)
    $opt = $pe + 24
    $magic = [BitConverter]::ToUInt16($b, $opt)
    $dirs = $opt + $(if ($magic -eq 0x20B) { 112 } else { 96 })
    $importRva = [BitConverter]::ToUInt32($b, $dirs + 8)
    $nsec = [BitConverter]::ToUInt16($b, $pe + 6)
    $sec = $opt + [BitConverter]::ToUInt16($b, $pe + 20)
    $toOff = {
        param($rva)
        for ($i = 0; $i -lt $nsec; $i++) {
            $s = $sec + 40 * $i
            $va = [BitConverter]::ToUInt32($b, $s + 12); $size = [BitConverter]::ToUInt32($b, $s + 16); $raw = [BitConverter]::ToUInt32($b, $s + 20)
            if ($rva -ge $va -and $rva -lt $va + [Math]::Max($size, [BitConverter]::ToUInt32($b, $s + 8))) { return $rva - $va + $raw }
        }
        return -1
    }
    $names = @()
    $d = & $toOff $importRva
    while ($d -ge 0 -and [BitConverter]::ToUInt32($b, $d + 12) -ne 0) {
        $n = & $toOff ([BitConverter]::ToUInt32($b, $d + 12))
        $e = $n; while ($b[$e] -ne 0) { $e++ }
        $names += [Text.Encoding]::ASCII.GetString($b, $n, $e - $n)
        $d += 20
    }
    $names
}
$queue = [Collections.Generic.Queue[string]]::new()
$queue.Enqueue('looker_video.dll')
$queue.Enqueue('archive.dll')
$shipped = @{}
while ($queue.Count -gt 0) {
    $name = $queue.Dequeue()
    if ($shipped.ContainsKey($name.ToLowerInvariant())) { continue }
    $src = Join-Path $installed "bin\$name"
    if (-not (Test-Path $src)) { continue } # Windows' own (KERNEL32, USER32...)
    $shipped[$name.ToLowerInvariant()] = $true
    Copy-Item $src $OutDir
    foreach ($dep in Get-PeImports $src) { $queue.Enqueue($dep) }
}
Write-Host "looker_video.dll ships with: $(($shipped.Keys | Sort-Object) -join ', ')"
foreach ($port in 'libheif', 'libde265', 'dav1d', 'libavif', 'ffmpeg', 'libass', 'freetype', 'harfbuzz', 'fribidi', 'zlib',
    'fluidsynth', 'libopenmpt', 'mpg123', 'libogg', 'libvorbis', 'libzvbi', 'liblc3', 'libaribcaption',
    'libarchive', 'bzip2', 'lz4', 'liblzma', 'zstd') {
    $c = Join-Path $installed "share\$port\copyright"
    if (Test-Path $c) { Copy-Item $c (Join-Path $OutDir "licenses\$port.txt") }
}
# MIDI's instruments: GeneralUser GS (S. Christian Collins; free to ship in software, LICENSE.txt), pinned to a
# commit and checked, kept outside Dropbox (32 MB) and downloaded once.
$sfDir = Join-Path $env:LOCALAPPDATA 'Looker\soundfont'
$sfRef = '684543d5e5efaef08d02be50dcda8d552478fa60'
$sfFiles = @{
    'GeneralUser-GS.sf2'       = @('GeneralUser-GS.sf2', '9575028C7A1F589F5770FCCC8CFF2734566AF40CD26ED836944E9A5152688CFE')
    'documentation/LICENSE.txt' = @('LICENSE.txt', '7B32EFEFDF95CE38A043799F0659853DDC00FBAA14D8C50F0ACA16B9B8B405BE')
}
New-Item -ItemType Directory $sfDir -Force | Out-Null
foreach ($k in $sfFiles.Keys) {
    $dst = Join-Path $sfDir $sfFiles[$k][0]
    if (-not (Test-Path $dst) -or (Get-FileHash $dst -Algorithm SHA256).Hash -ne $sfFiles[$k][1]) {
        Invoke-WebRequest "https://raw.githubusercontent.com/mrbumpy409/GeneralUser-GS/$sfRef/$k" -OutFile $dst
        if ((Get-FileHash $dst -Algorithm SHA256).Hash -ne $sfFiles[$k][1]) { throw "GeneralUser GS: $k doesn't match its hash" }
    }
}
Copy-Item (Join-Path $sfDir 'GeneralUser-GS.sf2') $OutDir
Copy-Item (Join-Path $sfDir 'LICENSE.txt') (Join-Path $OutDir 'licenses\GeneralUser-GS.txt')
foreach ($port in 'libraw', 'lcms', 'jasper', 'zlib') {
    Copy-Item (Join-Path $rawInstalled "share\$port\copyright") (Join-Path $OutDir "licenses\$port.txt")
}

$dev = Join-Path $env:LOCALAPPDATA 'Looker\native-target\release\codecs'
if (Test-Path (Split-Path $dev)) {
    if (Test-Path $dev) { Remove-Item $dev -Recurse -Force }
    Copy-Item $OutDir $dev -Recurse
}
Get-ChildItem $OutDir -Recurse -File | Select-Object @{ n = 'File'; e = { $_.FullName.Substring($OutDir.Length + 1) } }, Length | Format-Table -AutoSize
