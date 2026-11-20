# Drives examples/widgets through UI Automation: caret and IME survive passes
# every 10 ms, clicks and the password round trip work, and the native
# ToggleSwitch is in the tree.
param([string]$Exe = "target\debug\examples\widgets.exe")
Add-Type -AssemblyName UIAutomationClient, UIAutomationTypes, System.Windows.Forms
$automation = [System.Windows.Automation.AutomationElement]
$descendants = [System.Windows.Automation.TreeScope]::Descendants

function Start-Widgets([hashtable]$environment) {
    foreach ($name in "WIDGETS_TICK", "WIDGETS_IME") { Remove-Item "Env:$name" -ErrorAction SilentlyContinue }
    foreach ($entry in $environment.GetEnumerator()) { Set-Item "Env:$($entry.Key)" $entry.Value }
    $script:log = Join-Path $env:TEMP "winui-immediate-widgets.txt"
    $script:process = Start-Process $Exe -PassThru -RedirectStandardError $script:log
    Start-Sleep 3
    $script:window = $automation::RootElement.FindFirst([System.Windows.Automation.TreeScope]::Children,
        (New-Object System.Windows.Automation.PropertyCondition($automation::ProcessIdProperty, $script:process.Id)))
}
function Stop-Widgets {
    $script:window.GetCurrentPattern([System.Windows.Automation.WindowPattern]::Pattern).Close()
    $exited = $script:process.WaitForExit(5000)
    if (-not $exited) { Stop-Process $script:process -Confirm:$false }
    "  exited=$exited code=$($script:process.ExitCode)"
    $errors = Get-Content $script:log | Where-Object { $_ -match "panicked|error" }
    if ($errors) { "  STDERR: $errors" }
}
function Find-All($type) {
    @($script:window.FindAll($descendants, (New-Object System.Windows.Automation.PropertyCondition($automation::ControlTypeProperty, $type))))
}
function Texts { (Find-All ([System.Windows.Automation.ControlType]::Text) | ForEach-Object { $_.Current.Name }) -join " | " }
function Get-FilterBox { Find-All ([System.Windows.Automation.ControlType]::Edit) | Where-Object { -not $_.Current.IsPassword } | Select-Object -First 1 }
function Type-Keys([string]$keys) {
    foreach ($character in $keys.ToCharArray()) {
        [System.Windows.Forms.SendKeys]::SendWait([string]$character)
        Start-Sleep -Milliseconds 60
    }
}
function Value($element) { $element.GetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern).Current.Value }
function Button([string]$name) { Find-All ([System.Windows.Automation.ControlType]::Button) | Where-Object { $_.Current.Name -eq $name } | Select-Object -First 1 }
function Invoke($element) { $element.GetCurrentPattern([System.Windows.Automation.InvokePattern]::Pattern).Invoke(); Start-Sleep -Milliseconds 400 }

"== caret, with a pass every 10 ms"
Start-Widgets @{ WIDGETS_TICK = "10" }
$filter = Get-FilterBox
$filter.SetFocus(); Start-Sleep -Milliseconds 300
Type-Keys "entry 12"
[System.Windows.Forms.SendKeys]::SendWait("{LEFT}"); Start-Sleep -Milliseconds 100
Type-Keys "3"
Start-Sleep -Milliseconds 500
"  filter: '$(Value $filter)' (expect 'entry 132')"
"  labels: $(Texts)"
Stop-Widgets

"== clicks, password, native and cache, with a pass every 10 ms"
Start-Widgets @{ WIDGETS_TICK = "10" }
Invoke (Button "Reveal")
Start-Sleep -Milliseconds 600
"  cached row built $(@(Get-Content $script:log | Where-Object { $_ -eq 'secret row built' }).Count) times (expect 3: first pass, the click, the changed deps)"
"  after Reveal: $(Texts)"
$password = Find-All ([System.Windows.Automation.ControlType]::Edit) | Where-Object { $_.Current.IsPassword } | Select-Object -First 1
$password.SetFocus(); Start-Sleep -Milliseconds 300
Type-Keys "hunter2"
Invoke (Button "Unlock")
"  after Unlock: $(Texts); first button now '$((Find-All ([System.Windows.Automation.ControlType]::Button) | Select-Object -First 1).Current.Name)'"
"  native toggle switches: $(@($script:window.FindAll($descendants, (New-Object System.Windows.Automation.PropertyCondition($automation::ClassNameProperty, "ToggleSwitch")))).Count)"
Stop-Widgets

"== IME (Vietnamese Telex), with a pass every 10 ms"
Start-Widgets @{ WIDGETS_TICK = "10"; WIDGETS_IME = "vi" }
$filter = Get-FilterBox
$filter.SetFocus(); Start-Sleep -Milliseconds 500
Type-Keys "tieesng vieetj "
Start-Sleep -Milliseconds 600
"  filter: '$(Value $filter)' (expect 'tiếng việt ')"
"  labels: $(Texts)"
Stop-Widgets
