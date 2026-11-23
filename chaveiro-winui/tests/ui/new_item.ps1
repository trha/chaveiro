# Drives examples/vault.rs through UI Automation for the New button and the
# new item form: the split button's name and menu, the empty name error, a
# saved login that opens, Cancel, the discard dialog, and a typing stress run
# that counts the UI thread's allocations per pass while keys go into the
# form.
param([string]$Exe = "target\debug\examples\vault.exe")
Add-Type -AssemblyName UIAutomationClient, UIAutomationTypes, System.Windows.Forms
$automation = [System.Windows.Automation.AutomationElement]
$descendants = [System.Windows.Automation.TreeScope]::Descendants
$type = [System.Windows.Automation.ControlType]

function Start-Vault([hashtable]$environment) {
    Remove-Item Env:VAULT_BENCH -ErrorAction SilentlyContinue
    foreach ($entry in $environment.GetEnumerator()) { Set-Item "Env:$($entry.Key)" $entry.Value }
    $script:log = Join-Path $env:TEMP "chaveiro-new-item.txt"
    $script:process = Start-Process $Exe -PassThru -RedirectStandardError $script:log
    Start-Sleep 3
    if ($script:process.HasExited) { throw "the preview exited: $(Get-Content $script:log)" }
    $script:window = $automation::RootElement.FindFirst([System.Windows.Automation.TreeScope]::Children,
        (New-Object System.Windows.Automation.PropertyCondition($automation::ProcessIdProperty, $script:process.Id)))
}
function Stop-Vault {
    if (-not $script:process.HasExited) {
        $script:window.GetCurrentPattern([System.Windows.Automation.WindowPattern]::Pattern).Close()
    }
    $exited = $script:process.WaitForExit(5000)
    if (-not $exited) { Stop-Process $script:process -Confirm:$false }
    $errors = Get-Content $script:log | Where-Object { $_ -match "panicked|error" }
    if ($errors) { "  STDERR: $errors" }
}
function Find-All($kind) {
    @($script:window.FindAll($descendants, (New-Object System.Windows.Automation.PropertyCondition($automation::ControlTypeProperty, $kind))))
}
function Texts { (Find-All $type::Text | ForEach-Object { $_.Current.Name }) }
function Named([string]$name) {
    $script:window.FindFirst($descendants, (New-Object System.Windows.Automation.PropertyCondition($automation::NameProperty, $name)))
}
function Invoke-Named([string]$name) {
    $element = Named $name
    if (-not $element) { "  NO ELEMENT '$name'"; return }
    $element.GetCurrentPattern([System.Windows.Automation.InvokePattern]::Pattern).Invoke()
    Start-Sleep -Milliseconds 500
}
# The edit box whose header is $header; WinUI names a box by its header.
function Edit-Box([string]$header) { Find-All $type::Edit | Where-Object { $_.Current.Name -like "$header*" } | Select-Object -First 1 }
function Set-Text([string]$header, [string]$value) {
    (Edit-Box $header).GetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern).SetValue($value)
    Start-Sleep -Milliseconds 300
}
# Sends each SendKeys token of $keys on its own: one character, or a
# braced name such as {BS}.
function Type-Keys([string]$keys, [int]$delay = 60) {
    foreach ($token in [regex]::Matches($keys, "\{[^}]+\}|.")) {
        [System.Windows.Forms.SendKeys]::SendWait($token.Value)
        Start-Sleep -Milliseconds $delay
    }
}
function Form-Shown { [bool](Texts | Where-Object { $_ -match '^New (login|note)$' }) }

"== New button and menu"
Start-Vault @{}
$new = Named "New login (Ctrl+N)"
"  split button: $(if ($new) { $new.Current.ClassName } else { 'MISSING' }) (expect SplitButton)"
$new.GetCurrentPattern([System.Windows.Automation.ExpandCollapsePattern]::Pattern).Expand()
# The menu is a popup of its own, so it is found from the root, and it
# may take a moment to show.
foreach ($try in 1..10) {
    Start-Sleep -Milliseconds 300
    $menu = @($automation::RootElement.FindAll($descendants, (New-Object System.Windows.Automation.PropertyCondition($automation::ControlTypeProperty, $type::MenuItem))) |
        Where-Object { $_.Current.ProcessId -eq $script:process.Id })
    if ($menu.Count -ge 5) { break }
}
"  menu: $(($menu | ForEach-Object { "$($_.Current.Name)$(if (-not $_.Current.IsEnabled) { ' (off)' })" }) -join ', ')"
"  (expect Login, Card, Identity, Secure note, SSH key)"
[System.Windows.Forms.SendKeys]::SendWait("{ESC}"); Start-Sleep -Milliseconds 300

"== empty name, then a saved login"
Invoke-Named "New login (Ctrl+N)"
"  form: $((Texts | Where-Object { $_ -match '^New (login|note)$' }) -join ' | ') (expect New login)"
Invoke-Named "Save"
"  error: $((Texts | Where-Object { $_ -eq 'Enter a name' }) -join ' | ') (expect Enter a name)"
Set-Text "Name" "Forum"
Set-Text "Username" "alex"
Invoke-Named "Save"
Start-Sleep -Milliseconds 500
"  stderr: $((Get-Content $script:log | Where-Object { $_ -match '^create' }) -join ' | ') (expect create 10xx name=Forum kind=Login)"
"  details: $((Texts | Where-Object { $_ -match '^(Forum|alex|New login)$' }) -join ' | ') (expect Forum and alex, no New login)"

"== Ctrl+N from Notes makes a note; Cancel leaves"
$notes = @(Find-All $type::ListItem) | Where-Object { $_.Current.Name -eq "Notes" } | Select-Object -First 1
$notes.GetCurrentPattern([System.Windows.Automation.SelectionItemPattern]::Pattern).Select(); Start-Sleep -Milliseconds 500
"  tooltip name: $(if (Named 'New note (Ctrl+N)') { 'New note (Ctrl+N)' } else { 'MISSING' })"
(Edit-Box "Search").SetFocus(); Start-Sleep -Milliseconds 200
[System.Windows.Forms.SendKeys]::SendWait("^n"); Start-Sleep -Milliseconds 500
"  form: $((Texts | Where-Object { $_ -match '^New (login|note)$' }) -join ' | ') (expect New note)"
"  username box: $(if (Edit-Box 'Username') { 'present' } else { 'absent' }) (expect absent)"
Invoke-Named "Cancel"
"  after Cancel: form $(if (Form-Shown) { 'still open' } else { 'closed' }) (expect closed)"
Stop-Vault

"== leaving a form with changes asks first"
Start-Vault @{}
Invoke-Named "New login (Ctrl+N)"
Set-Text "Name" "Draft"
Invoke-Named "GitHub"
"  dialog: $((Texts | Where-Object { $_ -match '^Discard' }) -join ' | ') (expect Discard changes to Draft?)"
Invoke-Named "Keep editing"
"  after Keep editing: name '$((Edit-Box 'Name').GetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern).Current.Value)', dialog $(if (Named 'Keep editing') { 'open' } else { 'closed' }) (expect Draft, closed)"
Invoke-Named "GitHub"
Invoke-Named "Discard"
Start-Sleep -Milliseconds 300
"  after Discard: form $(if (Form-Shown) { 'open' } else { 'closed' }), details $((Texts | Where-Object { $_ -match '^(GitHub|alex-dev)$' }) -join ' | ') (expect closed, GitHub | alex-dev)"
Invoke-Named "New login (Ctrl+N)"
Invoke-Named "Amazon"
"  empty form, row click: dialog $(if (Named 'Keep editing') { 'open' } else { 'none' }), form $(if (Form-Shown) { 'open' } else { 'closed' }) (expect none, closed)"
Invoke-Named "New login (Ctrl+N)"
Set-Text "Username" "someone"
Invoke-Named "Lock (Ctrl+L)"
"  Lock: dialog $((Texts | Where-Object { $_ -match '^Discard' }) -join ' | ') (expect Discard the new login?)"
Invoke-Named "Keep editing"
"  after Keep editing: locked $([bool](Get-Content $script:log | Where-Object { $_ -eq 'lock' })) (expect False)"
Invoke-Named "Lock (Ctrl+L)"
Invoke-Named "Discard"
Start-Sleep -Milliseconds 500
"  after Discard: locked $([bool](Get-Content $script:log | Where-Object { $_ -eq 'lock' })) (expect True)"
Stop-Vault

# The warm-up covers launch and opening the form; the keys start once the
# preview prints that it measures, and end well before it stops. A pass
# runs every 10 ms besides the passes the keys cause.
"== typing stress: allocations per pass"
Start-Vault @{ VAULT_BENCH = "400,2500" }
Invoke-Named "New login (Ctrl+N)"
while (-not (Get-Content $script:log | Where-Object { $_ -eq "bench: measuring" })) { Start-Sleep -Milliseconds 100 }
$typed = "abcdefghijklmnopqrstuvwxyz0123456789{BS}{BS}{BS}{BS}{BS}"
foreach ($header in "Name", "Username", "Website", "Notes") {
    (Edit-Box $header).SetFocus(); Start-Sleep -Milliseconds 200
    Type-Keys $typed 20
}
"  values: $(("Name", "Username", "Website", "Notes" | ForEach-Object { (Edit-Box $_).GetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern).Current.Value }) -join ' | ') (expect abcdefghijklmnopqrstuvwxyz01234 in each)"
"  measuring still: $(-not (Get-Content $script:log | Where-Object { $_ -match '^bench: \d' })) (expect True: every key landed in the measured passes)"
$exited = $script:process.WaitForExit(60000)
"  $((Get-Content $script:log | Where-Object { $_ -match '^bench: \d' }) -join ' | ') (exited=$exited)"
Stop-Vault
