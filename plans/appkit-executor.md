# appkit-executor

Status: Done (2026-11-21)

Parent plans: `plans/appkit-immediate.md` (the runner and the main queue).
Where they differ, this plan wins.

## Goal

A new `appkit-executor` crate runs `!Send` futures on the macOS main
dispatch queue, the way `winui-executor` runs them on the WinUI
`DispatcherQueue`. It also turns completion blocks into futures: a
generic one-shot adapter for any Foundation completion handler, and
`off_thread` for blocking work on a global dispatch queue.

`chaveiro-appkit`'s `Io` then runs each effect as an owned task, as
`chaveiro-winui/src/io.rs` does. That replaces the `NSURLSessionDataTask`
list, the `JoinHandle` list and `reap` in `chaveiro-appkit/src/io.rs`.

## Out of scope

Keychain storage and Touch ID. They are the first later users of the
adapter, but each lands with its own plan. A shared executor crate for both
platforms is also out, because the two UI stacks may drift.

## Facts this plan rests on

`winui-executor/src/lib.rs` is the model. `spawn` stores the future in a
thread-local generational slot table and returns a `#[must_use]`
`SpawnHandle` whose drop aborts the task. A waker holds `(index,
generation)` and an `AtomicBool queued`, so a burst of wakes posts one
dispatcher callback. A poll moves the future out of the table
(`State::Polling`), so a poll may spawn or abort tasks. Futures drop with no
table borrow held. The crate has no tests, only `examples/check.rs`.

`appkit-immediate/src/runner.rs` already posts to the main queue with
`dispatch2::DispatchQueue::main().exec_async(..)` (dispatch2 0.3.1). That
call takes a `Send` closure and never refuses, unlike
`DispatcherQueue::TryEnqueue`. `dispatch2` also has
`DispatchQueue::global_queue(GlobalQueueIdentifier)` for pool work. The
main queue drains inside `NSApplication::run`, so a task polls between
passes, never inside one.

`chaveiro-appkit/src/io.rs` starts HTTP with a callback that sends
`Message::Io(Input)` through `Handle`, keeps `Retained<NSURLSessionDataTask>`
for `close` to cancel, and runs each key derivation on a bare
`std::thread::spawn`. `http::send` (`src/io/http.rs`) wraps the callback in
an `RcBlock` with a `Fn` bound. Foundation calls a completion handler once,
on a session thread.

`chaveiro-winui/src/io.rs` keeps `tasks: Vec<(TaskId, SpawnHandle)>`. Each
task ends by sending `Message::Io { task: Some(task), answer }`, and
`Io::finish` drops the entry. `close` clears the list, which aborts every
task. Its `off_thread` does not stop the pool job on drop; the job's result
drops on the pool thread.

The memory note `structured-concurrency.md` requires every task and pool
job to have an owner that waits or aborts, and calls `detach` a smell.

## Design

### Crate layout

```text
appkit-executor/
  Cargo.toml        # cfg(target_os = "macos"): dispatch2 0.3
  src/lib.rs        # spawn, SpawnHandle, task table, waker
  src/completion.rs # completion(), Completer, Completion
  src/off_thread.rs # off_thread()
  src/lib/tests.rs  # executor tests (run inside a main-queue harness)
  examples/check.rs # end to end under NSApplication
```

The crate depends only on `dispatch2`, so it has no AppKit or Foundation
types. It is empty on other targets, like `winui-executor`. It is added to
the workspace `members`.

### Executor

`spawn` copies `winui-executor` with three changes. The waker posts with
`DispatchQueue::main().exec_async(move || poll(index, generation))`, so the
`Arc<TaskWake>` holds no queue. `spawn` panics off the main thread, checked
with `pthread_main_np` through `libc`, or with `objc2::MainThreadMarker` if
the dependency is cheaper (both are already in the lock file). `detach` is
left out, by the structured concurrency rule.

```rust
pub fn spawn(future: impl 'static + Future<Output = ()>) -> SpawnHandle;

#[must_use = "dropping a SpawnHandle aborts the task"]
pub struct SpawnHandle {
    index: u32,
    generation: u32,
    thread_bound: PhantomData<*const ()>,
}

impl SpawnHandle {
    pub fn abort(self);
    /// True once the future returned Ready or was dropped.
    pub fn is_finished(&self) -> bool;
}
```

`is_finished` compares the slot's generation with the handle's, so it costs
one table lookup. It lets `Io` drop entries without a task id round trip, if
we want that (see decisions).

Main thread locals are not always destroyed at process exit. A task alive
at exit may never drop, so `Io::close` must abort tasks itself, which it
already does.

### Completion adapter

One shot, from any thread to one future. The future side owns a cancel
action, so dropping the future can stop the Foundation work.

```rust
/// Makes a linked pair. The completer may move to any thread.
pub fn completion<T: Send>() -> (Completer<T>, Completion<T>);

/// `complete` takes `&self`, so a completer fits a `Fn` block. The first
/// call wins.
pub struct Completer<T> {
    slot: Arc<Mutex<Slot<T>>>,
}

impl<T: Send> Completer<T> {
    /// Stores `value` and wakes the future. A later call drops its value.
    pub fn complete(&self, value: T);
}

/// Resolves to `Some(value)`, or `None` when the completer dropped
/// without completing.
pub struct Completion<T> {
    slot: Arc<Mutex<Slot<T>>>,
    cancel: Option<Box<dyn FnOnce()>>,
}

impl<T> Completion<T> {
    /// Runs `cancel` if this future drops before it resolves. `cancel` runs
    /// on the dropping thread, the main thread for a spawned task.
    pub fn cancel_with(self, cancel: impl 'static + FnOnce()) -> Self;
}

impl<T> Future for Completion<T> {
    type Output = Option<T>;
}

enum Slot<T> {
    Waiting(Option<Waker>),
    Done(T),
    /// The completer dropped without a value.
    Abandoned,
    Taken,
}
```

`None` covers a block that Foundation frees without calling, which happens
for a task the session drops at invalidation. The completer is not
`Clone`, so its drop alone marks the work abandoned.

### off_thread

```rust
/// Runs `job` on the default global queue. Resolves to `None` when the
/// job panicked. Dropping the future does not stop the job; its result
/// drops on the pool thread.
pub async fn off_thread<T: 'static + Send>(
    job: impl 'static + Send + FnOnce() -> T,
) -> Option<T>;
```

It is `completion()` plus `global_queue(Default).exec_async`, with
`catch_unwind` around `job`, because a panic must not unwind into
libdispatch.

### HTTP future

`chaveiro-appkit/src/io/http.rs` changes `send` to

```rust
pub async fn send(
    session: &NSURLSession,
    request: HttpRequest,
) -> Result<HttpResponse, HttpError>;
```

The block calls `completer.complete(result)`. The `Completion` gets
`cancel_with` over the `Retained<NSURLSessionDataTask>`, so an aborted task
cancels its request. A `None` maps to `HttpError { message: "The request
was dropped." }`. `Retained<NSURLSessionDataTask>` is `!Send`, which is
fine, because the future runs on the main thread.

### Io

`chaveiro-appkit/src/io.rs` mirrors the WinUI shape:

```rust
pub struct Io {
    handle: Handle<Message>,
    session: Retained<NSURLSession>,
    folder: PathBuf,
    /// Running effects. Dropping an entry aborts its task.
    tasks: Vec<(TaskId, SpawnHandle)>,
    next_task: u64,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub struct TaskId(u64);

pub enum Message {
    Io { task: TaskId, input: Input },
}
```

`run` spawns `http::send(..).await` for `Effect::Http` and
`off_thread(derive_master_key ..).await` for `Effect::Derive`. Each task
ends with `handle.send(Message::Io { task, input })`. `update` calls
`io.finish(task)` before it hands `input` to the core. `close` clears
`tasks`. `requests`, `derivations` and `reap` go away.

A cancelled HTTP task never sends, so the core sees no answer for it. That
matches WinUI, where `close` is the only abort.

## Tests

1. `completion` property test (proptest, in plain threads, no main queue
   needed): for any sequence of `complete`, completer drop and poll, the future resolves to
   exactly the first completed value, or to `None` only when no complete
   ran and the completer dropped. Polls that return Pending must store a
   waker that gets woken.
2. `cancel_with` check: dropping a pending `Completion` runs the cancel
   once; dropping a resolved one never runs it.
3. Executor checks in `examples/check.rs`, run under `NSApplication` as
   `winui-executor/examples/check.rs` does: the first poll happens after
   `spawn` returns; a wake from another thread polls on the main thread;
   a burst of 1000 wakes polls at most a few times; dropping the handle
   drops the future; a task that drops its own handle during its poll ends
   after that poll; a stale handle after slot reuse aborts nothing.
4. `off_thread` in the same example: the job runs on a non-main thread; a
   panicking job yields `None` and the app keeps running.
5. App smoke: `appkit-immediate/tools/smoke.sh` style login against the
   fixture server still works, and quitting during a login request cancels
   it with no crash.

## Steps

1. Add `appkit-executor` with `spawn`, `SpawnHandle` and the waker. Run
   `cargo check -p appkit-executor` and test 3. Done (2026-11-21):
   `cargo run -p appkit-executor --example check` passes all 6 checks
   (off_thread waits for step 2). The example drains the queue with
   `CFRunLoopRun`, because `dispatch_main` exits the main thread and
   destroys the task table first.
2. Add `completion` and `off_thread`. Run tests 1, 2 and 4. Done
   (2026-11-21): `cargo test -p appkit-executor` passes 5 tests (the
   property test, a cross-thread complete, 3 cancel checks); a mutation
   that drops the completer-drop wake fails the property test. The example passes 8
   checks. `off_thread` uses the user-initiated QoS queue, because a person
   waits for a key derivation. A second `complete` drops its value rather
   than panic, because a panic inside a Foundation block aborts.
3. Port `chaveiro-appkit/src/io/http.rs` and `io.rs` to tasks, and change
   `Message` and `update` in `main.rs`. Run `cargo build -p chaveiro-appkit`
   and test 5. Done (2026-11-21): clippy is clean. The app was driven
   with a temporary in-process script, because System Events and
   `screencapture` see no windows from the agent session. A wrong password
   against bitwarden.com shows the server's refusal. Cancel during the
   request returns to the form, and closing the window during the request
   quits with exit 0. `http::send` returns `impl Future + use<>` rather
   than an `async fn`, so the request starts in `run` and the future borrows
   no session and can be spawned. `Message::Io` carries `task:
   Option<TaskId>`, because the Store and DeviceSecret answers have no
   task.

## Settled

- **Finish by task id, as WinUI does** (2026-11-21). The two `Io` types
  stay alike.
- **`off_thread` does not wait for its job on drop** (2026-11-21). This
  matches WinUI. A key derivation may run unowned for a few seconds after
  `close`, and its result drops on the pool thread.
- **The main-thread check uses `objc2::MainThreadMarker`** (2026-11-21).

- **`Completer` is not `Clone`** (2026-11-21). `complete` takes `&self`,
  which already fits a `Fn` block, and no caller needs two completers. The
  live-completer counter went away with it.

- **New `appkit-executor` crate** (2026-11-21). It mirrors `winui-executor`
  and keeps the executor out of the UI crate.
- **Generic completion adapter in scope** (2026-11-21). Keychain and Touch
  ID will need it, so it lands with HTTP and `off_thread`.
