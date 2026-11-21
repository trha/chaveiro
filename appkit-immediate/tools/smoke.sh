#!/usr/bin/env bash
# Drives examples/smoke through System Events and checks its log. Needs
# Accessibility access for the terminal that runs it.
set -euo pipefail
root=$(cd "$(dirname "$0")/../.." && pwd)
cargo build -q -p appkit-immediate --example smoke --example grid --example form --example login --manifest-path "$root/Cargo.toml"
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

# Run 3: grid rows. Heights come back as "label: height" lines.
"$root/target/debug/examples/grid" >"$log" 2>&1 &
pid=$!
sleep 1.5
grid='tell application "System Events" to tell (first process whose name is "grid")'
heights() {
    for label in auto pixels "one star" "two stars"; do
        osascript -e "$grid to get item 2 of (get size of (first static text of window \"grid\" whose value is \"$label\"))"
    done | paste -sd' ' -
}
grid_ok() { # pixels is 40, two is twice one, auto keeps the label's own height
    read -r auto pixels one two <<<"$(heights)"
    # Accessibility reports frames, 2pt taller than the alignment rects the
    # constraints size.
    pixels=$((pixels - 2)) one=$((one - 2)) two=$((two - 2))
    [ "$pixels" -eq 40 ] && [ "$two" -ge $((2 * one - 1)) ] && [ "$two" -le $((2 * one + 1)) ] && [ "$auto" -lt 30 ]
}
grid_height() { osascript -e "$grid to get item 2 of (get size of window \"grid\")"; }
check "grid rows at the first size" grid_ok
check "the grid window opens at least 200 tall" "[ \$(grid_height) -ge 200 ]"
osascript -e "$grid to set size of window \"grid\" to {500, 600}" >/dev/null
sleep 0.4
check "grid rows after a resize" grid_ok
check "star rows grow with the window" "[ \$(heights | awk '{ print \$4 }') -gt 300 ]"
kill "$pid"

# Run 4: text fields. Typing needs the app in front, so this run takes the
# keyboard focus for a few seconds.
"$root/target/debug/examples/form" >"$log" 2>&1 &
pid=$!
sleep 1.5
form='tell application "System Events" to tell (first process whose name is "form")'
texts() { osascript -e "$form to get value of static texts of window \"form\""; }
type_into() { # field index, keystrokes
    osascript -e "$form to set frontmost to true" \
        -e "$form to set focused of text field $1 of window \"form\" to true" \
        -e "tell application \"System Events\" to keystroke \"$2\"" >/dev/null
    sleep 0.4
}
field_value() { osascript -e "$form to get value of text field $1 of window \"form\""; }
type_into 1 hello
check "typing reaches the app's value" "texts | grep -q 'name: hello'"
osascript -e 'tell application "System Events" to key code 36' >/dev/null
sleep 0.3
check "Return reaches return_pressed" "texts | grep -q 'returns: 1'"
type_into 2 ab
check "an app write during an edit shows" "[ \"\$(field_value 2)\" = AB ]"
# Left arrow puts the caret between A and B; the app's next write must keep it there.
osascript -e 'tell application "System Events" to key code 123' -e 'tell application "System Events" to keystroke "x"' >/dev/null
sleep 0.4
# The app rewrote the field after the x; the next keystroke shows where the
# caret ended up.
osascript -e 'tell application "System Events" to keystroke "y"' >/dev/null
sleep 0.4
check "an app write keeps the caret inside the text" "[ \"\$(field_value 2)\" = AXYB ]"
type_into 3 pw1
osascript -e "$form to click button \"Take\" of window \"form\"" >/dev/null
sleep 0.4
check "take returns the password and clears the field" "texts | grep -q 'took: Some(3), empty: true'"
kill "$pid"

# Run 5: the login form. Also takes the keyboard focus.
"$root/target/debug/examples/login" >"$log" 2>&1 &
pid=$!
sleep 1.5
login='tell application "System Events" to tell (first process whose name is "login")'
card='group 1 of window "Chaveiro"'
status() { osascript -e "$login to get value of static texts of $card"; }
button_y() { osascript -e "$login to get item 2 of (get position of button \"Log in\" of $card)"; }
title_buttons() { osascript -e "$login to get subrole of buttons of window \"Chaveiro\""; }
server_enabled() { osascript -e "$login to get enabled of pop up button 1 of $card"; }
check "a compact window shows only the close button" '[ "$(title_buttons)" = AXCloseButton ]'
osascript -e "$login to click pop up button 1 of $card" \
    -e "$login to click menu item \"bitwarden.eu\" of menu 1 of pop up button 1 of $card" >/dev/null
sleep 0.4
check "a pop-up pick reaches the app" "status | grep -q 'server: 1'"
osascript -e "$login to set frontmost to true" \
    -e "$login to set focused of combo box 1 of $card to true" \
    -e 'tell application "System Events" to keystroke "a@b.c"' \
    -e 'tell application "System Events" to key code 36' >/dev/null
sleep 0.4
check "combo box text reaches the app on Return" "status | grep -q 'email: a@b.c'"
before=$(button_y)
osascript -e "$login to set focused of text field 1 of $card to true" \
    -e 'tell application "System Events" to key code 36' >/dev/null
sleep 0.4
check "Return on an empty password shows the error" "status | grep -q 'Enter your master password'"
check "the error line keeps its place" "[ \"\$(button_y)\" = '$before' ]"
osascript -e "$login to set focused of text field 1 of $card to true" \
    -e 'tell application "System Events" to keystroke "pw"' >/dev/null
osascript -e "$login to click button \"Log in\" of $card" >/dev/null
sleep 0.4
check "a busy form disables its pop-up button" '[ "$(server_enabled)" = false ]'
check "the error clears on a good submit" "! status | grep -q 'Enter your master password'"
sleep 2
check "the result arrives and enables the form again" 'status | grep -q "signed in with 2 characters" && [ "$(server_enabled)" = true ]'
kill "$pid"

rm -f "$log"
exit "$failures"
