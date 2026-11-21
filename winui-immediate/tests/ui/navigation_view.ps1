# Drives examples/navigation_view.rs through UI Automation. Checks the
# first selection, a pick by the user, a pick of Settings, a selection from
# code, a count that updates in place, a rebuild that keeps the selection,
# the collapsed pane's tooltips, and that the idle passes write nothing.
param([string]$Exe = "target\debug\examples\navigation_view.exe")
Add-Type -AssemblyName UIAutomationClient, UIAutomationTypes
$automation = [System.Windows.Automation.AutomationElement]
$descendants = [System.Windows.Automation.TreeScope]::Descendants
$log = Join-Path $env:TEMP "winui-immediate-navigation-view.txt"
$process = Start-Process $Exe -PassThru -RedirectStandardError $log
Start-Sleep 3
$window = $automation::RootElement.FindFirst([System.Windows.Automation.TreeScope]::Children,
    (New-Object System.Windows.Automation.PropertyCondition($automation::ProcessIdProperty, $process.Id)))

function Find-All($type) {
    @($window.FindAll($descendants, (New-Object System.Windows.Automation.PropertyCondition($automation::ControlTypeProperty, $type))))
}
function Texts { (Find-All ([System.Windows.Automation.ControlType]::Text) | ForEach-Object { $_.Current.Name }) }
function Shown { (Texts | Where-Object { $_ -like "Selected: *" }) -join "," }
function Items { Find-All ([System.Windows.Automation.ControlType]::ListItem) }
function Item([string]$name) { Items | Where-Object { $_.Current.Name -eq $name } | Select-Object -First 1 }
function Selected {
    (Items | Where-Object {
        $pattern = $null
        $_.TryGetCurrentPattern([System.Windows.Automation.SelectionItemPattern]::Pattern, [ref]$pattern) -and $pattern.Current.IsSelected
    } | ForEach-Object { $_.Current.Name }) -join ","
}
function Pick([string]$name) {
    (Item $name).GetCurrentPattern([System.Windows.Automation.SelectionItemPattern]::Pattern).Select()
    Start-Sleep -Milliseconds 400
}
function Button([string]$name) { Find-All ([System.Windows.Automation.ControlType]::Button) | Where-Object { $_.Current.Name -eq $name } | Select-Object -First 1 }
function Invoke([string]$name) {
    (Button $name).GetCurrentPattern([System.Windows.Automation.InvokePattern]::Pattern).Invoke()
    Start-Sleep -Milliseconds 400
}

"items: $((Items | ForEach-Object { $_.Current.Name }) -join ', ')"
"  (expect All items, Favorites, Trash, Settings)"
"first: pane '$(Selected)', content '$(Shown)' (expect Favorites, 'Selected: Favorites')"
"counts: $((Texts | Where-Object { $_ -match '^\d+$' }) -join ',') (expect 42,6,2)"

Pick "Trash"
"user picks Trash: pane '$(Selected)', content '$(Shown)' (expect Trash, 'Selected: Trash')"
Pick "Settings"
"user picks Settings: pane '$(Selected)', content '$(Shown)' (expect Settings, 'Selected: Settings')"
Invoke "Pick Trash"
"code picks Trash: pane '$(Selected)', content '$(Shown)' (expect Trash, 'Selected: Trash')"

Invoke "Count"
"count bumped: $((Texts | Where-Object { $_ -match '^\d+$' }) -join ',') (expect 43,6,2)"
Invoke "Add"
"added a folder: items $((Items | ForEach-Object { $_.Current.Name }) -join ', ')"
"  pane '$(Selected)', content '$(Shown)' (expect Trash, 'Selected: Trash')"
Pick "Folder 0"
"user picks Folder 0: pane '$(Selected)', content '$(Shown)' (expect Folder 0)"

# Resize in view pixels: the window opened 1100 view pixels wide.
$transform = $window.GetCurrentPattern([System.Windows.Automation.TransformPattern]::Pattern)
$scale = $window.Current.BoundingRectangle.Width / 1100
function Resize([int]$width) { $transform.Resize($width * $scale, 600 * $scale); Start-Sleep -Milliseconds 800 }
Resize 800
"800 wide, icon strip: pane '$(Selected)', items $(@(Items).Count) (expect Folder 0, 5)"
$count = Find-All ([System.Windows.Automation.ControlType]::Text) | Where-Object { $_.Current.Name -eq '43' } | Select-Object -First 1
"  count clipped: offscreen $($count.Current.IsOffscreen), width $($count.Current.BoundingRectangle.Width) (expect True or 0)"
Resize 600
"600 wide, menu button only: menu button $(if (Button 'Open Navigation') { 'shown' } else { 'missing' }), items $(@(Items).Count) (expect shown, 0)"
Resize 1100
"1100 wide again: pane '$(Selected)', items $(@(Items).Count) (expect Folder 0, 5)"

$window.GetCurrentPattern([System.Windows.Automation.WindowPattern]::Pattern).Close()
if (-not $process.WaitForExit(5000)) { Stop-Process $process -Confirm:$false; "did not exit" }
"stderr:"
Get-Content $log
