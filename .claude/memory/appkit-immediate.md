---
name: appkit-immediate
description: Measured AppKit facts behind the appkit-immediate crate (pools, caret, stack alignment, table reuse, hugging) and how its smoke script works
metadata:
  type: project
---

`appkit-immediate` is the macOS counterpart of [[widgets-in-winui-immediate]],
planned in `plans/appkit-immediate.md` (all 9 steps done 2026-11-21). The
two crates share no code and may drift; the app crate is `chaveiro-appkit`
(login screen built 2026-11-21). Widget and option names follow AppKit classes.

Measured on macOS 27, each one easy to get wrong again:

- Before `NSApplication::run` there is no autorelease pool: the POC leaked
  12,270 objects. About 140 missing-pool lines remain from AppKit's own
  threads, which no app code can fix, so only check the main thread.
- `setStringValue` on a field being edited keeps the caret and selection.
  A caret check must move the caret inside the text and type after the
  app's write, or it passes with any write path.
- `NSStackView` has one alignment for all children; set it to
  `NotAnAttribute` and give each child its own cross-axis constraints.
  Alignment `Width` does not stretch children to the stack's width.
- Labels hug vertically at 750, above the window's 500, so a star grid row
  must lower its child's hugging or the window shrinks to the text.
- `makeViewWithIdentifier`'s reuse queue empties on every `reloadData`; the
  slot pool fed by `didRemoveRowView:` stays at one screen of rows.
- AppKit asks a table for its row count and rows while a pass runs (data
  source set, a window's first fit), so those calls must not borrow `Ui`.

**Why:** each of these cost a failed run or a wrong design in the plan.

**How to apply:** `appkit-immediate/tools/smoke.sh` checks all of them
through System Events. It needs Accessibility access for the shell's app,
which macOS can revoke mid-session (error -1728); Accessibility frames are
2pt taller than the alignment rects constraints size, and AX positions of
rows off screen are wrong.

## Idle pass check (plans/idle-pass-check.md)

- Debug builds rerun `view` after each pass that took no events and no
  messages; that rerun must record zero writes (`src/write_check.rs`).
- Setter hooks: hook a selector only on the highest class that defines
  it. Hooking a subclass and its superclass both loops forever, because
  the subclass's `super` call reaches a trampoline that looks up the
  receiver's class again. Stack overflow was the symptom.
- AppKit sends hooked setters to private views (`NSTextIndicatorOverlay`,
  the caret overlay of a focused secure field), so the hook counts only
  classes the crate creates.
- AppKit still calls setters while the main thread exits, after
  thread-locals are destroyed: use `try_with` in any hook.
- `labelWithString` calls `setStringValue:` itself.

## Learned while building chaveiro-appkit (2026-11-21)

- A stack view with `Align::End` (or Start or Center) had an ambiguous
  width and grew to fill its parent. Raising the stack's own hugging made
  it worse, because the buttons inside stretched. `cross_constraints` now
  adds a size-0 constraint at priority 1 for any child that is not stretched.
- `Response::padding` called after a stack's body used to mark the children
  stale, so the idle rerun rebuilt their constraints and the write check
  panicked. It now rebuilds them at once through `Ui::place_across`.
- From the Claude Code session, System Events sees 0 windows and
  `screencapture` fails, so `tools/smoke.sh` cannot run there. To check
  layout, render the content view in the app with
  `cacheDisplayInRect:toBitmapImageRep:` from `App::update`, never from
  `view`, where the layout writes trip the idle check.
