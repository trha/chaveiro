# winui-immediate flow nodes

Status: Done

Parent plans: `plans/winui-immediate.md` ("Crossing into the UI thread",
"Passes and scheduling"). Where they differ, this plan wins. "Changes to
other plans" lists the changes.

## Goal

Let a view write a sequence in time as one `async` block that draws. A flow
is a tree node that owns a future and the controls that future declares. The
view declares it like any widget. Each pass polls the future in place, and
the future draws through `Flow::frame`, with the pass's `Ui` and a value the
view lends it. A pass that leaves the flow out drops the future and its
controls, which cancels the future's work.

The first user is the clipboard copy in `chaveiro-winui/src/io.rs`. It is a
`winui_executor::spawn` task with retries, a 30 s wait, and a clear, and its
state lives in `Io::copied` and `Io::copy`. A flow replaces it with a loop
that also shows "Copied · clears in N s". Later users are prompts and
dialogs that ask, wait, and retry, such as unlocking with the master
password.

## Out of scope

The task node (`Ui::task`) and the stream node (`Ui::stream`) have already
landed, in change `uvuzqqpz` ("winui: simplify async crossing"). This plan
builds on them and does not change them. Inbound requests from other threads
(WP-08 passkeys, WP-10 SSH agent) need their own way in and are not part of
this plan.

## Facts this plan rests on

**Async crossing today.** `Ui::task` and `Ui::stream`, in
`winui-immediate/src/widget/task.rs`, are logical tree nodes. Their wakers
push the node id onto `TaskWakes::woken` and post one turn per burst. At the
start of each turn, `Runner::run_turn` calls `Ui::poll_woken_tasks`, before
the passes run. A pass that leaves a node out drops it, and with it the
future or stream (`Payload::Task`, `Payload::Stream` in
`winui-immediate/src/pass.rs`). `Mailbox`, `Handle`, `App::update` and
`Ui::request_pass_after` are gone, and `App` has only `view`.

**Wake protocol.** `TaskWake::signal` swaps `queued` with `Release`, and
`TaskWake::begin_poll` clears it with `swap(false, Acquire)`. With the
earlier `store(false, Release)`, loom found a lost wake
(`winui-immediate/src/widget/task/loom_tests.rs`,
`a_wake_during_a_turn_is_never_lost`). Run the loom tests with
`RUSTFLAGS="--cfg loom" CARGO_TARGET_DIR=target/loom cargo test -p winui-immediate --lib loom`.

**Cache nodes already replay controls.** `Payload::Cache` holds `emitted`,
the controls of its last run. `Ui::emit_cached` adds them to the open
container when a pass skips the cache's body, and the `Payload::Cache` arm
of `Ui::apply_visual` passes a run's controls up to the parent frame
(`winui-immediate/src/pass.rs`). A flow reuses both.

**Pass budget.** `MAX_PASSES` is 3 (`winui-immediate/src/runner.rs`). A
pass runs again when it consumed events or called `Ui::request_pass`.

**Allocation.** With the post handler built once, behind an
`AgileReference`, the vault bench measured 0 UI-thread allocations in 200
steady passes (`VAULT_BENCH=1 target/debug/examples/vault.exe`, measured
2026-11-22). A flow must not allocate on a pass that only replays.

**Executor.** `winui_executor::{sleep, sleep_until, sleep_until_clock,
interval, winrt, shutdown}` exist in `winui-executor/src/lib.rs`. `winrt`
cancels its WinRT operation on drop. Cancelling a Windows Hello request
does not close its prompt (measured with `chaveiro-winui/examples/hello_cancel.rs`).

**Lending a value with a lifetime compiles and refuses leaks.** A scratch
spike modeled `Ui::flow` with a thread-local slot and a `Lend` family for
`Screen<'s> { names: &'s [String], picked: Option<&'s str> }`. A closure kept
across two lends read and rewrote `'s` data, and printed
`picked Some("GitHub"), frames 3`. A closure that keeps `&'s str` outside
itself fails with E0521, and one that writes a shorter-lived `&str` into the
`'s` data fails with E0597. With `Lent` and `Owned`,
`flow(owned(&mut count), …)` and `flow(&mut screen, …)` needed no type
named at the call, and printed `count 3, picked Some("GitHub")`. The spike
lived in the session scratchpad and is not kept, so step 2 rebuilds it as a
test in the crate.

## Design

### Where a flow runs

A task runs at the start of a turn, before the view, and nothing can draw
there. A flow runs where the view declares it, inside the pass, with the
view's `&mut Ui` and the value the view lends. That is the only place both
exist, so a flow is never polled from `poll_woken_tasks`. Its waker reuses
`TaskWake` and only posts a turn. The turn's passes then reach the flow and
poll it.

Every pass that declares a flow polls it once, whether or not it was woken.
A correct future handles a poll it did not ask for. A leaf that does work on
every poll would pay for that, and its doc should say so.

### Drawing, replay, and one frame per pass

`Ui::flow` opens the flow's container lazily, on the first `Flow::frame`
call in a poll. The controls declared in that container go to the parent the
way a cache's run does. If a poll draws nothing, because the future is
waiting in an `.await` with no frame, `Ui::flow` replays the controls from
the last poll that drew, through `emit_cached`. So a future that showed
"Checking…" keeps it on screen while it derives a key.

`Ui::flow` polls the future exactly once per pass, with no loop while it
returns `Pending`. `Pending` means the future cannot go on until a wake, and
a wake leads to a later turn. That one poll still runs everything up to the
first `Pending`: awaits that are ready at once, code between awaits, and one
frame. A future that wakes itself during its poll is polled again in a
later turn, not in the same pass. A finished future is never polled again.
Its output waits for `take`, and passes replay its last controls until the
view leaves the flow out.

A frame that completes has already declared its controls for this pass,
because reading a click needs the button. So the next frame in the same poll
does not draw. It returns `Pending` and calls `Ui::request_pass`, and the
next step draws in the next pass. A completed step therefore costs one extra
pass, inside `MAX_PASSES`.

A screen that must react while work runs, such as a Cancel button during
"Checking…", races the work against a frame with
`futures::future::select`, so winui-immediate itself still depends only on
`futures-core`. Replayed controls are not declared in their pass, so their
events are not read. That is why the race is needed.

### Lending the view's value

**The signature we want.** The future is built once and polled on many
passes, and each pass lends a fresh borrow with its own lifetime. So the
flow's type must hold the lent type with its lifetime left open, and each
frame must work for whichever lifetime the current pass lends. In a
hypothetical Rust with type constructors as parameters, written here as
`S<'_>`, that is:

```rust
// Not valid Rust: `S` is a type constructor of kind `Lifetime -> Type`.
impl Ui {
    pub fn flow<'s, S<'_>, F>(
        &mut self,
        id: impl Hash,
        state: &mut S<'s>,
        start: impl FnOnce(Flow<S>) -> F,
    ) -> Result<FlowResponse<'_, F::Output>>
    where
        F: 'static + Future;
}

impl<S<'_>> Flow<S> {
    pub fn frame<R>(
        self,
        draw: impl for<'a, 'p> FnMut(&'a mut Ui, &'a mut S<'p>) -> Result<Option<R>>,
    ) -> Frame<S, R>;
}

// Call sites name no types:
ui.flow("copy", &mut self.io, |flow| ..);   // S = Io, which has no lifetime
ui.flow("pick", &mut screen, |flow| ..);    // S = VaultScreen<'_>
```

`Flow<S>` names the constructor, not one lifetime of it, so `Flow<S>` and
the future are `'static`. The frame closure is generic over `'p`, so it can
neither keep a `&'p` it reads, nor write shorter-lived data into the value.
Handing out `&'a mut S<'a>` instead would shrink `'p` behind a `&mut`, and
would be unsound.

**How the workaround approximates it.** Rust has no type constructors as
parameters, so each part of that signature gets a stand-in:

| Wanted | Workaround | Cost |
| --- | --- | --- |
| a parameter `S<'_>` | a `'static` marker `L: Lend`, whose `type Of<'s>` is the constructor | a marker type and an impl per lent type |
| `&mut S<'s>` | `&mut L::Of<'s>` | none |
| inferring `S` from `&mut S<'s>` | `Lent<'s>` maps the lent type back to its marker, so `Ui::flow` takes `&mut S` with `S: Lent<'s>` | one more impl per lent type |
| a type with no lifetime, such as `Io` | `Owned<T>`, a `repr(transparent)` wrapper, and `owned(&mut T)` | `owned(..)` at the call, and `get_mut()` to reach the value |
| `for<'p> FnMut(.., &mut S<'p>)` | `FnMut(&mut Ui, &mut L::Of<'_>)` | none: Rust already quantifies over `'_` there |

A type with no lifetime cannot get a blanket `impl<T: 'static> Lent<'_> for T`,
because that would overlap with `impl<'s> Lent<'s> for VaultScreen<'s>`.
Coherence ignores the `'static` bound. That is why plain values go through
`Owned`.

`lend!` pays the cost in the first and third rows, the marker and the two
impls, so a type with one lifetime is made lendable in one line:

```rust
lend!(pub(crate) Screen = VaultScreen<'_>);
```

That expands to the marker, `impl Lend for Screen { type Of<'s> = VaultScreen<'s>; }`
and `impl<'s> Lent<'s> for VaultScreen<'s> { type Family = Screen; }`. The
call site then reads like the wanted signature:
`ui.flow("pick", &mut screen, |flow| ..)`.

`macro_rules!` cannot replace a `'_` that sits anywhere inside a type, so
`lend!` takes only the form `Name<'_>`: a path whose first and only generic
argument is the lifetime. A type with more parameters, such as
`Rows<'_, T>`, writes the two impls by hand. Those are three short items,
and the doc of `Lend` shows them.

**The slot.** The caller holds `&mut S` for the whole `Ui::flow` call.
`Ui::flow` puts that pointer and its own `&mut Ui` in a thread-local slot
for the poll. A frame takes both out while its closure runs and puts them
back afterward, so a nested frame, or a frame outside a flow, panics instead
of making a second `&mut`. Nested flows save and restore the slot.

### Errors

A frame's closure returns `Result<Option<R>>`. `Ui::flow` returns the first
draw error of the pass, so the view's `?` handles it, as with every other
widget. The future does not see the error. Its frame stays pending, and the
pass unwinds as a failed build does today.

### Types and signatures

These go in a new module, `winui-immediate/src/widget/flow.rs`. It imports
`TaskWake` and `TaskWakes` from `widget/task.rs`, which makes their
constructor `pub(crate)`.

```rust
/// A family of lent types: `Of<'s>` is the type lent for a borrow of `'s`.
pub trait Lend: 'static {
    type Of<'s>;
}

/// A lent type, for a lifetime, names its family, so `Ui::flow` infers it.
pub trait Lent<'s>: 's {
    type Family: Lend<Of<'s> = Self>;
}

/// A `'static` value lent as itself.
#[repr(transparent)]
pub struct Owned<T>(T);

/// Views `value` as an `Owned`, so a plain `&mut` can be lent.
pub fn owned<T: 'static>(value: &mut T) -> &mut Owned<T>;

impl<T> Owned<T> {
    /// The lent value, as `owned` took it.
    pub fn get_mut(&mut self) -> &mut T;
}

pub struct OwnedFamily<T>(PhantomData<T>);
impl<T: 'static> Lend for OwnedFamily<T> {
    type Of<'s> = Owned<T>;
}
impl<'s, T: 'static> Lent<'s> for Owned<T> {
    type Family = OwnedFamily<T>;
}

/// Makes `$name<'_>` lendable to a flow: declares the marker `$family`,
/// whose `Of<'s>` is `$name<'s>`, and the `Lent` impl that points back to
/// it. Takes only a type whose one generic argument is its lifetime.
#[macro_export]
macro_rules! lend {
    ($vis:vis $family:ident = $($name:ident)::+ <'_>) => {
        $vis struct $family;

        impl $crate::Lend for $family {
            type Of<'s> = $($name)::+<'s>;
        }

        impl<'s> $crate::Lent<'s> for $($name)::+<'s> {
            type Family = $family;
        }
    };
}

impl Ui {
    /// Declares the flow `id`. The first pass that declares it calls
    /// `start`. Every pass that declares it polls the future once, right
    /// here, lending `state` and this `Ui` to the future's frames for that
    /// poll only. The controls the frames declare join the parent, as a
    /// cache's do. A poll that draws nothing shows the controls from the
    /// last poll that drew. A pass that leaves the flow out drops the
    /// future and its controls.
    ///
    /// Returns the first error a frame's closure returned in this pass.
    /// Panics when `id` held a flow of another family or output type.
    pub fn flow<'s, S, F>(
        &mut self,
        id: impl Hash,
        state: &mut S,
        start: impl FnOnce(Flow<S::Family>) -> F,
    ) -> Result<FlowResponse<'_, F::Output>>
    where
        S: Lent<'s>,
        F: 'static + Future,
        F::Output: 'static;
}

/// The future's handle to what `Ui::flow` lends. Holds no data.
pub struct Flow<L: Lend> {
    family: PhantomData<*mut L>,
}

impl<L: Lend> Clone for Flow<L> { /* .. */ }
impl<L: Lend> Copy for Flow<L> {}

impl<L: Lend> Flow<L> {
    /// Draws `draw` once per pass until it returns `Some`, then resolves
    /// with that value. A frame that completes ends the flow's drawing for
    /// this pass, so the next frame first draws in the next pass, which
    /// this asks for. Panics when polled outside `Ui::flow`, or inside
    /// another frame's closure.
    pub fn frame<R, D>(self, draw: D) -> Frame<L, R, D>
    where
        D: FnMut(&mut Ui, &mut L::Of<'_>) -> Result<Option<R>>;
}

/// The future of `Flow::frame`.
pub struct Frame<L: Lend, R, D> {
    draw: D,
    family: PhantomData<(*mut L, fn() -> R)>,
}

/// What `Ui::flow` returns.
pub struct FlowResponse<'u, T> {
    ui: &'u mut Ui,
    node: NodeId,
    output: PhantomData<fn() -> T>,
}

impl<T: 'static> FlowResponse<'_, T> {
    /// The output, once, in the pass the future finished in or a later one.
    pub fn take(&mut self) -> Option<T>;
    /// Whether the future has not finished yet.
    pub fn running(&mut self) -> bool;
}
```

The node's payload is a new variant in `winui-immediate/src/pass.rs`:

```rust
pub(crate) enum Payload {
    // ..
    /// From [`Ui::flow`]. Dropping it drops the future and its controls.
    Flow(Box<FlowNode>),
}

/// Contract: `future` is `None` exactly when `output` was set, and
/// `emitted` holds the controls of the last poll that drew.
pub(crate) struct FlowNode {
    future: Option<Pin<Box<dyn Future<Output = Box<dyn Any>>>>>,
    output: Option<Box<dyn Any>>,
    emitted: Vec<NodeId>,
    wake: Option<Arc<TaskWake>>,
}
```

The thread-local slot lives in `widget/flow.rs`. It holds the lent pointer
type-erased, the `Ui` pointer, whether this poll opened the container, and
whether a frame completed in this poll:

```rust
struct Lending {
    ui: *mut Ui,
    state: *mut (),
    opened: bool,
    frame_done: bool,
}

thread_local! {
    static LENDING: Cell<Option<Lending>> = const { Cell::new(None) };
}
```

The pointer is valid for the whole poll, because `Ui::flow` holds
`&mut self` and `&mut S` for the whole call. Each `unsafe` dereference sits
in `Frame::poll`, with a comment that states this.

### Examples

```rust
// The copy, with its countdown, in the vault's details pane.
ui.flow(("copy", copy), owned(&mut self.io), |flow| async move {
    let copied = /* clipboard::copy with retries, as in Io::run today */;
    for left in (1..=30).rev() {
        flow.frame(|ui, _| {
            ui.label("copied", format_args!("Copied · clears in {left} s"))?;
            Ok(Some(()))
        })
        .await;
        sleep(Duration::from_secs(1)).await;
    }
    clipboard::clear(copied);
})?;
```

## Tests

1. `lend_compile_fail` (doctests on `Flow::frame`): a closure that keeps
   `&'s` data outside itself fails to compile, and a closure that writes
   shorter-lived data into the lent value fails to compile. Each is a
   `compile_fail` doctest, so a change that makes either compile fails the
   test run.
2. `lend_macro` (doctest on `lend!`): `lend!(Screen = Screen<'_>)` for a
   type with a path and a lifetime compiles, a function bound by
   `S: Lent<'s>` infers its family from `&mut Screen<'_>` with no type
   named at the call, and a
   `compile_fail` doctest shows that `lend!(Rows = Rows<'_, u8>)` is
   refused, so the macro never accepts a form it would expand wrong.
3. `a_nested_frame_panics` and `a_frame_outside_a_flow_panics`
   (`widget/flow/tests.rs`): both panic with the slot's message, and none
   makes a second `&mut`.
4. `a_poll_that_draws_nothing_replays_the_last_controls`: a future that
   draws once, then waits, keeps its controls declared in later passes, and
   adds no new control.
5. `a_completed_frame_draws_the_next_step_in_the_next_pass`: after a frame
   returns `Some`, the same pass shows only that frame's controls, it asks
   for a pass, and the next pass shows the next frame's controls.
6. `leaving_a_flow_out_drops_its_future_and_controls`: a drop flag in the
   future is set, and the flow's controls are gone from the parent.
7. `the_lent_value_is_the_callers` (property test): over random sequences
   of passes that change the lent value between polls, each frame sees the
   value the pass lent, and its writes reach the caller after `Ui::flow`
   returns. It lends both a `lend!` type and an `owned(..)` value.
8. `a_replaying_pass_allocates_nothing`: the vault bench, with a flow that
   waits on `interval`, still measures 0 UI-thread allocations per steady
   pass.

## Steps

1. Edit `plans/winui-immediate.md` as "Changes to other plans" lists, so the
   parent plan describes the task and stream nodes that landed. Done
   (2026-11-22): "The app interface", "Crossing into the UI thread",
   "Passes and scheduling", the crate layout and test 5 are updated, and
   `plans/sans-io-vault-core.md` "The shell" has a superseded note.
2. Add `widget/flow.rs` with `Lend`, `Lent`, `Owned`, `owned`,
   `OwnedFamily`, `lend!` and the doctests, with no `Ui` code yet. Run
   tests 1 and 2. Done (2026-11-22), together with step 3: the doctests
   pass on nightly, and each `compile_fail` names its error code (E0521,
   E0597). A wrong code made its doctest fail, so the codes are checked.
3. Add `Payload::Flow`, `FlowNode`, the slot, `Ui::flow`, `Flow::frame` and
   `FlowResponse`. Run tests 3 to 7, the task tests, and the loom tests.
   Done (2026-11-22): `widget/flow.rs` and `widget/flow/tests.rs`, 8 flow
   tests pass (tests 3 to 7, a draw error test, and a take test), all 53
   crate tests pass, and the loom tests pass.
4. Add the crate-private `select` helper with its own small test, and an
   example in `winui-immediate/examples` that races a Cancel button
   against `sleep`. Done (2026-11-22): the example races with
   `futures::future::select` from a dev-dependency, and the crate has no
   `select` of its own (see Settled).
   `examples/flow.rs`, driven by UI Automation: the countdown fell from
   2.1 s to 1.2 s with no input, Cancel showed "Cancelled", Again went
   back to Start, and a run left alone showed "Done" and "1 jobs done".
5. Move Chaveiro's clipboard copy from `winui_executor::spawn` in
   `Io::run` to a flow in the vault view, with the countdown label. Remove
   `Io::copy`, and keep `Io::clear_clipboard` for lock. Run the vault UI
   script (`chaveiro-winui/tests/ui/vault.ps1`) and test 8. Done
   (2026-11-22): the copy is `Copies` in `chaveiro-winui/src/copy.rs`, owned by the app,
   which the vault example includes, and `vault::view` takes a `status`
   closure drawn in the title bar after the Offline tag (see Settled). In
   the example, Copy code put the code on the clipboard and showed
   "Copied · clears in 29 s", then 26 s three seconds later, and after
   31 s both the label and the clipboard were empty. The bench, with a
   waiting flow polled on every pass, measured 0 allocations in 200
   passes. The vault UI script passed. One of its runs failed keyboard
   steps and the next passed them all; those steps declare no copy
   flow, so the failure came from where its keystrokes landed.

## Changes to other plans

- `plans/winui-immediate.md` "The app interface": `App` has only `view`,
  and `run` takes `make: FnOnce() -> Result<A>`.
- `plans/winui-immediate.md` "Crossing into the UI thread": replace the
  `Mailbox` and `Handle` design with task, stream and flow nodes, and point
  to this plan.
- `plans/winui-immediate.md` "Passes and scheduling": a turn polls woken
  tasks and streams before its passes, and timed passes are interval
  streams or sleeping tasks.
- `plans/sans-io-vault-core.md` "The shell": a note says `App::update` and
  the mailbox are superseded by task and stream nodes. `plans/idle-pass-check.md`
  describes appkit-immediate, which keeps its mailbox, so it is unchanged.

## Settled

- **Async work lives in tree nodes, and the mailbox is gone** (2026-11-22).
  Change `uvuzqqpz`. A node is owned by the tree, so dropping it cancels its
  work, and no app state machine has to track tasks.
- **`take` does not ask for a pass** (2026-11-22). The use site decides,
  because Chaveiro reads answers at the top of `view`, before the state they
  change.
- **Timers are tasks or `interval` streams** (2026-11-22).
  `request_pass_after` was removed. One `interval` stream re-arms one timer,
  and the bench measured 0 allocations per steady pass.
- **Streams use `futures_core::Stream`** (2026-11-22). It is the standard
  trait and has no dependencies, so outside streams plug in.
- **A flow draws from inside its future, not from a second body closure**
  (2026-11-22). The `Ui` is already on the call stack whenever a flow is
  polled, so the future can reach it, and the borrowed state, through the
  slot.
- **No `drawing` combinator** (2026-11-22). Replay covers a screen that
  stays up during work. `futures::future::select` covers a screen that
  must react during work.
- **Lending uses `Lend`, `Lent` and `Owned`** (2026-11-22). Rust has no
  higher-kinded types, so a GAT family names the lent type for any `'s`, and
  `Lent` lets `Ui::flow` infer it. The spike showed both leaks are refused.
- **Draw errors come out of `Ui::flow`** (2026-11-22). That matches every
  other widget, and the future stays free of `?` on each frame.
- **Names are `flow`, `Flow` and `frame`** (2026-11-22).
- **A flow is polled on every pass that declares it** (2026-11-22). The
  future's waker is not the only reason a flow must run. A click on a
  frame's control arrives through the event path, and a completed frame
  asks for a pass that nothing else wakes. Polling only woken flows would
  need all three triggers: the waker, an event in the flow's subtree
  (`tree.has_event`), and the asked-for pass. Polling every pass needs
  none, and replay keeps a pass that draws nothing cheap.
- **One frame per pass** (2026-11-22). It keeps a step's controls whole
  within a pass, so the button a click came from never vanishes in the
  click's pass. Each step costs one more pass.
- **`lend!` takes only `Name<'_>`** (2026-11-22). `macro_rules!` handles
  that form without parsing a whole type, and Chaveiro's lent view types
  have one lifetime, so no proc-macro crate is needed.
- **No `select` of our own; the example uses `futures`** (2026-11-22). No
  code inside winui-immediate races futures, so a crate-private helper
  would be dead code. The flow example takes `futures` as a
  dev-dependency and uses `futures::future::select` with `pin_mut!`. An
  app that needs a race does the same.
- **The copy lives in `copy.rs`, owned by the app, and the vault view draws app status
  through a closure** (2026-11-22). `vault.rs` is shared with the vault
  example, so the view takes `status: impl FnOnce(&mut Ui) -> Result<()>`
  rather than knowing `Io`. A module of its own lets the example run the
  same copy code, so its UI script tests what the app runs. The status
  shows in the title bar after the Offline tag, where it stays visible on
  every pane.
- **`Owned` has `get_mut` instead of `Deref`** (2026-11-22). A frame
  always gets `&mut Owned<T>`, so the one way back to `&mut T` is a named
  call, which reads plainer than `**value` for a primitive.
- **A copy is UI, so it is not in `Io`** (2026-11-22). `Io` only talks to
  the OS and the outside world. `Copies` decides what the user sees and
  when, so `Chaveiro` owns it and sends `Effect::Copy` to it. The
  clipboard calls stay in `io::clipboard`, and `Io::run` treats a copy as
  unreachable.
