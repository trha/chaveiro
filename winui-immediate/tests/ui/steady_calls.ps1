# Runs examples/steady_calls.rs, which counts the WinUI calls of a pass with
# no changes and of a pass that changes one label, and fails unless they are
# 0 and 1. Build it first with:
#   cargo build --release -p winui-immediate --features count-calls --example steady_calls
# The default 300 rows (2,000 nodes) runs in about 2 s. `-Rows 4800` gives the
# example's 20,000 nodes, where WinUI's first layout holds the window for
# about 95 s.
param([string]$Exe = "target\release\examples\steady_calls.exe", [int]$Rows = 300)
$log = Join-Path $env:TEMP "winui-immediate-steady-calls.txt"
if ($Rows -gt 0) { $env:STEADY_ROWS = $Rows }
$process = Start-Process $Exe -PassThru -RedirectStandardError $log
$exited = $process.WaitForExit(300000)
Remove-Item Env:STEADY_ROWS -ErrorAction SilentlyContinue
if (-not $exited) { Stop-Process $process -Confirm:$false }
Get-Content $log
"exited=$exited code=$($process.ExitCode)"
if (-not $exited -or $process.ExitCode -ne 0) { exit 1 }
