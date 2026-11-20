# Drives examples/smoke.rs through UI Automation: two windows, clicks update
# the count, a background message updates the status, and each close request
# goes through the app.
param([string]$Exe = "target\debug\examples\smoke.exe")
Add-Type -AssemblyName UIAutomationClient, UIAutomationTypes
$automation = [System.Windows.Automation.AutomationElement]
$log = Join-Path $env:TEMP "winui-immediate-smoke.txt"
$process = Start-Process $Exe -PassThru -RedirectStandardError $log
Start-Sleep 3

function Windows {
    @($automation::RootElement.FindAll([System.Windows.Automation.TreeScope]::Children,
        (New-Object System.Windows.Automation.PropertyCondition($automation::ProcessIdProperty, $process.Id))))
}
function Texts($window) {
    ($window.FindAll([System.Windows.Automation.TreeScope]::Descendants,
        (New-Object System.Windows.Automation.PropertyCondition($automation::ControlTypeProperty, [System.Windows.Automation.ControlType]::Text))) |
        ForEach-Object { $_.Current.Name }) -join " | "
}

$windows = Windows
"windows at start: " + (($windows | ForEach-Object { $_.Current.Name }) -join ", ")
$main = $windows | Where-Object { $_.Current.Name -eq "winui-immediate smoke" }
$second = $windows | Where-Object { $_.Current.Name -eq "second window" }
"main before clicks: " + (Texts $main)
$button = $main.FindFirst([System.Windows.Automation.TreeScope]::Descendants,
    (New-Object System.Windows.Automation.PropertyCondition($automation::ControlTypeProperty, [System.Windows.Automation.ControlType]::Button)))
foreach ($click in 1..2) {
    $button.GetCurrentPattern([System.Windows.Automation.InvokePattern]::Pattern).Invoke()
    Start-Sleep -Milliseconds 300
}
"main after 2 clicks: " + (Texts $main)

$second.GetCurrentPattern([System.Windows.Automation.WindowPattern]::Pattern).Close()
Start-Sleep -Milliseconds 800
"windows after closing second: " + ((Windows | ForEach-Object { $_.Current.Name }) -join ", ")

$main.GetCurrentPattern([System.Windows.Automation.WindowPattern]::Pattern).Close()
$exited = $process.WaitForExit(5000)
if (-not $exited) { Stop-Process $process -Confirm:$false }
"exited=$exited code=$($process.ExitCode)"
"--- stderr"
Get-Content $log
