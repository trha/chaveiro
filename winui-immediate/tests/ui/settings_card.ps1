# Drives examples/settings_card.rs through UI Automation: clicks by Invoke,
# by a real mouse click and by Space reach the app; cards that are not
# clickable are groups named by their header; content sits beside the
# header in the wide column and under it in the narrow one; Vertical
# content stretches.
param([string]$Exe = "target\debug\examples\settings_card.exe")
Add-Type -AssemblyName UIAutomationClient, UIAutomationTypes, System.Windows.Forms
Add-Type @"
using System; using System.Runtime.InteropServices;
public static class CardMouse {
  [DllImport("user32.dll")] public static extern bool SetCursorPos(int x, int y);
  [DllImport("user32.dll")] public static extern void mouse_event(uint flags, int x, int y, uint data, UIntPtr extra);
  [DllImport("user32.dll")] public static extern bool SetProcessDPIAware();
  [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr window);
}
"@
[void][CardMouse]::SetProcessDPIAware()
$automation = [System.Windows.Automation.AutomationElement]
$descendants = [System.Windows.Automation.TreeScope]::Descendants
$log = Join-Path $env:TEMP "winui-immediate-settings-card.txt"
$process = Start-Process $Exe -PassThru -RedirectStandardError $log
# Open the handle now, or ExitCode reads null once the process exits.
$null = $process.Handle
Start-Sleep 3
$window = $automation::RootElement.FindFirst([System.Windows.Automation.TreeScope]::Children,
    (New-Object System.Windows.Automation.PropertyCondition($automation::ProcessIdProperty, $process.Id)))

function Find-All($type) {
    @($window.FindAll($descendants, (New-Object System.Windows.Automation.PropertyCondition($automation::ControlTypeProperty, $type))))
}
function Card { Find-All ([System.Windows.Automation.ControlType]::Button) | Where-Object { $_.Current.Name -eq "Authenticator app" } | Select-Object -First 1 }
function Clicks { (Card).Current.HelpText }
function Named($type, [string]$name) { Find-All $type | Where-Object { $_.Current.Name -eq $name } }

"start: '$(Clicks)' (expect 'Clicked 0 times')"
(Card).GetCurrentPattern([System.Windows.Automation.InvokePattern]::Pattern).Invoke(); Start-Sleep -Milliseconds 400
"after Invoke: '$(Clicks)' (expect 'Clicked 1 times')"
# Real input goes to the foreground window, so make sure it is this one.
[void][CardMouse]::SetForegroundWindow([IntPtr]$window.Current.NativeWindowHandle); Start-Sleep -Milliseconds 300
$box = (Card).Current.BoundingRectangle
[void][CardMouse]::SetCursorPos([int]($box.X + $box.Width / 2), [int]($box.Y + $box.Height / 2)); Start-Sleep -Milliseconds 100
[CardMouse]::mouse_event(2, 0, 0, 0, [UIntPtr]::Zero); [CardMouse]::mouse_event(4, 0, 0, 0, [UIntPtr]::Zero); Start-Sleep -Milliseconds 400
"after a mouse click: '$(Clicks)' (expect 'Clicked 2 times')"
(Card).SetFocus(); Start-Sleep -Milliseconds 200
[System.Windows.Forms.SendKeys]::SendWait(" "); Start-Sleep -Milliseconds 400
"after Space: '$(Clicks)' (expect 'Clicked 3 times')"

$groups = Find-All ([System.Windows.Automation.ControlType]::Group) | ForEach-Object { $_.Current.Name } | Sort-Object -Unique
"groups: $($groups -join ', ') (expect Hidden header, Right, Vertical)"

$texts = Named ([System.Windows.Automation.ControlType]::Text) "Right" | Sort-Object { $_.Current.BoundingRectangle.X }
$actions = Named ([System.Windows.Automation.ControlType]::Button) "Action" | Sort-Object { $_.Current.BoundingRectangle.X }
$narrow = $actions[0].Current.BoundingRectangle; $wide = $actions[1].Current.BoundingRectangle
$narrowHeader = $texts[0].Current.BoundingRectangle; $wideHeader = $texts[1].Current.BoundingRectangle
"narrow: Action under the header $($narrow.Y -gt $narrowHeader.Bottom) (expect True)"
"wide: Action right of the header $($wide.X -gt $wideHeader.Right) and on its line $($wide.Y -lt $wideHeader.Bottom) (expect True True)"

$edits = Find-All ([System.Windows.Automation.ControlType]::Edit) | Sort-Object { $_.Current.BoundingRectangle.X }
$cards = Named ([System.Windows.Automation.ControlType]::Group) "Vertical" | Sort-Object { $_.Current.BoundingRectangle.X }
for ($i = 0; $i -lt 2; $i++) {
    $edit = $edits[$i].Current.BoundingRectangle; $card = $cards[$i].Current.BoundingRectangle
    "Vertical text box stretches to within 24px of the card's right edge: $($card.Right - $edit.Right -lt 24) (gap $($card.Right - $edit.Right)) (expect True)"
}

$window.GetCurrentPattern([System.Windows.Automation.WindowPattern]::Pattern).Close()
$exited = $process.WaitForExit(5000)
if (-not $exited) { Stop-Process $process -Confirm:$false }
"exited=$exited code=$($process.ExitCode)"
Get-Content $log
