# Drives examples/vault_list through UI Automation (tests 8-10 of
# plans/winui-immediate.md): scrolling, Reveal and list_state, filtering,
# unlock, the details window's app-controlled close, and the callback count
# from the trace log. Run tests/ui/widgets.ps1 for caret and IME.
param([string]$Exe = "target\release\examples\vault_list.exe")
Add-Type -AssemblyName UIAutomationClient, UIAutomationTypes, System.Windows.Forms
$automation = [System.Windows.Automation.AutomationElement]
$children = [System.Windows.Automation.TreeScope]::Children
$descendants = [System.Windows.Automation.TreeScope]::Descendants
$env:RUST_LOG = "winui_immediate=trace"
$log = Join-Path $env:TEMP "winui-immediate-vault.txt"
$process = Start-Process $Exe -PassThru -RedirectStandardError $log
# Open the handle now, or ExitCode reads null once the process exits.
$null = $process.Handle
Remove-Item Env:RUST_LOG
Start-Sleep 3

function Windows {
    @($automation::RootElement.FindAll($children,
        (New-Object System.Windows.Automation.PropertyCondition($automation::ProcessIdProperty, $process.Id))))
}
$window = Windows | Where-Object { $_.Current.Name -eq "winui-immediate vault" }
$scrollable = $window.FindFirst($descendants,
    (New-Object System.Windows.Automation.PropertyCondition($automation::IsScrollPatternAvailableProperty, $true)))
$scroll = $scrollable.GetCurrentPattern([System.Windows.Automation.ScrollPattern]::Pattern)
$view = $scrollable.Current.BoundingRectangle

function Find-All($type) {
    @($window.FindAll($descendants, (New-Object System.Windows.Automation.PropertyCondition($automation::ControlTypeProperty, $type))))
}
function Button([string]$name) { Find-All ([System.Windows.Automation.ControlType]::Button) | Where-Object { $_.Current.Name -eq $name } | Select-Object -First 1 }
function Invoke($element) { $element.GetCurrentPattern([System.Windows.Automation.InvokePattern]::Pattern).Invoke(); Start-Sleep -Milliseconds 400 }
function Texts { (Find-All ([System.Windows.Automation.ControlType]::Text) | Select-Object -First 8 | ForEach-Object { $_.Current.Name }) -join " | " }
function Type-Keys([string]$keys) {
    foreach ($character in $keys.ToCharArray()) {
        [System.Windows.Forms.SendKeys]::SendWait([string]$character)
        Start-Sleep -Milliseconds 60
    }
}
function Check-Rows([string]$label) {
    $rows = @(Find-All ([System.Windows.Automation.ControlType]::Text) | Where-Object { $_.Current.Name -match '^entry (\d+)$' } | ForEach-Object {
        $box = $_.Current.BoundingRectangle
        if ($box.Bottom -gt $view.Top -and $box.Top -lt $view.Bottom) {
            [pscustomobject]@{ Number = [int]($_.Current.Name -replace 'entry ', ''); Top = $box.Top; Bottom = $box.Bottom }
        }
    } | Sort-Object Number)
    $numbers = $rows | ForEach-Object Number
    $consecutive = ($rows.Count -gt 0) -and (($numbers[-1] - $numbers[0] + 1) -eq $rows.Count)
    $covered = ($rows.Count -gt 0) -and ($rows[0].Top -le $view.Top + 1) -and ($rows[-1].Bottom -ge $view.Bottom - 1)
    "{0}: offset {1:N1}%, rows on screen {2} (entry {3}..{4}), consecutive={5}, covers viewport={6}" -f `
        $label, $scroll.Current.VerticalScrollPercent, $rows.Count, $numbers[0], $numbers[-1], $consecutive, $covered
}
# Converts a tracing duration ("19.2µs", "1.05ms", "830ns", "1.2s") to µs.
function Micros([string]$text) {
    if ($text -notmatch '^([\d.]+)(ns|µs|us|ms|s)$') { return $null }
    $value = [double]$Matches[1]
    switch ($Matches[2]) { "ns" { $value / 1000 } "ms" { $value * 1000 } "s" { $value * 1000000 } default { $value } }
}
function Summary([string]$span) {
    $times = @(Get-Content $log | Where-Object { $_ -match " $span(\{[^}]*\})?: .*close time\.busy=(\S+)" } | ForEach-Object {
        if ($_ -match "time\.busy=(\S+)") { Micros $Matches[1] }
    } | Sort-Object)
    if ($times.Count -eq 0) { return "${span}: none" }
    "{0}: {1}, p50 {2:N0} us, p90 {3:N0} us, max {4:N0} us" -f $span, $times.Count, $times[[int](($times.Count - 1) * 0.5)], $times[[int](($times.Count - 1) * 0.9)], $times[-1]
}

"== list"
Check-Rows "start"
Invoke (Button "Reveal")
"after Reveal on the first row: $(Texts)"
$scroll.SetScrollPercent([System.Windows.Automation.ScrollPattern]::NoScroll, 50); Start-Sleep -Milliseconds 500
Check-Rows "jump to 50%"
foreach ($step in 1..60) { $scroll.Scroll([System.Windows.Automation.ScrollAmount]::NoAmount, [System.Windows.Automation.ScrollAmount]::SmallIncrement); Start-Sleep -Milliseconds 16 }
Start-Sleep -Milliseconds 500
Check-Rows "60 small steps"
foreach ($step in 1..20) { $scroll.Scroll([System.Windows.Automation.ScrollAmount]::NoAmount, [System.Windows.Automation.ScrollAmount]::LargeIncrement); Start-Sleep -Milliseconds 16 }
Start-Sleep -Milliseconds 500
Check-Rows "20 page steps"
$scroll.SetScrollPercent([System.Windows.Automation.ScrollPattern]::NoScroll, 100); Start-Sleep -Milliseconds 500
Check-Rows "jump to end"
$scroll.SetScrollPercent([System.Windows.Automation.ScrollPattern]::NoScroll, 0); Start-Sleep -Milliseconds 500
"back at the top, entry 1 still revealed: $((Texts) -like '*entry 1 | password 1*')"

"== unlock"
$password = Find-All ([System.Windows.Automation.ControlType]::Edit) | Where-Object { $_.Current.IsPassword } | Select-Object -First 1
$password.SetFocus(); Start-Sleep -Milliseconds 300
Type-Keys "hunter2"
Invoke (Button "Unlock")
"status: $((Find-All ([System.Windows.Automation.ControlType]::Text) | Select-Object -First 1).Current.Name); Lock button present: $($null -ne (Button 'Lock'))"

"== filter"
$filter = Find-All ([System.Windows.Automation.ControlType]::Edit) | Where-Object { -not $_.Current.IsPassword } | Select-Object -First 1
$filter.SetFocus(); Start-Sleep -Milliseconds 300
Type-Keys "entry 12"
Start-Sleep -Milliseconds 600
$shown = @(Find-All ([System.Windows.Automation.ControlType]::Text) | Where-Object { $_.Current.Name -match '^entry \d+$' } | ForEach-Object { $_.Current.Name })
"status: $((Find-All ([System.Windows.Automation.ControlType]::Text) | Select-Object -First 1).Current.Name); rows shown: $($shown.Count), all match: $(@($shown | Where-Object { $_ -notmatch 'entry 12' }).Count -eq 0)"

"== windows (test 10)"
Invoke (Button "Open details")
"windows after Open details: $((Windows | ForEach-Object { $_.Current.Name }) -join ', ')"
$details = Windows | Where-Object { $_.Current.Name -eq "vault details" }
$details.GetCurrentPattern([System.Windows.Automation.WindowPattern]::Pattern).Close(); Start-Sleep -Milliseconds 500
$details = Windows | Where-Object { $_.Current.Name -eq "vault details" }
$detailsText = if ($details) { ($details.FindAll($descendants, (New-Object System.Windows.Automation.PropertyCondition($automation::ControlTypeProperty, [System.Windows.Automation.ControlType]::Text))) | ForEach-Object { $_.Current.Name }) -join " | " } else { "(gone)" }
"after the first close request: details window open=$($null -ne $details), says '$detailsText'"
$details.GetCurrentPattern([System.Windows.Automation.WindowPattern]::Pattern).Close(); Start-Sleep -Milliseconds 500
"after the second close request: windows $((Windows | ForEach-Object { $_.Current.Name }) -join ', ')"
$window.GetCurrentPattern([System.Windows.Automation.WindowPattern]::Pattern).Close()
$exited = $process.WaitForExit(5000)
if (-not $exited) { Stop-Process $process -Confirm:$false }
"closing the last window: exited=$exited code=$($process.ExitCode), run returned: $((Get-Content $log) -contains 'run returned')"

"== log"
$lines = Get-Content $log
$panics = $lines | Where-Object { $_ -match "panicked" }
if ($panics) { "PANIC: $panics" }
$delegates = $lines | Where-Object { $_ -match "shared delegates" }
$registrations = @($lines | Where-Object { $_ -match "subscribe kind=(\w+) source=(\d+)" } | ForEach-Object {
    if ($_ -match "subscribe kind=(\w+) source=(\d+)") { "$($Matches[1])@$($Matches[2])" }
})
$kinds = $registrations | ForEach-Object { ($_ -split '@')[0] } | Group-Object | ForEach-Object { "$($_.Name)=$($_.Count)" }
"delegate objects: $(($delegates | Select-Object -First 1) -replace '.*count=(\d+).*', '$1') (logged $(@($delegates).Count) time)"
"registrations: $($registrations.Count), unique control and kind pairs: $(@($registrations | Sort-Object -Unique).Count) ($($kinds -join ', '))"
Summary "turn"
Summary "realize"
