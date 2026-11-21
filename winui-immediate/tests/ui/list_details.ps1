# Drives examples/list_details.rs through UI Automation: a row click selects
# its entry, Show reveals the secret and another row hides it again, the
# command bar's button reaches the app, and the list scrolls.
param([string]$Exe = "target\debug\examples\list_details.exe", [string]$Shot = "")
Add-Type -AssemblyName UIAutomationClient, UIAutomationTypes
$automation = [System.Windows.Automation.AutomationElement]
$descendants = [System.Windows.Automation.TreeScope]::Descendants
$log = Join-Path $env:TEMP "winui-immediate-list-details.txt"
$process = Start-Process $Exe -PassThru -RedirectStandardError $log
Start-Sleep 3
$window = $automation::RootElement.FindFirst([System.Windows.Automation.TreeScope]::Children,
    (New-Object System.Windows.Automation.PropertyCondition($automation::ProcessIdProperty, $process.Id)))

function Find-All($type) {
    @($window.FindAll($descendants, (New-Object System.Windows.Automation.PropertyCondition($automation::ControlTypeProperty, $type))))
}
function Button([string]$name) { Find-All ([System.Windows.Automation.ControlType]::Button) | Where-Object { $_.Current.Name -eq $name } | Select-Object -First 1 }
function Invoke($element) { $element.GetCurrentPattern([System.Windows.Automation.InvokePattern]::Pattern).Invoke(); Start-Sleep -Milliseconds 400 }
function Texts { (Find-All ([System.Windows.Automation.ControlType]::Text) | ForEach-Object { $_.Current.Name } | Where-Object { $_ -match 'Select an|secret-|••|^Entry \d+$' }) -join " | " }

"open: $(Texts)"
"  (expect the visible row names and 'Select an entry.')"
Invoke (Button "Entry 3")
"click Entry 3: $(Texts)"
"  (expect a title 'Entry 3' and dots)"
Invoke (Button "Show secret")
"show: $(Texts)"
"  (expect secret-3)"
Invoke (Button "Entry 5")
"click Entry 5: $(Texts)"
"  (expect dots again, no secret-5)"
$favorite = Find-All ([System.Windows.Automation.ControlType]::Button) | Where-Object { $_.Current.Name -eq "Favorite" } | Select-Object -First 1
Invoke $favorite
"favorite button now: '$((Find-All ([System.Windows.Automation.ControlType]::Button) | Where-Object { $_.Current.Name -match 'favorite' }).Current.Name)' (expect Unfavorite)"
$scroll = Find-All ([System.Windows.Automation.ControlType]::Pane) | Where-Object { $_.GetSupportedPatterns() -contains [System.Windows.Automation.ScrollPattern]::Pattern } | Select-Object -First 1
$scroll.GetCurrentPattern([System.Windows.Automation.ScrollPattern]::Pattern).SetScrollPercent(-1, 100)
Start-Sleep -Milliseconds 600
$last = Button "Entry 200"
"scrolled to the end: Entry 200 is $(if ($last) { 'there' } else { 'missing' })"
if ($last) { Invoke $last; "click Entry 200: $(Texts)" }
if ($Shot) {
    Add-Type -AssemblyName System.Drawing
    $rect = $window.Current.BoundingRectangle
    $bitmap = New-Object System.Drawing.Bitmap ([int]$rect.Width), ([int]$rect.Height)
    $graphics = [System.Drawing.Graphics]::FromImage($bitmap)
    $graphics.CopyFromScreen([int]$rect.X, [int]$rect.Y, 0, 0, $bitmap.Size)
    $bitmap.Save($Shot)
}
$window.GetCurrentPattern([System.Windows.Automation.WindowPattern]::Pattern).Close()
if (-not $process.WaitForExit(5000)) { Stop-Process $process -Confirm:$false; "did not exit" }
"stderr (expect favorite 5):"
Get-Content $log
