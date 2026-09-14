<#
.SYNOPSIS
    Cold-start timing for the currently registered Looker (or an alternating A/B between registered layouts).

.DESCRIPTION
    For each run: kills Looker, launches it through the app-execution alias (empty, then with -File), and polls
    until a top-level window exists ("window") and, with a file, until the title shows the file name ("ready").
    The in-app timeline (%TEMP%\looker-startup.log, written by Helpers/StartupTrace.cs: ms since process
    creation for each startup phase, plus "first Win2D Draw (image=True/False)") is the authoritative
    "pixels on screen" number; this script prints the fastest run's timeline at the end.

    This machine's background load (Dropbox, Premiere, ...) makes single runs vary by 30-100 ms, and the first
    launch after any (re)registration is ~2x slow, so: the first launch is discarded, several runs are taken,
    and min/median are reported. To compare two builds, pass -Layouts (label -> AppxManifest.xml) and the
    script registers each in turn, alternating per round so drift cancels out.

.EXAMPLE
    pwsh scripts/Measure-Startup.ps1
    pwsh scripts/Measure-Startup.ps1 -Runs 6 -File "$env:USERPROFILE\Pictures\Looker Test Photos\01 JPEG.jpg"
    pwsh scripts/Measure-Startup.ps1 -Layouts @{ r2r = "$env:LOCALAPPDATA\Looker\dev-x64\AppxManifest.xml"; aot = "$env:LOCALAPPDATA\Looker\dev-x64-aot\AppxManifest.xml" }
#>
[CmdletBinding()]
param(
    [hashtable]$Layouts,
    [int]$Rounds = 2,
    [int]$Runs = 4,
    [string]$File = "$env:USERPROFILE\Pictures\Looker Test Photos\01 JPEG.jpg"
)
$alias = "$env:LOCALAPPDATA\Microsoft\WindowsApps\Looker.exe"
$trace = Join-Path $env:TEMP 'looker-startup.log'
$results = @()

function Stop-Looker {
    Get-Process Looker -ErrorAction SilentlyContinue | Stop-Process -Force -ErrorAction SilentlyContinue
    while (Get-Process Looker -ErrorAction SilentlyContinue) { Start-Sleep -Milliseconds 50 }
}
function Register-Layout($manifest) {
    for ($i = 0; $i -lt 8; $i++) {
        try { Add-AppxPackage -Register $manifest -ForceUpdateFromAnyVersion -ErrorAction Stop; return } catch { Start-Sleep -Seconds 2 }
    }
    throw "could not register $manifest"
}
function Invoke-Launch($file) {
    $sw = [Diagnostics.Stopwatch]::StartNew()
    if ($file) { Start-Process $alias -ArgumentList "`"$file`"" } else { Start-Process $alias }
    $tWindow = $null; $tReady = $null
    while ($sw.ElapsedMilliseconds -lt 15000) {
        $p = Get-Process Looker -ErrorAction SilentlyContinue | Select-Object -First 1
        if ($p) {
            $p.Refresh()
            if (-not $tWindow -and $p.MainWindowHandle -ne 0) { $tWindow = $sw.ElapsedMilliseconds }
            if ($tWindow -and (-not $file -or $p.MainWindowTitle -eq [IO.Path]::GetFileName($file))) { $tReady = $sw.ElapsedMilliseconds; break }
        }
        Start-Sleep -Milliseconds 4
    }
    Start-Sleep -Milliseconds 1400   # let the trace flush (1.2 s after Loaded)
    Stop-Looker
    Start-Sleep -Milliseconds 400
    return @{ Window = $tWindow; Ready = $tReady }
}
function Measure-Layout($label) {
    Invoke-Launch $null | Out-Null   # discard: first launch after (re)registration
    for ($i = 1; $i -le $Runs; $i++) {
        $e = Invoke-Launch $null
        $script:results += [pscustomobject]@{ Layout = $label; Mode = 'empty'; WindowMs = $e.Window; ReadyMs = $e.Ready }
        if ($File) {
            $f = Invoke-Launch $File
            $script:results += [pscustomobject]@{ Layout = $label; Mode = 'file'; WindowMs = $f.Window; ReadyMs = $f.Ready }
        }
    }
}

Stop-Looker
if (Test-Path $trace) { Clear-Content $trace }

if ($Layouts) {
    for ($round = 1; $round -le $Rounds; $round++) {
        $order = if ($round % 2 -eq 1) { $Layouts.Keys | Sort-Object } else { $Layouts.Keys | Sort-Object -Descending }
        foreach ($label in $order) {
            Register-Layout $Layouts[$label]
            $loc = (Get-AppxPackage -Name fayewave.Looker-PhotoViewer).InstallLocation
            if ($loc -ne (Split-Path $Layouts[$label])) { throw "registration for $label did not take (same version as the current one? bump it): $loc" }
            Measure-Layout $label
        }
    }
}
else {
    $loc = (Get-AppxPackage -Name fayewave.Looker-PhotoViewer).InstallLocation
    Write-Host "Registered layout: $loc"
    Measure-Layout 'registered'
}

$results | Format-Table -AutoSize | Out-String -Width 120 | Write-Host
Write-Host '=== summary (ms; "ready" = title shows the file name, not pixels — see the trace below for those) ==='
foreach ($g in ($results | Group-Object Layout, Mode | Sort-Object Name)) {
    $w = @($g.Group.WindowMs | Where-Object { $_ } | Sort-Object); $r = @($g.Group.ReadyMs | Where-Object { $_ } | Sort-Object)
    if ($w.Count -eq 0) { Write-Host ("{0,-24} NO WINDOW (crashed? see %TEMP%\looker-crash.log / Event Viewer)" -f $g.Name); continue }
    Write-Host ("{0,-24} window min {1,4} med {2,4}   ready min {3,4} med {4,4}   (n={5})" -f $g.Name, $w[0], $w[[int]($w.Count / 2)], $r[0], $r[[int]($r.Count / 2)], $w.Count)
}

if (Test-Path $trace) {
    $sections = (Get-Content $trace -Raw) -split '(?m)^=== ' | Where-Object { $_ -match 'first Win2D Draw start' }
    $fastest = $sections | Sort-Object { if ($_ -match '(\d+) ms  \(\+\s*\d+\)  \[t1\] first Win2D Draw start') { [int]$Matches[1] } else { 99999 } } | Select-Object -First 1
    Write-Host "`n=== fastest in-app timeline (ms since process start) ==="
    $fastest -split "`n" | Where-Object { $_ -match ' ms ' } | ForEach-Object { Write-Host $_.Trim() }
}
