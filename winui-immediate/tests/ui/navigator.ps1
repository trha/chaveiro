# Drives examples/navigator.rs through UI Automation. Checks that a slide
# shows both screens for a while and then only the new one, that Lock never
# shows both, that a screen's state goes with it, and that a quick Next then
# Back ends on one screen.
param([string]$Exe = "target\debug\examples\navigator.exe")
Add-Type -AssemblyName UIAutomationClient, UIAutomationTypes, System.Windows.Forms
$automation = [System.Windows.Automation.AutomationElement]
$descendants = [System.Windows.Automation.TreeScope]::Descendants
$log = Join-Path $env:TEMP "winui-immediate-navigator.txt"
$process = Start-Process $Exe -PassThru -RedirectStandardError $log
# Open the handle now, or ExitCode reads null once the process exits.
$null = $process.Handle
Start-Sleep 3
$window = $automation::RootElement.FindFirst([System.Windows.Automation.TreeScope]::Children,
    (New-Object System.Windows.Automation.PropertyCondition($automation::ProcessIdProperty, $process.Id)))

function Find-All($type) {
    @($window.FindAll($descendants, (New-Object System.Windows.Automation.PropertyCondition($automation::ControlTypeProperty, $type))))
}
# The titles of the screens in the tree, such as "First screen".
function Screens {
    (Find-All ([System.Windows.Automation.ControlType]::Text) | ForEach-Object { $_.Current.Name } |
        Where-Object { $_ -like "* screen" } | Sort-Object) -join " + "
}
function Button([string]$name) { Find-All ([System.Windows.Automation.ControlType]::Button) | Where-Object { $_.Current.Name -eq $name } | Select-Object -First 1 }
function Note { Find-All ([System.Windows.Automation.ControlType]::Edit) | Select-Object -First 1 }
function Value($element) { $element.GetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern).Current.Value }
# Invokes the button, then samples the screens every 20 ms for 700 ms.
# Returns each distinct sample in order.
function Watch([string]$name) {
    (Button $name).GetCurrentPattern([System.Windows.Automation.InvokePattern]::Pattern).Invoke()
    $seen = New-Object System.Collections.Generic.List[string]
    $clock = [Diagnostics.Stopwatch]::StartNew()
    while ($clock.ElapsedMilliseconds -lt 700) {
        $sample = Screens
        if ($seen.Count -eq 0 -or $seen[$seen.Count - 1] -ne $sample) { $seen.Add($sample) }
        Start-Sleep -Milliseconds 20
    }
    $seen -join " -> "
}

function Height { [int]$window.Current.BoundingRectangle.Height }
"start: $(Screens) (expect First screen)"
$short = Height
$note = Note
$note.SetFocus(); Start-Sleep -Milliseconds 200
[System.Windows.Forms.SendKeys]::SendWait("hello"); Start-Sleep -Milliseconds 300
"note before: '$(Value $note)' (expect 'hello')"

"Next: $(Watch 'Next') (expect First screen -> First screen + Second screen -> Second screen)"
"window height after Next: $(Height), start $short (expect taller)"
"Back: $(Watch 'Back') (expect Second screen -> First screen + Second screen -> First screen)"
"window height after Back: $(Height) (expect $short)"
"note after: '$(Value (Note))' (expect '', the state went with the screen)"
[void](Watch 'Next')
"Lock: $(Watch 'Lock') (expect Second screen -> First screen, never both)"

(Button 'Next').GetCurrentPattern([System.Windows.Automation.InvokePattern]::Pattern).Invoke()
Start-Sleep -Milliseconds 80
(Button 'Back').GetCurrentPattern([System.Windows.Automation.InvokePattern]::Pattern).Invoke()
Start-Sleep -Milliseconds 800
"quick Next then Back: $(Screens) (expect First screen)"

# The title bar's back button sits in the drag area, so only a real mouse
# click shows that it takes input; Invoke would skip hit testing.
Add-Type @"
using System; using System.Runtime.InteropServices;
public static class Mouse {
  [DllImport("user32.dll")] public static extern bool SetCursorPos(int x, int y);
  [DllImport("user32.dll")] public static extern void mouse_event(uint flags, int x, int y, uint data, UIntPtr extra);
  [DllImport("user32.dll")] public static extern bool SetProcessDPIAware();
}
"@
[void][Mouse]::SetProcessDPIAware()
[void](Watch 'Next')
"buttons: $((Find-All ([System.Windows.Automation.ControlType]::Button) | ForEach-Object { "$($_.Current.Name)/$($_.Current.AutomationId)" }) -join ', ')"
$titleBack = Find-All ([System.Windows.Automation.ControlType]::Button) | Where-Object { $_.Current.Name -eq 'Back' } |
    Sort-Object { $_.Current.BoundingRectangle.Top } | Select-Object -First 1
"title bar back: '$($titleBack.Current.Name)' visible=$(-not $titleBack.Current.IsOffscreen)"
$box = $titleBack.Current.BoundingRectangle
[void][Mouse]::SetCursorPos([int]($box.X + $box.Width / 2), [int]($box.Y + $box.Height / 2))
Start-Sleep -Milliseconds 100
[Mouse]::mouse_event(2, 0, 0, 0, [UIntPtr]::Zero); [Mouse]::mouse_event(4, 0, 0, 0, [UIntPtr]::Zero)
Start-Sleep -Milliseconds 800
"after clicking the title bar back: $(Screens) (expect First screen)"
"buttons named Back on the first screen: $(@(Find-All ([System.Windows.Automation.ControlType]::Button) | Where-Object { $_.Current.Name -eq 'Back' -and -not $_.Current.IsOffscreen }).Count) (expect 0)"

$window.GetCurrentPattern([System.Windows.Automation.WindowPattern]::Pattern).Close()
$exited = $process.WaitForExit(5000)
if (-not $exited) { Stop-Process $process -Confirm:$false }
"exited=$exited code=$($process.ExitCode)"
"--- stderr"
Get-Content $log
