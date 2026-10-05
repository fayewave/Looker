<#
.SYNOPSIS
    Packages the native Looker: an unsigned .msixupload for Partner Center (-Store), or a signed sideload .msix.

.DESCRIPTION
    Lays the package out like Publish-Native.ps1 (NativeLayout.ps1) but keeps the manifest's version as it is
    (revision 0, as the Store requires: bump <Identity Version> in native/Package.appxmanifest before each
    submission), packs it with the SDK's makeappx into artifacts/<Platform>/Looker_<version>_<Platform>.msix,
    then either wraps it as .msixupload for the Store (which signs it) or signs it with signtool for sideloading
    (-Thumbprint from New-SigningCert.ps1, or -PfxPath/-PfxPassword; install with Install-Package.ps1).

.EXAMPLE
    pwsh scripts/Build-NativePackage.ps1 -Store
    pwsh scripts/Build-NativePackage.ps1 -PfxPath artifacts/cert/looker.pfx -PfxPassword Looker
#>
[CmdletBinding()]
param(
    [ValidateSet('x64')]
    [string]$Platform = 'x64',
    [switch]$Store,
    [string]$Thumbprint,
    [string]$PfxPath,
    [string]$PfxPassword,
    [string]$OutDir = (Join-Path $PSScriptRoot '..\artifacts')
)
$ErrorActionPreference = 'Stop'
$root = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
. (Join-Path $PSScriptRoot 'NativeLayout.ps1')
if (-not $Store -and -not $Thumbprint -and -not $PfxPath) {
    throw 'Pass -Store (Partner Center upload) or a signing certificate (-Thumbprint / -PfxPath) for sideloading.'
}

$layout = Join-Path ([IO.Path]::GetTempPath()) "looker-native-package-$Platform"
$version = New-NativeLayout -Root $root -OutDir $layout -RequireCodecs
if ($version -notmatch '\.0$') { throw "The Store needs revision 0; the manifest says $version." }

$packageDir = [IO.Path]::GetFullPath((Join-Path $OutDir $Platform))
New-Item -ItemType Directory -Force $packageDir | Out-Null
$msix = Join-Path $packageDir "Looker_${version}_$Platform.msix"
$makeappx = Find-SdkTool 'makeappx.exe'
$log = & $makeappx pack /d $layout /p $msix /o 2>&1
if ($LASTEXITCODE -ne 0) { $log | Out-Host; throw "makeappx pack failed ($LASTEXITCODE)" }

if ($Store) {
    # An .msixupload is a zip holding the package (and optionally its symbols); the Store signs it.
    $upload = [IO.Path]::ChangeExtension($msix, '.msixupload')
    $zip = [IO.Path]::ChangeExtension($msix, '.zip')
    Remove-Item $upload, $zip -ErrorAction SilentlyContinue
    Compress-Archive -Path $msix -DestinationPath $zip
    Move-Item $zip $upload
    Remove-Item $msix
    Write-Host ""
    Write-Host "Store upload package: $upload"
    Write-Host "Upload it under Partner Center > Looker > Packages, then submit."
}
else {
    $signtool = Find-SdkTool 'signtool.exe'
    $sign = @('sign', '/fd', 'SHA256')
    if ($Thumbprint) { $sign += '/sha1', $Thumbprint }
    else {
        $sign += '/f', ([IO.Path]::GetFullPath($PfxPath))
        if ($PfxPassword) { $sign += '/p', $PfxPassword }
    }
    & $signtool @sign $msix | Out-Host
    if ($LASTEXITCODE -ne 0) { throw "signtool failed ($LASTEXITCODE)" }
    Write-Host ""
    Write-Host "Package: $msix"
    Write-Host "Install: pwsh scripts/Install-Package.ps1 -Msix '$msix' -Cert artifacts/cert/looker.cer"
}
