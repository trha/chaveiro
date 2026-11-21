# appkit-immediate widgets

Status: Plan

Parent plans: `plans/winui-immediate.md` (the widget set and the pass model).
This plan copies that design to AppKit. The two crates share no code and may
drift apart. Where this plan and the WinUI plan differ, this plan wins for
`appkit-immediate`.

## Goal

Give `appkit-immediate` the widgets a macOS Chaveiro app needs:
`window`, `stack_view`, `box_view`, `label`, `button`, `grid`, `table_view`,
`text_field`, `secure_text_field`, `pop_up_button`, `combo_box`, `native`,
`cached` and `state`, plus field and `Response`
options for forms. Also fix the runner gaps that the POC exposed: the
autorelease pool, the main menu, quit requests and panics.

The consumer is a separate `chaveiro-appkit` crate, and it shares almost no
UI code with `chaveiro-winui`. So `winui-immediate` serves as a proven
design to borrow from, not as an API to match. The pass model (one `view`
over `&mut App`, write on change, one shared target) carries over because it
works. A widget or option is ported only when it fits macOS, and its API
follows AppKit where the two differ. Each section below marks where the
AppKit version differs from WinUI and why.

## Out of scope

A `Backend` trait or a shared core crate: the two crates are allowed to
drift. API compatibility with `winui-immediate`: no code calls both. The
`chaveiro-appkit` app crate itself, which gets its own plan. Also out: `.app` bundling, custom drawing, theming, and a custom secure
text field.

## Facts this plan rests on

**The POC** (`appkit-immediate/`, uncommitted on 2026-11-20). `tree.rs`,
`children.rs` and `mailbox.rs` are copied from `winui-immediate`, and their 14
tests pass. One `Target` ObjC class (`src/events.rs`) is the action target and
window delegate for the whole UI. It maps the sender's address to its
`NodeId` through `register` and `forget`. A pass runs from a main-queue
dispatch block (`src/runner.rs`), and a pass that consumed events schedules
one more. The smoke example was checked through System Events: clicks, the
background message, closing the second window, and `run` returning after the
main window closed.

**Autorelease, measured on 2026-11-21.** Running the POC smoke example with
`OBJC_DEBUG_MISSING_POOLS=YES` logged 12,270 objects autoreleased with no pool
in place, so leaked: `CALayer`, `CAContext`, `CUIRenditionKey`, XPC proxies and
others. AppKit creates them inside window and view setup during the first
pass, which runs before `NSApplication::run` and so outside any run-loop pool.

**Table view reuse, measured on 2026-11-21** with
`appkit-immediate/examples/table_reuse.rs`: 10,000 rows, about 600 scripted
scroll steps, `reloadData` twice, fixed and automatic row heights.

| | pool, fixed | queue, fixed | pool, auto | queue, auto |
|---|---|---|---|---|
| shells made | 19 | 57 | 28 | 84 |
| row views added / removed | 12,200 / 12,181 | 12,219 / 12,200 | 17,743 / 17,715 | 17,772 / 17,744 |
| shown twice, removed unshown, freed twice | 0 | 0 | 0 | 0 |

In pool mode the delegate's own free list, fed by
`tableView:didRemoveRowView:forRow:`, stays at the shown row count, and every
remove matches one shown shell. The removed row view still holds its cell
when the cell is handed out again (6,391 and 8,941 times), and reparenting it
causes no double show. In queue mode `makeViewWithIdentifier` misses exactly
one screen of rows after each `reloadData` (19 × 3, 28 × 3), so AppKit drops
its queue on reload. A tree that keeps every shell would then hold one dead
screen of slots per reload.

**WinUI changes since the POC copy** (`git log 405edf7..HEAD`, commit
`cd6ca43` and `plans/winui-immediate.md` WP-09). `tree.rs`, `children.rs`,
`mailbox.rs` and `widget/cache.rs` are unchanged, so the copies are current.
New are `combo_box`, `editable_combo_box`, `info_bar` with `Severity`, and
`card`. `Response` gains `enabled`, `style`, `align`, `width`, `padding`,
`access_key`, `focus` and `left`. Fields (text, password and combo boxes)
gain `header`, `required`, `error` and `hint`, which share an `ErrorLine`
whose 16px line is reserved on first use, plus `FieldHandle` with
`Ui::set_error` and `Ui::focus`. Text and password boxes gain
`enter_pressed`. Buttons gain `accent` and `busy`. Windows gain `mica`,
`compact` and `fit_to_content`, and start cloaked until their first pass
ends or their first fit settles (at most 3 resizes). `take_password` returns
`Zeroizing<String>` and wipes the HSTRING. `run(make)` replaces
`run(app, start)`: `make` gets the `Handle` on the UI thread and returns
`Result<App>`. Open WinUI gaps: `card` colors are fixed at creation, and
`invalidate` does not rewrite `accent` or `busy`.

**WinUI widgets** (`winui-immediate/src/widget/*.rs`, before WP-09):
- `grid(id, &[Length], body)` with `Length::{Auto, Pixels, Star}` sets
  `RowDefinitions` and `Grid.Row` by position.
- `list(id, len, revision, RowFn<A>)` uses an `ItemsRepeater` whose element
  factory builds rows during layout. Rows come from a slot pool, and
  `list_state` and `retain_list_state` hold per-item state.
- `text_box(id, placeholder, &mut String)` copies the text into the app's
  value on `TextChanged`, and holds app writes back while an IME composition
  is open.
- `password_box(id, placeholder)` returns a `PasswordField` with `take()` and
  `handle()`, and `Ui::take_password` reads the box and clears it.
- `native(id, create, update)` wraps a control this crate has no widget for.
- `cached(id, deps, body)` and `state(id, init)` live in `widget/cache.rs`.
- Every widget returns `Result`, and `Response::invalidate` forces rewrites.

**AppKit:**
- AppKit runs on the main thread only, and objc2 enforces this with
  `MainThreadMarker`.
- Controls send one action to one target. A window, text field or table has
  one delegate, which gets every event kind.
- Layout is Auto Layout, solved once per run-loop turn, after our pass.
- `NSStackView` acts like a flex box. Nothing in AppKit has star sizing.
- A text field is edited through the window's shared field editor, and
  `controlTextDidChange:` fires on each change.
- A view-based `NSTableView` asks its delegate for row views during layout,
  and reports removed rows through `tableView:didRemoveRowView:forRow:`.

## Design

### Differences from WinUI at a glance

| Area | winui-immediate | appkit-immediate | Why |
|---|---|---|---|
| Errors | every widget returns `Result` | plain values | AppKit setters do not fail |
| Event wiring | one delegate per event kind, subscribed on the first ask | one shared `Target` for the whole UI, keyed by sender address, registered when a widget is created | a control has one target, and an object has one delegate |
| Layout | measure and arrange; `StackPanel` gives children infinite length | Auto Layout; `NSStackView` gives children the stack's own size | engine model |
| Grid | `Grid` + `RowDefinitions` + `Grid.SetRow` | plain `NSView` + constraints rebuilt on change | no star sizing in AppKit |
| List | `ScrollViewer` + `ItemsRepeater` + element factory | `NSScrollView` + view-based `NSTableView`, with the shared `Target` as data source and delegate | AppKit's virtualizing list |
| IME | `CompositionStarted`/`Ended` events | read `hasMarkedText` on the field editor during the pass | AppKit has no composition event |
| Window close | `AppWindow.Closing` with cancel | `windowShouldClose:` returns NO | |
| Quit | the last window closes | also Cmd+Q, Dock Quit and logout, through `applicationShouldTerminate:` | macOS convention |
| Menu | none needed | default app, Edit and Window menus | Cmd+C, Cmd+V and Cmd+Q route through the menu |
| Memory | COM reference counts | `Retained` plus an autorelease pool per pass | autoreleased returns |
| Run | `run(make) -> Result<()>` | `run(make)`, no `Result` | `make` builds the app on the main thread once AppKit is up |
| Cloak until fit | DWM cloak | the window is ordered front only after its first fit settles | AppKit shows nothing before `orderFront` |
| Enter, focus left | `KeyDown` and `LostFocus` delegates | text field action (`fire:`) and `controlTextDidEndEditing:` | target-action fires on Enter |
| Password copies | `PasswordBox.Password` returns an HSTRING | `NSSecureTextField.stringValue` returns an NSString; the field editor keeps a buffer | see "Secure text field" |

### Names

Widgets take the AppKit class name of the view they create, in snake case,
so a reader who knows AppKit finds the class from the call. WinUI names
appear only in the WinUI column below.

| winui-immediate | appkit-immediate | AppKit class |
|---|---|---|
| `window` | `window` | `NSWindow` |
| `stack` | `stack_view` | `NSStackView` |
| `card` | `box_view` | `NSBox` (`box` is a Rust keyword) |
| `grid` | `grid` | none: a plain `NSView` with constraints |
| `label` | `label` | `NSTextField` from `labelWithString` |
| `button` | `button` | `NSButton` |
| `text_box`, `TextBoxResponse` | `text_field`, `TextFieldResponse` | `NSTextField` |
| `password_box`, `PasswordField`, `PasswordHandle` | `secure_text_field`, `SecureTextFieldResponse`, `SecureTextFieldHandle` | `NSSecureTextField` |
| `combo_box` | `pop_up_button`, `PopUpButtonResponse` | `NSPopUpButton` |
| `editable_combo_box` | `combo_box`, `ComboBoxResponse` | `NSComboBox` |
| `list`, `ListId`, `list_state` | `table_view`, `TableViewId`, `table_view_state` | `NSTableView` in an `NSScrollView` |
| `native` | `native` | any `NSView` |
| `close_requested` | `should_close` | `windowShouldClose:` |
| `quit_requested` | `should_terminate` | `applicationShouldTerminate:` |

`combo_box` means a different control on each crate: on WinUI it is the
drop-down list, here it is the editable box with suggestions, as in AppKit.

### Runner

```rust
// src/runner.rs
pub trait App: 'static {
    type Message: 'static + Send;
    fn update(&mut self, message: Self::Message);
    fn view(&mut self, ui: &mut Ui);
}

/// Builds the app with `make` on the main thread once AppKit is up, then
/// runs passes until a pass declares no window. Panics off the main thread.
/// Re-raises the first panic of `make` or a pass after closing every window.
pub fn run<A: App>(make: impl FnOnce(Handle<A::Message>) -> A);
```

`Runner::turn` and the `make` call each run inside
`objc2::rc::autoreleasepool`. Without it the first pass leaks its AppKit
temporaries (see the measurement in the facts). On run-loop passes the pool
nests inside the run loop's own and frees each pass's temporaries when the
pass ends. `turn` runs
the pass inside `std::panic::catch_unwind(AssertUnwindSafe(..))`. On a panic
it stores the payload, calls `Ui::close_all` (ported from WinUI `pass.rs`),
closes the mailbox, and stops the app. `run` re-raises the payload with
`resume_unwind` after `application.run()` returns. WinUI has no such
boundary. On AppKit a panic must not unwind through libdispatch or ObjC
frames.

`run` installs a main menu once, built by `fn main_menu(mtm) ->
Retained<NSMenu>`:
- the app menu holds Quit (`terminate:`, Cmd+Q);
- Edit holds Undo, Redo, Cut, Copy, Paste and Select All, with nil targets,
  so the responder chain reaches the field editor;
- Window holds Minimize (`performMiniaturize:`) and Close (`performClose:`,
  which reaches `windowShouldClose:`).

The shared `Target` also implements `NSApplicationDelegate`.
`applicationShouldTerminate:` pushes `EventKind::QuitRequested` on the root
node and returns `NSTerminateCancel`. The app sees it as:

```rust
impl Ui {
    /// Whether the user asked to quit (Cmd+Q, Dock, logout) since the last
    /// pass. The app quits by declaring no window.
    pub fn should_terminate(&self) -> bool;
}
```

### Events

`EventKind` gains `TextChanged` and `QuitRequested`. `Target` gains these
protocol methods: `NSTextFieldDelegate::controlTextDidChange:` (the sender is
the notification's object), `NSApplicationDelegate::applicationShouldTerminate:`,
and the table methods under "List". `Events` keeps `register`, `forget`,
`happened` and `begin_pass`. Rows are the one case that needs no `register`:
they are reached through their table's sender.

### Response and stale writes

This ports WinUI's `Response::invalidate`, `ControlNode::stale` and
`must_write`, so an imperative write followed by `invalidate` is not lost.

```rust
impl Response<'_> {
    pub fn view(&self) -> Retained<NSView>;
    /// Makes the next pass write every managed property of this view.
    pub fn invalidate(&mut self);
}
```

Every view this crate creates gets
`setTranslatesAutoresizingMaskIntoConstraints(false)`, except a window's
content view.

### Window

The API is unchanged from the POC: `window(id, title, body) ->
WindowResponse` and `should_close()`, renamed from the POC's
`close_requested`. New: after the first pass that
creates the window, the runner sets the content size to the larger of
`content.fittingSize` and 360×200, then centers the window. WinUI sizes a
window by its own default instead.

### Stack view

`stack_view(id, orientation, spacing, body)`, renamed from the POC's `stack`. The doc drops WinUI's warning that a list never
scrolls inside a stack. Under Auto Layout a list in a stack scrolls once the
stack has a bounded height.

### Grid

```rust
pub fn grid(&mut self, id: impl Hash, rows: &[Length], body: impl FnOnce(&mut Ui)) -> Response<'_>;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Length { Auto, Pixels(f64), Star(f64) }

// widget.rs
Control::Grid {
    container: Retained<NSView>,
    rows: Vec<Length>,
    /// Active constraints placing the children; replaced as a set.
    constraints: Vec<Retained<NSLayoutConstraint>>,
}
```

The grid differs most from WinUI. Its rows become constraints, and a pure
function decides which ones:

```rust
// src/widget/grid/rules.rs, free of AppKit types so it can be tested
pub(crate) enum Rule {
    /// The child's top edge sits on the container's top edge (row 0) or on
    /// the bottom edge of the row above.
    Top { row: usize },
    /// The last child's bottom edge sits on the container's bottom edge.
    /// Left out when no row is `Star`, so the grid hugs its content.
    Bottom { row: usize },
    Height { row: usize, points: f64 },
    /// `height(row) = ratio * height(unit)`, where `unit` is the first
    /// `Star` row.
    Share { row: usize, unit: usize, ratio: f64 },
}
pub(crate) fn rules(rows: &[Length], children: usize) -> Vec<Rule>;
```

Every child is also pinned to the container's leading and trailing edges.
`Auto` rows get no height rule, so their intrinsic size applies.

`Pass::apply_visual` gets a `Grid` arm. It applies the child edits with
`addSubview` and `removeFromSuperview`. When the children or `rows` changed,
it calls `NSLayoutConstraint::deactivateConstraints(old)`, builds the
constraints from `rules`, and activates them. This takes the place of WinUI's
`place_in_rows`. The grid panics when it has more children than rows.

### Label and button

Unchanged from the POC, apart from `must_write` and `stale`. The button stays
subscribed from creation: one map entry costs less than WinUI's lazy
subscription logic.

### Text field

```rust
pub fn text_field(&mut self, id: impl Hash, placeholder: &str, value: &mut String) -> TextFieldResponse<'_>;
impl TextFieldResponse<'_> { pub fn changed(&self) -> bool; }

Control::TextField { field: Retained<NSTextField>, text: String, placeholder: String }
```

The contract matches WinUI: the control owns the text while the user edits,
and app writes never break the caret or a composition. The mechanism differs
in three ways.

- **Read.** When `TextChanged` happened, read `field.stringValue()` and copy
  it into `value` if it differs from `text`.
- **Composition.** WinUI tracks a `composing` flag from events. Here the pass
  checks `field.currentEditor()`, downcasts it to `NSTextView`, and asks
  `hasMarkedText()`. A write while that is true waits for a later pass, and
  the text-changed event that ends the composition schedules that pass.
- **Write while editing.** When `currentEditor` exists, write through it with
  `setString` and restore the old selection, clamped to the new length.
  Setting `stringValue` resets the caret. When no editor exists, use
  `setStringValue`.

The `Target` is the field's delegate and is registered for it.

### Secure text field

```rust
pub fn secure_text_field(&mut self, id: impl Hash, placeholder: &str) -> SecureTextFieldResponse<'_>;
impl SecureTextFieldResponse<'_> {
    pub fn take(&mut self) -> Zeroizing<String>;
    pub fn handle(&self) -> SecureTextFieldHandle;
}
impl Ui { pub fn take_password(&mut self, handle: SecureTextFieldHandle) -> Zeroizing<String>; }
```

`NSSecureTextField` keeps WinUI's read-once model. While
it has focus, AppKit turns on secure keyboard input by itself.

**Known gap, accepted on 2026-11-20.** `stringValue` returns an NSString copy
that Rust cannot wipe, and the field editor keeps its own buffer. Neither
clears on lock. `take` returns `Zeroizing<String>` like WinUI. It copies the
UTF-16 units with `getCharacters:range:` into a `Zeroizing<Vec<u16>>`, clears
the field, decodes into the returned `Zeroizing<String>` (WinUI's
`secret_from_wide`), and wipes the buffer. The NSString itself stays
unwiped. `changed`, `is_empty`, `password_is_empty`, `return_pressed`,
`header`, `required` and `error` port as on WinUI; `changed` comes from
`controlTextDidChange:`. A separate spike checks these copies
against `.claude/memory/threat-model.md`. This plan does not close the gap.

### Table view

```rust
pub type RowFn<A> = fn(&mut A, &mut Ui, TableViewId, usize);
pub fn table_view<A: App>(&mut self, id: impl Hash, len: usize, revision: u64, row: RowFn<A>) -> TableViewId;
pub fn table_view_state<T: 'static>(&mut self, table: TableViewId, item: u64, init: impl FnOnce() -> T) -> &mut T;
pub fn retain_table_view_state(&mut self, table: TableViewId, keep: impl FnMut(u64) -> bool);

Control::TableView {
    scroll: Retained<NSScrollView>,
    table: Retained<NSTableView>,
    data: Box<TableData>, // ported from WinUI: row fn, slots, free list, len, revision, item_state
}
Control::Row { shell: Retained<NSView>, content: Option<NodeId> }
```

The API and the slot pool match WinUI. The AppKit parts differ:

- **Views.** `NSScrollView` wraps a view-based `NSTableView` with one column,
  no header, `usesAutomaticRowHeights = true`, and the shared `Target` as
  both data source and delegate. The table is registered for its node.
- **Row count.** `numberOfRowsInTableView:` returns `TableData::len`.
- **Row views.** `tableView:viewForTableColumn:row:` calls into the runner
  through the `CURRENT` thread-local. The runner takes a free slot or
  `Tree::adopt`s a new one, builds the row with `RowFn`, and returns the
  shell. This is WinUI's `RowFactory::GetElement`.
- **Recycling.** `tableView:didRemoveRowView:forRow:` returns the slot to the
  free list, like WinUI's `RecycleElement`. AppKit's own
  `makeViewWithIdentifier` reuse is not used: its queue empties on
  `reloadData` (see the measurement in the facts), which would strand slots.
  A slot taken from the free list may still sit in a removed row view;
  returning it to AppKit reparents it, which the spike showed is safe.
- **Changes in count or revision.** These queue `table.reloadData()` in
  `Ui::reloads`. The runner calls it after the pass with no borrow held,
  because AppKit may ask for rows inside it. This replaces WinUI's
  `Ui::sources`.

The steady-pass rebuild of realized rows matches WinUI
(`Ui::build_rows` after `view`).

### Form widgets

```rust
pub fn pop_up_button(&mut self, id: impl Hash, items: &[&str], selected: &mut usize) -> PopUpButtonResponse<'_>;
pub fn combo_box(&mut self, id: impl Hash, items: &[&str], text: &mut String) -> ComboBoxResponse<'_>;
pub fn box_view(&mut self, id: impl Hash, width: f64, body: impl FnOnce(&mut Ui)) -> Response<'_>;
```

| WinUI | AppKit | Difference |
|---|---|---|
| `ComboBox` | `pop_up_button`: `NSPopUpButton`; the pick fires `fire:` | |
| editable `ComboBox` | `combo_box`: `NSComboBox`; text reaches the app on Enter or focus left | |
| `card` | `box_view`: `NSBox` (`.custom`, 8pt corners, `controlBackgroundColor` fill, `separatorColor` border) holding the stack | AppKit's dynamic colors follow the theme, so the WinUI color gap disappears |
| field `header` | a label above the control, in a 3-row field stack (header, control, `ErrorLine`) | WinUI controls have a built-in header |
| `ErrorLine` | a horizontal stack of an `exclamationmark.circle` symbol and a 12pt label in `systemRedColor`; a ≥16pt height constraint reserves the line | same contract |
| `required` | appends " *" to the header | WinUI uses a header template |
| `accent` | `default_button`: `keyEquivalent = "\r"`: AppKit draws the default button in the accent color, and Return presses it | Return also clicks it |
| `busy` | a small spinning `NSProgressIndicator` before the title, in a horizontal stack | |
| `mica` | `visual_effect`: `NSVisualEffectView` as the content view's background | |
| `compact` | `fullSizeContentView`, transparent title bar, hidden minimize and zoom buttons, not resizable | |
| `fit_to_content` | `size_to_fit`: `setContentSize(fittingSize)` after layout; ordered front once it settles | no cloak needed |

`Response` options map as follows:
- `enabled` uses `NSControl.isEnabled`.
- `align` sets hugging priority and a leading, center or trailing alignment
  constraint inside a stack.
- `width` adds a width constraint, kept in the node.
- `padding` uses `NSStackView.edgeInsets`.
- `make_first_responder` (WinUI `focus`) calls `window.makeFirstResponder`;
  `Ui::make_first_responder(field)` takes a `FieldHandle`.
- `did_end_editing` (WinUI `left`) comes from `controlTextDidEndEditing:`.
- `return_pressed` (WinUI `enter_pressed`) comes from the text field's
  action, which AppKit sends on Return.
- `style` takes a small `Style` enum, `Title`, `Body` or `Caption`, mapped
  to `NSFont.preferredFont(forTextStyle:)`, because AppKit has no resource
  dictionary.

### Native

```rust
pub fn native(
    &mut self,
    id: impl Hash,
    create: impl FnOnce(MainThreadMarker) -> Retained<NSView>,
    update: impl FnOnce(&NSView),
) -> Response<'_>;
```

### Cached and state

`widget/cache.rs` is ported almost as it is, with `Payload::Cache` and
`Payload::State`. `Tree::find` and `Tree::has_event` move back out of the
test-only impl. The differences are no `Result` and no WinUI types.

## Tests

1. Copied core tests (`tree`, `children`, `mailbox`): unchanged, they must
   keep passing.
2. Cache tests: ported from `winui-immediate/src/widget/cache/tests.rs` where
   they drive only the tree.
3. `grid::rules` property tests, each a separate check:
   - every child gets exactly one `Top`;
   - `Bottom` appears exactly when some row is `Star`;
   - for any two `Star` rows, the ratios they imply equal the ratio of their
     weights;
   - `Auto` rows get no height rule.

   The oracle reads the rules back into heights for a given unit height,
   which is a different shape from the builder.
4. Example `examples/smoke.rs`, extended. A script,
   `appkit-immediate/tools/smoke.sh`, drives it through System Events like
   the POC check did:
   - clicks update the count;
   - typing changes `value`;
   - an app write while typing keeps the caret;
   - closing a window works;
   - Cmd+Q produces `should_terminate`;
   - `run` returns.
5. Example `examples/vault_table.rs`, ported from WinUI: 10,000 rows in a
   `Star` grid row, in a `table_view`. Scroll it and check that row views stay bounded. Log the
   slot count.
6. Panic check: an example flag makes `view` panic on the third pass. `run`
   must close the windows and re-raise the panic without aborting.

## Work breakdown

The main agent owns every step, one at a time, because all steps edit
`pass.rs` or `events.rs`.

## Steps

1. Commit the POC and this plan. Rename the POC's `stack` to
   `stack_view` and `close_requested` to `should_close`.
2. Runner: `run(make)`, autorelease pool around `turn` and `make` (recheck with
   `OBJC_DEBUG_MISSING_POOLS=YES`: no missing-pool lines), panic boundary with `close_all`, main menu, app
   delegate, `should_terminate`. Runs tests 1, 4 (quit and close) and 6.
3. `Response::invalidate`, `stale` and `must_write`; `translatesAutoresizing…`
   off; window sized to fit. Runs test 4.
4. Grid: `rules` with its property tests, the `Control::Grid` arm in
   `apply_visual`. Runs test 3 and a smoke grid with Auto, Pixels and Star
   rows.
5. Text field, then secure text field. Runs test 4 (typing, caret, composition with
   a Japanese input source by hand).
6. Form widgets and options: field stack with `ErrorLine`, `FieldHandle`,
   `pop_up_button`, `combo_box`, `box_view`, `default_button`, `busy`, window `visual_effect`,
   `compact` and `size_to_fit`, `Response` options. Runs a login-form smoke script
   (pick, Return, error line reserved, disabled pop-up button).
7. Table view with the slot pool, `table_view_state` and deferred `reloadData`. Runs
   test 5.
8. `native`, `cached`, `state`. Runs test 2.
9. `cargo +nightly fmt`, clippy, and `cargo doc --no-deps
   --document-private-items` with no broken links. Update the memory notes.

## Decisions to confirm

None open.

## Settled

- **No `Backend` trait** (2026-11-20). It is too early, and each OS gets its
  own client.
- **One shared `Target` for the whole UI** (2026-11-20). One ObjC object
  keyed by sender address.
- **Grid from constraints** (2026-11-20). It keeps the WinUI `Length` API,
  including `Star`.
- **Password box keeps the WinUI API and records the copy gap** (2026-11-20).
  A custom secure field costs too much for now.
- **Scope covers all widgets plus the runner fixes** (2026-11-20).
- **The API follows AppKit, not `winui-immediate`** (2026-11-21). A separate
  `chaveiro-appkit` crate is the only consumer, and it shares almost no UI
  code with `chaveiro-winui`, so matching signatures buys nothing.
- **The slot pool owns row reuse, not `makeViewWithIdentifier`**
  (2026-11-21). The spike counts balance in pool mode, and AppKit's queue
  drops a screen of views on every `reloadData`.
- **Widget names follow AppKit classes** (2026-11-21). See "Names".
- **Options take AppKit names** (2026-11-21): `default_button`,
  `visual_effect`, `size_to_fit`, `make_first_responder`, `did_end_editing`,
  `return_pressed`. Options AppKit has no term for keep their names:
  `header`, `required`, `error`, `hint`, `busy`, `padding`, `compact`.
- **`grid` keeps its name** (2026-11-21). No AppKit class matches it.
- **No `access_key`** (2026-11-21). macOS has no Alt mnemonics, and no
  shared code needs the method to exist.
- **`style` takes a `Style` enum** (2026-11-21): `Title`, `Body`,
  `Caption`, mapped to `NSFont.preferredFont(forTextStyle:)`. AppKit has no
  style keys, and an enum fails at build time instead of at runtime.
- **No `info_bar`** (2026-11-21). A message bar is not an established
  macOS pattern, and AppKit has no control for it. Errors go in a field's
  `ErrorLine`; a macOS screen that needs a page-level message decides its
  own form, such as an alert sheet. `Severity` is not ported.
- **Autorelease pool per pass and around `make`** (2026-11-21). The
  measurement shows 12,270 leaked objects without it.
