# immediate-tree

`immediate-tree` brings an immediate mode API on top of a retained UI
toolkit. The rendering is still done by the native toolkit, so the app gets a
truly native look and feel.

The API is heavily inspired by [egui], [RAD Debugger] (the `raddbg` layer), and Rust's own [`Future`]
trait.

This crate only contains the core that every platform shares. Each toolkit has its own platform crate:

| Crate | Toolkit |
|---|---|
| `winui-immediate` | WinUI 3 |
| `appkit-immediate` | AppKit |
| `dom-immediate` | The browser DOM (PoC only) |

## Show me the code

```rust
use futures::future::{Either, select};
use std::pin::pin;
use winui_immediate::{App, Orientation, Ui, owned};

// The application state is a pure Rust struct. No signals, messages, hooks, lenses, or callbacks.
struct Notes {
    draft: String,
    notes: NoteList,
    saving: Option<String>,
}

struct NoteList {
    /// Vec<(note id, note)>
    items: Vec<(u64, String)>,
    /// Changes with every edit of `items` so a cache can compare it cheaply.
    revision: u64,
}

impl NoteList {
    fn push(&mut self, note: String) {
        // The revision only grows, so it also serves as a fresh id.
        self.items.push((self.revision, note));
        self.revision += 1;
    }

    fn clear(&mut self) {
        self.items.clear();
        self.revision += 1;
    }
}

impl App for Notes {
    // A plain Rust function that describes the UI. `if`, `match` and `for` are the control
    // flow. The whole view runs on every pass.
    fn view(&mut self, ui: &mut Ui) {
        ui.window("main", "Notes", |ui| {
            ui.stack("content", Orientation::Vertical, 8.0, |ui| {
                // Events are read in place, where the widget is declared.
                if editor(ui, &mut self.draft) && self.saving.is_none() {
                    self.saving = Some(std::mem::take(&mut self.draft));
                }
                // Async is just another call. The future is dropped when the
                // view no longer declares it.
                if let Some(note) = &self.saving {
                    let note = note.clone();
                    match ui.task("save", || save(note)).take() {
                        Some(note) => {
                            self.notes.push(note);
                            self.saving = None;
                        }
                        None => ui.label("saving", "Saving..."),
                    }
                }
                // Expensive subtrees can be explicitly cached. The body runs
                // again only when its deps change.
                ui.cache("notes", self.notes.revision, |ui| {
                    // Keyed by id, so a note keeps its node when others go.
                    for (id, note) in &self.notes.items {
                        ui.label(("note", id), note);
                    }
                });
                clear(ui, &mut self.notes);
            });
        });
    }
}

// A component is just a function taking normal Rust references, checked by
// the borrow checker like any other code.
fn editor(ui: &mut Ui, draft: &mut String) -> bool {
    ui.text_box("draft", "New note", draft);
    ui.button("add", "Add").clicked()
}

// Temporal composition: let rustc write the state machine for you.
fn clear(ui: &mut Ui, notes: &mut NoteList) {
    ui.flow("clear", owned(notes), |flow| async move {
        loop {
            // A frame's closure runs on every pass. The first time the
            // closure returns `Some(value)`, the frame completes with `value`.
            flow.frame(|ui, _| ui.button("clear", "Clear all").clicked().then_some(()))
                .await;
            let confirmed = flow
                .frame(|ui, _| {
                    if ui.button("yes", "Delete every note").clicked() {
                        return Some(true);
                    }
                    ui.button("no", "Keep them").clicked().then_some(false)
                })
                .await;
            if !confirmed {
                continue;
            }
            // Plain futures compose with frames: race the request against a
            // frame the user can cancel from.
            let progress = flow.frame(|ui, _| {
                ui.label("deleting", "Deleting…");
                ui.button("cancel", "Cancel").clicked().then_some(())
            });
            let deleted = match select(pin!(delete_all()), pin!(progress)).await {
                Either::Left((deleted, _)) => deleted,
                // Dropping the request future cancels it.
                Either::Right(_) => continue,
            };
            // The flow can still mutably borrow app state between steps.
            flow.frame(|ui, notes| {
                if deleted {
                    notes.get_mut().clear();
                    return Some(());
                }
                ui.label("failed", "The server kept the notes.");
                ui.button("ok", "OK").clicked().then_some(())
            })
            .await;
        }
    });
}

/// Stores the note on the server and returns it.
async fn save(note: String) -> String { /* … */ }

/// Deletes every note on the server. Returns `true` on success.
async fn delete_all() -> bool { /* … */ }
```

## The application owns the control flow (mostly)

A pass reuses the native call stack to describe the shadow tree, depth first.
This fits Rust's memory model perfectly. The app state is a plain `&mut` on the stack,
each borrow nests with the call that makes it. The application is the sole owner
of the state and nothing touches it outside a pass.

The native UI toolkit still owns the event loop. Its callbacks only record
the events and ask for a pass, and the pass reads them where each widget is
declared.

## Design

### The shadow tree

The core keeps a keyed tree of nodes across passes:

- **Native UI nodes** hold the toolkit's objects: windows, controls, menus.
- **Core nodes** hold what the toolkit does not keep: UI state (`state`), the
  inputs of the last pass (`cache`), and running futures (`task`, `stream`,
  `flow`).

Each pass rebuilds the entire tree from the root. A node's identity is its path of
keys from the root, such as `"main" / "content" / ("note", id)`. The
caller picks each key, such as `"save"` or `("note", id)`, and must make
sure that every path is unique.

- A new path creates the node.
- A node lives as long as each pass declares its path, so its native object
  and its state survive across passes. The first pass that leaves it out
  drops its subtree promptly, along with all its tasks, streams, flows and
  native UI nodes.
- Native UI nodes are reconciled by keeping the longest common prefix and
  the longest common suffix of a container's children, between the previous pass
  and the running pass. The middle is reconciled by removing the old children
  and inserting the new ones. The algorithm is deliberately kept simple and
  makes no moves, so a moved child keeps its node but is removed and inserted
  again.

A platform crate extends the core by specifying how native UI nodes are
constructed, given their children, and destroyed. Everything else lives
here.

### The view function is `Future::poll`

| `Future` | immediate-tree |
|---|---|
| `poll(cx)`, called by the executor | `view(ui)`, called by the runner |
| `Waker::wake()` asks for another poll | `request_pass`, and the callback that records the events the native toolkit sends, ask for another pass |
| `Poll::Pending` / `Poll::Ready` | A response that is empty this pass, or holds a click, a task output, a stream item |
| Dropping a future cancels it | Dropping a node cancels its future and releases its native UI nodes |
| State kept across polls in the future | State kept across passes in the tree |

Both are polled from the root, but they record different progress. A future
progresses in time: its state is a resumption point (where the last `.await` was waiting).
The view progresses in space: its state is the shape of the tree, found by
path, and each pass describes the whole tree as it is now. `flow` lets a
future also describe the view as it progresses in time.

### A closed set of widgets

Each platform crate holds its widgets in one enum. Adding a control means
one new variant in the platform crate. This keeps the toolkit's rules (what
must run after a pass, how events subscribe, what a drop must release) in one
place, where one `match` covers every case. It favors simplicity and
correctness over third-party widgets.

## Tradeoffs

**The design bets on Rust being fast.** The whole shadow tree is rebuilt on every pass, so the
entire UI is on the hot path. Games update hundreds of thousands of entities
at 60 Hz, so there is no reason a simple GUI app can't keep up. Caching is inevitable,
but the budget is clearly defined, and any slowness is immediately noticeable.
The costly part is the call into the toolkit, so the platform crates set a property only
when its value changed. Use `cache(id, deps, body)` to skip a subtree whose inputs did
not change. For long lists, use a virtualized list, which builds only the
rows on screen.

**The usual immediate mode shortcomings:**

- **State tearing.** Widgets declared before a change show the old state
  until the next pass. Take responses before you read the state they change,
  or call `request_pass`.
- **Explicit skipping.** `cache` runs its body again only when `deps` changes. A value the
  body reads that is missing from `deps` goes stale.
- **Explicit keys.** Every path must be unique. A key in a loop must hold
  the item's stable identity, not just its position, to survive reordering.
- **Single thread.** The view runs on the UI thread. Compute intensive tasks can
  be offloaded to a threadpool and wake the UI thread with the result. Alternatively,
  work can be split across several frames.

[egui]: https://github.com/emilk/egui
[RAD Debugger]: https://github.com/EpicGamesExt/raddebugger
[`Future`]: https://doc.rust-lang.org/std/future/trait.Future.html
