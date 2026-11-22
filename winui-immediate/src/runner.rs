//! The app interface and the loop that runs passes on the UI thread.

use crate::{
    bindings::*,
    mailbox::{Handle, Mailbox},
    pass::Ui,
    startup,
    tree::NodeId,
    widget::index_source,
};
use std::{
    cell::{Cell, RefCell},
    rc::{Rc, Weak},
    sync::Arc,
};
use windows_core::{Error, IInspectable, Interface, Result};

/// The most passes one turn runs. Three covers the longest chain in use: a
/// list row reads a click, the next pass's view turns it into an action,
/// and a third draws the action's result. Raise it only for a new chain
/// that needs it, because each pass rebuilds the whole view, and a higher
/// cap hides an app that asks for a pass on every pass.
const MAX_PASSES: u32 = 3;

/// An app: state that the UI thread updates from messages and shows through
/// [`view`](Self::view).
pub trait App: 'static {
    /// A result sent to the UI thread through a [`Handle`].
    type Message: 'static + Send;

    /// Applies one message. Runs on the UI thread before the next pass.
    fn update(&mut self, message: Self::Message);

    /// Declares every open window. Runs on every pass; a pass runs after
    /// messages and after events.
    fn view(&mut self, ui: &mut Ui) -> Result<()>;
}

/// Starts WinUI on this thread, builds the app with `make`, then runs passes
/// until a pass declares no window. `make` runs on the UI thread once WinUI
/// is up, so it may create WinRT objects and use the dispatcher. Returns the
/// error of `make`, or the first error a pass returned, after closing every
/// window.
pub fn run<A: App>(make: impl 'static + FnOnce(Handle<A::Message>) -> Result<A>) -> Result<()> {
    let failure = Rc::new(RefCell::new(None));
    let launch_failure = Rc::clone(&failure);
    startup::start(move || {
        let dispatcher = DispatcherQueue::GetForCurrentThread()?;
        let mailbox = Arc::new(Mailbox::new({
            let dispatcher = dispatcher.clone();
            move || {
                // The handler captures nothing, so it can be built on any
                // thread; it finds the runner through the UI thread's local.
                let handler = DispatcherQueueHandler::new(|| {
                    CURRENT.with(|current| {
                        if let Some(current) = current.borrow().as_ref() {
                            current.turn();
                        }
                    })
                });
                matches!(dispatcher.TryEnqueue(&handler), Ok(true))
            }
        }));
        // A message sent from `make` waits in the mailbox: its wake finds
        // the runner through the thread local, which is set below.
        let app = make(Handle::new(Arc::clone(&mailbox)))?;
        let timer = dispatcher.CreateTimer()?;
        timer.SetIsRepeating(false)?;
        // The tick finds the runner through the thread local, as a message's
        // wake does, so the timer and the runner hold no cycle.
        let tick = timer.Tick(|_, _| {
            CURRENT.with(|current| {
                if let Some(current) = current.borrow().as_ref() {
                    current.turn();
                }
            })
        })?;
        let runner = Rc::new_cyclic(|weak: &Weak<Runner<A>>| {
            let wake = {
                let weak = weak.clone();
                Rc::new(move || {
                    if let Some(runner) = weak.upgrade() {
                        runner.schedule();
                    }
                }) as Rc<dyn Fn()>
            };
            let turn = {
                let weak = weak.clone();
                DispatcherQueueHandler::new(move || {
                    if let Some(runner) = weak.upgrade() {
                        runner.turn();
                    }
                })
            };
            Runner {
                ui: RefCell::new(Ui::new(wake)),
                app: RefCell::new(app),
                mailbox,
                messages: RefCell::new(Vec::new()),
                scheduled: Cell::new(false),
                missed: Cell::new(false),
                dispatcher,
                turn,
                timer,
                _tick: tick,
                failure: launch_failure,
            }
        });
        CURRENT.with(|current| *current.borrow_mut() = Some(runner.clone() as Rc<dyn Turn>));
        runner.turn();
        Ok(())
    })?;
    CURRENT.with(|current| current.borrow_mut().take());
    match failure.borrow_mut().take() {
        Some(error) => Err(error),
        None => Ok(()),
    }
}

/// The state behind [`run`], owned by the UI thread.
struct Runner<A: App> {
    ui: RefCell<Ui>,
    app: RefCell<A>,
    mailbox: Arc<Mailbox<A::Message>>,
    /// Drained messages, kept to reuse the buffer's capacity.
    messages: RefCell<Vec<A::Message>>,
    /// Whether `turn` is queued on the dispatcher and has not run yet.
    scheduled: Cell<bool>,
    /// A turn ran inside this one and did nothing, so another is due.
    missed: Cell<bool>,
    dispatcher: DispatcherQueue,
    /// Built once and queued again for every turn.
    turn: DispatcherQueueHandler,
    /// Runs a turn when the time a pass asked for with
    /// [`Ui::request_pass_after`] is up. Each turn sets it again or stops it.
    timer: DispatcherQueueTimer,
    /// Dropping it ends the tick subscription.
    _tick: windows_core::EventRevoker,
    failure: Rc<RefCell<Option<Error>>>,
}

impl<A: App> Runner<A> {
    /// Queues one turn unless one is already queued.
    fn schedule(&self) {
        if self.scheduled.replace(true) {
            return;
        }
        // A refused enqueue means the dispatcher is shutting down, so there
        // is no window left to build.
        if !matches!(self.dispatcher.TryEnqueue(&self.turn), Ok(true)) {
            self.scheduled.set(false);
        }
    }

    /// Applies queued messages, then runs passes while the last one consumed
    /// events or asked for another, up to [`MAX_PASSES`]: widgets declared
    /// before the one that read an event still show the old state.
    ///
    /// Panics when the last allowed pass still consumed events or asked for
    /// another, because that is a bug in the app.
    fn run_turn(&self) {
        self.scheduled.set(false);
        if self.failure.borrow().is_some() {
            return;
        }
        // A call that pumps messages inside a pass, such as ShellExecuteW,
        // runs this turn inside the running one. It waits for that one to
        // end, which queues it then; queuing it now would spin while the
        // call waits.
        if self.app.try_borrow_mut().is_err() || self.ui.try_borrow_mut().is_err() {
            self.missed.set(true);
            return;
        }
        let mut messages = self.messages.take();
        self.mailbox.drain(&mut messages);
        {
            let mut app = self.app.borrow_mut();
            for message in messages.drain(..) {
                app.update(message);
            }
        }
        *self.messages.borrow_mut() = messages;
        let result = tracing::debug_span!("turn").in_scope(|| {
            let mut passes = 0;
            while self.pass()? {
                passes += 1;
                assert!(
                    passes < MAX_PASSES,
                    "pass {MAX_PASSES} of one turn asked for another; the app loops or needs a longer chain"
                );
            }
            self.set_sources()
        });
        if let Err(error) = result {
            *self.failure.borrow_mut() = Some(error);
            self.ui.borrow_mut().close_all();
            self.finish();
            return;
        }
        if self.missed.take() {
            self.schedule();
        }
        let wake = self.ui.borrow_mut().take_wake();
        // A timer that fails to start leaves the window as it is until the
        // next event, which is all the app can see of it.
        let armed = self.timer.Stop().and_then(|()| match wake {
            Some(after) => {
                let after = windows_time::TimeSpan::try_from(after)
                    .unwrap_or(windows_time::TimeSpan { duration: i64::MAX });
                self.timer.SetInterval(after)?;
                self.timer.Start()
            }
            None => Ok(()),
        });
        if let Err(error) = armed {
            tracing::warn!(%error, "the timed pass did not start");
        }
    }

    /// Runs one pass: the view, then every realized list row. Returns whether
    /// it consumed events or asked for another pass.
    fn pass(&self) -> Result<bool> {
        {
            let mut ui = self.ui.borrow_mut();
            let mut app = self.app.borrow_mut();
            let consumed = ui.begin();
            app.view(&mut ui)?;
            ui.build_rows(&mut *app)?;
            let again = consumed || ui.take_pass_request();
            if ui.end()? > 0 {
                return Ok(again);
            }
        }
        // No window is left, so no pass is wanted either.
        self.finish();
        Ok(false)
    }

    /// Gives each list whose rows changed its new item source, then scrolls
    /// the rows asked for into view. Runs with no borrow held, because
    /// WinUI may ask for rows inside both.
    fn set_sources(&self) -> Result<()> {
        let mut sources = std::mem::take(&mut self.ui.borrow_mut().sources);
        for (repeater, len) in sources.drain(..) {
            repeater.SetItemsSource(&index_source(len).cast::<IInspectable>()?)?;
        }
        self.ui.borrow_mut().sources = sources;
        let mut reveals = std::mem::take(&mut self.ui.borrow_mut().reveals);
        for (repeater, index, focus) in reveals.drain(..) {
            let index = i32::try_from(index).expect("a list has fewer than 2^31 rows");
            let row = repeater.GetOrCreateElement(index)?;
            row.StartBringIntoView()?;
            // The row is the list's shell, which takes no focus, so focus
            // goes to the first control in it that does. A row with none
            // leaves focus where it was.
            if focus && let Ok(target) = FocusManager::FindFirstFocusableElement(&row) {
                target.cast::<UIElement>()?.Focus(FocusState::Keyboard)?;
            }
        }
        self.ui.borrow_mut().reveals = reveals;
        Ok(())
    }

    fn realize(&self, list: NodeId, index: usize) -> Result<UIElement> {
        let _span = tracing::trace_span!("realize", index).entered();
        let mut ui = self
            .ui
            .try_borrow_mut()
            .expect("WinUI asks for list rows during layout, never inside a pass");
        if !ui.is_list(list) {
            const E_FAIL: windows_core::HRESULT = windows_core::HRESULT(0x8000_4005_u32 as i32);
            return Err(Error::new(E_FAIL, "the list was dropped"));
        }
        let mut app = self
            .app
            .try_borrow_mut()
            .expect("WinUI asks for list rows during layout, never inside a pass");
        ui.realize(list, index, &mut *app)
    }

    fn recycle(&self, list: NodeId, element: &UIElement) {
        let mut ui = self
            .ui
            .try_borrow_mut()
            .expect("WinUI recycles list rows during layout, never inside a pass");
        if ui.is_list(list) {
            ui.recycle(list, element);
        }
    }

    /// Stops taking messages and ends the message loop. Closing the last
    /// window ends it too, but a pass that never opened one must end it here.
    fn finish(&self) {
        self.mailbox.close();
        startup::exit();
    }
}

/// Builds row `index` of `list` for WinUI's layout pass.
pub(crate) fn realize_row(list: NodeId, index: usize) -> Result<UIElement> {
    current().realize(list, index)
}

/// Returns a row WinUI no longer shows to its list's free rows.
pub(crate) fn recycle_row(list: NodeId, element: &UIElement) {
    current().recycle(list, element);
}

fn current() -> Rc<dyn Turn> {
    CURRENT
        .with(|current| current.borrow().clone())
        .expect("WinUI calls list factories only while `run` runs")
}

/// A runner seen without its app type, so capture-free handlers can reach it.
trait Turn {
    fn turn(&self);
    fn realize(&self, list: NodeId, index: usize) -> Result<UIElement>;
    fn recycle(&self, list: NodeId, element: &UIElement);
}

impl<A: App> Turn for Runner<A> {
    fn turn(&self) {
        self.run_turn();
    }

    fn realize(&self, list: NodeId, index: usize) -> Result<UIElement> {
        Runner::realize(self, list, index)
    }

    fn recycle(&self, list: NodeId, element: &UIElement) {
        Runner::recycle(self, list, element);
    }
}

thread_local! {
    /// The runner of this UI thread, while [`run`] runs.
    static CURRENT: RefCell<Option<Rc<dyn Turn>>> = const { RefCell::new(None) };
}
