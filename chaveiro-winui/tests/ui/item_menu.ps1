# Drives the item menu of examples/vault.rs with real mouse clicks, because
# a right-click has no UI Automation pattern: a right-click on a row opens
# the menu and keeps the details of the selected item, the folder submenu
# checks the current folder, and the entries reach the app. More in the
# details pane opens the same menu. The copy entries and their Ctrl keys
# reach the app, and a re-prompt item offers no password copy.
param([string]$Exe = "target\debug\examples\vault.exe", [string]$Shot = "")
Add-Type -AssemblyName UIAutomationClient, UIAutomationTypes, System.Drawing
Add-Type -Name M -Namespace W -MemberDefinition '[DllImport("user32.dll")] public static extern bool SetCursorPos(int x,int y); [DllImport("user32.dll")] public static extern void mouse_event(int f,int x,int y,int d,int e); [DllImport("user32.dll")] public static extern void keybd_event(byte k,byte s,int f,int e);'
$a = [System.Windows.Automation.AutomationElement]
$scope = [System.Windows.Automation.TreeScope]
$log = Join-Path $env:TEMP "chaveiro-menu.txt"
$process = Start-Process $Exe -PassThru -RedirectStandardError $log
Start-Sleep 3
$window = $a::RootElement.FindFirst($scope::Children, (New-Object System.Windows.Automation.PropertyCondition($a::ProcessIdProperty, $process.Id)))
function Named($root, [string]$name) { $root.FindFirst($scope::Descendants, (New-Object System.Windows.Automation.PropertyCondition($a::NameProperty, $name))) }
function Click-At($element, [int]$button) {
    $r = $element.Current.BoundingRectangle
    [W.M]::SetCursorPos([int]($r.X + 40), [int]($r.Y + $r.Height / 2)) | Out-Null
    $down, $up = if ($button -eq 2) { 8, 16 } else { 2, 4 }
    [W.M]::mouse_event($down, 0, 0, 0, 0); [W.M]::mouse_event($up, 0, 0, 0, 0)
    Start-Sleep -Milliseconds 700
}
# Menus are popups of the process, outside the window's subtree.
function Menu-Items {
    $condition = New-Object System.Windows.Automation.PropertyCondition($a::ProcessIdProperty, $process.Id)
    @($a::RootElement.FindAll($scope::Children, $condition) | ForEach-Object {
        $_.FindAll($scope::Descendants, (New-Object System.Windows.Automation.PropertyCondition($a::ControlTypeProperty, [System.Windows.Automation.ControlType]::MenuItem)))
    } | ForEach-Object { $_ })
}
function Menu-Names { (Menu-Items | ForEach-Object {
    $state = try { $_.GetCurrentPattern([System.Windows.Automation.TogglePattern]::Pattern).Current.ToggleState } catch { $null }
    if ($state -eq 'On') { "[x] $($_.Current.Name)" } else { $_.Current.Name }
}) -join ", " }
function Send-Ctrl([char]$letter) {
    [W.M]::keybd_event(0x11, 0, 0, 0); [W.M]::keybd_event([byte]$letter, 0, 0, 0)
    [W.M]::keybd_event([byte]$letter, 0, 2, 0); [W.M]::keybd_event(0x11, 0, 2, 0)
    Start-Sleep -Milliseconds 700
}
function Menu-Item([string]$name) { Menu-Items | Where-Object { $_.Current.Name -eq $name } | Select-Object -First 1 }

(Named $window "GitHub").GetCurrentPattern([System.Windows.Automation.InvokePattern]::Pattern).Invoke(); Start-Sleep -Milliseconds 700
"Ctrl+U, Ctrl+P and Ctrl+T on GitHub, the selected item, then its Copy code button"; Send-Ctrl 'U'; Send-Ctrl 'P'; Send-Ctrl 'T'
(Named $window "Copy code").GetCurrentPattern([System.Windows.Automation.InvokePattern]::Pattern).Invoke(); Start-Sleep -Milliseconds 700
(Named $window "Amazon").GetCurrentPattern([System.Windows.Automation.InvokePattern]::Pattern).Invoke(); Start-Sleep -Milliseconds 700
"Amazon open: footer $([bool](Named $window 'Folder: Personal'))"
Click-At (Named $window "GitHub") 2
"right-click GitHub: menu '$(Menu-Names)'"
"  details still Amazon: $([bool](Named $window 'Folder: Personal'))"
if ($Shot) {
    $r = $window.Current.BoundingRectangle
    $bitmap = New-Object System.Drawing.Bitmap ([int]$r.Width), ([int]$r.Height)
    [System.Drawing.Graphics]::FromImage($bitmap).CopyFromScreen([int]$r.X, [int]$r.Y, 0, 0, $bitmap.Size)
    $bitmap.Save($Shot)
}
Click-At (Menu-Item "Copy username") 1
Click-At (Named $window "GitHub") 2
Click-At (Menu-Item "Move to folder") 1
"folder submenu: '$(Menu-Names)'"
Click-At (Menu-Item "Personal") 1
"after Personal: menu '$(Menu-Names)' (expect none)"
Click-At (Named $window "GitHub") 2
Click-At (Menu-Item "Ask for master password") 1
Click-At (Named $window "GitHub") 2
"right-click GitHub again: menu '$(Menu-Names)' (expect [x] Ask for master password)"
Click-At (Menu-Item "Open website") 1
"Ctrl+P on GitHub with re-prompt (expect no copy)"
(Named $window "GitHub").GetCurrentPattern([System.Windows.Automation.InvokePattern]::Pattern).Invoke(); Start-Sleep -Milliseconds 700
Send-Ctrl 'P'
(Named $window "Amazon").GetCurrentPattern([System.Windows.Automation.InvokePattern]::Pattern).Invoke(); Start-Sleep -Milliseconds 700
Click-At (Named $window "More") 1
"More on Amazon: menu '$(Menu-Names)'"
Click-At (Menu-Item "Move to trash") 1
"stderr:"; $window.GetCurrentPattern([System.Windows.Automation.WindowPattern]::Pattern).Close()
if (-not $process.WaitForExit(5000)) { Stop-Process $process -Confirm:$false; "did not exit" }
Get-Content $log
