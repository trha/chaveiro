---
name: windows-reactor-internals
description: How windows-reactor 0.100 runs a render, crosses into WinUI's event loop, and decides what to skip
metadata:
  type: reference
---

Read from the source of `windows-reactor` 0.100.0 (crates.io) on 2026-11-20.
Master was at 0.101 then, so check that paths and lines still match after an
upgrade. Paths are under `src/` of the crate.

## Startup

- `App::run` / `App::run_component` create one `Pump` per window, all on one
  UI thread (`app.rs:732`). `App::run_windows` makes one pump per root.
- Apps that are not packaged load the Windows App Runtime through
  `TryCreatePackageDependency`, or use a copy bundled with the app
  (`native/winui/bootstrap.rs`).
- The UI thread gets per-monitor DPI awareness and STA COM, then
  `Application::Start` gives the thread to WinUI. The first window is built in
  the `OnLaunched` shim.

## Model

- A `Component` has `create`, `input_changed`, `update` and `view`
  (`core/component.rs:983`). `Input` must be `Clone + PartialEq + 'static`.
- `update` does not receive `Input`. A child that needs its input in `update`
  must copy it in `create` and `input_changed`.
- Hooks still exist on `ViewContext`: `use_context` and `use_effect`.

## One dispatch, from message to WinUI

1. A click or text change only queues a `QueuedEvent` and asks for a wake.
   `LocalSender::send` only queues and never calls `update` inline.
2. The scheduler keeps at most one pending ticket and posts one
   `DispatcherQueueHandler` with `TryEnqueueWithPriority`
   (`native/winui/mod.rs:3262-3335`).
3. The handler runs native events (their callbacks run here and usually send
   messages), then drains up to 64 messages. Each message runs `update` and
   marks its component dirty.
4. It composes once for all the dirty components, so many messages make one
   render. It plans on a copy of the tree: an arena split into chunks of 256
   slots, each behind an `Rc` that is copied on its first write.
5. The plan is a list of commands applied to WinUI. The copy replaces the old
   tree only if the native apply succeeds. A native error stops the window for
   good.

Work left over after the 64-item limit is posted again at Low priority.

## Background work

- `spawn_background` runs on the Windows thread pool and needs
  `Send + 'static` work. Its result goes into an `Arc<Mutex<..>>` queue.
- The background waker posts at most one dispatcher handler at a time, guarded
  by an `AtomicBool` (`mod.rs:4127`). It returns `false` when the dispatcher
  refuses the post. Then every queued result becomes a rejection, which is
  delivered only for tasks started with `spawn_background_with_rejection`.
- Async WinRT results also arrive through a `Mutex` queue plus a waker.

## What gets skipped

- A component's `view` runs again when it got a message, when its new `Input`
  is `!=` the old one, or when a context value it reads changed. There is no
  `should_update` hook and no memo of the output.
- A plain element subtree is never skipped by pointer. It is walked against the
  retained tree, and walked twice when it changed.
- Props are compared as one `Rc`, then one by one against the last native
  values. `Callback` equality is by identity.
- The runtime records the property values it writes and drops the WinUI events
  that only echo them, so a controlled `TextBox` does not loop.
- Keyed children use an O(n log n) longest-increasing-subsequence diff with an
  early exit on equal keys. Applying moves is O(n × moves).
- Virtual lists (ItemsRepeater) pool their row shells, but any change to the
  key list resets the whole collection.

## Retained tree representation

- `Arena<T>` (`core/arena.rs`): `Rc<Vec<Rc<Vec<Slot>>>>`, 256 slots per chunk,
  each slot `{ generation, Option<Rc<T>> }`. `NodeId` is index + generation, so
  stale ids read `None`. A slot whose generation hits `u32::MAX` is never
  reused. `next_id()` predicts the id of the next insert.
- `Node` (`core/engine.rs:32`): `parent`, `children: Vec<NodeId>`,
  `key: Option<Key>`, `data: NodeData`. `NodeData` has one variant per kind:
  structural (Application, Window, Fragment, Slot, Provider), overlays
  (Tooltip, Flyout, Menu, ContentDialog), `Component { TypeId, ScopeId }`,
  `Native { kind, NativeState }`, `Virtual { VirtualModel, realized map }`.
- `NativeState`: `desired` props (`Rc`), the last values written to WinUI
  (`BTreeMap<PropertyId, ..>`), and event revisions (`BTreeMap`). WinUI handles
  are not in the tree. The runtime maps `NodeId` to handle.
- `Tree` (`engine.rs:225`) = the arena plus side maps that are each one
  `Rc<FxHashMap>` (components, exit transitions, attachments, window
  declarations, title bars). `ProviderStore` is chunked in three levels.
- The tree is logical: Component, Slot, Fragment and Provider nodes have no
  WinUI object. Planning returns the native roots of each subtree to join
  each control to its nearest native ancestor.
- `View` (`element.rs:434`) is rebuilt by every `view` call: `Rc<Vec<KeyedView>>`
  child lists, `Box` for single children, `Rc` props, `String` text.

## Mount

`ComponentLoop::mount` → `Pump::mount_view` (`core/pump/mod.rs:198`): start
from `Tree::new()`, add Application and Window nodes, then
`mount_planned_view` (`planner/view.rs:1803`) recurses. A Component view calls
`create` (scope `Reserved`), adds a Component node with a Slot child, calls
`view` at once and recurses into the result. Native elements push `Create`,
initial property and event commands, then `InsertChild` per child, so a
subtree is built before it joins its parent. The root must yield exactly one
native root, otherwise `StructureUnsupported`. Then the normal publish runs.

## Component identity

- State lives in `ComponentStore.scopes: ScopeArena` (plain `Vec`, generations,
  not copy on write). The tree node holds only `ScopeId`.
- Identity = path in the retained tree: position (static children get
  `Key::Position`) or key (keyed children) at each level, plus the component's
  `TypeId` at the end (`planner/view.rs:135-165`). Same type → keep the scope
  and call `apply_input`; different type → destroy and create.
- Removal drops the struct, runs effect cleanups in reverse, cancels its
  background tasks, drops its queued messages and its context subscriptions.

## Copying and commit

- `self.tree.clone()` is O(1). It runs in `compose_dirty_components`
  (`turn.rs:125`) and `process_realizations` (`native_work.rs:471`).
- Fast path (`turn.rs:223`): one dirty component whose view is one leaf
  control with no children. It diffs against the live tree, applies, then
  writes `desired` in place with `native_mut`. No tree clone.
- Copies happen on the first write to a shared part through `Rc::make_mut`
  (chunk list, chunk, node, each side map), and on node removal through
  `Rc::unwrap_or_clone`, which deep-clones a shared node just to drop it.
  Node clone copies `children` and both `BTreeMap`s. A `Virtual` node clone
  copies maps sized by the realized rows.
- After the swap the old tree drops, counts fall to 1, and the next dispatch
  pays its copies again.
- Commit order (`core/pump/publish.rs:49`): validate references → apply
  commands to WinUI → write applied values into the candidate →
  `self.tree = candidate` → publish scopes, run effects → post-publish
  commands. The swap is a plain field write on the UI thread.
- Planning failure (before apply): candidate dropped, reserved scopes removed,
  some components re-armed. Apply failure: no rollback, WinUI is half-updated,
  pump poisoned for good.
- The transaction covers only the tree. `update`, `create` and
  `input_changed` mutate component state in place during planning, and those
  changes stay when planning fails.
- In functional terms: plan is about `Tree × Views -> (Tree, [Command])`,
  apply is the effect, and the commit writes a single-threaded mutable ref
  after the effect succeeds. It is persistent while shared and mutates in
  place once unique, like Clojure transients.

See [[windows-reactor-pitfalls]] for what this means when writing app code.
