# Idle pass check

Status: Done

Parent plans: `plans/appkit-immediate.md` ("Response and stale writes").
Where they differ, this plan wins.

## Goal

In debug builds, `appkit-immediate` proves that a pass with nothing new to
show sends no property write to AppKit. When a pass writes anyway, the app
panics with the node's key, the widget kind and the AppKit selector. This
catches the class of bug fixed in `1c4f3c6`, where `required()` rewrote a
field header on every pass, on any UI without knowing its widgets.

## Out of scope

Release builds, which get no check and no cost. `winui-immediate`. Writes
made while AppKit asks for table rows between passes, because no pass is
running then.

## Facts this plan rests on

**Pass loop.** `Runner::pass` (`appkit-immediate/src/runner.rs:179`) drains
messages into `App::update`, then runs `Ui::begin`, `App::view`,
`Ui::build_rows` and `Ui::end`. `begin` returns whether events arrived. A pass
that consumed events schedules one more pass (`runner.rs:278-280`), because a
widget declared before the one that read the event shows old state. That
catch-up pass may write legitimately, so a write check must skip the pass
right after events.

**Where update writes happen.** An idle pass creates no widget, so the
setters inside `visit` creation closures never run in it. About 90 setter
calls exist in `src/`, and most are creation-time. The update path decides
writes with `must_write(stale, written, declared)` (`src/widget.rs:195`) at
21 sites in 9 widget files. These update writes skip `must_write` and compare
on their own:
- `Response::enabled`, `style`, `align`, `width` and `padding`
  (`src/widget.rs:74-160`);
- `FieldHandle` header and error line (`src/widget/field.rs`, `set_header`
  and `error_line`);
- window options and `show_new_windows` (`src/widget/window.rs`);
- child edits in `Pass::apply_visual` (`src/pass.rs:200-390`): `addSubview`,
  `removeFromSuperview`, constraint activation and hugging priority.

The `required()` bug was in this second group, so a check that only hooks
`must_write` would have missed it.

**Writes the app makes on purpose.** `native(id, create, update)` runs
`update` on every pass, and the smoke example's native view shows a new
count each pass (`tools/smoke.sh:37`, "a native view is updated every
pass"). The smoke app also counts passes to plan its panic
(`examples/smoke.rs:67-68`).

**objc2 runtime access.** objc2 0.6.5 exposes `ffi::class_replaceMethod` and
`ffi::method_setImplementation` (`objc2-0.6.5/src/ffi/class.rs:144`,
`ffi/method.rs:46`). It is not yet shown that a replaced setter can call the
original implementation and keep AppKit working. Step 1 tests this.

## Design

### Check rule

A pass is *settled* when it consumed no events and its `update` drained no
messages. In debug builds, after each settled pass, the runner runs one
*check pass* at once, in the same turn: `begin`, `view`, `build_rows` and
`end`, with no message drain. The check pass must record zero writes. If it
consumed events, which AppKit can queue during a pass, the result is
discarded. A pass right after an event is settled, so every interaction
gets one check. A pass that follows only a message does not.

A check pass runs app code, so its panic takes the same `catch_unwind` path
as any pass.

### Recorder

The recorder is debug-only state that lives in a thread-local, because
writes are recorded deep inside `&mut` borrows of the tree, where no `&mut
Ui` is free. It holds one owner's state and is reset by the runner at the
start of each check pass.

```rust
// src/write_check.rs, compiled only with debug_assertions
pub(crate) struct Write {
    /// The node that decided the write; `None` for a write seen only by the
    /// setter hook.
    pub(crate) node: Option<NodeId>,
    /// The AppKit selector, or the `must_write` call site.
    pub(crate) what: &'static str,
    /// The receiver's address, for a write seen by the setter hook.
    pub(crate) receiver: usize,
}

/// Starts recording. Returns the writes seen since the previous `begin`.
pub(crate) fn begin() -> Vec<Write>;
/// Stops recording and returns what was recorded.
pub(crate) fn end() -> Vec<Write>;
/// Runs `body` with recording paused, for app code that writes on purpose.
pub(crate) fn paused<R>(body: impl FnOnce() -> R) -> R;
pub(crate) fn record(write: Write);
```

In release builds each function is an empty `#[inline(always)]` stub, so the
call sites need no `cfg`.

### Recording through must_write

`must_write` gains the node and records when it returns true:

```rust
#[track_caller]
fn must_write<T: ?Sized + PartialEq>(
    node: NodeId,
    stale: bool,
    written: &T,
    declared: &T,
) -> bool;
```

`#[track_caller]` gives the call site as `what`, so the panic names the
property. `enabled`, `style` and `padding` move onto `must_write` with
`stale` false. The other writes that compare on their own call
`write_check::record(node)` where they decide to write. `apply_visual`
records one write per child edit under its parent node.

`Response::invalidate` keeps its resets of `enabled`, `style` and
`padding`. The widget body takes `stale` before the options run, so the
options never see it.

### Setter hook

`write_check::install(mtm)` runs once in `run` in debug builds. For a fixed
table of AppKit setters, it replaces each implementation with a trampoline
that calls `record` and then the original IMP. There is one trampoline per
argument type: object, `bool`, `f64`, `NSInteger`, `NSSize`, `NSEdgeInsets`.

```rust
struct Hook {
    class: &'static CStr,
    selector: &'static CStr,
    argument: Argument,
}
enum Argument { Object, Bool, Float, Integer, Size, EdgeInsets }
```

Each selector is hooked only on the highest class that defines it, and
`install` asserts this. If a subclass and its superclass were both hooked,
the subclass method's call to `super` would reach the superclass's
trampoline, which finds the receiver's nearest hook again and loops. The
first table covers each selector this crate sends on the update path:
`setStringValue:`, `setTitle:`, `setPlaceholderString:`, `setEnabled:`,
`setFont:`, `setHidden:`, `setOrientation:`, `setSpacing:`, `setEdgeInsets:`,
`setConstant:`, `setActive:`, `setPriority:`, `setKeyEquivalent:` and
`setContentHuggingPriority:forOrientation:`. A hook counts only while the
recorder is on, so AppKit's own calls outside a check pass are ignored. An
AppKit call inside a check pass happens only because our code sent
something, which is itself a write.

The hook records only sends to the classes this crate creates, plus any
`NSLayoutConstraint`. AppKit also sends hooked setters to its private views,
such as `NSTextIndicatorOverlay`, the caret overlay of a focused secure
field. The hook reads its thread-local with `try_with`, because AppKit
still sends hooked setters while the main thread exits.

When the check fails, the runner pairs each hook record with a node by
looking for a control whose view, or a view the control holds, has that
address. When none matches, it prints the receiver's class.

### Exemptions

`native`'s `update` closure runs inside `write_check::paused`, because it
writes by contract. Cached bodies need no exemption, because an unchanged
`cached` skips its body.

### Failure output

```
idle pass wrote 2 times:
  label key=Key(…/"status") src/widget/label.rs:34
  field key=Key(…/"password") setStringValue: on NSTextField
```

Then the runner panics with that text, through the normal panic path.

## Tests

1. Recorder unit test: `begin`, `record`, `paused` and `end` return exactly
   the writes recorded while on and not paused.
2. Hook spike: replace `-[NSTextField setStringValue:]`, set a value, and
   check that the value shows and that the count rose by one. Run with
   `cargo run --example write_hook`.
3. Deliberate break: undo `1c4f3c6` locally, run `login`, and check that the
   first check pass panics with the field header, through the setter hook.
4. Smoke: `tools/smoke.sh` builds debug examples, so every example runs with
   the check on. It adds "no idle pass wrote" checks, which grep each
   example's log for `idle pass wrote`, and keeps its 36 checks passing.

## Steps

1. Hook spike (test 2). Done (2026-11-21): `examples/write_hook.rs` passes. The original runs, a subclass is counted, and a sibling class with the same selector is not. AppKit itself calls `setStringValue:` inside `labelWithString`, so the spike counts deltas. Gate: if the original IMP cannot be called, or AppKit
   misbehaves, drop the setter hook. The plan then relies on `must_write`
   alone, and the decision goes back to the user.
2. `src/write_check.rs` with the recorder and release stubs, and test 1. Done (2026-11-21): 4 recorder tests pass. The release stubs are `cfg!(debug_assertions)` branches.
3. `must_write` with the node and `#[track_caller]`. Move the self-comparing
   writes onto it. Simplify `invalidate`. Record child edits in
   `apply_visual`. Clippy, unit tests and smoke. Done (2026-11-21), but
   `invalidate` is unchanged (see "Recording through must_write").
4. The check pass in `Runner::turn`, failure output, and `paused` around
   `native`'s update. Done (2026-11-21): the check runs in `Runner::pass`.
   `size_to_fit`'s layout also runs paused.
5. Setter hook table and node lookup. Run test 3. Done (2026-11-21). With the `required()` bug put back, `login` panics on its first idle pass, and the report names both fields twice: once at `field.rs:49`, once as `setStringValue:`. A raw `setStringValue` added to `label` on every pass is caught by the hook alone. The first hook table looped until each selector was hooked once (see "Setter hook").
6. Smoke checks (test 4), fmt, clippy and rustdoc. Mark this plan done. Done (2026-11-21): `tools/smoke.sh` passes 41 of 41 checks, which adds 5 idle checks. The first run failed on the secure field's caret overlay and on a thread-local read during exit; both are fixed. fmt, clippy, rustdoc, 28 unit tests and a release build are clean.

## Settled

- **Record at `must_write`** (2026-11-21). One recording point covers the
  update branches, and self-comparing writes move onto it.
- **Catch bypasses with a debug setter hook** (2026-11-21). A runtime hook
  sees writes that skip `must_write`, with no text heuristic.
- **Check after each settled pass** (2026-11-21). The catch-up pass after an
  event may write, so the check runs one pass after it.
- **Scope: the check plus a smoke run of every example** (2026-11-21).
- **Accept shifted pass numbers in debug builds** (2026-11-21). The
  user approved the plan with this as the recommended choice. A panic in a
  check pass takes the normal panic path, which smoke run 2 checks.
- **The recorder is a thread-local** (2026-11-21). Approved with the plan.
  Writes happen under `&mut` borrows of the tree.
