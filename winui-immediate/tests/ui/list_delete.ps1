# Drives examples/list_delete.rs through UI Automation: clicks Delete on one
# row once, then reports which entries the app deleted. Exactly one, the
# clicked one, must go, although the new length makes WinUI rebuild rows.
param([string]$Exe = "target\debug\examples\list_delete.exe", [string]$Target = "Delete 3")
Add-Type -AssemblyName UIAutomationClient, UIAutomationTypes
$automation = [System.Windows.Automation.AutomationElement]
$log = Join-Path $env:TEMP "winui-immediate-list-delete.txt"
$process = Start-Process $Exe -PassThru -RedirectStandardError $log
Start-Sleep 3
$window = $automation::RootElement.FindFirst([System.Windows.Automation.TreeScope]::Children,
    (New-Object System.Windows.Automation.PropertyCondition($automation::ProcessIdProperty, $process.Id)))
$button = $window.FindFirst([System.Windows.Automation.TreeScope]::Descendants,
    (New-Object System.Windows.Automation.PropertyCondition($automation::NameProperty, $Target)))
"found '$Target': $($null -ne $button)"
$button.GetCurrentPattern([System.Windows.Automation.InvokePattern]::Pattern).Invoke()
Start-Sleep -Milliseconds 800
$window.GetCurrentPattern([System.Windows.Automation.WindowPattern]::Pattern).Close()
$exited = $process.WaitForExit(5000)
if (-not $exited) { Stop-Process $process -Confirm:$false }
$deleted = @(Get-Content $log | Select-String '^deleted (\d+)' | ForEach-Object { $_.Matches[0].Groups[1].Value })
"deleted: $($deleted -join ', ')"
"exited=$exited"
if ($deleted.Count -ne 1) { exit 1 }
