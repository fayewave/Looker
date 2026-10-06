# Shared by Publish-Native.ps1 (dev registration) and Build-NativePackage.ps1 (Store / sideload package):
# builds the native exe and lays out a package folder (Looker.exe, logo PNGs, AppxManifest.xml, resources.pri).
# Dot-source it, then call New-NativeLayout.

function Find-SdkTool([string]$name) {
    $tool = Get-ChildItem "C:\Program Files (x86)\Windows Kits\10\bin\*\x64\$name" -ErrorAction SilentlyContinue |
        Sort-Object FullName -Descending | Select-Object -First 1
    if (-not $tool) { throw "$name not found (Windows SDK)." }
    return $tool.FullName
}

# -Revision: the Identity Version's fourth field. Dev registration stamps a unique one (registering the same
# version from another folder is a silent no-op); a Store package keeps 0 (the Store requires it).
# -RequireCodecs: fail when the codec DLLs (scripts/Build-Codecs.ps1) are missing, rather than warn; a Store package
# without them can't open HEIC on machines without the paid HEVC Video Extension.
# -Publisher: replaces the manifest's Publisher, which must equal the signing certificate's subject (the GitHub
# release, signed with Trusted Signing, is its own package family beside the Store's).
function New-NativeLayout([string]$Root, [string]$OutDir, [int]$Revision = -1, [switch]$RequireCodecs, [string]$Publisher) {
    $native = Join-Path $Root 'native'
    $exe = Join-Path $env:LOCALAPPDATA 'Looker\native-target\release\looker.exe'

    Push-Location $native
    try {
        & cargo build --release 2>&1 | Out-Host   # not into the function's output (the version)
        if ($LASTEXITCODE -ne 0) { throw "cargo build failed ($LASTEXITCODE)" }
    }
    finally { Pop-Location }

    # A fresh layout every time, so nothing stale (an old exe, a removed asset) lingers in the package.
    if (Test-Path $OutDir) { Remove-Item $OutDir -Recurse -Force }
    New-Item -ItemType Directory $OutDir | Out-Null
    New-Item -ItemType Directory (Join-Path $OutDir 'Assets') | Out-Null
    Copy-Item $exe (Join-Path $OutDir 'Looker.exe')
    Copy-Item (Join-Path $Root 'native\assets\*.png') (Join-Path $OutDir 'Assets')

    # HEIC/AVIF fallback decoders and their licences (loaded only when WIC can't decode a file).
    $codecs = Join-Path $env:LOCALAPPDATA 'Looker\codecs'
    if (Test-Path (Join-Path $codecs 'heif.dll')) {
        Copy-Item $codecs (Join-Path $OutDir 'codecs') -Recurse
        Copy-Item (Join-Path $Root 'native\assets\Fonts\Inter-LICENSE.txt') (Join-Path $OutDir 'codecs\licenses\Inter.txt')
    }
    elseif ($RequireCodecs) { throw "Codec DLLs missing ($codecs): run scripts/Build-Codecs.ps1 first." }
    else { Write-Warning "Codec DLLs missing ($codecs): HEIC/AVIF need the Windows extensions in this build. Run scripts/Build-Codecs.ps1." }

    $manifest = Get-Content (Join-Path $native 'Package.appxmanifest') -Raw
    if ($Revision -ge 0) {
        $manifest = [regex]::Replace($manifest, '(<Identity[^>]*\sVersion=")(\d+)\.(\d+)\.(\d+)\.\d+(")', { param($m) "$($m.Groups[1].Value)$($m.Groups[2].Value).$($m.Groups[3].Value).$($m.Groups[4].Value).$Revision$($m.Groups[5].Value)" })
    }
    if ($Publisher) {
        $escaped = [Security.SecurityElement]::Escape($Publisher)
        $manifest = [regex]::Replace($manifest, '(<Identity[^>]*\sPublisher=")[^"]*(")', { param($m) "$($m.Groups[1].Value)$escaped$($m.Groups[2].Value)" })
    }
    $manifestPath = Join-Path $OutDir 'AppxManifest.xml'
    Set-Content $manifestPath $manifest -NoNewline

    # resources.pri: without it the shell can't pick Square44x44Logo.targetsize-24.png for "Square44x44Logo.png".
    $makepri = Find-SdkTool 'makepri.exe'
    $work = Join-Path ([IO.Path]::GetTempPath()) 'looker-pri'
    New-Item -ItemType Directory $work -Force | Out-Null
    $config = Join-Path $work 'priconfig.xml'
    & $makepri createconfig /cf $config /dq en-US /pv 10.0.0 /o | Out-Null
    if ($LASTEXITCODE -ne 0) { throw "makepri createconfig failed ($LASTEXITCODE)" }
    # One resources.pri with every scale: the default config splits scales into resources.scale-*.pri for
    # resource packages, which a single (unbundled) package never installs.
    $cfg = [xml](Get-Content $config -Raw)
    foreach ($p in @($cfg.SelectNodes('//packaging'))) { [void]$p.ParentNode.RemoveChild($p) }
    $cfg.Save($config)
    & $makepri new /pr $OutDir /cf $config /mn $manifestPath /of (Join-Path $OutDir 'resources.pri') /o | Out-Null
    if ($LASTEXITCODE -ne 0) { throw "makepri new failed ($LASTEXITCODE)" }

    return ([xml]$manifest).Package.Identity.Version
}
