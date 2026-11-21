# Drives examples/vault.rs through UI Automation: the pane's entries and
# counts, picking a filter, picking Settings, and Lock.
param([string]$Exe = "target\debug\examples\vault.exe")
Add-Type -AssemblyName UIAutomationClient, UIAutomationTypes
$automation = [System.Windows.Automation.AutomationElement]
$descendants = [System.Windows.Automation.TreeScope]::Descendants
$log = Join-Path $env:TEMP "chaveiro-vault.txt"
$process = Start-Process $Exe -PassThru -RedirectStandardError $log
Start-Sleep 3
$window = $automation::RootElement.FindFirst([System.Windows.Automation.TreeScope]::Children,
    (New-Object System.Windows.Automation.PropertyCondition($automation::ProcessIdProperty, $process.Id)))

function Find-All($type) {
    @($window.FindAll($descendants, (New-Object System.Windows.Automation.PropertyCondition($automation::ControlTypeProperty, $type))))
}
function Items { Find-All ([System.Windows.Automation.ControlType]::ListItem) }
# Each item's name and the count drawn inside it.
function Entries {
    (Items | ForEach-Object {
        $texts = @($_.FindAll($descendants, (New-Object System.Windows.Automation.PropertyCondition($automation::ControlTypeProperty, [System.Windows.Automation.ControlType]::Text))) |
            ForEach-Object { $_.Current.Name } | Where-Object { $_ -match '^\d+$' })
        "$($_.Current.Name) $($texts -join '')".Trim()
    }) -join ", "
}
function Texts { (Find-All ([System.Windows.Automation.ControlType]::Text) | ForEach-Object { $_.Current.Name }) }
function Content { (Texts | Where-Object { $_ -match '^\d+ items?$|^Settings are' }) -join " | " }
function Pick([string]$name) {
    (Items | Where-Object { $_.Current.Name -eq $name } | Select-Object -First 1).GetCurrentPattern([System.Windows.Automation.SelectionItemPattern]::Pattern).Select()
    Start-Sleep -Milliseconds 400
}

"entries: $(Entries)"
"  (expect All items 9, Favorites 3, Logins 4, Cards 1, Identities 1, Notes 1, SSH keys 1,"
"   Unreadable folder 1, Personal 3, Work 3, No folder 2, Trash 2, Settings)"
"open: content '$(Content)' (expect '9 items')"
Pick "Work"
"pick Work: content '$(Content)' (expect '3 items')"
Pick "Identities"
"pick Identities: content '$(Content)' (expect '1 item')"
Pick "Settings"
"pick Settings: content '$(Content)' (expect 'Settings are not built yet.')"
$lock = Find-All ([System.Windows.Automation.ControlType]::Button) | Where-Object { $_.Current.Name -eq "Lock" } | Select-Object -First 1
$lock.GetCurrentPattern([System.Windows.Automation.InvokePattern]::Pattern).Invoke()
if (-not $process.WaitForExit(5000)) { Stop-Process $process -Confirm:$false; "did not exit after Lock" }
"stderr (expect lock):"
Get-Content $log
