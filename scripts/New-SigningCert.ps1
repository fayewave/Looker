<#
.SYNOPSIS
    Creates a self-signed code-signing certificate for sideloading Looker and exports it.

.DESCRIPTION
    The certificate Subject must equal the Publisher in Package.appxmanifest (CN=75645224-1CB5-47AA-A845-256EA45E9908), otherwise
    signing succeeds but Add-AppxPackage rejects the package. The cert goes into the current user's
    personal store (for signing) and is exported as:

        artifacts/cert/looker.pfx   private key, password-protected  -> used by Build-NativePackage.ps1
        artifacts/cert/looker.cer   public part                       -> installed on machines that sideload

    Run once; re-run to rotate. Prints the thumbprint at the end.
#>
[CmdletBinding()]
param(
    [string]$Subject = 'CN=75645224-1CB5-47AA-A845-256EA45E9908',
    [string]$OutDir = (Join-Path $PSScriptRoot '..\artifacts\cert'),
    [string]$Password = 'Looker',
    [int]$ValidYears = 5
)
$ErrorActionPreference = 'Stop'
$OutDir = [IO.Path]::GetFullPath($OutDir)
New-Item -ItemType Directory -Force $OutDir | Out-Null

$cert = New-SelfSignedCertificate `
    -Type Custom `
    -Subject $Subject `
    -KeyUsage DigitalSignature `
    -FriendlyName 'Looker sideload signing' `
    -CertStoreLocation 'Cert:\CurrentUser\My' `
    -TextExtension @('2.5.29.37={text}1.3.6.1.5.5.7.3.3', '2.5.29.19={text}') `
    -NotAfter (Get-Date).AddYears($ValidYears)

$secure = ConvertTo-SecureString -String $Password -AsPlainText -Force
Export-PfxCertificate -Cert $cert -FilePath (Join-Path $OutDir 'looker.pfx') -Password $secure | Out-Null
Export-Certificate -Cert $cert -FilePath (Join-Path $OutDir 'looker.cer') | Out-Null

Write-Host "Created $Subject"
Write-Host "  Thumbprint : $($cert.Thumbprint)"
Write-Host "  PFX        : $(Join-Path $OutDir 'looker.pfx')  (password: $Password)"
Write-Host "  CER        : $(Join-Path $OutDir 'looker.cer')"
Write-Host ""
Write-Host "Next: pwsh scripts/Build-NativePackage.ps1 -Thumbprint $($cert.Thumbprint)"
