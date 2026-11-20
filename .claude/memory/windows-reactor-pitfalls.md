---
name: windows-reactor-pitfalls
description: Performance and design pitfalls in windows-reactor 0.100, with the measured cost and the fix for each
metadata:
  type: project
---

Measured in the spike in `src/main.rs` on 2026-11-20 with a counting global
allocator, a release build and 1,000 keyed rows. Each count covers one root
render plus the background task that triggered it.

| Root re-render where… | Rows re-rendered | Allocations | Bytes |
|---|---|---|---|
| no row input changes | 0 | 4,091 | 1.15 MB |
| one row changes | 1–2 | 4,176 | 1.16 MB |
| every row changes | 1,000 | 42,214 | 6.0 MB |
| first mount | 1,000 | 61,353 | 15.5 MB |

A skipped row costs about 4 allocations, and they all come from building its
input in the parent's `view`. A re-rendered row costs about 42 allocations and
6 KB.

**Why:** these traps follow from how Reactor decides what to skip, see
[[windows-reactor-internals]].

**How to apply:**

- **Capturing callbacks re-render every row.** `context.message(Msg::X(id))`
  wraps a closure that captures the message, so it gets a new identity on each
  render, and a row `Input` holding it is never equal. Measured: 2 rows went
  2, 4, 6, 8 renders over 4 root renders. Pass one
  `Callback<u64>` from `context.callback(Msg::X)` (an enum variant used as a
  function captures nothing) and have the row call it with its id. Measured: it
  stayed at 2.
- **Keep row inputs cheap to build and compare.** The parent builds every
  row's input on each render, even for rows that are skipped. Prefer `Rc<str>`
  over `String` for text that is shared.
- **Any message re-renders its component**, because `update` cannot report
  "nothing changed". Send messages to the smallest component that owns the
  state.
- **Virtual lists reset on any key change.** Use
  `ItemsRepeater::new().virtual_source(VirtualSource::new(key_revision, len,
  key_fn, view_fn))` inside a `ScrollViewer` in a `Grid` row with
  `GridLength::Star`, so the list gets a finite height. Bump `key_revision`
  whenever the visible list changes. A changed revision makes Reactor read
  every key (O(len)). Equal keys keep the rows, different keys reset the
  list, and WinUI then builds only the rows in view (48 in the spike) in a
  later dispatch. Measured on 2026-11-20, release build, time from `update` to
  the end of publication, without layout or row realization:
  5,000 entries 1–8 ms, 50,000 entries 5–25 ms. About one allocation per
  entry, which came from `to_lowercase` in the spike's filter. Store the
  lowercase names once instead. The fallback of a keyed `StackPanel` is not
  needed.
- **Every input must be controlled.** A change event records the user's value
  as the native value, and an unset property in the next view counts as a
  difference, so Reactor sends `ClearProperty` and the box clears itself
  (`plan_native_properties`, `core/pump/planner/element.rs:42-62`). A
  `PasswordBox` or `TextBox` without `.password(..)` / `.text(..)` flashes the
  typed text and then empties. Always pass the value from state.
- **Passwords cannot stay out of Rust memory.** Because the box must be
  controlled, the password lives in a `String` in state. Measured on
  2026-11-20 with `src/bin/password_probe.rs` after typing an 11-character
  marker: 5–6 full UTF-8 copies in the process (state, props, the tree's
  last-value map and its per-dispatch clones, event payloads, plus freed
  buffers that are never wiped) and 2 UTF-16 copies inside WinUI. Each
  keystroke also leaves a freed copy of the shorter prefix. Wiping state on
  lock does not remove the others.
- **Fix that works: a wiping global allocator plus clearing on submit.** A
  `#[global_allocator]` that zeroes each block in `dealloc` (and moves by hand
  in `realloc`, so the old block is wiped too) removes every freed copy. On
  submit, `mem::take` the password out of state, so the next view sets the
  box to `""` and the live copies in props and the tree drop. Measured with
  the probe, 11-character marker, same session:

  | | while typed | after submit |
  |---|---|---|
  | no wiping | 16 UTF-8 | 8 UTF-8 |
  | wiping allocator | 3 UTF-8 | 0 UTF-8 |

  The 3 live copies while typing are state, the props' `desired` and the
  tree's last-value map. WinUI keeps 1–2 UTF-16 copies either way, outside our
  reach.

  Cost, measured on 2026-11-20 with the list spike (`src/main.rs`,
  `SPIKE_WIPE=1`, 50,000 entries, release build, 3 alternating runs each):
  mount 146.9 ms without wiping and 146.6 ms with it, and the 8-step filter
  sequence 250.0 ms vs 252.6 ms. The difference is within run-to-run noise
  (±10 ms), so the wiping allocator is effectively free for this app. Freeing
  very large buffers costs one `memset` of their size, which was not tested.
- **Popup window with only the close button works without a fork.** Reactor
  has no presenter options and no `HWND` access, but in a `use_effect` after
  the first publication the app can find its window with `EnumThreadWindows`
  on the current thread (class `WinUIDesktopWin32WindowClass`), remove
  `WS_MINIMIZEBOX | WS_MAXIMIZEBOX | WS_THICKFRAME`, and call `SetWindowPos`
  with `SWP_FRAMECHANGED`. Add `WS_EX_TOOLWINDOW` and `HWND_TOPMOST` for no
  taskbar button and always on top. Put a `TitleBar` element in the view to
  extend content into the caption. Checked on 2026-11-20 with
  `src/bin/popup.rs`: only the X shows, the edge does not resize, the style
  survives a re-render, and the X closes the app.
- **Drawing in the title bar area.** The `TitleBar` row is what looks like top
  padding. To draw under it, give the background element `.grid_row(0)
  .grid_row_span(2)` and list the `TitleBar` after it, so it sits on top.
  Interactive controls go in its slots: `.slot(TitleBarSlot::Content, ..)` or
  `TitleBarSlot::RightHeader`. Checked with computer use: the band reaches
  the top edge, a `TextBox` in the Content slot takes focus, and dragging
  the empty title bar area still moves the window. Controls under the
  `TitleBar` row, outside its slots, do not get clicks there.
- **To drive the app with computer use**, request access with the full exe
  path (`...\target\release\<name>.exe`). The bare name or window title is not
  found. `SendKeys` from PowerShell did not reach the controls.
- **A `TextBox` echoes text that the state sets.** When `view` sets new text
  (for example, clearing the filter), WinUI later raises `TextChanged` with the
  same text. Reactor's echo filter misses it, so it arrives as a normal
  message and caused a second full filter pass in the spike. Ignore a text
  message equal to the current state:
  `Msg::Filter(text) if text == self.filter => {}`.
- **`content(..)` returns a `View`,** so call layout setters such as
  `grid_row` before it.
- **Only the UI thread runs `update` and `view`.** Anything slow goes through
  `spawn_background`, whose work must be `Send + 'static`.
- **Shared clients cannot go in `Input`.** It needs `Clone + PartialEq`. Let the
  root own the client as a `&'static` from `Box::leak` and let rows send
  messages up to it, which avoids `Arc`.
- **State resets when a component's path changes:** a different type at the
  same place (`if`/`else` between components), a changed key, a positional
  child that shifts index, a move to another parent, or a wrapper added
  above it. Use keyed children for dynamic lists, and keep screens that must
  keep state mounted or hold that state in the root.
- **The top-level view must yield exactly one control.** A root `Fragment`
  with two controls fails the mount with `StructureUnsupported`.
- **Removing many rows deep-clones each removed node once** before dropping
  it (`Rc::unwrap_or_clone` in `arena.rs:102`).
- **Queues have size limits:** 4096 local messages, 4096 background messages and
  64 background tasks running at once. When one is full, `send` and
  `Callback::call` return `false` and the message is lost.
- **A native apply error stops the window for good.** Commands run one by one
  with no rollback, so after a failure WinUI holds a half-applied plan and
  the pump marks itself poisoned (`core/pump/publish.rs:4,28`). Typical causes
  are a WinUI call that returns an HRESULT (`RuntimeError::Native`), such as a
  bad style resource key, or a bug in Reactor's own planning.
- **A panic in an event callback aborts the process** (`invoke_callback`,
  `native/winui/mod.rs:4166`). Keep callbacks to sending messages.
- **`catch_unwind` wraps every `view`,** so a panic in `view` is caught, then
  raised again by the pump.
