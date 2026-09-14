<#
.SYNOPSIS
    Installs (or updates) a sideloaded Looker .msix, trusting its self-signed certificate first.

.DESCRIPTION
    Trusting the certificate requires the LocalMachine\TrustedPeople store, so this step elevates
    (one UAC prompt) if the shell is not already an administrator. The package install itself runs
    as the current user. Safe to re-run: an already-trusted cert is skipped and a newer package
    updates in place (window placement, MRU and settings are preserved).

.EXAMPLE
    pwsh scripts/Install-Package.ps1 -Msix artifacts/x64/Looker_1.0.3.0_x64.msix -Cert artifacts/cert/looker.cer
#>
[CmdletBinding()]
param(
    [Parameter(Mandatory)]
    [string]$Msix,
    [string]$Cert
)
$ErrorActionPreference = 'Stop'
$Msix = [IO.Path]::GetFullPath($Msix)
if (-not (Test-Path $Msix)) { throw "package not found: $Msix" }

if ($Cert) {
    $Cert = [IO.Path]::GetFullPath($Cert)
    if (-not (Test-Path $Cert)) { throw "certificate not found: $Cert" }
    $thumb = (New-Object System.Security.Cryptography.X509Certificates.X509Certificate2($Cert)).Thumbprint
    $trusted = Get-ChildItem Cert:\LocalMachine\TrustedPeople | Where-Object Thumbprint -eq $thumb
    if ($trusted) {
        Write-Host "Certificate $thumb already trusted."
    }
    else {
        $isAdmin = ([Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()).IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)
        if ($isAdmin) {
            Import-Certificate -FilePath $Cert -CertStoreLocation Cert:\LocalMachine\TrustedPeople | Out-Null
        }
        else {
            Write-Host 'Trusting the signing certificate (UAC prompt)...'
            $cmd = "Import-Certificate -FilePath '$Cert' -CertStoreLocation Cert:\LocalMachine\TrustedPeople | Out-Null"
            $p = Start-Process -FilePath (Get-Process -Id $PID).Path -Verb RunAs -Wait -PassThru -ArgumentList @('-NoProfile', '-Command', $cmd)
            if ($p.ExitCode -ne 0) { throw 'certificate import was cancelled or failed' }
        }
        Write-Host "Trusted certificate $thumb."
    }
}

Write-Host "Installing $Msix ..."
Add-AppxPackage -Path $Msix -ForceUpdateFromAnyVersion
$pkg = Get-AppxPackage -Name 'fayewave.Looker-PhotoViewer'
Write-Host "Installed $($pkg.PackageFullName)"
Write-Host 'Launch: Looker (Start menu) or `Looker photo.jpg` from a terminal; set as default in Settings > Apps > Default apps.'
