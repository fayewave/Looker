# Dumps Looker's UI Automation tree, then invokes a few nodes, on a hidden dev window.
. "$PSScriptRoot\ops.ps1"
Add-Type -AssemblyName UIAutomationClient, UIAutomationTypes
$settings = Join-Path $env:LOCALAPPDATA 'Looker\native\settings.txt'
$backup = "$settings.a11ytest"
if (Test-Path $settings) { Copy-Item $settings $backup -Force }
function Dump([string]$title, [int]$max = 60) {
  "=== $title"
  $root = [System.Windows.Automation.AutomationElement]::FromHandle($script:hwnd)
  $w = [System.Windows.Automation.TreeWalker]::RawViewWalker
  $c = $w.GetFirstChild($root); $n = 0
  while ($c -and $n -lt $max) {
    $cur = $c.Current
    $pats = ($c.GetSupportedPatterns() | ForEach-Object { $_.ProgrammaticName -replace 'PatternIdentifiers.Pattern', '' }) -join ','
    $state = ''
    try { $t = $c.GetCurrentPattern([System.Windows.Automation.TogglePattern]::Pattern); $state += " toggle=" + $t.Current.ToggleState } catch {}
    try { $s = $c.GetCurrentPattern([System.Windows.Automation.SelectionItemPattern]::Pattern); if ($s.Current.IsSelected) { $state += " selected" } } catch {}
    try { $v = $c.GetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern); $state += " value='" + $v.Current.Value + "'" } catch {}
    $f = if ($cur.HasKeyboardFocus) { ' [FOCUS]' } else { '' }
    "{0,-14} {1,-40} id={2} [{3}]{4}{5}" -f ($cur.ControlType.ProgrammaticName -replace 'ControlType.', ''), $cur.Name, $cur.AutomationId, $pats, $state, $f
    $c = $w.GetNextSibling($c); $n++
  }
}
function Find([string]$auto) {
  $root = [System.Windows.Automation.AutomationElement]::FromHandle($script:hwnd)
  $cond = New-Object System.Windows.Automation.PropertyCondition ([System.Windows.Automation.AutomationElement]::AutomationIdProperty), $auto
  $root.FindFirst([System.Windows.Automation.TreeScope]::Children, $cond)
}
try {
  Start-Looker @("`"$PSScriptRoot\photos\green.jpg`"")
  [void][System.Windows.Automation.AutomationElement]::FromHandle($script:hwnd)  # makes the window build its nodes
  Start-Sleep -Milliseconds 800
  Key 0x27; Start-Sleep -Milliseconds 600   # Right: the next photo, for a frame after the reader came
  Dump 'photo'
  $e = Find 'Tool(Explorer)'
  if ($e) { $e.GetCurrentPattern([System.Windows.Automation.InvokePattern]::Pattern).Invoke(); Start-Sleep -Milliseconds 1200; Dump 'explorer opened' 80 } else { 'no Tool(Explorer)' }
  $e = Find 'Tool(Settings)'
  if ($e) { $e.GetCurrentPattern([System.Windows.Automation.InvokePattern]::Pattern).Invoke(); Start-Sleep -Milliseconds 1200; Dump 'settings' 80 }
  Shot 'a11y-settings' | Out-Null
} finally {
  Stop-Looker
  if (Test-Path $backup) { Move-Item $backup $settings -Force }
}
