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

/// Starts WinUI on this thread, runs `start`, then runs passes until a pass
/// declares no window. Returns the first error a pass returned, after closing
/// every window.
pub fn run<A: App>(app: A, start: impl 'static + FnOnce(&mut A, Handle<A::Message>)) -> Result<()> {
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
                dispatcher,
                turn,
                failure: launch_failure,
            }
        });
        CURRENT.with(|current| *current.borrow_mut() = Some(runner.clone() as Rc<dyn Turn>));
        start(
            &mut runner.app.borrow_mut(),
            Handle::new(Arc::clone(&runner.mailbox)),
        );
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
    dispatcher: DispatcherQueue,
    /// Built once and queued again for every turn.
    turn: DispatcherQueueHandler,
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

    /// Applies queued messages, then runs a pass, and a second one when the
    /// first consumed events: widgets declared before the one that read an
    /// event still show the old state.
    fn run_turn(&self) {
        self.scheduled.set(false);
        if self.failure.borrow().is_some() {
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
            self.pass()
                .and_then(|consumed| match consumed {
                    true => self.pass().map(drop),
                    false => Ok(()),
                })
                .and_then(|()| self.set_sources())
        });
        if let Err(error) = result {
            *self.failure.borrow_mut() = Some(error);
            self.ui.borrow_mut().close_all();
            self.finish();
        }
    }

    /// Runs one pass: the view, then every realized list row. Returns whether
    /// it consumed events.
    fn pass(&self) -> Result<bool> {
        let consumed = {
            let mut ui = self.ui.borrow_mut();
            let mut app = self.app.borrow_mut();
            let consumed = ui.begin();
            app.view(&mut ui)?;
            ui.build_rows(&mut *app)?;
            if ui.end()? > 0 {
                return Ok(consumed);
            }
            consumed
        };
        self.finish();
        Ok(consumed)
    }

    /// Gives each list whose rows changed its new item source. Runs with no
    /// borrow held, because WinUI may ask for rows inside `SetItemsSource`.
    fn set_sources(&self) -> Result<()> {
        let mut sources = std::mem::take(&mut self.ui.borrow_mut().sources);
        for (repeater, len) in sources.drain(..) {
            repeater.SetItemsSource(&index_source(len).cast::<IInspectable>()?)?;
        }
        self.ui.borrow_mut().sources = sources;
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
