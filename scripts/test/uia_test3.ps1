. "$PSScriptRoot\ops.ps1"
Add-Type -AssemblyName UIAutomationClient, UIAutomationTypes
$A = [System.Windows.Automation.AutomationElement]
function Kids { $root = $A::FromHandle($script:hwnd); $w = [System.Windows.Automation.TreeWalker]::RawViewWalker; $c = $w.GetFirstChild($root); while ($c) { $c; $c = $w.GetNextSibling($c) } }
function Find([string]$auto) { Kids | Where-Object { $_.Current.AutomationId -eq $auto } | Select-Object -First 1 }
function Sel([string]$auto) { (Find $auto).GetCurrentPattern([System.Windows.Automation.SelectionItemPattern]::Pattern).Select(); Start-Sleep -Milliseconds 900 }
function Show([string]$like) { Kids | Where-Object { $_.Current.AutomationId -like $like } | ForEach-Object { $v = ''; try { $v = " value='" + $_.GetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern).Current.Value + "'" } catch {}; "{0,-10} {1,-45} {2}{3}" -f ($_.Current.ControlType.ProgrammaticName -replace 'ControlType.', ''), $_.Current.Name, $_.Current.AutomationId, $v } }
Set-Content "$PSScriptRoot\photos\notes.txt" "First line of the notes.`r`nSecond line."
try {
  Start-Looker @("`"$PSScriptRoot\photos\green.jpg`"")
  [void]$A::FromHandle($script:hwnd); Start-Sleep -Milliseconds 500; Key 0x25; Start-Sleep -Milliseconds 500
  (Find 'Tool(Settings)').GetCurrentPattern([System.Windows.Automation.InvokePattern]::Pattern).Invoke(); Start-Sleep -Milliseconds 900
  foreach ($i in 1..4) { Sel "PageNav($i)"; "--- section $i"; Show 'Combo*'; Show 'Toggle*' | Select-Object -First 3; Show 'ThemeChip*' | Select-Object -First 3 }
  Stop-Looker
  Start-Looker @("`"$PSScriptRoot\photos\notes.txt`"")
  [void]$A::FromHandle($script:hwnd); Start-Sleep -Milliseconds 500; Key 0x23; Start-Sleep -Milliseconds 500
  "--- text"
  Show 'TextEditor*'
  $doc = Kids | Where-Object { $_.Current.ControlType -eq [System.Windows.Automation.ControlType]::Document } | Select-Object -First 1
  if ($doc) { "doc: " + $doc.Current.Name + " focus=" + $doc.Current.HasKeyboardFocus; $tp = $doc.GetCurrentPattern([System.Windows.Automation.TextPattern]::Pattern); "text: " + $tp.DocumentRange.GetText(100) } else { 'no document' }
  Show 'Text*' | Select-Object -First 8
} finally { Stop-Looker; Remove-Item "$PSScriptRoot\photos\notes.txt" -ErrorAction SilentlyContinue }
