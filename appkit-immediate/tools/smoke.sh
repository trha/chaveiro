#!/usr/bin/env bash
# Drives examples/smoke through System Events and checks its log. Needs
# Accessibility access for the terminal that runs it.
set -euo pipefail
root=$(cd "$(dirname "$0")/../.." && pwd)
cargo build -q -p appkit-immediate --example smoke --manifest-path "$root/Cargo.toml"
binary="$root/target/debug/examples/smoke"
log=$(mktemp -t smoke.XXXXXX)
failures=0

process='tell application "System Events" to tell (first process whose name is "smoke")'
main='window "appkit-immediate smoke"'

check() { # name, command that must succeed
    if eval "$2"; then echo "ok   $1"; else echo "FAIL $1"; failures=$((failures + 1)); fi
}
labels() { osascript -e "$process to get value of static texts of $main"; }
wait_exit() { for _ in $(seq 50); do kill -0 "$1" 2>/dev/null || return 0; sleep 0.1; done; return 1; }

# Run 1: clicks, the background message, closing the second window, Quit.
OBJC_DEBUG_MISSING_POOLS=YES "$binary" >"$log" 2>&1 &
pid=$!
sleep 1.5
main_thread=$(sed -n 's/^main thread //p' "$log")
check "no missing autorelease pools on the main thread" "[ -n '$main_thread' ] && ! grep -q 'MISSING POOLS: ($main_thread)' '$log'"
size() { osascript -e "$process to get size of $main"; }
check "a new window opens at least 360x200" "size | awk -F', ' '{ exit !(\$1 >= 360 && \$2 >= 200) }'"
check "background message arrives" "labels | grep -q 'hello from a background thread'"
osascript -e "$process to click button \"Click me\" of $main" >/dev/null
sleep 0.4
check "one click counts once" "labels | grep -q '^1 clicks'"
check "a click adds the odd label" "labels | grep -q 'odd count'"
osascript -e "$process to click button \"Click me\" of $main" >/dev/null
sleep 0.4
check "a second click removes it" "labels | grep -q '^2 clicks' && ! labels | grep -q 'odd count'"
osascript -e "$process to click (first button of window \"second window\" whose subrole is \"AXCloseButton\")" >/dev/null
sleep 0.4
windows() { osascript -e "$process to get name of windows"; }
check "closing the second window drops it" "[ \"\$(windows)\" = 'appkit-immediate smoke' ]"
osascript -e "$process to click menu item \"Quit\" of menu 1 of menu bar item 2 of menu bar 1" >/dev/null
check "Quit ends the app" "wait_exit $pid"
check "Quit reaches should_terminate" "grep -q 'should terminate' '$log'"
check "run returns and drops the app" "grep -q 'app dropped' '$log' && grep -q 'run returned' '$log'"

# Run 2: a panic in view closes the windows and resumes the panic.
SMOKE_PANIC=1 "$binary" >"$log" 2>&1 &
pid=$!
status=0
wait "$pid" || status=$?
check "a panicking pass ends the process with the panic" "[ $status -ne 0 ] && grep -q 'planned panic' '$log'"
check "the app is dropped after a panic" "grep -q 'app dropped' '$log'"
check "no abort after a panic" "! grep -qi 'abort' '$log'"

rm -f "$log"
exit "$failures"
