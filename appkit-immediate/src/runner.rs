//! The app interface and the loop that runs passes on the main thread.

use crate::{
    mailbox::{Handle, Mailbox},
    pass::Ui,
};
use dispatch2::DispatchQueue;
use objc2::{
    MainThreadMarker, MainThreadOnly,
    rc::{Retained, autoreleasepool},
    sel,
};
use objc2_app_kit::{
    NSApplication, NSApplicationActivationPolicy, NSEvent, NSEventModifierFlags, NSEventType,
    NSMenu, NSMenuItem,
};
use objc2_foundation::{NSPoint, NSString};
use std::{
    any::Any,
    cell::{Cell, RefCell},
    panic::{AssertUnwindSafe, catch_unwind, resume_unwind},
    rc::{Rc, Weak},
    sync::Arc,
};

/// An app: state that the main thread updates from messages and shows
/// through [`view`](Self::view).
pub trait App: 'static {
    /// A result sent to the main thread through a [`Handle`].
    type Message: 'static + Send;

    /// Applies one message. Runs on the main thread before the next pass.
    fn update(&mut self, message: Self::Message);

    /// Declares every open window. Runs on every pass; a pass runs after
    /// messages and after events.
    fn view(&mut self, ui: &mut Ui);
}

/// Starts AppKit on this thread, builds the app with `make`, then runs
/// passes until a pass declares no window. `make` runs once AppKit is up, so
/// it may create AppKit objects; a message it sends waits for the first
/// pass.
///
/// Installs a main menu with Quit, the Edit commands and the Window
/// commands. A panic in [`App::update`] or [`App::view`] closes every window,
/// and `run` then resumes it.
///
/// Panics when called off the main thread.
pub fn run<A: App>(make: impl FnOnce(Handle<A::Message>) -> A) {
    let mtm = MainThreadMarker::new().expect("run is called on the main thread");
    // Until `NSApplication::run` starts, no run-loop pool exists, and AppKit
    // autoreleases objects in setup, in `make` and in the first pass. Each
    // later pass has its own pool, inside the run loop's.
    let (runner, open) = autoreleasepool(|_| {
        let application = NSApplication::sharedApplication(mtm);
        application.setActivationPolicy(NSApplicationActivationPolicy::Regular);
        application.setMainMenu(Some(&main_menu(mtm)));
        let mailbox = Arc::new(Mailbox::new(|| {
            // The block captures nothing, so it can be built on any thread;
            // it finds the runner through the main thread's local.
            DispatchQueue::main().exec_async(turn_current);
            true
        }));
        let app = make(Handle::new(Arc::clone(&mailbox)));
        let runner = Rc::new_cyclic(|weak: &Weak<Runner<A>>| {
            let weak = weak.clone();
            let wake = Rc::new(move || {
                if let Some(runner) = weak.upgrade() {
                    runner.schedule();
                }
            });
            Runner {
                ui: RefCell::new(Ui::new(mtm, wake)),
                app: RefCell::new(app),
                mailbox,
                messages: RefCell::new(Vec::new()),
                scheduled: Cell::new(false),
                running: Cell::new(false),
                panic: RefCell::new(None),
            }
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

fn turn_current() {
    let current = CURRENT.with_borrow(Option::clone);
    if let Some(current) = current {
        current.turn();
    }
}

trait Turn {
    /// Applies queued messages and runs one pass. Returns whether a window
    /// is still open.
    fn turn(&self) -> bool;
}

struct Runner<A: App> {
    ui: RefCell<Ui>,
    app: RefCell<A>,
    mailbox: Arc<Mailbox<A::Message>>,
    /// Reused buffer for draining the mailbox.
    messages: RefCell<Vec<A::Message>>,
    /// Whether a pass is queued on the main queue.
    scheduled: Cell<bool>,
    /// Whether `NSApplication::run` was entered and must be stopped.
    running: Cell<bool>,
    /// The panic of a failed pass, which [`run`] resumes.
    panic: RefCell<Option<Box<dyn Any + Send>>>,
}

impl<A: App> Runner<A> {
    /// Queues one pass, unless one is already queued.
    fn schedule(&self) {
        if !self.scheduled.replace(true) {
            DispatchQueue::main().exec_async(turn_current);
        }
    }

    /// Applies messages and runs one pass. Returns whether events arrived
    /// and how many windows are open.
    fn pass(&self) -> (bool, usize) {
        let mut app = self.app.borrow_mut();
        let mut messages = self.messages.borrow_mut();
        self.mailbox.drain(&mut messages);
        for message in messages.drain(..) {
            app.update(message);
        }
        let mut ui = self.ui.borrow_mut();
        let consumed = ui.begin();
        app.view(&mut ui);
        (consumed, ui.end())
    }
}

impl<A: App> Turn for Runner<A> {
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
        self.mailbox.close();
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
