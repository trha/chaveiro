# winui-immediate

Status: Done

Parent plans: none. This plan changes `plans/sans-io-vault-core.md`; see
"Changes to other plans". After approval, step 1 copies this file to
`plans/winui-immediate.md`, which then becomes the plan of record.

## Goal

A library crate at `./winui-immediate` that gives Rust apps an
immediate-mode UI over the WinUI 3 retained tree. App code is one `view`
function over `&mut App`. Each build diffs the declared tree against a cache
of live controls and issues only the WinUI calls that changed something. The
crate supersedes the spike in `src/bin/immediate/` (moved to `spikes/` by
WP-00) and, later, Windows Reactor
as the vault app's shell.

The crate meets four requirements from the user:

1. Tight control over allocations: a steady-state build that changes nothing
   performs zero Rust heap allocations, and every other allocation has a named
   cause.
2. Few registered callbacks: one delegate object per event kind for the whole
   window, registered on a control only when the app first asks for that
   event.
3. Imperative access to the underlying `UIElement` and typed control, plus a
   widget for arbitrary native controls.
4. Explicit cache nodes: `cached` skips a subtree whose inputs are unchanged,
   and `state` holds UI-only data that is not the app's concern.

## Out of scope

The vault shell itself (screens, login, sync) stays in
`plans/sans-io-vault-core.md` WP-05. An AppKit backend, a backend trait,
variable-width text measurement, theming and animation are not part of this
plan.

## Facts this plan rests on

**Spike, `src/bin/immediate/` (built and measured 2026-11-20; details in
`.claude/memory/immediate-mode-spike.md`).** Bindings come from
`tools/winui-bindgen` (windows-bindgen 0.100, `--minimal`) over the installed
`Microsoft.WindowsAppRuntime.2_2.4.0.0_x64` metadata; minimal mode emits only
the members `filter.txt` names, and inherited members need `.cast()`.
Startup (`winui.rs`) follows Reactor: `TryCreatePackageDependency` +
`AddPackageDependency`, STA, `Application::compose`, and
`XamlControlsResources` merged in `OnLaunched` (doing it in the `Start`
callback fails with `E_UNEXPECTED`).

Measured in release with 1,000 entries: a build that changes nothing takes
5 µs with 0 allocations and 2 WinUI calls; a build where every visible name
changes takes 11 µs with 25 calls. Creating a control costs about 45 µs and
dropping a row about 500 µs, so reuse matters more than the diff. With the
`ItemsRepeater` list, builds take p50 47 µs plus 18 µs per realized row, and
a whole scroll test creates 252 controls against 1,924 without reuse.

Behaviors proven by UI Automation tests: the caret survives rebuilds every
10 ms ("entry 12", Left, "3" gives "entry 132"); Vietnamese Telex composition
survives them (12 of 12 runs); scrolling shows consecutive rows covering the
viewport; Reveal and Unlock work only after adding a second build pass when a
build consumed events (one-build lag).

**Spike mechanisms this plan replaces.** Prune walks every node each build
(`Ui::prune`). Each control gets its own closure delegate capturing its key
(`Ui::button`, `Ui::text_box`). The repeater path reaches the app through
`REPEATER_HOOKS` and a `RefCell` runner in `main.rs`. Rows rebuild every
build even when nothing changed.

**WinUI.** `ButtonBase.Click`, `TextBox.TextChanged`,
`PasswordBox.PasswordChanged` and `ScrollViewer.ViewChanged` do not bubble,
so each needs a registration on its source; `KeyDown` bubbles. A delegate
receives `sender`, so one delegate object can serve many controls. An
`ItemsRepeater` calls `IElementFactory::GetElement` during layout, and may
call it inside `SetItemsSource`.

**Threat model (`.claude/memory/threat-model.md`).** What matters is what
outlives lock. The password stays in the `PasswordBox` and is read once on
submit, then cleared.

## Architecture

```text
 UI thread
 ┌──────────────────────────────────────────────────────────────────┐
 │ Runner<A: App>                                                   │
 │   DispatcherQueue callback (coalesced)                           │
 │     pass 1: begin → app.view(ui) → rows → end                    │
 │     pass 2 if pass 1 consumed events                             │
 │     apply pending ItemsSource changes (no borrows held)          │
 │   GetElement / RecycleElement (during WinUI layout)              │
 │     ui.realize → list's RowFn(app, ui, index)                    │
 │   Handle::send(msg) from any thread → app.update(msg) → passes   │
 ├──────────────────────────────────────────────────────────────────┤
 │ Ui:  pass state, frames, widgets          (pass.rs, widget/*)    │
 │ Tree<Payload>: slab, keys, stamps, drops  (tree.rs, pure)        │
 │ children::edits: ordered child diff       (children.rs, pure)    │
 │ Events: shared delegates → inbox          (events.rs)            │
 └──────────────────────────────────────────────────────────────────┘
```

The crate owns the run loop and the app value, so it can call back into the
app from WinUI's layout pass without the app building a `RefCell` runner.
The pure parts (`tree.rs`, `children.rs`) hold all bookkeeping and have no
WinUI types, so they get property tests. WinUI calls live only in
`pass.rs`, `widget/*`, `events.rs` and `runner.rs`.

### The app interface

```rust
pub trait App: 'static {
    /// A result sent to the UI thread from another thread.
    type Message: 'static + Send;

    /// Applies one message. Runs on the UI thread before the next pass.
    fn update(&mut self, message: Self::Message);

    /// Builds the whole window. Runs on every pass.
    fn view(&mut self, ui: &mut Ui) -> windows_core::Result<()>;
}

/// Builds row `index` of a list. Given to `Ui::list` as a plain function.
pub type RowFn<A> = fn(&mut A, &mut Ui, usize) -> windows_core::Result<()>;

/// Starts WinUI on this thread and runs passes until a pass declares no
/// window. `start` receives the handle before the first pass.
pub fn run<A: App>(app: A, start: impl FnOnce(&mut A, Handle<A::Message>)) -> windows_core::Result<()>;
```

Rows are not part of the trait. Each list takes its own `RowFn<A>` at the
call site, next to the code that declares the list. WinUI calls
`GetElement` during layout, outside `view`, so the row builder must be
reachable without the closure scope of `view`; a `fn` pointer is, and it
cannot capture borrowed data by mistake, because a row gets its data from
`&mut A`. The list node stores it type-erased as
`Box<dyn Fn(&mut dyn Any, &mut Ui, usize) -> Result<()>>`, a closure that
downcasts the app to `A` and calls the `fn` pointer; the runner passes the
app as `&mut dyn Any`. The box is made once, when the list node is created,
and again only if a later pass passes a different `fn` pointer. `view`,
`update` and rows never run at the same time, so each borrows the app
mutably.

### Windows

Windows are widgets. `view` declares each open window with `ui.window`, and
a window the pass does not declare is closed, as any undeclared control is
dropped. The tree's root node has window nodes as its only children; each
window's body must yield exactly one visual root, which becomes
`Window.Content`.

```rust
impl Ui {
    /// Declares an open window. Created on its first pass, activated once.
    pub fn window(&mut self, id: impl Hash, title: &str,
        body: impl FnOnce(&mut Ui) -> Result<()>) -> Result<WindowResponse<'_>>;
}

impl WindowResponse<'_> {
    /// True on the pass after the user asked to close this window.
    pub fn close_requested(&mut self) -> bool;
    pub fn window(&self) -> &Window;
}
```

The user's close goes through the app. The crate subscribes the shared
`AppWindow.Closing` delegate, sets `Cancel = true`, and delivers
`EventKind::CloseRequested`. The app then stops declaring the window, or
keeps it to ask "discard changes?" first; when the window is no longer
declared, drop on detach calls `Window.Close()`. When a pass declares no
window at all, the runner leaves the message loop and `run` returns.

All windows run on the one UI thread, so they share the `Ui`, the mailbox,
the delegates and the scheduler, and one pass rebuilds every window. A
window whose subtree did not change costs one `cached` check if the app
wraps its body. WinUI 3 can put windows on separate threads, each with its
own `DispatcherQueue`, but that would need one `App` per thread; this crate
does not support it.

Per-window state the crate keeps: the `Window` and its `AppWindow`, the
`KeyDown` registration on its content (one per window), and whether
`Activate` has run. Lists and their factories are unaffected: a factory
calls back into the same runner whichever window it is in.

### Crossing into the UI thread

The crate owns the inbound half of domain crossing and nothing else. Work
leaves the UI thread however the app chooses (`windows_threading::submit`, a
thread, a WinRT async operation), and its result comes back as an
`A::Message` through a `Handle`.

```rust
/// Sends messages to the UI thread from any thread. Cheap to clone.
pub struct Handle<M> { shared: Arc<Mailbox<M>> }

struct Mailbox<M> {
    /// Messages not yet applied. The UI thread swaps this with a reused
    /// buffer, so a steady stream allocates only when it outgrows capacity.
    queue: Mutex<Vec<M>>,
    /// Set while one wake is posted and not yet run.
    wake_posted: AtomicBool,
    /// Cleared when the window closes; `send` then drops the message.
    open: AtomicBool,
    dispatcher: DispatcherQueue,
}

impl<M: 'static + Send> Handle<M> {
    /// Queues `message` and wakes the UI thread. Returns false, and drops the
    /// message, when the window has closed or the dispatcher refused the wake.
    pub fn send(&self, message: M) -> bool;
}
```

`send` pushes under the lock, then posts a wake only if `wake_posted` was
false. The wake's handler is a `DispatcherQueueHandler` that captures
nothing, so it can be created on any thread; it finds the runner through a
thread-local on the UI thread. The runner clears `wake_posted`, swaps the
queue out, calls `A::update` for each message in order, then runs the passes
once for the whole batch. A burst of 1,000 results therefore costs one wake
and one build, as in Reactor's background queue
(`core/component.rs` `TaskSpawner::spawn_inner`, `background_dispatch_waker`).

Drop and shutdown rules: when the window closes, the runner clears `open`
and drops queued messages, so a message type that holds secrets must wipe
itself on drop. The queue is unbounded; the vault core bounds its own
in-flight work by request id, and stale results are the core's to discard,
not the crate's. WinRT callbacks that already arrive on the UI thread, such
as delegates on XAML objects, use the event path, not the mailbox.

### Keys and the tree

```rust
/// Hash of the parent's key and the widget's id (ahash).
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct Key(u64);

/// Slab index plus generation, so a stale id reads nothing. Generations
/// start at 1, so `Option<NodeId>` has the same size as `NodeId`.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct NodeId { index: u32, generation: NonZeroU32 }

const _: () = assert!(size_of::<Option<NodeId>>() == size_of::<NodeId>());

pub(crate) struct Tree<P> {
    slots: Vec<Slot<P>>,
    free: Vec<u32>,
    by_key: AHashMap<Key, NodeId>,
    pass: u32,
}

struct Slot<P> { generation: NonZeroU32, node: Option<Node<P>> }

pub(crate) struct Node<P> {
    key: Key,
    /// Only `mark_event` reads it, to stamp ancestors.
    parent: Option<NodeId>,
    /// Pass in which this node was last visited or kept.
    stamp: u32,
    /// Pass in which an event targeted this node or a descendant.
    event_stamp: u32,
    /// Logical children in build order, as of the last pass that built this node.
    children: Vec<NodeId>,
    payload: P,
}
```

`parent` exists for one reader. A cache hit must still run its body when an
event targets a node inside it, or the click is lost. At the start of a pass
`mark_event` walks from each event's target up through `parent` and stamps
`event_stamp`, so `cached` answers "does my subtree have an event?" in O(1).
Without the pointer, `cached` would have to search its subtree for each
event. It costs 8 bytes per node, and nothing else uses it.

Stamps are `u32`, because only equality with the current pass matters.
Passes run on events, not frames, so at even 1,000 passes a second `u32`
lasts 49 days. When `pass` would wrap, `begin_pass` resets every live node's
stamps to 0 and restarts at 1. That is one O(nodes) walk per 4 billion
passes, and it rules out a stale stamp equal to the current pass.

A slot's generation also wraps from `u32::MAX` back to 1. The spike's arena
and Reactor's retire such a slot instead; this crate does the same, so an id
can never alias a later node.

`Tree` replaces the spike's global prune with drop on detach. When a
container closes, the tree compares its old logical children with the ones
visited this pass: any old child whose `stamp` is not the current pass is
dropped with its whole subtree. That costs O(children of the containers
built this pass), not O(all nodes). A subtree skipped by `cached` is never
walked, because its parent's child list still names its root and the root's
stamp is current. Visiting a key whose stamp is already current panics with
"duplicate id", which catches two widgets with the same id in one parent.

The tree's operations, all pure:

```rust
impl<P> Tree<P> {
    pub fn begin_pass(&mut self) -> u32;
    /// Finds or creates the child `key` under `parent` and stamps it.
    pub fn visit(&mut self, parent: Option<NodeId>, key: Key, create: impl FnOnce() -> P) -> (NodeId, Visit);
    /// Stamps a node without visiting its subtree (cache hit).
    pub fn keep(&mut self, id: NodeId);
    /// Replaces `parent`'s logical children and drops the detached subtrees,
    /// passing each dropped payload to `on_drop`.
    pub fn close(&mut self, parent: NodeId, visited: &mut Vec<NodeId>, on_drop: impl FnMut(P));
    /// Sets `event_stamp` on `id` and its ancestors.
    pub fn mark_event(&mut self, id: NodeId);
}

pub(crate) enum Visit { Created, Existing }
```

`close` swaps `visited` into the node and leaves the old list in `visited`,
cleared, for reuse; with a spare-list pool in `Ui`, a steady pass allocates
nothing.

### Payload

```rust
pub(crate) enum Payload {
    Window(WindowNode),
    Control(ControlNode),
    Cache(CacheData),
    State(Box<dyn Any>),
    List(ListData),
}

pub(crate) struct ControlNode {
    /// The control as a `UIElement`, for child collections and imperative access.
    element: UIElement,
    /// The same control as its concrete WinUI type, for its own setters.
    control: Control,
    /// Visual children applied to the WinUI collection (panels only).
    visual: Vec<NodeId>,
    /// Last text this layer wrote or read back. Capacity is reused.
    text: String,
    /// Last numbers written: [spacing or height, Canvas.Top, ...].
    numbers: [f64; 2],
    /// Event kinds subscribed on this control.
    subscribed: EventSet,
    flags: ControlFlags, // hidden, composing, invalidated
}

/// The concrete WinUI control behind a node, one variant per widget.
pub(crate) enum Control {
    Stack { panel: StackPanel, children: UIElementCollection },
    Grid { grid: Grid, children: UIElementCollection, rows: Vec<Length> },
    Label(TextBlock),
    Button(Button),
    TextBox(TextBox),
    PasswordBox(PasswordBox),
    List { scroll: ScrollViewer, repeater: ItemsRepeater },
    /// From `Ui::native`: only `element` is known.
    Native,
}

/// `Payload::Window`: a window node. Its `element` is the content root.
pub(crate) struct WindowNode {
    window: Window,
    app_window: AppWindow,
    content: Option<NodeId>,
    title: String,
    activated: bool,
}

pub(crate) struct CacheData {
    /// The dependencies of the last run; reused in place when the type matches.
    deps: Option<Box<dyn Any>>,
    /// Visual nodes this cache contributed to its parent on its last run.
    emitted: Vec<NodeId>,
}
```

Logical and visual children differ because `Cache` and `State` nodes have no
WinUI element. Each frame collects both lists. A control frame applies its
visual list to its `UIElementCollection` with `children::edits`. A cache
frame passes its visual list up to the parent frame and stores it in
`emitted`, so a cache hit re-emits the same controls without running its
body.

### Child list edits

```rust
pub(crate) enum Edit { RemoveAt(u32), InsertAt(u32, NodeId) }

/// Edits that turn `old` into `new`: keep the common prefix and suffix,
/// remove the old middle, insert the new middle. Both lists hold unique ids.
pub(crate) fn edits(old: &[NodeId], new: &[NodeId]) -> impl Iterator<Item = Edit>;
```

This is the spike's ordered strategy. Panels in this crate are ordered; the
canvas set-difference path goes away with the canvas list.

### The pass and widgets

```rust
pub struct Ui {
    tree: Tree<Payload>,
    frames: Vec<Frame>,
    spare: Vec<Vec<NodeId>>,     // reused child lists
    scratch: String,             // reused formatting buffer
    events: Events,
    pending_sources: Vec<PendingSource>,
    stats: Stats,
}

struct Frame { node: NodeId, logical: Vec<NodeId>, visual: Vec<NodeId>, kind: FrameKind }

pub struct Response<'u> { ui: &'u mut Ui, node: NodeId }

impl Response<'_> {
    pub fn element(&self) -> &UIElement;
    pub fn cast<T: windows_core::Interface>(&self) -> windows_core::Result<T>;
    /// Forgets the values this layer last wrote, so the next pass writes
    /// every managed property again. Call after writing one imperatively.
    pub fn invalidate(&mut self);
}

impl Ui {
    pub fn stack(&mut self, id: impl Hash, orientation: Orientation, spacing: f64,
        body: impl FnOnce(&mut Ui) -> Result<()>) -> Result<Response<'_>>;
    pub fn label(&mut self, id: impl Hash, text: std::fmt::Arguments<'_>) -> Result<Response<'_>>;
    pub fn button(&mut self, id: impl Hash, label: &str) -> Result<Button<'_>>;          // .clicked()
    pub fn text_box(&mut self, id: impl Hash, placeholder: &str, value: &mut String) -> Result<Changed<'_>>;
    pub fn password_box(&mut self, id: impl Hash, placeholder: &str) -> Result<PasswordField<'_>>; // .take()
    /// A control this crate has no widget for. `create` runs once; `update`
    /// runs every pass. The crate owns its place in the tree and its lifetime.
    pub fn native(&mut self, id: impl Hash, create: impl FnOnce() -> Result<UIElement>,
        update: impl FnOnce(&UIElement) -> Result<()>) -> Result<Response<'_>>;
    pub fn cached<D: 'static + PartialEq>(&mut self, id: impl Hash, deps: D,
        body: impl FnOnce(&mut Ui) -> Result<()>) -> Result<()>;
    /// UI-only state. `init` runs when the node is created.
    /// A value derived from inputs puts the inputs in `id` (see "Cache nodes").
    pub fn state<T: 'static>(&mut self, id: impl Hash, init: impl FnOnce() -> T) -> &mut T;
    /// Children go into consecutive rows. Gives each child a definite height,
    /// so a `list` in a `Star` row fills it and scrolls.
    pub fn grid(&mut self, id: impl Hash, rows: &[Length],
        body: impl FnOnce(&mut Ui) -> Result<()>) -> Result<Response<'_>>;
    /// A virtual list of `len` rows that fills the space its parent gives it.
    /// `row` builds a row; see `RowFn`.
    pub fn list<A: App>(&mut self, id: impl Hash, len: usize, revision: u64,
        row: RowFn<A>) -> Result<ListId>;
    /// UI-only state for one item of a list, kept on the list node by item key.
    pub fn list_state<T: 'static>(&mut self, list: ListId, item: u64, init: impl FnOnce() -> T) -> &mut T;
}
```

```rust
/// A grid row height.
#[derive(Clone, Copy, PartialEq)]
pub enum Length { Auto, Pixels(f64), Star(f64) }
```

Layout comes from WinUI panels: `stack` for content that sizes itself, and
`grid` for anything that must fill space. A `StackPanel` measures children
with unbounded height, so a list inside a stack would never scroll; the
list's docs say to place it in a `Star` grid row. The grid stores its last
`rows` in the node and rewrites `RowDefinitions` only when they change, and
sets `Grid.Row` on a child only when its index changes.

A widget call visits its key, creates the control on `Visit::Created`,
compares each managed property with the stored last value, and writes only
the ones that differ. Labels format into `scratch` and compare before
copying, as in the spike. Managed values are owned by the layer: an app that
writes one imperatively must call `invalidate`.

Text editing keeps the spike's rules. `text_box` reads the control's text on
`TextChanged` and writes `value` only when the app changed it. It also
subscribes to `TextCompositionStarted` and `TextCompositionEnded`, and while
a composition is open it defers app writes until the composition ends.
`password_box` never mirrors the password; `PasswordField::take` reads it
once and clears the box.

### Cache nodes

`cached(id, deps, body)` runs `body` when `deps` differs from the stored
value, when the node is new, or when `event_stamp` shows an event for a node
inside it this pass; otherwise it calls `tree.keep` and re-emits `emitted`.
It stores `deps` by assigning into the existing `Box<dyn Any>` when the type
matches, so a deps change allocates only when the type changes. `state` is a
logical leaf node with `Payload::State`; it lives as long as its parent keeps
calling it, and is dropped with it. It holds UI data such as an expanded
flag, a scroll anchor, or sorted indexes, which the app should not own. A
value derived from inputs puts the inputs in the id,
`ui.state(("matches", filter), || compute())`: new inputs declare a new
node, `init` computes again, and the old node drops when its container
closes.

### Events and callbacks

```rust
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum EventKind { Click, TextChanged, PasswordChanged, CompositionStarted,
    CompositionEnded, ViewChanged, KeyDown, CloseRequested }

pub(crate) struct Events {
    /// One delegate object per kind, created once per window.
    delegates: Delegates,
    /// COM identity (IUnknown pointer) of each control that is subscribed
    /// to an event or is a list row.
    senders: AHashMap<usize, NodeId>,
    /// Filled by the delegates; swapped out at the start of each pass.
    inbox: Rc<RefCell<Vec<(usize, EventKind)>>>,
    current: Vec<(NodeId, EventKind)>,
}
```

Each delegate pushes `(sender identity, kind)` and requests a pass. At the
start of a pass the crate resolves senders to nodes, drops events from dead
controls, and marks each target with `tree.mark_event`. A widget subscribes
its control to a kind the first pass the app asks for it (`clicked()`,
`text_box`), by calling the event's `add_*` vtable entry with the shared
delegate. No per-control closure exists, so registration allocates nothing
in Rust. Handlers need no revocation: the element holds the delegate, and
dropping the element releases both. `KeyDown` is the one bubbling event:
one handler on each window's content, resolved by walking `OriginalSource` up
to the nearest known control.

### Passes and scheduling

The runner keeps the spike's coalescing: a pending flag and one
`DispatcherQueue` callback. A pass that consumed events runs a second pass,
because widgets built before the reader still show old state. After the
passes, the runner releases every borrow, then applies pending
`SetItemsSource` calls, since WinUI may call `GetElement` inside them.

### The list (`ItemsRepeater` only)

`list(id, len, revision, row)` builds a `ScrollViewer` with an
`ItemsRepeater` whose `ItemTemplate` is the crate's `IElementFactory` and
whose source holds the boxed indexes `0..len`. A changed `len` or `revision`
queues a new source.

```rust
pub(crate) struct ListData {
    /// Row node and the item index it shows. About one screen of rows, so a
    /// `Vec` with `swap_remove` beats a map.
    realized: Vec<(NodeId, usize)>,
    /// Recycled rows, kept for reuse.
    free: Vec<NodeId>,
    row: Box<dyn Fn(&mut dyn Any, &mut Ui, usize) -> Result<()>>,
    /// The `RowFn<A>` the box wraps, to tell when the app passes a new one.
    row_address: usize,
    /// Per-item UI state from `list_state`, by item key.
    item_state: AHashMap<u64, Box<dyn Any>>,
    len: usize,
    revision: u64,
}
```

`GetElement` calls `ui.realize(list, index)`: it takes a free row or creates
one, runs the list's row function, and returns the row's element in the same
layout pass. `RecycleElement` gets the element back; `Events::senders`
already maps every control's COM identity to its node, so it resolves the
row with no list-specific map, and moves it from `realized` to `free`. After
each `view`, the runner runs the row function for every realized row, so
rows show current state; an app wraps a row body in `cached` when its inputs
rarely change. Free rows stay in the tree as logical children of the list
node, so drop on detach keeps them.
Row nodes are keyed by slot, so UI state for an item must not live in the
row: `list_state::<T>(list, item_key)` stores it on the list node instead.

### Allocation contract

A pass allocates only when: a node or control is created; a stored string
grows past its capacity; a child list grows past its capacity; a cache's
deps change type; `state` is first created (or its id changes); or a burst of
messages outgrows the mailbox buffer. WinUI's own allocations (HSTRINGs, COM objects) happen on
the OS heap and are counted as WinUI calls instead.

## Crate layout

```text
Cargo.toml                       virtual workspace: members = ["winui-immediate", "spikes"]
spikes/                          every earlier spike, unchanged (WP-00), incl. windows-reactor
winui-immediate/
  Cargo.toml                     windows-core, -collections, -reference 0.100, ahash; dev: proptest
  src/lib.rs                     public API re-exports, crate docs
  build.rs, bindings.txt         generate the bindings into OUT_DIR
  src/bindings.rs                include! of the output, shared delegates, add_* helpers
  src/startup.rs                 bootstrap, app shim (from spike winui.rs)
  src/runner.rs                  Runner<A>, scheduling, passes, Handle, factory hooks
  src/tree.rs                    Tree<P>, Node, NodeId, Key (pure)
  src/children.rs                edits (pure)
  src/pass.rs                    Ui, Frame, Payload, the pass core
  src/events.rs                  Events, delegates, sender map
  src/widget.rs                  shared widget helpers
  src/widget/{label,button,text_box,password_box,stack,grid,native,list}.rs
  examples/vault_list.rs         port of the spike app
  tests/ui/{caret,click,ime,scroll}.ps1   UI Automation scripts from the spike
tools/winui-bindgen/             spikes only (filter spikes.txt)
```

## Tests

1. `children::edits` (proptest): random unique `old` and `new`; applying the
   edits to `old` yields `new`; the common prefix and suffix get no edit.
2. `Tree` drop on detach (proptest, `Tree<()>`): random sequences of passes,
   each a random tree with random cache hits; after each pass the live nodes
   are exactly those reachable from the root through logical children, each
   node has one parent, and `by_key` names only live nodes.
3. `Tree` duplicate id: visiting one key twice in a pass panics.
4. `cached` body runs (proptest over a model): the body runs exactly when
   deps changed, the node is new, or an event targets its subtree.
5. Mailbox: 8 threads each send 10,000 numbered messages while the UI
   thread drains; every message arrives exactly once, each sender's messages
   arrive in order, at most one wake is posted at a time, and `send` after
   close returns false. Runs against the mailbox with a fake wake, no WinUI.
6. Steady pass allocates nothing: a counting allocator with a thread-local
   counter; a repeated identical pass over `Tree<()>` with spare lists
   allocates 0 after warm-up.
7. `examples/vault_list.rs` with `IMMEDIATE_BENCH=idle`: asserts 0 Rust
   allocations per steady pass and prints build time and WinUI calls.
8. UI Automation scripts against the example: caret (expects "entry 132"),
   click (Reveal shows "password 1", Unlock flips to Lock), IME with
   Vietnamese Telex (expects "tiếng việt"), scroll (consecutive rows covering
   the viewport after jump, small steps, page steps and end).
9. Callback count: the example logs registrations; after a scroll test it
   reports one delegate object per kind and one registration per subscribed
   control.
10. Windows: the example's "Open details" button declares a second window.
   Closing it through UI Automation (`WindowPattern.Close`) delivers
   `close_requested`; the window stays open until the app stops declaring
   it, then closes. Closing the last window makes `run` return.

## Work breakdown

### WP-00: move the spikes

Status: done (2026-11-20). The repo had no commits and `src/` was untracked,
so a plain move replaced `git mv`. The root lists only `spikes` as a member
until WP-01 creates `winui-immediate`, because Cargo rejects a member
directory that does not exist. `cargo build -p spikes --bins` builds
`spikes`, `immediate`, `password_probe` and `popup` with no warnings; the old
`bitwarden-winui` binary is now named `spikes`.  
Owner: main agent  
Depends on: none

Scope: move `src/` (the Reactor spike `main.rs`, `bin/password_probe.rs`,
`bin/popup.rs`, `bin/immediate/`) to `spikes/src/` with `git mv`; move the
current `[dependencies]`, including `windows-reactor` and `windows-sys`, to
`spikes/Cargo.toml` (package `spikes`); turn the root `Cargo.toml` into a
virtual workspace (`[workspace] members = ["winui-immediate", "spikes"]`,
`resolver = "3"`). `tools/winui-bindgen` keeps its own workspace.

Deliverable: the same binaries, built from `spikes`.

Acceptance: `cargo build -p spikes --bins` succeeds, and the root manifest
has no `[package]` and no `windows-reactor`.

Handoff: build log, `git status`.

### WP-01: crate skeleton, bindings, startup

Status: done (2026-11-20). Bindings come from `winui-immediate/build.rs`
(windows-bindgen as a build dependency, filter `winui-immediate/bindings.txt`,
metadata from the installed `Microsoft.WindowsAppRuntime.2_2.4.0.0_x64`
package or `WINUI_IMMEDIATE_WINMD`) into `OUT_DIR`; `src/bindings.rs`
includes them and adds the helpers below. The build script puts the folder of
the running `cargo` on PATH, because windows-bindgen runs `rustfmt` by name;
the build passes with `~/.cargo/bin` off PATH. Decision gate result: the raw
slots are public vtable fields, but the delegate boxes and vtable structs are
private to the generated module, so `src/bindings.rs` `include!`s the output
and adds `shared` constructors (`RoutedEventHandler`,
`TextChangedEventHandler`, `KeyEventHandler`, `EventHandler<T>`,
`TypedEventHandler<S, A>`) and `add_*` helpers (`add_click`,
`add_text_changed`, `add_text_composition_started/ended`,
`add_password_changed`, `add_view_changed`, `add_key_down`, `add_closing`).
No fallback was needed. `Window.AppWindow` lives on `IWindow2`. Smoke
(`examples/smoke.rs`, UI Automation): invoking Close fires the shared click
delegate with the button as sender, closing the window fires the shared
`Closing` delegate, and in both cases the message loop ends and the process
exits 0. The smoke entry is `winui_immediate::smoke`, hidden, until WP-03
adds `run`.  
Owner: main agent  
Depends on: WP-00

Scope: add `winui-immediate` to the root `members`, `winui-immediate/Cargo.toml`,
`src/lib.rs`, `src/startup.rs`, `src/bindings.rs`, `tools/winui-bindgen`
output path and filter (add `ItemsRepeater`, `IElementFactory`,
`TextCompositionStarted/Ended`, `KeyDown`, `Window.Close`,
`Window.AppWindow`, `AppWindow.Closing` and its `Cancel`,
`VisualTreeHelper.GetParent`, `Grid` with `RowDefinitions` and `Grid.SetRow`,
`UIElement.Visibility`).

Deliverable: `run` opens one empty window from a hard-coded pass; the bindings expose each event's
raw `add_*` vtable entry.

Acceptance: `cargo build -p winui-immediate` with no warnings; a smoke
example opens and closes a window. Decision gate: if the vtable `add_*`
entries are not reachable from the generated code, fall back to one small
forwarding closure per control (no captured key) and record the change.

Handoff: build log, generated bindings size.

### WP-02: tree and child edits

Status: done (2026-11-20). `cargo test -p winui-immediate --lib`: 11 tests
pass (5 proptests for `edits`, 2 proptests for drop on detach, duplicate id,
stale id after slot reuse, event marking, zero allocations). The main agent
did it; the package was small enough that a subagent would have cost more
than it saved. Changes against the design above: the tree owns a root node
(`Tree::new(root_payload)`, `Tree::root()`), so `visit` and `close` always
take a parent `NodeId`; `keep` is gone, because `visit` already stamps a
cache node and drop on detach never walks its untouched descendants; the
spare-list pool lives in `Tree` (`take_list`, `recycle_list`), and dropped
nodes return their child lists to it. `Key::child` uses ahash with fixed
seeds, so keys are reproducible in tests. Measured: the first three passes
over a fixed tree allocate while the pool settles (the third allocates one
list); every later pass allocates nothing.  
Owner: main agent  
Depends on: WP-01

Scope: `src/tree.rs`, `src/children.rs`, their tests.

Deliverable: pure `Tree<P>` and `edits` with tests 1–3 and 6.

Acceptance: `cargo test -p winui-immediate tree children` passes.

Handoff: changed paths, test output.

### WP-03: pass, events, runner

Status: done (2026-11-20). New modules: `events.rs` (shared `Click` and
`CloseRequested` delegates, sender identity map, lazy subscription),
`pass.rs` (`Ui`, frames, `Payload`), `mailbox.rs` (`Mailbox`, `Handle`),
`runner.rs` (`App`, `run`, coalesced turns, second pass). `lib.rs` exports
`App`, `run`, `Handle`, `Ui`, `Orientation`, `WindowResponse`,
`ButtonResponse`; the temporary `smoke` entry is gone. Changes against the
design: the window, stack, label and button widgets landed here, because the
acceptance needs them; WP-04 adds the rest and `Response`. The mailbox is its
own module so test 5 runs without WinUI. `DispatcherQueueHandler` is not
`Send`, so `Handle::send` builds one capture-free handler per wake (not per
message) on the sending thread; the UI thread's own scheduler reuses one
handler. A failed pass stores the error, closes every window and the
mailbox, and `run` returns the error. `Ui` uses its own `Orientation` enum, so
the generated types stay private. Results: 14 unit tests pass (test 5 sends
80,000 messages from 8 threads: each arrives once, in order per sender, with
at most one wake outstanding). `tests/ui/smoke.ps1` against
`examples/smoke.rs`: two windows open; two clicks show "2 clicks"; a message
from a background thread updates the status line; closing the second window
reaches the app as `close_requested` and the window closes when the app stops
declaring it; closing the main window ends the loop, `run` returns, and the
process exits 0.  
Owner: main agent  
Depends on: WP-02

Scope: `src/pass.rs`, `src/events.rs`, `src/runner.rs`.

Deliverable: `Ui` with frames and `Response`, shared delegates with lazy
subscription, coalesced passes with the second pass, the mailbox and
`Handle::send` with `A::update`.

Acceptance: the smoke example declares two windows; the first shows a stack with a label and a button whose
click updates the label.

Handoff: changed paths, run log.

### WP-04: widgets and imperative access

Status: done (2026-11-20). `pass.rs` now holds only the pass core;
`widget.rs` holds `Control`, `Response` and `must_write`, and
`widget/{window,stack,grid,label,button,text_box,password_box,native}.rs` one
widget each. Every widget returns a `Response` (or a type that derefs to
one). `ControlNode` gained `stale` (set by `Response::invalidate`, cleared
after the next write) and `grid_row` (`Grid.SetRow` only when a child's
position changes). Events gained `TextChanged`, `CompositionStarted` and
`CompositionEnded` with shared delegates. Changes against the design: the
public escape hatch trades in `windows_core::IInspectable` (`Response::element`,
`Response::cast`, `WindowResponse::window`, `native`'s `create`), because
the generated bindings and their methods are crate-private; an app casts to
its own bindings. A `PasswordField` borrows `Ui`, so a button declared after
it cannot take the password; `PasswordField::handle` returns a `Copy`
`PasswordHandle` for `Ui::take_password` later in the same pass.
`ButtonResponse::clicked` takes `&mut self`. Fixed a hang found here: a first
pass that failed, or declared no window, left the message loop running with
nothing to end it; the runner now calls `Application.Exit` (added to
`bindings.txt`) whenever a pass leaves no window open, so `run` returns. The
first failure seen was `E_NOTIMPL` from the example building a composable
`ProgressRing` with `RoActivateInstance`; the example now uses the sealed
`ToggleSwitch`. Results, `tests/ui/widgets.ps1` against
`examples/widgets`: caret ("entry 12", Left, "3" gives "entry 132", 9 edits
seen) with a pass every 10 ms; Reveal shows "password 1"; typing a password
and Unlock gives "Unlocked" and the button reads "Lock"; the native
`ToggleSwitch` is in the UI Automation tree; Vietnamese Telex gives
"tiếng việt " with a pass every 10 ms. Telex must be activated after the
first window exists; activating it before (in `run`'s `start`) has no effect.
The WP-03 smoke script still passes; 14 unit tests pass. Not tested: an app
write held back during an open composition, and `Response::invalidate`.  
Owner: main agent  
Depends on: WP-03

Scope: `src/widget.rs`, `src/widget/{label,button,text_box,password_box,stack,grid,native}.rs`.

Deliverable: the widget set, `Response::{element, cast, invalidate}`,
composition-aware `text_box`, read-once `password_box`.

Acceptance: caret, click and IME scripts (test 8) pass against a widgets
example.

Handoff: changed paths, script output.

### WP-05: cache nodes

Status: done (2026-11-20). `widget/cache.rs` adds `cached`, `state` and
`memo`; `pass.rs` adds `Payload::Cache(CacheNode { deps, emitted })` and
`Payload::State(Box<dyn Any>)`. A cache closes like a container, but its
declared controls go to the parent's visual list and into `emitted`; a
skipped run re-emits `emitted` (`Ui::emit_cached`) and never walks the
subtree, which drop on detach keeps. Changes against the design: `state` and
`memo` return `Result`, because declaring a node can fail; `memo`'s
`compute` takes `&D`; `list_state` moves to WP-06, because it lives on the
list node that WP-06 creates. Results: 17 unit tests pass. Test 4 (proptest
over random passes that declare or omit the cache, change its deps, and
inject events into a node inside it) checks that the body runs exactly when
the node is new, the deps changed, or an event reached it; further tests
check that state inside a cache survives skipped runs and dies with the
cache, and that a memo recomputes only on a deps change. In
`examples/widgets`, the Reveal row is cached on `revealed`: with a pass every
10 ms, its body ran 3 times (first pass, the click, the changed deps) and the
row stayed on screen in between; every other check in
`tests/ui/widgets.ps1` still passes.  
Owner: main agent  
Depends on: WP-03

Scope: `cached`, `state`, `memo`, `list_state` in `src/pass.rs`; test 4.

Deliverable: cache skipping with event re-entry and in-place deps storage.

Acceptance: test 4 passes; a counting example shows a cached subtree's body
runs zero times across idle passes.

Handoff: changed paths, test output.

### WP-06: ItemsRepeater list

Status: done (2026-11-20). `widget/list.rs` adds `list`, `list_state`,
`ListId`, `RowFn` and the `RowFactory` element factory; `ListData` lives in
`Control::List`, and each row is a `Control::Row` whose `ContentControl`
shell WinUI keeps while the row's control changes inside it. `Tree::adopt`
adds a row WinUI asks for during layout to the list's children at once.
`runner.rs` builds every realized row after `view` (`Ui::build_rows`), sets
item sources after the passes with no borrow held, and serves
`realize_row` and `recycle_row` from `CURRENT`. `WINUI_IMMEDIATE_LOG` prints
turn and realize times. Changes against the design: rows are keyed by slot
(`("row", slot)`) and found again by element on recycle with a linear scan of
`realized`, not through `Events::senders`. A bug found and fixed: WinUI asks
for rows between passes, so a reused row's controls still carried the last
pass's stamp and the rebuild panicked with "duplicate id"; `realize` now
starts a new pass number. Results, `tests/ui/list.ps1` against
`examples/list.rs` (release, 1,000 entries): after a jump to 50%, 60 small
steps, 20 page steps and a jump to the end, the rows on screen were
consecutive and covered the viewport; Reveal in a row shows "password 1";
filtering "entry 12" shows exactly the 11 matching rows; the process exits 0.
Scrolling runs no turns, only realizes: 878 realizes, p50 19 µs, p90 43 µs,
max 355 µs (spike: p50 18 µs). Steady turns with about 24 realized rows
(`LIST_TICK=20`): p50 67 µs, p90 98 µs (spike: 47 µs); every pass rebuilds
every realized row, about 2.5 µs each, which `cached` around a row body would
remove. The smoke and widgets scripts and 17 unit tests still pass. Not
tested: rows of different heights, and `list_state`.
Follow-up (2026-11-20), from a review of what can outlive its use:
a failed build left open frames behind (`realize` runs outside a pass, so no
`close_all` cleaned up), and both `realize` and `close_all` discarded open
frames, which left nodes created in the failed build live but unreachable.
`Ui::unwind(depth)` now closes open frames normally, attaching every visited
node; `close_all` and a failed `realize` use it, and a row whose build failed
goes back to `free` instead of `realized`. Test
`a_failed_build_leaves_no_node_behind` passes, and fails with 4 live nodes
instead of 1 when the old discard is put back. `RowFn` now also receives the
`ListId`, because a row could not reach `list_state` without it; the list
example keeps its revealed flag in `list_state`, and after scrolling to the
end and back, entry 1 is still revealed. `retain_list_state(list, keep)`
drops the state of items that left the app's data; without a call, item state
lives as long as the list. The free-row pool still keeps its peak size while
the list lives.  
Owner: main agent  
Depends on: WP-04, WP-05

Scope: `src/widget/list.rs`, factory and hooks in `src/runner.rs`.

Deliverable: `list` with synchronous realize, row reuse, post-view row
rebuild, deferred sources.

Acceptance: scroll script (test 8) passes; scroll-test builds stay within
the spike's repeater numbers (p50 near 47 µs, realize p50 near 18 µs).

Handoff: changed paths, scroll log.

### WP-07: example port

Status: done (2026-11-20)  
Owner: main agent  
Depends on: WP-06

Result: `examples/vault_list.rs` ports the spike (unlock row, filter, 1,000
rows with Reveal through `list_state`, a details window that asks for a
second close). `VAULT_BENCH=1` reported 200 steady passes with 0 UI-thread
allocations in three runs. `tests/ui/vault.ps1` replaces `list.ps1`:
consecutive rows cover the viewport after jump, 60 small steps, 20 page
steps and end; Reveal survives scrolling away and back; Unlock flips to
Lock; filter "entry 12" shows 11 matching rows; the first close of the
details window shows "Close again to confirm", the second closes it, and
closing the main window returns from `run` with exit code 0. Test 9: 5
delegate objects, 76 registrations, all on distinct (control, kind) pairs.
Span times: realize p50 20 µs, p90 46 µs over 923 rows. Logging moved from
`WINUI_IMMEDIATE_LOG` to `tracing` (debug span `turn`, trace span `realize`,
trace event `subscribe`). Caret and IME stay in `widgets.ps1`, which passes,
as does `smoke.ps1`.

Scope: `winui-immediate/examples/vault_list.rs`,
`winui-immediate/tests/ui/*.ps1`. The spike in
`spikes/src/bin/immediate/` stays as it is, for comparison.

Deliverable: the spike app on the crate; tests 7–10 pass.

Acceptance: all four scripts pass; test 7 reports 0 allocations per steady
pass.

Handoff: script output, bench output.

### WP-08: vault plan update

Status: done (2026-11-20)  
Owner: main agent  
Depends on: WP-07

Result: the vault plan's goal, "The shell" facts, architecture diagram, crate
layout, dependencies, Hello window id, the "Fit with winui-immediate and the
threat model" section, step 7 and its WP-05 now use the crate. The shell
moves to a new `app` workspace member, and its `app/src/shell/` paths and
the `spikes/` probe path follow. The login password stays in the
`PasswordBox` and is taken once on submit. `grep Reactor` on the vault plan
finds nothing. The memory note marks the spike as superseded and points to
this plan.

Scope: `plans/sans-io-vault-core.md`, `.claude/memory/immediate-mode-spike.md`.

Deliverable: the edits in "Changes to other plans"; the memory note points to
the crate.

Acceptance: the vault plan names no Reactor API in its shell sections.

Handoff: diff of both files.

### WP-09: Fluent widgets for the Chaveiro login screen

Status: done (2026-11-21)  
Owner: main agent  
Depends on: WP-04

Result: `Ui::combo_box`, `Ui::info_bar` with `Severity`, and `Ui::card`.
`Response::enabled`, `style`, `align`, `width` and `padding`. `header` on
text boxes, password boxes and combo boxes. `enter_pressed` on text and
password boxes. `accent` and `busy` (a 16px ring before the label) on
buttons. `mica`, `compact` (content in the title bar, close button only,
fixed size) and `fit_to_content` on windows. Every window starts cloaked and
is uncloaked at the end of its first pass, or after its first fit settles
(at most 3 resizes), so the resize is never seen. Enter, SelectionChanged
and SizeChanged are new shared delegates. Each property is written only when
it changes, and `invalidate` resets the new records.

Acceptance: `tests/ui/widgets.ps1` "combo box, Enter, info bar" checks a
pick, an app write after a pick, three Enters, a disabled combo box and the
info bar. Every earlier script still passes.

Later on 2026-11-21: `run(make)` replaces `run(app, start)`. `make` gets the
handle on the UI thread once WinUI is up and returns `Result<App>`, so an app
holds its runner directly and a failed setup comes back from `run`.

Gaps: `card` looks up its theme brushes once, so a theme change does not
recolor it. `accent` and `busy` are not rewritten by `invalidate`.

Waves: WP-00, then WP-01, then WP-02, then WP-03, then WP-04 and WP-05 in parallel
(disjoint files), then WP-06, WP-07, WP-08.

## Steps

1. Copy this plan to `plans/winui-immediate.md`. Done (2026-11-20).
2. WP-00; build the spikes. Done (2026-11-20): the spikes build from
   `spikes/`, and the root is a virtual workspace.
3. WP-01. Done (2026-11-20): bindings from `build.rs`, shared delegates
   registered through raw slots, smoke example closes cleanly both ways.
4. WP-02; run tests 1–3 and 6. Done (2026-11-20): 11 tests pass; steady
   passes allocate nothing after a three-pass warm-up.
5. WP-03; run test 5 and the smoke example. Done (2026-11-20): 14 tests
   pass; the smoke script passes every check.
6. WP-04 and WP-05; run test 4 and the caret, click and IME scripts.
   WP-04 done (2026-11-20): caret, click, password, native and IME checks
   pass in `tests/ui/widgets.ps1`. WP-05 done (2026-11-20): test 4 and the
   cache tests pass; the cached Reveal row rebuilds only when needed.
7. WP-06; run the scroll script. Done (2026-11-20): scroll, Reveal and
   filter checks pass; realize p50 19 µs, steady turn p50 67 µs.
8. WP-07; run tests 7–10.
9. WP-08.

## Changes to other plans

- `plans/sans-io-vault-core.md` "Architecture": the diagram's "Reactor root
  component" becomes the `winui-immediate` `App`; `spawn_background` becomes
  a thread-pool job that returns `Msg::Core(Input)` through `Handle::send`,
  and `A::update` calls `core.handle`.
- "Fit with Reactor and the threat model": rename to "Fit with
  winui-immediate and the threat model". `view` reads `core.screen()`
  directly with no `Rc<str>` copies for rows. The login password stays in
  the `PasswordBox` and is taken once on submit (threat model option A),
  instead of living in the core's `LoginForm` as a controlled value.
- WP-05 "shell": the root is now a virtual workspace and the old
  `src/main.rs` lives in `spikes/`, so the shell goes in a new app crate that
  depends on `winui-immediate` and `vault-core`; it names no Reactor API.

## Settled

- **No `memo`; derived values use `state` keyed by their inputs**
  (2026-11-20). The user asked whether `memo` was needed. `ui.state((name,
  inputs), || compute())` behaves the same, at the cost of a `Hash` bound,
  comparison through a 64-bit key, and one node created and dropped per
  change. `memo` was removed after WP-05; `Ui::state`'s docs show the pattern,
  and a unit test checks that the value recomputes only on new inputs and
  that the old node is dropped.

- **Bindings are generated by a build script** (2026-11-20). The user asked
  for it: the filter is the one source of truth and no generated file is
  checked in. The cost is a build-time dependency on the installed runtime's
  metadata, with `WINUI_IMMEDIATE_WINMD` as the override.

- **ahash everywhere** (2026-11-20). The user chose `ahash::AHashMap` for
  every map and `ahash` for `Key` hashing. Keys need only be stable within
  one process, so ahash's per-process random seed is fine.
- **`Tree::by_key` uses a pass-through hasher** (2026-11-20). The user
  pointed out that a `Key` is already an ahash output, so `KeyHasher` in
  `tree.rs` returns it as is. Other maps (`Events::senders`, keyed by COM
  pointers, which are aligned and poorly spread) keep ahash.
- **The crate owns inbound domain crossing only: a typed `A::Message`
  mailbox with one coalesced wake** (2026-11-20). The user wants the least
  indirection: no boxed closure per message, no crate-owned task runner.
- **Per-item UI state lives on the list node, keyed by item key
  (`list_state`)** (2026-11-20). Rows are keyed by slot and reused, so
  row-local `state` would follow the slot.
- **The root becomes a virtual workspace** (2026-11-20), with members
  `winui-immediate` and `spikes`.
- **All existing code moves to a `spikes` crate, and `windows-reactor` goes
  with it** (2026-11-20). The user: everything so far is a spike. The root
  keeps no package, so no app crate depends on Reactor.
- **WinUI only, no backend trait** (2026-11-20). The user chose concrete WinUI
  types; the pure bookkeeping still gets property tests through `Tree<P>`.
- **The list uses `ItemsRepeater` only** (2026-11-20). Variable row heights
  from the start; the slot-pool Canvas list is not ported.
- **Shared delegates, lazy subscription** (2026-11-20). One delegate object
  per event kind, registered on a control the first time the app asks.
- **Scope covers the crate, the spike port and the vault plan update**
  (2026-11-20).
