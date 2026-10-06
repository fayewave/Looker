<#
.SYNOPSIS
    Builds the GitHub release of Looker: an MSIX signed with Azure Trusted Signing plus the .appinstaller that
    keeps installs up to date, and uploads both to a GitHub release (a draft unless -Publish).

.DESCRIPTION
    People install Looker from GitHub by opening Looker.appinstaller from the latest release. Windows (App
    Installer) then reads that file again at its fixed address, releases/latest/download/Looker.appinstaller, on
    every launch and in the background, and installs a newer version itself; Looker's Settings > Updates asks
    the same file ("Update now" installs at once).

    The package keeps the Store's identity Name but takes the Trusted Signing certificate's subject as its
    Publisher (they must match), so it is its own package family beside a Store install.

    Version: native/Package.appxmanifest's <Identity Version> (revision 0, as for the Store). Bump it first;
    the tag is v<major>.<minor>.<build>. A draft doesn't change what releases/latest serves, so nothing reaches
    anyone until it is published (-Publish, or on github.com).

    Needs, once:
      - winget install Microsoft.Azure.TrustedSigningClientTools   (signtool's dlib)
      - signing in to Azure as an account with the "Trusted Signing Certificate Profile Signer" role on the
        certificate profile (az login, or Connect-AzAccount: the dlib uses DefaultAzureCredential)
      - artifacts/cert/trusted-signing.json: {"Endpoint": "https://<region>.codesigning.azure.net",
        "CodeSigningAccountName": "<account>", "CertificateProfileName": "<profile>"}
      - artifacts/cert/github-publisher.txt: the certificate's subject, exactly ("CN=..., O=..., L=..., S=..., C=...")
      - scripts/Build-Codecs.ps1 output, and gh signed in.

.EXAMPLE
    pwsh scripts/Publish-GitHubRelease.ps1                   # draft release for review
    pwsh scripts/Publish-GitHubRelease.ps1 -Publish -Notes 'Faster thumbnails.'
#>
[CmdletBinding()]
param(
    [string]$TrustedSigning = (Join-Path $PSScriptRoot '..\artifacts\cert\trusted-signing.json'),
    [string]$Publisher,
    [string]$Repo = 'fayewave/Looker',
    [string]$Notes = '',
    [switch]$Publish
)
$ErrorActionPreference = 'Stop'
$root = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
if (-not (Test-Path $TrustedSigning)) { throw "Trusted Signing metadata not found: $TrustedSigning" }
if (-not $Publisher) {
    $file = Join-Path $root 'artifacts\cert\github-publisher.txt'
    if (-not (Test-Path $file)) { throw "Pass -Publisher or write the certificate subject to $file." }
    $Publisher = (Get-Content $file -Raw).Trim()
}

$identity = ([xml](Get-Content (Join-Path $root 'native\Package.appxmanifest') -Raw)).Package.Identity
$version = $identity.Version
$tag = 'v' + ($version -replace '\.0$', '')
& gh release view $tag --repo $Repo *> $null
if ($LASTEXITCODE -eq 0) { throw "Release $tag already exists: bump <Identity Version> in native/Package.appxmanifest." }

$msix = & (Join-Path $PSScriptRoot 'Build-NativePackage.ps1') -TrustedSigning $TrustedSigning -Publisher $Publisher | Select-Object -Last 1
$signer = (Get-AuthenticodeSignature $msix).SignerCertificate
if ($signer -and $signer.Subject -ne $Publisher) {
    throw "Signed by '$($signer.Subject)' but the manifest says '$Publisher': write the subject exactly as signed."
}

# The .appinstaller: its own address (fixed, always the latest release's copy) and this release's package.
$dir = Split-Path $msix
$appinstaller = Join-Path $dir 'Looker.appinstaller'
$self = "https://github.com/$Repo/releases/latest/download/Looker.appinstaller"
$package = "https://github.com/$Repo/releases/download/$tag/$(Split-Path $msix -Leaf)"
$e = { param($s) [Security.SecurityElement]::Escape($s) }
@"
<?xml version="1.0" encoding="utf-8"?>
<AppInstaller xmlns="http://schemas.microsoft.com/appx/appinstaller/2021" Version="$version" Uri="$(& $e $self)">
  <MainPackage Name="$(& $e $identity.Name)" Publisher="$(& $e $Publisher)" Version="$version" ProcessorArchitecture="x64" Uri="$(& $e $package)" />
  <UpdateSettings>
    <OnLaunch HoursBetweenUpdateChecks="0" />
    <AutomaticBackgroundTask />
    <ForceUpdateFromAnyVersion>true</ForceUpdateFromAnyVersion>
  </UpdateSettings>
</AppInstaller>
"@ | Set-Content $appinstaller -Encoding utf8NoBOM

$ghArgs = @('release', 'create', $tag, $msix, $appinstaller, '--repo', $Repo, '--title', "Looker $($tag.TrimStart('v'))", '--notes', $Notes)
if (-not $Publish) { $ghArgs += '--draft' }
& gh @ghArgs
if ($LASTEXITCODE -ne 0) { throw "gh release create failed ($LASTEXITCODE)" }
Write-Host ""
if ($Publish) { Write-Host "Published ${tag}: installs update to it on their next launch." }
else { Write-Host "Draft ${tag} created: publish it on GitHub to ship it." }
