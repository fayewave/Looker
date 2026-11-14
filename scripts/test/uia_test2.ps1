# Menus, dialogs, switches and focus through UI Automation, on a hidden dev window.
. "$PSScriptRoot\ops.ps1"
Add-Type -AssemblyName UIAutomationClient, UIAutomationTypes
$settings = Join-Path $env:LOCALAPPDATA 'Looker\native\settings.txt'
$backup = "$settings.a11ytest"
if (Test-Path $settings) { Copy-Item $settings $backup -Force }
$A = [System.Windows.Automation.AutomationElement]
function Kids { $root = $A::FromHandle($script:hwnd); $w = [System.Windows.Automation.TreeWalker]::RawViewWalker; $c = $w.GetFirstChild($root); while ($c) { $c; $c = $w.GetNextSibling($c) } }
function Line($c) {
  $cur = $c.Current; $state = ''
  try { $t = $c.GetCurrentPattern([System.Windows.Automation.TogglePattern]::Pattern); $state += " toggle=" + $t.Current.ToggleState } catch {}
  try { $s = $c.GetCurrentPattern([System.Windows.Automation.SelectionItemPattern]::Pattern); if ($s.Current.IsSelected) { $state += " selected" } } catch {}
  try { $v = $c.GetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern); $state += " value='" + $v.Current.Value + "'" } catch {}
  $f = if ($cur.HasKeyboardFocus) { ' [FOCUS]' } else { '' }
  "{0,-12} {1,-50} id={2}{3}{4} acc={5}" -f ($cur.ControlType.ProgrammaticName -replace 'ControlType.', ''), $cur.Name, $cur.AutomationId, $state, $f, $cur.AcceleratorKey
}
function Dump([string]$title, [string]$like = '*') { "=== $title"; Kids | Where-Object { $_.Current.AutomationId -like $like } | ForEach-Object { Line $_ } }
function Find([string]$auto) { Kids | Where-Object { $_.Current.AutomationId -eq $auto } | Select-Object -First 1 }
function Invoke([string]$auto) { $e = Find $auto; if (-not $e) { "!! no $auto"; return }; $e.GetCurrentPattern([System.Windows.Automation.InvokePattern]::Pattern).Invoke(); Start-Sleep -Milliseconds 900 }
# Focus changes as seen by a client.
$script:events = [System.Collections.ArrayList]::new()
try {
  Start-Looker @("`"$PSScriptRoot\photos\green.jpg`"")
  [void]$A::FromHandle($script:hwnd); Start-Sleep -Milliseconds 500; Key 0x25; Start-Sleep -Milliseconds 500
  Dump 'viewport' 'Viewport'
  Dump 'tabs' 'Tab*'
  Dump 'zoom' 'Tool(Zoom)'
  Invoke 'Tool(More)'
  Dump 'More menu' 'MenuItem*'
  Key 0x1B
  Invoke 'Tool(Delete)'
  Dump 'delete dialog' 'Dialog*'
  "nodes under the dialog: " + @(Kids | Where-Object { $_.Current.AutomationId -like 'Tool*' }).Count
  Invoke 'DialogButton(1)'
  Dump 'after Cancel' 'Dialog*'
  "photo still there: " + (Test-Path "$PSScriptRoot\photos\red.jpg")
  # The explorer: click a row, then Down moves the focus.
  $row = Find 'ExplorerRow(0)'
  if ($row) {
    $r = $row.Current.BoundingRectangle; $wr = New-Object W+RECT; [void][W]::GetWindowRect($script:hwnd, [ref]$wr)
    $cl = New-Object W+RECT
    $row.GetCurrentPattern([System.Windows.Automation.SelectionItemPattern]::Pattern).Select(); Start-Sleep -Milliseconds 900
    Dump 'row clicked' 'ExplorerRow*'
    Key 0x28; Start-Sleep -Milliseconds 400
    Dump 'after Down' 'ExplorerRow*'
    Dump 'viewport now' 'Viewport'
  }
  Invoke 'Tool(Settings)'
  Dump 'settings, before' 'Toggle(Recents)'
  $t = Find 'Toggle(Recents)'; $t.GetCurrentPattern([System.Windows.Automation.TogglePattern]::Pattern).Toggle(); Start-Sleep -Milliseconds 900
  Dump 'settings, toggled' 'Toggle(Recents)'
  $t = Find 'Toggle(Recents)'; $t.GetCurrentPattern([System.Windows.Automation.TogglePattern]::Pattern).Toggle(); Start-Sleep -Milliseconds 900
  Dump 'settings, back' 'Toggle(Recents)'
  Dump 'combos' 'Combo*'
  "everything under Settings: " + @(Kids | Where-Object { $_.Current.AutomationId -like 'Tool*' -or $_.Current.AutomationId -like 'StripCell*' }).Count
  Dump 'nav' 'PageNav*'
} finally {
  Stop-Looker
  if (Test-Path $backup) { Move-Item $backup $settings -Force }
}
