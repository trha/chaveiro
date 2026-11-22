# immediate-tree: the core shared by winui-immediate and appkit-immediate

Status: spikes 1 to 4 done on 2026-11-22. See "Spike results" for where the
code differs from the draft above.

## Goal

One crate, `immediate-tree`, holds the code that does not touch a platform: the
tree, the frame stack, cache, state, task, stream and flow, with their tests
and loom tests. winui-immediate and appkit-immediate keep their widgets,
events, runner, and the code that edits native children. A port from WinUI
to AppKit then covers only widgets.

## What each platform gives the core

```rust
// immediate_tree::platform — for platform crates, not for apps.

/// The platform's `Ui`. The core's free functions take `&mut S` and hand
/// the same `&mut S` to user closures, so a closure sees the platform's
/// widgets.
pub trait Shell: Sized + 'static {
    /// The platform's own payloads: windows, controls, menus. A flat enum.
    type Native;
    /// `windows_core::Error` on WinUI, `!` on AppKit.
    type Error;

    fn core(&mut self) -> &mut Core<Self::Native>;
    fn core_ref(&self) -> &Core<Self::Native>;

    /// Makes `declared` the native children of `node`, and leaves the
    /// previous list in `declared`. The core calls it only for `Native`
    /// nodes, because it handles cache and flow nodes itself.
    fn apply_visual(
        &mut self,
        node: NodeId,
        declared: &mut Vec<NodeId>,
    ) -> Result<(), Self::Error>;

    /// Releases what a dropped native node holds outside the tree, such as
    /// event handlers. Called after the tree let go of the node.
    fn release(&mut self, native: Self::Native);
}

pub struct Core<N> { /* tree, frames, task wakes, pass request, drop buffer */ }

pub enum Node<N> {
    Root,
    Cache(CacheNode),
    State(Box<dyn Any>),
    Task(TaskNode),
    Stream(StreamNode),
    Flow(Box<FlowNode>),
    Native(N),
}

impl<N> Core<N> {
    /// `post` asks the UI thread for a turn from any thread, and returns
    /// false once the thread can no longer run one.
    pub fn new(post: impl 'static + Send + Sync + Fn() -> bool) -> Self;

    pub fn tree(&self) -> &Tree<Node<N>>;
    pub fn tree_mut(&mut self) -> &mut Tree<Node<N>>;

    /// The open containers, innermost first.
    pub fn frames(&self) -> impl Iterator<Item = NodeId>;

    pub fn visit(&mut self, id: impl Hash, kind: ChildKind, create: impl FnOnce() -> N)
        -> (NodeId, Visit);
    /// `visit` for a native create that can fail. Calls `create` only when
    /// the key is new.
    pub fn try_visit<E>(&mut self, id: impl Hash, kind: ChildKind,
        create: impl FnOnce() -> Result<N, E>) -> Result<(NodeId, Visit), E>;

    pub fn open(&mut self, node: NodeId);
    pub fn open_scoped(&mut self, node: NodeId, scope: impl Hash);

    pub fn begin_pass(&mut self);
    pub fn request_pass(&mut self);
    pub fn take_pass_request(&mut self) -> bool;
    /// Polls every task and stream woken since the last turn.
    pub fn poll_woken_tasks(&mut self);
}

/// Closes the innermost container. Native nodes go through
/// `Shell::apply_visual`; dropped native nodes go through `Shell::release`.
pub fn close<S: Shell>(shell: &mut S) -> Result<(), S::Error>;
/// Closes every frame above `depth`, keeping the first error.
pub fn unwind<S: Shell>(shell: &mut S, depth: usize);
/// Drops every node, as a pass that declares nothing would.
pub fn close_all<S: Shell>(shell: &mut S);
```

`Tree`, `NodeId`, `Key`, `Visit` and `ChildKind` move as they are and become
`pub` inside `immediate_tree::platform`.

Two borrow points shape this:

- `release` needs `&mut S`, but the tree is borrowed while it drops nodes.
  `Tree::close` pushes dropped native payloads into a buffer that `Core`
  reuses, and `close` drains it into `release` once the borrow ends.
- `apply_visual` never sees `Cache` or `Flow`, so the platform `match`
  covers only its own variants and loses its four `unreachable!` arms.

## What apps call

Free functions generic over `S: Shell`. Each platform wraps them in one-line
inherent methods on its `Ui`, so app code does not change.

```rust
// immediate_tree

pub fn state<S: Shell, T: 'static>(shell: &mut S, id: impl Hash,
    init: impl FnOnce() -> T) -> &mut T;

pub fn cache<S: Shell, D: 'static + PartialEq>(shell: &mut S, id: impl Hash, deps: D,
    body: impl FnOnce(&mut S) -> Result<(), S::Error>) -> Result<(), S::Error>;

pub fn task<S: Shell, F>(shell: &mut S, id: impl Hash, start: impl FnOnce() -> F)
    -> TaskResponse<'_, F::Output>
where F: 'static + Future, F::Output: 'static;

pub fn stream<S: Shell, T>(shell: &mut S, id: impl Hash, start: impl FnOnce() -> T)
    -> StreamResponse<'_, T::Item>
where T: 'static + Stream, T::Item: 'static;

pub fn flow<'s, S: Shell, L, F>(shell: &mut S, id: impl Hash, state: &mut L,
    start: impl FnOnce(Flow<L::Family, S>) -> F)
    -> Result<FlowResponse<'_, F::Output>, S::Error>
where L: Lent<'s>, F: 'static + Future, F::Output: 'static;

/// The future's handle. `S` is the platform `Ui` that frames draw into.
pub struct Flow<L: Lend, S: Shell> { .. }

impl<L: Lend, S: Shell> Flow<L, S> {
    pub fn frame<R, D>(self, draw: D) -> Frame<L, S, R, D>
    where D: FnMut(&mut S, &mut L::Of<'_>) -> Result<Option<R>, S::Error>;
}

pub trait Lend: 'static { .. }
pub trait Lent<'s>: 's { .. }
pub struct Owned<T>(T);
pub struct OwnedFamily<T>(..);
macro_rules! lend { .. }   // paths through $crate
```

The responses borrow only their own node, so they carry no platform type:

```rust
pub struct TaskResponse<'u, T> { node: &'u mut TaskNode, .. }
pub struct StreamResponse<'u, T> { node: &'u mut StreamNode, .. }
pub struct FlowResponse<'u, T> { node: &'u mut FlowNode, .. }
```

The lending slot stores `*mut S` cast to `*mut ()`, because the thread-local
cannot be generic. `Frame` knows `S` from its closure type and casts back.
A `TypeId` in the lending catches a mismatch in debug builds.

## The platform side

```rust
// winui-immediate
pub struct Ui {
    core: Core<Payload>,       // Payload loses Root, Cache, State, Task, Stream, Flow
    events: Events,
    scratch: String,
    lists: Vec<NodeId>,
    // ...
}

impl Shell for Ui {
    type Native = Payload;
    type Error = windows_core::Error;
    // ...
}

/// Wraps the core handle, so app code never names `immediate_tree`.
pub struct Flow<L: Lend>(immediate_tree::Flow<L, Ui>);
pub struct TaskResponse<'u, T>(immediate_tree::TaskResponse<'u, T>);
// StreamResponse, FlowResponse: same shape, methods forward.

#[doc(inline)]
pub use immediate_tree::{Lend, Lent, Owned, OwnedFamily, lend};

impl Ui {
    /// The full doc lives here, because apps read this crate's docs.
    pub fn task<F>(&mut self, id: impl Hash, start: impl FnOnce() -> F)
        -> TaskResponse<'_, F::Output>
    where F: 'static + Future, F::Output: 'static
    {
        TaskResponse(immediate_tree::task(self, id, start))
    }
    // state, stream, cache, flow, request_pass: same shape.
}
```

Apps depend on the platform crate only. Every public type and method is the
platform's own, or a `#[doc(inline)]` re-export of a trait or macro. The
`lend!` macro expands through `$crate`, so apps need no `immediate-tree`
dependency.

appkit-immediate sets `Error = !` and exposes signatures without `Result`.
`let Ok(x) =` on a `Result<T, !>` compiles on rustc 1.100 (spiked on rmac):

```rust
pub fn cache<D: 'static + PartialEq>(&mut self, id: impl Hash, deps: D,
    body: impl FnOnce(&mut Ui)) {
    let Ok(()) = immediate_tree::cache(self, id, deps, |ui| Ok(body(ui)));
}

impl<L: Lend> Flow<L> {
    pub fn frame<R>(self, mut draw: impl FnMut(&mut Ui, &mut L::Of<'_>) -> Option<R>)
        -> impl Future<Output = R> {
        self.0.frame(move |ui, state| Ok(draw(ui, state)))
    }
}
```

The mac checkout (rmac `~/ws/chaveiro`, commit `3f9a067`) already removed
the mailbox and ported task and stream, with the same `TaskWakes::new(post)`.
So both platforms pass one `post` to `Core::new`. The Windows checkout of
appkit-immediate is stale; port from the mac copy.

## Exhaustive matches

- Core matches `Node<N>` exhaustively, with one `Native(_)` arm.
- The platform matches `N` exhaustively in `apply_visual` and `release`,
  because the core never hands it a core variant.
- `Node` is not `#[non_exhaustive]`, so platform code can still match every
  variant when it needs to.

What changes: no single `match` lists both core and platform variants. Today
`release` and `apply_visual` list every variant, so a new core variant fails
to compile there. After the split, the core's own `close` match takes that
role. Platform code that reads payloads by kind, such as `enclosing_window`,
already uses `matches!` or `_`, so it loses nothing.

## Costs

- About six wrapper methods and four wrapper types per platform. Their docs
  are the public docs, so each platform keeps its own copy of the doc text.
  The core functions keep short docs for platform authors.
- Platform code writes `Node::Native(Payload::Control(..))` where it now
  writes `Payload::Control(..)`. A helper `fn native(&self, node) -> &Payload`
  hides most of these.
- One `unsafe` cast in `Frame::poll` gains a type parameter.

## A DOM port

A later `dom-immediate` over `web-sys` fits the same `Shell`:

| Shell piece | DOM |
|---|---|
| `Native` | an enum over element kinds, with their listener `Closure`s |
| `apply_visual` | the same `edits()` diff, as `insertBefore` / `removeChild` |
| `release` | drop the listener `Closure`s |
| `Error` | `JsValue` |
| `post` | schedule a pass with `requestAnimationFrame` |

Platform work, none of it in the core:

- `post`: std's `Waker` requires `Send + Sync`, so `TaskWake` and its
  `post` keep the bound even on a target with one thread. On wasm, `post`
  is a plain `fn` that reads the JS callback from a `thread_local`, which
  meets the bound with no `unsafe`.
- Synchronous re-entry: removing a focused element fires `blur` and
  `focusout`, and `focus()` fires `focus`, inside the call. Events queue as
  they do on WinUI inside `SetItemsSource`.
- Text inputs: setting `value` moves the caret, as appkit's `write_check`
  handles.
- Lists: no toolkit asks for rows during layout, so the list computes its
  visible rows from the scroll position, and `Tree::adopt` goes unused.
- Windows: one document, so `Window` maps to the page root or to dialogs.
- Every DOM call crosses into JS, and strings are copied when they cross.

## Spikes

1. Move `Tree` and cache with state into `immediate-tree`, behind `Shell`
   and `Node<N>`. Build both platform crates and run the tree and cache
   tests.
2. Move task, stream and flow, with their tests and loom tests.
3. `cargo check --target wasm32-unknown-unknown -p immediate-tree`. A clean
   build shows the core has no platform code left.
4. A minimal `dom-immediate`: `Shell` over `web-sys`, a stack, a label, a
   button and a text input, with one task, run in a browser. It shows
   whether the re-entry and caret handling fit the platform side alone.

## Spike results

All four spikes build and pass their tests.

- Windows: `immediate-tree` (36 tests, 2 loom tests), `winui-immediate` (19
  tests, 2 `compile_fail` docs), `chaveiro-winui`. Four winui examples start
  and keep running.
- Mac (rmac `~/ws/chaveiro`, a new jj change on top of the working copy):
  `appkit-immediate`, ported from the mac copy, and `chaveiro-appkit` build
  with all targets. Tests and loom tests pass. Four examples run with the
  idle pass check on.
- `cargo check --target wasm32-unknown-unknown -p immediate-tree` is clean.
- `dom-immediate/spike/run.ps1` runs the DOM spike in headless Edge. Clicks,
  typing, a kept caret, a blur, a task on `setTimeout` and a two-step flow
  all show in the page. Removing the focused input fires `blur` inside the
  pass, and the queue absorbs it.

Where the code differs from the draft:

- `state`, `task`, `stream` and `poll_woken_tasks` are methods of `Core`,
  because they run no user code that needs the shell. Only `cache`, `flow`,
  `close`, `unwind` and `close_all` take the shell.
- `poll_woken_tasks` returns whether any node was woken; the AppKit runner
  uses it.
- A platform's `Shell::Native` is a public newtype, `NativeNode(Payload)`,
  with a crate-private field. `impl Shell for Ui` is public, so a
  crate-private `Payload` there is error E0446, and making `Payload` public
  would drag every node type with it.
- `Core::declared_visual(depth)` gives the AppKit toolbar the window's
  children while the toolbar is open.
- On AppKit a flow frame that completes sets the core's pass request, and
  `Ui::end` turns it into `request_pass`, because the AppKit runner has its
  own request flag.
- The flow lending records the `TypeId` of its family and shell, and a
  frame asserts them before its unsafe casts. Without it, a `Flow` handle
  carried into another flow's future read that flow's state as the wrong
  type. The test `a_frame_carried_into_a_flow_of_another_family_panics`
  fails when the assert is removed.
- `immediate-tree` turns off `ahash`'s runtime randomness, which pulled
  `getrandom` into the wasm build. Keys use fixed seeds anyway.
- The DOM spike uses `Error = !` with `expect` on DOM calls that cannot
  fail, not `JsValue`.