//! The app interface and the loop that runs passes on the main thread.

use crate::{
    mailbox::{Handle, Mailbox},
    pass::Ui,
};
use dispatch2::DispatchQueue;
use objc2::MainThreadMarker;
use objc2_app_kit::{
    NSApplication, NSApplicationActivationPolicy, NSEvent, NSEventModifierFlags, NSEventType,
};
use objc2_foundation::NSPoint;
use std::{
    cell::{Cell, RefCell},
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

/// Starts AppKit on this thread, runs `start`, then runs passes until a pass
/// declares no window.
///
/// Panics when called off the main thread.
pub fn run<A: App>(mut app: A, start: impl FnOnce(&mut A, Handle<A::Message>)) {
    let mtm = MainThreadMarker::new().expect("run is called on the main thread");
    let application = NSApplication::sharedApplication(mtm);
    application.setActivationPolicy(NSApplicationActivationPolicy::Regular);
    let mailbox = Arc::new(Mailbox::new(|| {
        // The block captures nothing, so it can be built on any thread; it
        // finds the runner through the main thread's local.
        DispatchQueue::main().exec_async(turn_current);
        true
    }));
    start(&mut app, Handle::new(Arc::clone(&mailbox)));
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
        }
    });
    CURRENT.set(Some(Rc::clone(&runner) as Rc<dyn Turn>));
    if runner.turn() {
        runner.running.set(true);
        #[expect(deprecated, reason = "the replacement needs macOS 14")]
        application.activateIgnoringOtherApps(true);
        application.run();
    }
    CURRENT.set(None);
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
}

impl<A: App> Runner<A> {
    /// Queues one pass, unless one is already queued.
    fn schedule(&self) {
        if !self.scheduled.replace(true) {
            DispatchQueue::main().exec_async(turn_current);
        }
    }
}

impl<A: App> Turn for Runner<A> {
    fn turn(&self) -> bool {
        self.scheduled.set(false);
        let mut app = self.app.borrow_mut();
        let mut messages = self.messages.borrow_mut();
        self.mailbox.drain(&mut messages);
        for message in messages.drain(..) {
            app.update(message);
        }
        let mut ui = self.ui.borrow_mut();
        let consumed = ui.begin();
        app.view(&mut ui);
        let windows = ui.end();
        if windows > 0 {
            // A widget declared before the one that read an event shows the
            // old state, so one more pass catches it up.
            if consumed {
                self.schedule();
            }
            return true;
        }
        self.mailbox.close();
        if self.running.replace(false) {
            stop(ui.mtm);
        }
        false
    }
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
