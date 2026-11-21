//! Spike: which reuse model a view-based `NSTableView` supports for the
//! `table_view` widget.
//!
//! `pool` mode never calls `makeViewWithIdentifier`; it keeps its own free
//! list, fed by `tableView:didRemoveRowView:forRow:`. `queue` mode calls
//! `makeViewWithIdentifier` and keeps every shell alive in a `Vec`, as the
//! tree would. Both scroll 10,000 rows through scripted steps, call
//! `reloadData` in the middle, and print counters at the end.
//!
//! Run: `cargo run -p appkit-immediate --example table_reuse -- pool|queue [auto]`.
//! `auto` turns on `usesAutomaticRowHeights`.

// The program is macOS only. On other targets it is empty, so it checks but
// does not link.
#![cfg_attr(not(target_os = "macos"), no_main)]
#![cfg(target_os = "macos")]

use dispatch2::{DispatchQueue, DispatchTime};
use objc2::{
    DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send, rc::Retained,
    runtime::ProtocolObject,
};
use objc2_app_kit::{
    NSApplication, NSApplicationActivationPolicy, NSBackingStoreType, NSControlTextEditingDelegate,
    NSScrollView, NSTableColumn, NSTableRowView, NSTableView, NSTableViewDataSource,
    NSTableViewDelegate, NSTextField, NSUserInterfaceItemIdentification, NSView, NSWindow,
    NSWindowStyleMask,
};
use objc2_foundation::{NSInteger, NSObject, NSObjectProtocol, NSPoint, NSRect, NSSize, NSString};
use std::{
    cell::{Cell, RefCell},
    collections::{HashMap, HashSet},
    time::Duration,
};

const ROWS: NSInteger = 10_000;

fn main() {
    let mode = match std::env::args().nth(1).as_deref() {
        Some("pool") => Mode::Pool,
        Some("queue") => Mode::Queue,
        _ => panic!("usage: table_reuse pool|queue [auto]"),
    };
    let automatic = std::env::args().nth(2).as_deref() == Some("auto");
    let mtm = MainThreadMarker::new().expect("main runs on the main thread");
    let application = NSApplication::sharedApplication(mtm);
    application.setActivationPolicy(NSApplicationActivationPolicy::Regular);
    let window = unsafe {
        NSWindow::initWithContentRect_styleMask_backing_defer(
            NSWindow::alloc(mtm),
            NSRect::new(NSPoint::new(200.0, 200.0), NSSize::new(300.0, 400.0)),
            NSWindowStyleMask::Titled,
            NSBackingStoreType::Buffered,
            false,
        )
    };
    unsafe { window.setReleasedWhenClosed(false) };
    let scroll = NSScrollView::new(mtm);
    let table = NSTableView::new(mtm);
    let column =
        NSTableColumn::initWithIdentifier(NSTableColumn::alloc(mtm), &NSString::from_str("column"));
    table.addTableColumn(&column);
    table.setHeaderView(None);
    table.setUsesAutomaticRowHeights(automatic);
    let source = Source::new(mtm, mode);
    unsafe {
        table.setDataSource(Some(ProtocolObject::from_ref(&*source)));
        table.setDelegate(Some(ProtocolObject::from_ref(&*source)));
    }
    scroll.setDocumentView(Some(&table));
    window.setContentView(Some(&scroll));
    window.makeKeyAndOrderFront(None);
    STATE.set(Some((source, table)));
    step(0);
    application.run();
}

/// The scripted scroll positions, then `reloadData`, then more positions.
fn script() -> Vec<Action> {
    let mut actions = Vec::new();
    actions.extend((0..ROWS).step_by(37).map(Action::Scroll));
    actions.extend((0..ROWS).rev().step_by(53).map(Action::Scroll));
    actions.push(Action::Reload);
    actions.extend((0..60).map(|index| Action::Scroll((index * 7919) % ROWS)));
    actions.push(Action::Reload);
    actions.extend((0..ROWS / 2).step_by(41).map(Action::Scroll));
    actions.push(Action::Report);
    actions
}

/// Runs action `index`, then schedules the next on a later run-loop turn,
/// so AppKit lays out between steps.
fn step(index: usize) {
    let actions = script();
    STATE.with_borrow(|state| {
        let (source, table) = state.as_ref().expect("set before the first step");
        match actions[index] {
            Action::Scroll(row) => table.scrollRowToVisible(row),
            Action::Reload => {
                source.ivars().reloads.set(source.ivars().reloads.get() + 1);
                table.reloadData();
            }
            Action::Report => {
                source.report(table);
                std::process::exit(0);
            }
        }
    });
    let when = DispatchTime::try_from(Duration::from_millis(4)).expect("a small delay fits");
    DispatchQueue::main()
        .after(when, move || step(index + 1))
        .expect("the main queue accepts work");
}

thread_local! {
    static STATE: RefCell<Option<(Retained<Source>, Retained<NSTableView>)>> =
        const { RefCell::new(None) };
}

#[derive(Clone, Copy)]
enum Action {
    Scroll(NSInteger),
    Reload,
    Report,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Mode {
    Pool,
    Queue,
}

struct Counters {
    mode: Mode,
    /// Every shell ever made, kept alive as the tree would.
    shells: Vec<Retained<NSTextField>>,
    /// Shell address to its index in `shells`.
    known: HashMap<usize, usize>,
    /// Shells AppKit currently shows, by address.
    shown: HashSet<usize>,
    /// Pool mode: shells free for reuse.
    free: Vec<usize>,
    view_requests: u64,
    queue_hits: u64,
    queue_misses: u64,
    pool_hits: u64,
    added: u64,
    removed: u64,
    removed_without_cell: u64,
    /// A shell returned for a row while it was still shown for another.
    shown_twice: u64,
    /// A remove for a shell that was not shown.
    removed_unshown: u64,
    /// A remove for a shell already in the free list.
    freed_twice: u64,
    /// Pool mode: a shell handed out while AppKit still holds it in a row.
    reused_while_held: u64,
}

pub(crate) struct Ivars {
    counters: RefCell<Counters>,
    reloads: Cell<u64>,
}

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[ivars = Ivars]
    struct Source;

    unsafe impl NSObjectProtocol for Source {}

    unsafe impl NSTableViewDataSource for Source {
        #[unsafe(method(numberOfRowsInTableView:))]
        fn number_of_rows(&self, _table: &NSTableView) -> NSInteger {
            ROWS
        }
    }

    unsafe impl NSControlTextEditingDelegate for Source {}

    unsafe impl NSTableViewDelegate for Source {
        #[unsafe(method_id(tableView:viewForTableColumn:row:))]
        fn view_for(
            &self,
            table: &NSTableView,
            _column: Option<&NSTableColumn>,
            row: NSInteger,
        ) -> Option<Retained<NSView>> {
            let mtm = self.mtm();
            let mut counters = self.ivars().counters.borrow_mut();
            counters.view_requests += 1;
            let shell = match counters.mode {
                Mode::Queue => {
                    let identifier = NSString::from_str("row");
                    let reused = unsafe { table.makeViewWithIdentifier_owner(&identifier, None) };
                    match reused {
                        Some(view) => {
                            counters.queue_hits += 1;
                            let address = Retained::as_ptr(&view).addr();
                            let index = *counters
                                .known
                                .get(&address)
                                .expect("the queue returns only our shells");
                            counters.shells[index].clone()
                        }
                        None => {
                            counters.queue_misses += 1;
                            let shell = counters.make(mtm);
                            shell.setIdentifier(Some(&identifier));
                            shell
                        }
                    }
                }
                Mode::Pool => match counters.free.pop() {
                    Some(address) => {
                        counters.pool_hits += 1;
                        let index = counters.known[&address];
                        let shell = counters.shells[index].clone();
                        if unsafe { shell.superview() }.is_some() {
                            counters.reused_while_held += 1;
                        }
                        shell
                    }
                    None => counters.make(mtm),
                },
            };
            let address = Retained::as_ptr(&shell).addr();
            if !counters.shown.insert(address) {
                counters.shown_twice += 1;
            }
            shell.setStringValue(&NSString::from_str(&format!("row {row}")));
            Some(Retained::into_super(Retained::into_super(shell)))
        }

        #[unsafe(method(tableView:didAddRowView:forRow:))]
        fn did_add(&self, _table: &NSTableView, _row_view: &NSTableRowView, _row: NSInteger) {
            self.ivars().counters.borrow_mut().added += 1;
        }

        #[unsafe(method(tableView:didRemoveRowView:forRow:))]
        fn did_remove(&self, _table: &NSTableView, row_view: &NSTableRowView, _row: NSInteger) {
            let mut counters = self.ivars().counters.borrow_mut();
            counters.removed += 1;
            let Some(cell) = row_view.viewAtColumn(0) else {
                counters.removed_without_cell += 1;
                return;
            };
            let address = Retained::as_ptr(&cell).addr();
            if !counters.shown.remove(&address) {
                counters.removed_unshown += 1;
            }
            if counters.mode == Mode::Pool {
                if counters.free.contains(&address) {
                    counters.freed_twice += 1;
                } else {
                    counters.free.push(address);
                }
            }
        }
    }
);

impl Source {
    fn new(mtm: MainThreadMarker, mode: Mode) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(Ivars {
            counters: RefCell::new(Counters {
                mode,
                shells: Vec::new(),
                known: HashMap::new(),
                shown: HashSet::new(),
                free: Vec::new(),
                view_requests: 0,
                queue_hits: 0,
                queue_misses: 0,
                pool_hits: 0,
                added: 0,
                removed: 0,
                removed_without_cell: 0,
                shown_twice: 0,
                removed_unshown: 0,
                freed_twice: 0,
                reused_while_held: 0,
            }),
            reloads: Cell::new(0),
        });
        unsafe { msg_send![super(this), init] }
    }

    fn report(&self, table: &NSTableView) {
        let counters = self.ivars().counters.borrow();
        let visible = table.rowsInRect(table.visibleRect()).length;
        // Shells AppKit holds in a row right now, counted from the table.
        let mut held = 0;
        for row in 0..table.numberOfRows() {
            if table
                .viewAtColumn_row_makeIfNecessary(0, row, false)
                .is_some()
            {
                held += 1;
            }
        }
        let held_and_free = counters
            .free
            .iter()
            .filter(|address| {
                let shell = &counters.shells[counters.known[address]];
                unsafe { shell.superview() }.is_some()
            })
            .count();
        println!("mode {:?}", counters.mode);
        println!("reloads {}", self.ivars().reloads.get());
        println!("visible rows {visible}");
        println!("rows holding a view {held}");
        println!("shells made {}", counters.shells.len());
        println!("view requests {}", counters.view_requests);
        println!(
            "queue hits {} misses {}",
            counters.queue_hits, counters.queue_misses
        );
        println!("pool hits {}", counters.pool_hits);
        println!(
            "row views added {} removed {}",
            counters.added, counters.removed
        );
        println!("removed without cell {}", counters.removed_without_cell);
        println!("shown by our count {}", counters.shown.len());
        println!("shown twice {}", counters.shown_twice);
        println!("removed but not shown {}", counters.removed_unshown);
        println!("freed twice {}", counters.freed_twice);
        println!("reused while held {}", counters.reused_while_held);
        println!("free and still in a row {held_and_free}");
    }
}

impl Counters {
    fn make(&mut self, mtm: MainThreadMarker) -> Retained<NSTextField> {
        let shell = NSTextField::labelWithString(&NSString::new(), mtm);
        let address = Retained::as_ptr(&shell).addr();
        self.known.insert(address, self.shells.len());
        self.shells.push(shell.clone());
        shell
    }
}
