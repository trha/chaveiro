//! The app interface and the loop that runs passes on the main thread.

use crate::{pass::Ui, tree::NodeId, write_check};
use dispatch2::DispatchQueue;
use objc2::{
    MainThreadMarker, MainThreadOnly,
    rc::{Retained, autoreleasepool},
    sel,
};
use objc2_app_kit::{
    NSApplication, NSApplicationActivationPolicy, NSEvent, NSEventModifierFlags, NSEventType,
    NSMenu, NSMenuItem, NSView,
};
use objc2_foundation::{NSIndexSet, NSInteger, NSPoint, NSString};
use std::{
    any::Any,
    cell::{Cell, RefCell},
    panic::{AssertUnwindSafe, catch_unwind, resume_unwind},
    rc::Rc,
};

/// An app: state on the main thread that [`view`](Self::view) shows. Async
/// work runs as task nodes the view declares, with [`Ui::task`].
pub trait App: 'static {
    /// Declares every open window. Runs on every pass; a pass runs after
    /// events, after woken tasks, and when a pass asks for another. For
    /// state that changes with time, declare a task that sleeps until the
    /// change.
    fn view(&mut self, ui: &mut Ui);
}

/// Starts AppKit on this thread, builds the app with `make`, then runs
/// passes until a pass declares no window. `make` runs once AppKit is up, so
/// it may create AppKit objects.
///
/// Installs a main menu with Quit, the Edit commands and the Window
/// commands. A panic in [`App::view`] closes every window, and `run` then
/// resumes it. The tree drops with the runner before `run` returns, so
/// every task node is dropped by then.
///
/// Panics when called off the main thread.
pub fn run<A: App>(make: impl FnOnce() -> A) {
    let mtm = MainThreadMarker::new().expect("run is called on the main thread");
    write_check::install();
    // Until `NSApplication::run` starts, no run-loop pool exists, and AppKit
    // autoreleases objects in setup, in `make` and in the first pass. Each
    // later pass has its own pool, inside the run loop's.
    let (runner, open) = autoreleasepool(|_| {
        let application = NSApplication::sharedApplication(mtm);
        application.setActivationPolicy(NSApplicationActivationPolicy::Regular);
        application.setMainMenu(Some(&main_menu(mtm)));
        // Every callback finds the runner through the thread local, so none
        // holds it and no cycle forms. The post block captures nothing, so a
        // task's waker can queue it from any thread.
        let post = || {
            DispatchQueue::main().exec_async(turn_current);
            true
        };
        let wake = Rc::new(|| {
            if let Some(current) = CURRENT.with_borrow(Option::clone) {
                current.schedule();
            }
        });
        let runner = Rc::new(Runner {
            ui: RefCell::new(Ui::new(mtm, wake, post)),
            app: RefCell::new(make()),
            scheduled: Cell::new(false),
            running: Cell::new(false),
            panic: RefCell::new(None),
            deferred_reloads: RefCell::new(Vec::new()),
            deferred_recycles: RefCell::new(Vec::new()),
        });
        runner.ui.borrow().adopt_application(&application);
        CURRENT.set(Some(Rc::clone(&runner) as Rc<dyn Turn>));
        let open = runner.turn();
        if open {
            runner.running.set(true);
            #[expect(deprecated, reason = "the replacement needs macOS 14")]
            application.activateIgnoringOtherApps(true);
        }
        (runner, open)
    });
    if open {
        NSApplication::sharedApplication(mtm).run();
    }
    let panic = autoreleasepool(|_| {
        CURRENT.set(None);
        NSApplication::sharedApplication(mtm).setDelegate(None);
        let panic = runner.panic.take();
        drop(runner);
        panic
    });
    if let Some(payload) = panic {
        resume_unwind(payload);
    }
}

thread_local! {
    /// The running runner, for blocks queued from other threads.
    static CURRENT: RefCell<Option<Rc<dyn Turn>>> = const { RefCell::new(None) };
}

/// Builds row `row` of the table view `table` for AppKit's layout. Returns
/// an empty placeholder, and reloads the table after the pass, when AppKit
/// asks while a pass runs, such as during a window's first fit.
pub(crate) fn realize_row(mtm: MainThreadMarker, table: NodeId, row: usize) -> Retained<NSView> {
    let current = CURRENT.with_borrow(Option::clone);
    current
        .and_then(|current| current.realize(table, row))
        .unwrap_or_else(|| NSView::new(mtm))
}

/// Returns the row whose cell view is at address `view` to `table`'s free
/// rows.
pub(crate) fn recycle_row(table: NodeId, view: usize) {
    let current = CURRENT.with_borrow(Option::clone);
    if let Some(current) = current {
        current.recycle(table, view);
    }
}

/// Runs a turn now, from an AppKit callback that needs the app's answer
/// before it returns, such as a menu about to open. Does nothing while a
/// pass runs.
pub(crate) fn turn_now() {
    let current = CURRENT.with_borrow(Option::clone);
    if let Some(current) = current
        && !current.busy()
    {
        current.turn();
    }
}

fn turn_current() {
    let current = CURRENT.with_borrow(Option::clone);
    if let Some(current) = current {
        current.turn();
    }
}

trait Turn {
    /// Polls woken tasks and runs one pass. Returns whether a window is
    /// still open.
    fn turn(&self) -> bool;
    /// Queues a turn, unless one is already queued.
    fn schedule(&self);
    /// Builds a table row; [`None`] when it cannot be built now.
    fn realize(&self, table: NodeId, row: usize) -> Option<Retained<NSView>>;
    fn recycle(&self, table: NodeId, view: usize);
    /// Whether a pass or a row build holds the app.
    fn busy(&self) -> bool;
}

struct Runner<A: App> {
    ui: RefCell<Ui>,
    app: RefCell<A>,
    /// Whether a pass is queued on the main queue.
    scheduled: Cell<bool>,
    /// Whether `NSApplication::run` was entered and must be stopped.
    running: Cell<bool>,
    /// The panic of a failed pass, which [`run`] resumes.
    panic: RefCell<Option<Box<dyn Any + Send>>>,
    /// Table views that asked for rows while a pass ran, reloaded after it.
    deferred_reloads: RefCell<Vec<NodeId>>,
    /// Rows AppKit removed while a pass ran, recycled after it.
    deferred_recycles: RefCell<Vec<(NodeId, usize)>>,
}

impl<A: App> Runner<A> {
    /// Polls woken tasks and runs one pass, then in debug builds the idle
    /// check. Returns whether events arrived and how many windows are open.
    fn pass(&self) -> (bool, usize) {
        let woken = self.ui.borrow_mut().poll_woken_tasks();
        let (consumed, windows) = self.view();
        // A pass that took nothing new in and asked for no rewrite has
        // caught up, so the same view again must write nothing.
        let ui = self.ui.borrow();
        let settled = !ui.invalidated && !ui.requested.get();
        drop(ui);
        if cfg!(debug_assertions) && !consumed && !woken && settled && windows > 0 {
            write_check::begin();
            let (consumed, windows) = self.view();
            let writes = write_check::end();
            assert!(
                consumed || writes.is_empty(),
                "{}",
                self.ui.borrow().describe_writes(&writes)
            );
            return (consumed, windows);
        }
        (consumed, windows)
    }

    /// Runs the app's view once. Returns whether events arrived and how many
    /// windows are open.
    fn view(&self) -> (bool, usize) {
        let mut app = self.app.borrow_mut();
        let mut ui = self.ui.borrow_mut();
        let consumed = ui.begin();
        app.view(&mut ui);
        ui.build_rows(&mut *app);
        (consumed, ui.end())
    }

    /// Recycles and reloads what table views asked for during the pass, and
    /// reloads the tables the pass changed. Runs with no borrow held,
    /// because AppKit asks for rows inside `reloadData`.
    fn settle_tables(&self) {
        for (table, view) in self.deferred_recycles.take() {
            self.recycle(table, view);
        }
        let mut tables = std::mem::take(&mut self.ui.borrow_mut().reloads);
        {
            let ui = self.ui.borrow();
            for table in self.deferred_reloads.take() {
                if let Some(table) = ui.table_of(table) {
                    tables.push(table);
                }
            }
        }
        let selections = std::mem::take(&mut self.ui.borrow_mut().selections);
        for table in tables {
            table.reloadData();
        }
        for (table, selected) in selections {
            match selected {
                Some(row) => {
                    table.selectRowIndexes_byExtendingSelection(
                        &NSIndexSet::indexSetWithIndex(row),
                        false,
                    );
                    table.scrollRowToVisible(row as NSInteger);
                }
                None => unsafe { table.deselectAll(None) },
            }
        }
    }

    /// Stores the panic, closes every window and stops the app, for a panic
    /// outside a pass, in a row function.
    fn fail(&self, payload: Box<dyn Any + Send>) {
        *self.panic.borrow_mut() = Some(payload);
        autoreleasepool(|_| self.ui.borrow_mut().close_all());
        if self.running.replace(false) {
            stop(self.ui.borrow().mtm);
        }
    }
}

impl<A: App> Turn for Runner<A> {
    fn schedule(&self) {
        if !self.scheduled.replace(true) {
            DispatchQueue::main().exec_async(turn_current);
        }
    }

    fn realize(&self, table: NodeId, row: usize) -> Option<Retained<NSView>> {
        if self.panic.borrow().is_some() {
            return None;
        }
        let (Ok(mut ui), Ok(mut app)) = (self.ui.try_borrow_mut(), self.app.try_borrow_mut())
        else {
            // A pass is running: AppKit lays out inside it, such as for a
            // window's first fit.
            self.deferred_reloads.borrow_mut().push(table);
            return None;
        };
        if ui.table_len(table).is_none_or(|len| row >= len) {
            return None;
        }
        // A panic must not unwind into AppKit's layout.
        let outcome = autoreleasepool(|_| {
            catch_unwind(AssertUnwindSafe(|| ui.realize(table, row, &mut *app)))
        });
        drop((ui, app));
        match outcome {
            Ok(view) => Some(view),
            Err(payload) => {
                self.fail(payload);
                None
            }
        }
    }

    fn recycle(&self, table: NodeId, view: usize) {
        let Ok(mut ui) = self.ui.try_borrow_mut() else {
            self.deferred_recycles.borrow_mut().push((table, view));
            return;
        };
        if ui.table_len(table).is_some() {
            ui.recycle(table, view);
        }
    }

    fn busy(&self) -> bool {
        self.ui.try_borrow_mut().is_err() || self.app.try_borrow_mut().is_err()
    }

    fn turn(&self) -> bool {
        self.scheduled.set(false);
        if self.panic.borrow().is_some() {
            return false;
        }
        // A panic must not unwind into libdispatch or AppKit frames, so it
        // stops here and `run` resumes it.
        let outcome = autoreleasepool(|_| catch_unwind(AssertUnwindSafe(|| self.pass())));
        let windows = match outcome {
            Ok((consumed, windows)) => {
                if windows > 0 {
                    autoreleasepool(|_| self.settle_tables());
                    // Each waits until its menu closes, and passes run
                    // meanwhile, so none may hold a borrow.
                    let popups = std::mem::take(&mut self.ui.borrow_mut().popups);
                    for (menu, view) in popups {
                        autoreleasepool(|_| crate::widget::pop_up(&menu, &view));
                    }
                }
                // A widget declared before the one that read an event shows
                // the old state, so one more pass catches it up.
                if consumed && windows > 0 {
                    self.schedule();
                }
                windows
            }
            Err(payload) => {
                *self.panic.borrow_mut() = Some(payload);
                autoreleasepool(|_| self.ui.borrow_mut().close_all());
                0
            }
        };
        if windows > 0 {
            return true;
        }
        if self.running.replace(false) {
            stop(self.ui.borrow().mtm);
        }
        false
    }
}

/// The app menu with Quit, the Edit menu and the Window menu. Edit and Window
/// items have no target, so the responder chain routes them to the key
/// window and its field editor.
fn main_menu(mtm: MainThreadMarker) -> Retained<NSMenu> {
    let item = |title: &str, action, key: &str| unsafe {
        NSMenuItem::initWithTitle_action_keyEquivalent(
            NSMenuItem::alloc(mtm),
            &NSString::from_str(title),
            Some(action),
            &NSString::from_str(key),
        )
    };
    let submenu = |title: &str, items: &[Retained<NSMenuItem>]| {
        let menu = NSMenu::initWithTitle(NSMenu::alloc(mtm), &NSString::from_str(title));
        for entry in items {
            menu.addItem(entry);
        }
        let holder = NSMenuItem::new(mtm);
        holder.setSubmenu(Some(&menu));
        (holder, menu)
    };
    let bar = NSMenu::new(mtm);
    let (app_holder, _) = submenu("App", &[item("Quit", sel!(terminate:), "q")]);
    let (edit_holder, _) = submenu(
        "Edit",
        &[
            item("Undo", sel!(undo:), "z"),
            item("Redo", sel!(redo:), "Z"),
            NSMenuItem::separatorItem(mtm),
            item("Cut", sel!(cut:), "x"),
            item("Copy", sel!(copy:), "c"),
            item("Paste", sel!(paste:), "v"),
            item("Select All", sel!(selectAll:), "a"),
        ],
    );
    let (window_holder, window_menu) = submenu(
        "Window",
        &[
            item("Minimize", sel!(performMiniaturize:), "m"),
            item("Close", sel!(performClose:), "w"),
        ],
    );
    bar.addItem(&app_holder);
    bar.addItem(&edit_holder);
    bar.addItem(&window_holder);
    NSApplication::sharedApplication(mtm).setWindowsMenu(Some(&window_menu));
    bar
}

/// Makes `NSApplication::run` return. `stop` takes effect after the next
/// event, so post one.
fn stop(mtm: MainThreadMarker) {
    let application = NSApplication::sharedApplication(mtm);
    application.stop(None);
    let event = NSEvent::otherEventWithType_location_modifierFlags_timestamp_windowNumber_context_subtype_data1_data2(
        NSEventType::ApplicationDefined,
        NSPoint::new(0.0, 0.0),
        NSEventModifierFlags::empty(),
        0.0,
        0,
        None,
        0,
        0,
        0,
    )
    .expect("an application-defined event needs no window");
    application.postEvent_atStart(&event, true);
}
