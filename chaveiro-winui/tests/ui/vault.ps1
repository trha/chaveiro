# Drives examples/vault.rs through UI Automation: the pane's entries and
# counts, picking a filter, the item list and details, Show, the command
# bar, an unreadable item, the re-prompt note, Settings, and Lock.
param([string]$Exe = "target\debug\examples\vault.exe", [string]$Shot = "")
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
# Each pane item's name and the count drawn inside it.
function Entries {
    (Items | ForEach-Object {
        $texts = @($_.FindAll($descendants, (New-Object System.Windows.Automation.PropertyCondition($automation::ControlTypeProperty, [System.Windows.Automation.ControlType]::Text))) |
            ForEach-Object { $_.Current.Name } | Where-Object { $_ -match '^\d+$' })
        "$($_.Current.Name) $($texts -join '')".Trim()
    }) -join ", "
}
function Texts { (Find-All ([System.Windows.Automation.ControlType]::Text) | ForEach-Object { $_.Current.Name }) }
function Count { (Texts | Where-Object { $_ -match '^\d+ items?$|^Settings are' }) -join " | " }
function Buttons { (Find-All ([System.Windows.Automation.ControlType]::Button) | ForEach-Object { $_.Current.Name } | Where-Object { $_ }) -join ", " }
function Button([string]$name) { Find-All ([System.Windows.Automation.ControlType]::Button) | Where-Object { $_.Current.Name -eq $name } | Select-Object -First 1 }
function Invoke([string]$name) {
    $button = Button $name
    if (-not $button) { "  NO BUTTON '$name'"; return }
    $button.GetCurrentPattern([System.Windows.Automation.InvokePattern]::Pattern).Invoke()
    Start-Sleep -Milliseconds 500
}
function Pick([string]$name) {
    (Items | Where-Object { $_.Current.Name -eq $name } | Select-Object -First 1).GetCurrentPattern([System.Windows.Automation.SelectionItemPattern]::Pattern).Select()
    Start-Sleep -Milliseconds 500
}
# The texts of the details pane that the checks below look for.
function Details { (Texts | Where-Object { $_ -match '^(Username|Password|••|Tr0ub4dor|Website|https://|Folder:|This item|Chaveiro can|Select an item|Rua das)' }) -join " | " }

"entries: $(Entries)"
"  (expect All items 10, Favorites 3, Logins 5, Cards 1, Identities 1, Notes 1, SSH keys 1,"
"   Unreadable folder 1, Personal 4, Work 3, No folder 2, Trash 2, Settings)"
"open: count '$(Count)' (expect '10 items'), details '$(Details)' (expect 'Select an item...')"
Pick "Work"
"pick Work: count '$(Count)' (expect '3 items'), rows: $(Buttons)"
"  (expect Deploy key, GitHub, Mail server among the buttons)"
Invoke "GitHub"
"open GitHub: $(Details)"
"  (expect Username, Password, dots, two Website captions, the URLs, the folder line)"
Invoke "Show"
"show password: $(Details)"
"  (expect Tr0ub4dor&3-xK9)"
Invoke "Unfavorite"
"after Unfavorite: buttons $(Buttons)"
"  (expect Favorite now)"
Invoke "Mail server"
"open Mail server: $(Details)"
"  (expect no Tr0ub4dor: secrets hide when another item opens)"
Pick "All items"
Invoke "Can't read this item"
"unreadable: $(Details)"
"  (expect This item didn't decrypt.)"
Invoke "Home Wi-Fi"
"re-prompt: $(Details)"
"  (expect This item asks for your master password...)"
Invoke "Alex Lima"
"identity: $(Details)"
"  (expect the address starting Rua das Flores)"
Invoke "Move to trash"
Pick "Trash"
"trash: count '$(Count)' (expect '3 items')"
Invoke "Alex Lima"
Invoke "Restore"
"trash after Restore: count '$(Count)' (expect '2 items')"
function Search-Box { Find-All ([System.Windows.Automation.ControlType]::Edit) | Select-Object -First 1 }
function Search([string]$text) {
    (Search-Box).GetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern).SetValue($text)
    Start-Sleep -Milliseconds 500
}
# The list's row names, in order.
function Rows { (Find-All ([System.Windows.Automation.ControlType]::Button) | ForEach-Object { $_.Current.Name } | Where-Object { $_ -and $_ -notmatch '^(Lock|Show|Hide|Favorite|Unfavorite|Move to trash|Restore|Close|Minimize|Maximize|Back)$' }) -join ", " }
"search box in trash: name '$((Search-Box).Current.Name)' help '$((Search-Box).Current.HelpText)' (expect Search trash somewhere)"
Pick "All items"
"search box in All items: name '$((Search-Box).Current.Name)' help '$((Search-Box).Current.HelpText)'"
Search "a"
"search 'a': $(Count) rows: $(Rows)"
"  (expect Alex Lima, Amazon first as prefixes)"
Search "github.com"
"search 'github.com': rows: $(Rows) (expect GitHub, by its address)"
Search "srv"
"search 'srv': rows: $(Rows) (expect Mail server, fuzzy)"
Search "zzz"
"search 'zzz': $(Texts | Where-Object { $_ -match 'match your search' }) (expect No items match your search.)"
Pick "Work"
Search "e"
"search 'e' in Work: rows: $(Rows) (expect only Work items: Deploy key, Mail server by name, then GitHub by its username)"
Search ""
"cleared: $(Count) (expect '3 items')"
# Keys go to the focused control, so these use SendKeys on the window.
Add-Type -AssemblyName System.Windows.Forms
function Send([string]$keys) { [System.Windows.Forms.SendKeys]::SendWait($keys); Start-Sleep -Milliseconds 500 }
Pick "All items"
(Button "Amazon").SetFocus()
Send "^f"
"Ctrl+F: search box focused $((Search-Box).Current.HasKeyboardFocus) (expect True), placeholder '$((Search-Box).Current.Name)' (expect Search all items (Ctrl+F))"
Send "{DOWN}"
"Down from Alex Lima, selected since Restore: $(Details) (expect Amazon: Folder: Personal)"
Send "{DOWN}{DOWN}"
"Down twice more: $(Details) (expect GitHub's fields)"
Send "{UP}"
"Up: $(Details) (expect Deploy key: dots, Folder: Work)"
Send "{UP}{UP}{UP}"
"Up past the top: $(Details) (expect This item didn't decrypt.)"
Send "{END}"
for ($step = 0; $step -lt 12; $step++) { Send "{DOWN}" }
Send "{END}"
"Down past the end: last row 'Shared drive' shown $([bool](Button 'Shared drive')), details $(Details) (expect True, Chaveiro can't show this type...)"
# A click selects the row; focus alone, as UI Automation sets it, does not.
Invoke "Amazon"
(Button "Amazon").SetFocus()
Send "{DOWN}"
"Down on the Amazon row: $(Details), focus on '$($automation::FocusedElement.Current.Name)'"
"  (expect Deploy key: dots, Folder: Work; focus on Deploy key)"
Send "{DOWN}{DOWN}{UP}"
"Down, Down, Up on the rows: focus on '$($automation::FocusedElement.Current.Name)' (expect GitHub)"
if ($Shot) {
    Pick "All items"
    Invoke "GitHub"
    Add-Type -AssemblyName System.Drawing
    $rect = $window.Current.BoundingRectangle
    $bitmap = New-Object System.Drawing.Bitmap ([int]$rect.Width), ([int]$rect.Height)
    $graphics = [System.Drawing.Graphics]::FromImage($bitmap)
    $graphics.CopyFromScreen([int]$rect.X, [int]$rect.Y, 0, 0, $bitmap.Size)
    $bitmap.Save($Shot)
}
Pick "Settings"
"pick Settings: '$(Count)' (expect 'Settings are not built yet.')"
Pick "All items"
Invoke "Lock"
if (-not $process.WaitForExit(5000)) { Stop-Process $process -Confirm:$false; "did not exit after Lock" }
"stderr (expect open 11, save 11 favorite=false, open 12, close, open 15, open 14, trash 14, restore 14,"
"  then the keys: open 10 16 11 16 10 14, close, open 14 10 16 11 15 12 18 13 17, then the rows: open 10 16 11 15 11, lock):"
Get-Content $log
