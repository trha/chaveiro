# Drives the vault window's title bar of examples/vault.rs with real mouse
# clicks, because UI Automation's Invoke skips the hit testing that decides
# between a click and a window drag: the account button, the search box and
# Lock take clicks, the empty middle drags the window, and the search box
# stays put when the Offline tag shows. Run it with -Sync synced and with
# -Sync offline.
param([string]$Sync = "synced", [string]$Shot = "", [string]$Exe = "target\debug\examples\vault.exe")
Add-Type -AssemblyName UIAutomationClient, UIAutomationTypes, System.Drawing
Add-Type -Name M -Namespace W2 -MemberDefinition '[DllImport("user32.dll")] public static extern bool SetCursorPos(int x,int y); [DllImport("user32.dll")] public static extern void mouse_event(int f,int x,int y,int d,int e);'
$a = [System.Windows.Automation.AutomationElement]; $scope = [System.Windows.Automation.TreeScope]
$env:VAULT_SYNC = $Sync
$log = Join-Path $env:TEMP "chaveiro-bar.txt"
$process = Start-Process $Exe -PassThru -RedirectStandardError $log; Start-Sleep 3
try {
$condition = New-Object System.Windows.Automation.PropertyCondition($a::ProcessIdProperty, $process.Id)
$window = $a::RootElement.FindFirst($scope::Children, $condition)
function Named($name) { $window.FindFirst($scope::Descendants, (New-Object System.Windows.Automation.PropertyCondition($a::NameProperty, $name))) }
function Click($x, $y) { [W2.M]::SetCursorPos([int]$x, [int]$y) | Out-Null; [W2.M]::mouse_event(2,0,0,0,0); [W2.M]::mouse_event(4,0,0,0,0); Start-Sleep -Milliseconds 600 }
function Center($e) { $r = $e.Current.BoundingRectangle; ($r.X + $r.Width / 2), ($r.Y + $r.Height / 2) }
$r = $window.Current.BoundingRectangle
if ($Shot) { $b = New-Object System.Drawing.Bitmap ([int]$r.Width), 60; [System.Drawing.Graphics]::FromImage($b).CopyFromScreen([int]$r.X, [int]$r.Y, 0, 0, $b.Size); $b.Save($Shot) }
$search = Named "Search all items (Ctrl+F)"; $sr = $search.Current.BoundingRectangle
"search box: at x $([int]($sr.X - $r.X)) (expect the same with -Sync synced and offline)"
# A real click on the account button opens the flyout.
$x, $y = Center (Named "Account"); Click $x $y
$open = @($a::RootElement.FindAll($scope::Children, $condition) | % { $_.FindAll($scope::Descendants, [System.Windows.Automation.Condition]::TrueCondition) } | % { $_ } | ? { $_.Current.Name -match 'Synced|Offline ·' }).Count -gt 0
"click Account: flyout open $open (expect True)"
Click ($r.X + 400) ($r.Y + 500)
# A real click in the search box focuses it.
$x, $y = Center $search; Click $x $y
"click search: focused $($search.Current.HasKeyboardFocus) (expect True)"
# A drag on the empty middle moves the window.
$before = $window.Current.BoundingRectangle
[W2.M]::SetCursorPos([int]($r.X + 220), [int]($r.Y + 20)) | Out-Null; [W2.M]::mouse_event(2,0,0,0,0)
for ($i = 1; $i -le 10; $i++) { [W2.M]::SetCursorPos([int]($r.X + 220 + $i * 6), [int]($r.Y + 20 + $i * 4)) | Out-Null; Start-Sleep -Milliseconds 30 }
[W2.M]::mouse_event(4,0,0,0,0); Start-Sleep -Milliseconds 500
$after = $window.Current.BoundingRectangle
"drag empty bar: moved by $([int]($after.X - $before.X)), $([int]($after.Y - $before.Y)) (expect 60, 40)"
# A real click on Lock locks.
$x, $y = Center (Named "Lock (Ctrl+L)"); Click $x $y
"click Lock: exited $($process.WaitForExit(3000)) (expect True)"
} finally { if (-not $process.HasExited) { Stop-Process $process -Force }; $env:VAULT_SYNC = $null }
"stderr: $((Get-Content $log) -join ', ')"
