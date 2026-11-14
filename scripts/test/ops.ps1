# Drives a hidden dev Looker: start, capture, post clicks/keys (DIPs = pixels with LOOKER_DPI=96), stop.
# usage: . ops.ps1 ; Start-Looker <args>; Shot <name>; Click x y [-Right]; Key <vk>; Stop-Looker
Add-Type -TypeDefinition @'
using System; using System.Runtime.InteropServices; using System.Text;
public static class W {
  public delegate bool EnumProc(IntPtr h, IntPtr l);
  [DllImport("user32.dll")] public static extern bool EnumWindows(EnumProc f, IntPtr l);
  [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr h, out uint pid);
  [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr h);
  [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern int GetClassName(IntPtr h, StringBuilder s, int n);
  [DllImport("user32.dll")] public static extern bool SetWindowPos(IntPtr h, IntPtr after, int x, int y, int cx, int cy, uint f);
  [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
  [DllImport("user32.dll")] public static extern bool GetClientRect(IntPtr h, out RECT r);
  [DllImport("user32.dll")] public static extern bool PrintWindow(IntPtr h, IntPtr hdc, uint f);
  [DllImport("user32.dll")] public static extern bool PostMessageW(IntPtr h, uint m, IntPtr w, IntPtr l);
  [StructLayout(LayoutKind.Sequential)] public struct RECT { public int L, T, R, B; }
  public static IntPtr Find(uint pid) {
    IntPtr found = IntPtr.Zero;
    EnumWindows((h, l) => { uint p; GetWindowThreadProcessId(h, out p); var sb = new StringBuilder(64); GetClassName(h, sb, 64);
      if (p == pid && sb.ToString().StartsWith("Looker")) { found = h; return false; } return true; }, IntPtr.Zero);
    return found;
  }
}
'@
Add-Type -AssemblyName System.Drawing
$script:exe = Join-Path $env:LOCALAPPDATA 'Looker\native-target\release\looker.exe'
$script:shots = $PSScriptRoot

function Start-Looker([string[]]$LookerArgs) {
  $env:LOOKER_NEW_INSTANCE = '1'; $env:LOOKER_DPI = '96'; $env:LOOKER_COLOR = $(if ($env:TEST_COLOR) { $env:TEST_COLOR } else { 'srgb' }); $env:LOOKER_MUTE = '1'
  $script:proc = Start-Process $script:exe -ArgumentList $LookerArgs -WindowStyle Hidden -PassThru
  for ($i = 0; $i -lt 100 -and ($script:hwnd = [W]::Find($script:proc.Id)) -eq [IntPtr]::Zero; $i++) { Start-Sleep -Milliseconds 100 }
  if ($script:hwnd -eq [IntPtr]::Zero) { throw 'no Looker window' }
  # HWND_BOTTOM, SWP_NOMOVE|SWP_NOSIZE|SWP_NOACTIVATE|SWP_SHOWWINDOW
  [void][W]::SetWindowPos($script:hwnd, [IntPtr]1, 0, 0, 0, 0, 0x0002 -bor 0x0001 -bor 0x0010 -bor 0x0040)
  Start-Sleep -Seconds 3
}

function Shot([string]$name) {
  $r = New-Object W+RECT; [void][W]::GetWindowRect($script:hwnd, [ref]$r)
  $bmp = New-Object System.Drawing.Bitmap ($r.R - $r.L), ($r.B - $r.T)
  $g = [System.Drawing.Graphics]::FromImage($bmp); $hdc = $g.GetHdc()
  [void][W]::PrintWindow($script:hwnd, $hdc, 2); $g.ReleaseHdc($hdc); $g.Dispose()
  $path = Join-Path $script:shots "$name.png"; $bmp.Save($path); $bmp.Dispose(); $path
}

function lp([int]$x, [int]$y) { [IntPtr](($y -shl 16) -bor ($x -band 0xFFFF)) }

function Click([int]$x, [int]$y, [switch]$Right) {
  $l = lp $x $y
  [void][W]::PostMessageW($script:hwnd, 0x0200, [IntPtr]0, $l)  # WM_MOUSEMOVE
  Start-Sleep -Milliseconds 60
  if ($Right) { [void][W]::PostMessageW($script:hwnd, 0x0204, [IntPtr]2, $l); [void][W]::PostMessageW($script:hwnd, 0x0205, [IntPtr]0, $l) }
  else { [void][W]::PostMessageW($script:hwnd, 0x0201, [IntPtr]1, $l); Start-Sleep -Milliseconds 40; [void][W]::PostMessageW($script:hwnd, 0x0202, [IntPtr]0, $l) }
  Start-Sleep -Milliseconds 500
}

function Key([int]$vk) {
  [void][W]::PostMessageW($script:hwnd, 0x0100, [IntPtr]$vk, [IntPtr]1); [void][W]::PostMessageW($script:hwnd, 0x0101, [IntPtr]$vk, [IntPtr]0xC0000001)
  Start-Sleep -Milliseconds 300
}

function Stop-Looker { if ($script:proc -and -not $script:proc.HasExited) { Stop-Process -Id $script:proc.Id -Force } }

function Down([int]$x, [int]$y) { $l = lp $x $y; [void][W]::PostMessageW($script:hwnd, 0x0200, [IntPtr]0, $l); Start-Sleep -Milliseconds 30; [void][W]::PostMessageW($script:hwnd, 0x0201, [IntPtr]1, $l); Start-Sleep -Milliseconds 30 }
function DragTo([int]$x, [int]$y, [int]$from = -1, [int]$step = 6) {
  if ($from -ge 0) { for ($v = $from; [math]::Abs($x - $v) -gt $step; $v += [math]::Sign($x - $v) * $step) { [void][W]::PostMessageW($script:hwnd, 0x0200, [IntPtr]1, (lp $v $y)); Start-Sleep -Milliseconds 8 } }
  [void][W]::PostMessageW($script:hwnd, 0x0200, [IntPtr]1, (lp $x $y)); Start-Sleep -Milliseconds 700
}
function Up([int]$x, [int]$y) { [void][W]::PostMessageW($script:hwnd, 0x0202, [IntPtr]0, (lp $x $y)); Start-Sleep -Milliseconds 800 }
