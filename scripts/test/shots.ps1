param(
    [Parameter(Mandatory)][string]$File,
    [double[]]$At = @(2.5),
    [string]$Out = "$PSScriptRoot\shot",
    [string[]]$Keys = @(),  # posted (WM_KEYDOWN/UP to Looker's own window) right after the first capture
    [int]$W = 1280,
    [int]$H = 800
)
# Launches the branch's dev build on $File (muted, its own instance, the branch's codecs, sRGB), captures the window
# with PrintWindow at the given seconds after its window appears, then closes it.
Add-Type @"
using System;
using System.Runtime.InteropServices;
public static class W {
  [StructLayout(LayoutKind.Sequential)] public struct RECT { public int L, T, R, B; }
  [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
  [DllImport("user32.dll")] public static extern bool PrintWindow(IntPtr h, IntPtr dc, uint f);
  [DllImport("user32.dll")] public static extern bool PostMessageW(IntPtr h, uint m, IntPtr w, IntPtr l);
  [DllImport("user32.dll")] public static extern bool MoveWindow(IntPtr h, int x, int y, int w, int hh, bool r);
}
"@
Add-Type -AssemblyName System.Drawing
$env:LOOKER_NEW_INSTANCE = '1'
$env:LOOKER_MUTE = '1'
$env:LOOKER_COLOR = 'srgb'
$env:LOOKER_CODECS = "$env:LOCALAPPDATA\Looker\codecs-subs"
$exe = "$env:LOCALAPPDATA\Looker\native-target-subs\release\looker.exe"
$p = Start-Process $exe -ArgumentList "`"$File`"" -PassThru
$t0 = Get-Date
while ($p.MainWindowHandle -eq 0 -and ((Get-Date) - $t0).TotalSeconds -lt 10) { Start-Sleep -Milliseconds 100; $p.Refresh() }
$h = $p.MainWindowHandle
[W]::MoveWindow($h, 60, 60, $W, $H, $true) | Out-Null
$t1 = Get-Date
$i = 0
foreach ($s in $At) {
    $wait = $s - ((Get-Date) - $t1).TotalSeconds
    if ($wait -gt 0) { Start-Sleep -Milliseconds ([int]($wait * 1000)) }
    $r = New-Object W+RECT
    [W]::GetWindowRect($h, [ref]$r) | Out-Null
    $bmp = New-Object System.Drawing.Bitmap ($r.R - $r.L), ($r.B - $r.T)
    $g = [System.Drawing.Graphics]::FromImage($bmp)
    $dc = $g.GetHdc()
    [W]::PrintWindow($h, $dc, 2) | Out-Null
    $g.ReleaseHdc($dc)
    $g.Dispose()
    $path = "$Out-$i.png"
    $bmp.Save($path)
    $bmp.Dispose()
    "$path at $s s"
    if ($i -eq 0) {
        foreach ($k in $Keys) {
            $vk = [int][char]$k.ToUpper()
            if ($k -eq 'space') { $vk = 0x20 }
            [W]::PostMessageW($h, 0x100, [IntPtr]$vk, [IntPtr]0) | Out-Null
            [W]::PostMessageW($h, 0x101, [IntPtr]$vk, [IntPtr]0) | Out-Null
            Start-Sleep -Milliseconds 150
        }
    }
    $i++
}
Stop-Process -Id $p.Id -Force
