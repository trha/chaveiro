# Drives examples/second_factor.rs through UI Automation and real keys:
# focus on open, typing moves through the boxes, Backspace and the arrow,
# Home and End keys move between them, a
# paste fills them, a missing digit and a refused code show their errors,
# the right code goes through, and email's resend link and caption work.
param([string]$Exe = "target\debug\examples\second_factor.exe")
Add-Type -AssemblyName UIAutomationClient, UIAutomationTypes, System.Windows.Forms
$automation = [System.Windows.Automation.AutomationElement]
$descendants = [System.Windows.Automation.TreeScope]::Descendants

function Start-Preview([string]$method) {
    $env:PREVIEW_METHOD = $method
    $script:log = Join-Path $env:TEMP "chaveiro-second-factor.txt"
    $script:process = Start-Process $Exe -PassThru -RedirectStandardError $script:log
    Start-Sleep 3
    $script:window = $automation::RootElement.FindFirst([System.Windows.Automation.TreeScope]::Children,
        (New-Object System.Windows.Automation.PropertyCondition($automation::ProcessIdProperty, $script:process.Id)))
}
function Stop-Preview {
    $script:window.GetCurrentPattern([System.Windows.Automation.WindowPattern]::Pattern).Close()
    if (-not $script:process.WaitForExit(5000)) { Stop-Process $script:process -Confirm:$false }
    $errors = Get-Content $script:log | Where-Object { $_ -match "panicked|error" }
    if ($errors) { "  STDERR: $errors" }
}
function Find-All($type) {
    @($script:window.FindAll($descendants, (New-Object System.Windows.Automation.PropertyCondition($automation::ControlTypeProperty, $type))))
}
function Boxes { Find-All ([System.Windows.Automation.ControlType]::Edit) | Where-Object { $_.Current.Name -like "Digit *" } }
function Value($element) { $element.GetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern).Current.Value }
function Code { ((Boxes) | ForEach-Object { $v = Value $_; if ($v) { $v } else { "_" } }) -join "" }
function Focused { [System.Windows.Automation.AutomationElement]::FocusedElement.Current.Name }
function Texts { (Find-All ([System.Windows.Automation.ControlType]::Text) | ForEach-Object { $_.Current.Name }) -join " | " }
function Keys([string]$keys) { foreach ($c in $keys.ToCharArray()) { [System.Windows.Forms.SendKeys]::SendWait([string]$c); Start-Sleep -Milliseconds 80 } }
function Press([string]$key) { [System.Windows.Forms.SendKeys]::SendWait($key); Start-Sleep -Milliseconds 150 }
function Button([string]$name) { Find-All ([System.Windows.Automation.ControlType]::Button) | Where-Object { $_.Current.Name -eq $name } | Select-Object -First 1 }
function Invoke($element) { $element.GetCurrentPattern([System.Windows.Automation.InvokePattern]::Pattern).Invoke(); Start-Sleep -Milliseconds 300 }

"== authenticator"
Start-Preview "app"
"  boxes: $(@(Boxes).Count) (expect 6), focused on open: '$(Focused)' (expect 'Digit 1 of 6')"
Keys "12"
"  after typing 12: code $(Code), focus '$(Focused)' (expect 12____, 'Digit 3 of 6')"
Press "{BACKSPACE}"
"  Backspace in the empty third box: focus '$(Focused)' (expect 'Digit 2 of 6')"
Press "{BACKSPACE}"
"  Backspace in the second box: code $(Code) (expect 1_____)"
Press "{ENTER}"
"  Enter with a box empty: $(Texts) (expect 'Enter all 6 digits'), focus '$(Focused)' (expect 'Digit 2 of 6')"
Keys "2"
"  typing clears the error: $(if ((Texts) -match 'Enter all') { 'no' } else { 'yes' }) (expect yes)"
[System.Windows.Forms.Clipboard]::SetText("1234 56")
(Boxes)[0].SetFocus(); Start-Sleep -Milliseconds 200
Press "^v"
"  paste '1234 56' into the first box: code $(Code) (expect 123456)"
(Boxes)[5].SetFocus(); Start-Sleep -Milliseconds 100
Press "{BACKSPACE}"; Keys "7"
"  code now $(Code) (expect 123457)"
Press "{LEFT}"; "  Left: focus '$(Focused)' (expect 'Digit 5 of 6')"
Press "{LEFT}"; Press "{RIGHT}"; "  Left then Right: focus '$(Focused)' (expect 'Digit 5 of 6')"
Press "{HOME}"; "  Home: focus '$(Focused)' (expect 'Digit 1 of 6')"
Press "{END}"; "  End: focus '$(Focused)' (expect 'Digit 6 of 6')"
Keys "9"; "  typing after End replaces the last digit: code $(Code) (expect 123459)"
Press "{BACKSPACE}"; Keys "7"
Invoke (Button "Continue")
"  while checking: button named '$(@(Find-All ([System.Windows.Automation.ControlType]::Button) | Where-Object { $_.Current.Name -like 'Verif*' }).Count)' times Verifying (expect 1), first box enabled $((Boxes)[0].Current.IsEnabled) (expect False)"
Start-Sleep -Milliseconds 1500
"  refused: code $(Code) (expect ______), error shown $((Texts) -match 'didn')  (expect True), focus '$(Focused)' (expect 'Digit 1 of 6')"
Keys "123456"; Press "{ENTER}"
Start-Sleep -Milliseconds 1500
"  accepted: $(@(Get-Content $script:log | Where-Object { $_ -eq 'accepted' }).Count) (expect 1)"
Stop-Preview

"== email"
Start-Preview "email"
"  caption: $((Texts) -match 'f\*\*\*@example.com') (expect True)"
$link = Find-All ([System.Windows.Automation.ControlType]::Hyperlink) | Select-Object -First 1
"  link: '$($link.Current.Name)' (expect 'Send the code again')"
Invoke $link
"  after click: '$(($link).Current.Name)' enabled $($link.Current.IsEnabled) (expect 'Sending…' False)"
Start-Sleep -Milliseconds 1500
"  after the send: '$($link.Current.Name)' enabled $($link.Current.IsEnabled) (expect 'Send the code again' True)"
Stop-Preview

"== yubikey"
Start-Preview "yubikey"
"  boxes: $(@(Boxes).Count) (expect 0), focused on open: '$(Focused)' (expect 'YubiKey code')"
Press "{ENTER}"
"  Enter while empty: $((Texts) -match 'Touch your YubiKey') (expect True)"
Keys "cccc"; Press "{ENTER}"
Start-Sleep -Milliseconds 1500
"  accepted: $(@(Get-Content $script:log | Where-Object { $_ -eq 'accepted' }).Count) (expect 1)"
Stop-Preview

"== new device"
Start-Preview "device"
"  title: $((Texts) -match 'Verify this device'), remember box: $(@(Find-All ([System.Windows.Automation.ControlType]::CheckBox)).Count) (expect True, 0)"
Stop-Preview
