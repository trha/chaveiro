# Builds the spike for wasm, inlines it into one HTML page, runs the page in
# headless Edge, and prints the JSON the harness wrote.
#
#   dom-immediate/spike/run.ps1 -WasmBindgen <path to wasm-bindgen.exe>
param([Parameter(Mandatory)][string]$WasmBindgen)
$ErrorActionPreference = 'Stop'
$root = Resolve-Path "$PSScriptRoot/../.."
$out = "$root/target/dom-spike"
cargo build -p dom-immediate --target wasm32-unknown-unknown --example spike
if ($LASTEXITCODE) { exit $LASTEXITCODE }
& $WasmBindgen --target no-modules --no-typescript --out-dir $out "$root/target/wasm32-unknown-unknown/debug/examples/spike.wasm"
if ($LASTEXITCODE) { exit $LASTEXITCODE }
# A page from file:// cannot fetch the module, so the bytes go inline.
$wasm = [Convert]::ToBase64String([IO.File]::ReadAllBytes("$out/spike_bg.wasm"))
$glue = Get-Content "$out/spike.js" -Raw
$harness = Get-Content "$PSScriptRoot/harness.js" -Raw
$page = @"
<!doctype html>
<meta charset="utf-8">
<title>dom-immediate spike</title>
<div id="app"></div>
<pre id="result">not run</pre>
<script>$glue</script>
<script>$harness</script>
<script>
  const bytes = Uint8Array.from(atob("$wasm"), (c) => c.charCodeAt(0));
  try {
    wasm_bindgen.initSync({ module: bytes });
    document.getElementById("result").textContent = "started";
    drive().catch((error) => (document.getElementById("result").textContent = "drive failed: " + error + "\nerrors: " + JSON.stringify(window.__errors) + "\npanic: " + document.body.dataset.panic));
  } catch (error) {
    document.getElementById("result").textContent = "init failed: " + error + "\n" + error.stack;
  }
</script>
"@
Set-Content "$out/spike.html" $page -NoNewline
$edge = 'C:\Program Files (x86)\Microsoft\Edge\Application\msedge.exe'
$dom = & $edge --headless=new --disable-gpu --virtual-time-budget=10000 --user-data-dir="$out/edge-profile" --dump-dom "file:///$($out -replace '\\','/')/spike.html" 2>$null | Out-String
if ($dom -match '(?s)<pre id="result">(.*?)</pre>') { [Net.WebUtility]::HtmlDecode($Matches[1]) } else { $dom }
