<#
.SYNOPSIS
    Builds a signed, sideloadable .msix of Looker from the command line (no Visual Studio).

.DESCRIPTION
    Wraps `dotnet build` with the single-project MSIX packaging properties. Signing uses either a
    certificate in the current user's store (-Thumbprint, from New-SigningCert.ps1) or a .pfx file
    (-PfxPath/-PfxPassword). Output lands in artifacts/<Platform>/.

.EXAMPLE
    pwsh scripts/Build-Package.ps1 -Store            # unsigned .msixupload for Partner Center
.EXAMPLE
    pwsh scripts/Build-Package.ps1 -Thumbprint 0123ABCD...
    pwsh scripts/Build-Package.ps1 -Platform ARM64 -PfxPath artifacts/cert/looker.pfx -PfxPassword Looker
#>
[CmdletBinding()]
param(
    [ValidateSet('x64', 'ARM64')]
    [string]$Platform = 'x64',
    [ValidateSet('Debug', 'Release')]
    [string]$Configuration = 'Release',
    [string]$Thumbprint,
    [string]$PfxPath,
    [string]$PfxPassword,
    [switch]$Store,
    [string]$OutDir = (Join-Path $PSScriptRoot '..\artifacts')
)
$ErrorActionPreference = 'Stop'
$root = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
$project = Join-Path $root 'src\Looker\Looker.csproj'
$packageDir = [IO.Path]::GetFullPath((Join-Path $OutDir $Platform)) + '\'
New-Item -ItemType Directory -Force $packageDir | Out-Null

if (Get-Process Looker -ErrorAction SilentlyContinue) {
    throw 'Looker is running and locks Looker.exe; close it first.'
}

$props = @(
    "-p:Platform=$Platform",
    '-p:GenerateAppxPackageOnBuild=true',
    '-p:AppxBundle=Never',
    "-p:UapAppxPackageBuildMode=$(if ($Store) { 'StoreUpload' } else { 'SideloadOnly' })",
    '-p:AppxSymbolPackageEnabled=false',
    '-p:GenerateTestArtifacts=false',
    "-p:AppxPackageDir=$packageDir"
)
if ($Store) {
    # The Store signs the package itself; the manifest's Identity must already carry the
    # Partner Center values (Name / Publisher / PublisherDisplayName).
    $props += '-p:AppxPackageSigningEnabled=false'
}
elseif ($Thumbprint) {
    $props += '-p:AppxPackageSigningEnabled=true'
    $props += "-p:PackageCertificateThumbprint=$Thumbprint"
}
elseif ($PfxPath) {
    $props += '-p:AppxPackageSigningEnabled=true'
    $props += "-p:PackageCertificateKeyFile=$([IO.Path]::GetFullPath($PfxPath))"
    if ($PfxPassword) { $props += "-p:PackageCertificatePassword=$PfxPassword" }
}
else {
    Write-Warning 'No -Thumbprint / -PfxPath: producing an UNSIGNED package (Add-AppxPackage will refuse it).'
    $props += '-p:AppxPackageSigningEnabled=false'
}

Write-Host "dotnet build $project -c $Configuration $($props -join ' ')"
& dotnet build $project -c $Configuration @props
if ($LASTEXITCODE -ne 0) { throw "build failed ($LASTEXITCODE)" }

if ($Store) {
    $upload = Get-ChildItem -Path $packageDir -Recurse -Filter '*.msixupload' | Sort-Object LastWriteTime -Descending | Select-Object -First 1
    if (-not $upload) { throw "no .msixupload found under $packageDir" }
    Write-Host ""
    Write-Host "Store upload package: $($upload.FullName)"
    Write-Host "Upload it under Partner Center > Looker > Packages, then submit."
}
else {
    $msix = Get-ChildItem -Path $packageDir -Recurse -Filter '*.msix' | Sort-Object LastWriteTime -Descending | Select-Object -First 1
    if (-not $msix) { throw "no .msix found under $packageDir" }
    Write-Host ""
    Write-Host "Package: $($msix.FullName)"
    Write-Host "Install: pwsh scripts/Install-Package.ps1 -Msix '$($msix.FullName)' -Cert artifacts/cert/looker.cer"
}
