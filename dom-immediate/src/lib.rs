//! A spike: an immediate-mode UI over the browser DOM, built on
//! `immediate-tree`. It has a stack, a label, a button and a text input,
//! and the shared caches, state, tasks and flows. See
//! `plans/immediate-tree.md`, "A DOM port".

// The crate is wasm only and empty on other targets.
#![cfg(target_arch = "wasm32")]

use immediate_tree::{ChildKind, Core, Edit, Lent, Node, NodeId, Shell, Visit, edits};
use std::{
    cell::{Cell, RefCell},
    future::Future,
    hash::Hash,
    pin::Pin,
    rc::Rc,
    task::{Context, Poll, Waker},
};
use wasm_bindgen::{JsCast, prelude::*};
use web_sys::{Document, Element, Event, HtmlElement, HtmlInputElement};

#[doc(inline)]
pub use immediate_tree::{Lend, Owned, OwnedFamily, lend, owned};

/// The app: state and a view of it.
pub trait App: 'static {
    fn view(&mut self, ui: &mut Ui);
}

/// Mounts `app` in `mount` and runs a pass after every event and woken
/// task. Returns at once; the browser drives the passes.
pub fn run(mount: Element, app: impl App) {
    // A panic traps with no message; the page shows it on <body data-panic>.
    std::panic::set_hook(Box::new(|info| {
        if let Some(body) = web_sys::window()
            .and_then(|window| window.document())
            .and_then(|document| document.body())
        {
            // Showing the message is best effort; the trap follows anyway.
            let _ = body.set_attribute("data-panic", &info.to_string());
        }
    }));
    let document = mount
        .owner_document()
        .expect("a mounted element belongs to a document");
    let scheduled = Rc::new(Cell::new(false));
    let runner: Rc<RefCell<Option<Runner>>> = Rc::new(RefCell::new(None));
    let schedule: Rc<dyn Fn()> = Rc::new({
        let (scheduled, runner) = (Rc::clone(&scheduled), Rc::downgrade(&runner));
        move || {
            if scheduled.replace(true) {
                return;
            }
            let (scheduled, runner) = (Rc::clone(&scheduled), runner.clone());
            let turn = Closure::once_into_js(move || {
                scheduled.set(false);
                if let Some(runner) = runner.upgrade() {
                    runner
                        .borrow_mut()
                        .as_mut()
                        .expect("the runner is set before the first turn")
                        .turn();
                }
            });
            web_sys::window()
                .expect("the page has a window")
                .set_timeout_with_callback(turn.unchecked_ref())
                .expect("setTimeout accepts a function");
        }
    });
    WAKE.with(|wake| *wake.borrow_mut() = Some(Rc::clone(&schedule)));
    let events = Rc::new(Events {
        queue: RefCell::new(Vec::new()),
        schedule: Rc::clone(&schedule),
    });
    *runner.borrow_mut() = Some(Runner {
        ui: Ui {
            // Wakers need `Send + Sync`, and a JS callback is neither, so
            // the post reaches the callback through a thread-local. Wasm
            // has one thread, so the thread-local is the page's.
            core: Core::new(|| {
                WAKE.with(|wake| {
                    if let Some(wake) = &*wake.borrow() {
                        wake();
                    }
                });
                true
            }),
            document,
            mount,
            events,
            touched: Vec::new(),
        },
        app: Box::new(app),
    });
    // Leaked on purpose: the page owns the runner until it unloads.
    std::mem::forget(runner);
    schedule();
}

thread_local! {
    /// Schedules a turn; set by [`run`].
    static WAKE: RefCell<Option<Rc<dyn Fn()>>> = const { RefCell::new(None) };
}

struct Runner {
    ui: Ui,
    app: Box<dyn App>,
}

impl Runner {
    /// Polls woken tasks, then runs passes until one leaves no event and no
    /// request behind.
    fn turn(&mut self) {
        self.ui.core.poll_woken_tasks();
        for _ in 0..8 {
            self.ui.begin();
            self.app.view(&mut self.ui);
            // Closes the page, then the root.
            let Ok(()) = immediate_tree::close(&mut self.ui);
            let Ok(()) = immediate_tree::close(&mut self.ui);
            let again = self.ui.core.take_pass_request();
            if !again && self.ui.events.queue.borrow().is_empty() {
                return;
            }
        }
        // A view that asks for a pass every pass would spin; let the next
        // turn continue instead.
        (self.ui.events.schedule)();
    }
}

/// Builds the UI. The app gets it in [`App::view`].
pub struct Ui {
    core: Core<NativeNode>,
    document: Document,
    mount: Element,
    events: Rc<Events>,
    /// Nodes that got an event this pass, whose `happened` lists the next
    /// pass clears.
    touched: Vec<NodeId>,
}

/// Events the DOM listeners queued since the last pass. A listener only
/// pushes here, so a listener the DOM runs inside a call a pass makes,
/// such as `blur` when a pass removes the focused input, touches no state
/// the pass has borrowed.
struct Events {
    queue: RefCell<Vec<(NodeId, EventKind)>>,
    schedule: Rc<dyn Fn()>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum EventKind {
    Click,
    Input,
    Blur,
}

impl EventKind {
    fn name(self) -> &'static str {
        match self {
            EventKind::Click => "click",
            EventKind::Input => "input",
            EventKind::Blur => "blur",
        }
    }
}

/// A DOM payload, public only because [`Shell`] names it.
pub struct NativeNode(DomNode);

/// Contract: `visual` lists the nodes whose elements are `element`'s
/// children, in order, for a stack or the page.
struct DomNode {
    element: Element,
    kind: Kind,
    visual: Vec<NodeId>,
    /// The text last written, for a label or a button.
    text: String,
    /// Removed before they drop, so the DOM never calls a dropped closure.
    listeners: Vec<(EventKind, Closure<dyn FnMut(Event)>)>,
    /// The events that reached the node this pass.
    happened: Vec<EventKind>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    /// The mount element; it holds the view's top controls.
    Page,
    Stack,
    Label,
    Button,
    TextInput,
}

impl Shell for Ui {
    type Native = NativeNode;
    type Error = !;

    fn core(&mut self) -> &mut Core<NativeNode> {
        &mut self.core
    }

    fn apply_visual(&mut self, node: NodeId, declared: &mut Vec<NodeId>) -> Result<(), !> {
        let tree = &mut self.core.tree;
        let Node::Native(NativeNode(native)) = &mut tree.node_mut(node).payload else {
            unreachable!("the core applies only native nodes")
        };
        assert!(
            matches!(native.kind, Kind::Page | Kind::Stack) || declared.is_empty(),
            "only a stack or the page has children"
        );
        // Afterwards `declared` holds the old list.
        std::mem::swap(&mut native.visual, declared);
        let parent = native.element.clone();
        let Node::Native(NativeNode(native)) = &tree.node(node).payload else {
            unreachable!("matched above")
        };
        for edit in edits(declared, &native.visual) {
            match edit {
                Edit::RemoveAt(index) => {
                    let child = parent
                        .children()
                        .item(index)
                        .expect("the element shows the old list");
                    // Removing a focused input fires `blur` right here; its
                    // listener only queues.
                    child.remove();
                }
                Edit::InsertAt(index, child) => {
                    let Node::Native(NativeNode(child)) = &tree.node(child).payload else {
                        unreachable!("visual children are native")
                    };
                    let next = parent.children().item(index);
                    parent
                        .insert_before(&child.element, next.as_deref().map(AsRef::as_ref))
                        .expect("an element can hold another");
                }
            }
        }
        Ok(())
    }

    fn release(&mut self, native: NativeNode) {
        for (kind, listener) in &native.0.listeners {
            native
                .0
                .element
                .remove_event_listener_with_callback(kind.name(), listener.as_ref().unchecked_ref())
                .expect("removing a listener cannot fail");
        }
    }
}

impl Ui {
    /// Starts a pass: delivers the queued events, then opens the page.
    fn begin(&mut self) {
        for node in self.touched.drain(..) {
            if self.core.tree.get(node).is_some() {
                native_mut(&mut self.core, node).happened.clear();
            }
        }
        self.core.tree.begin_pass();
        for (node, kind) in self.events.queue.take() {
            // A node dropped after its event gets nothing.
            if self.core.tree.get(node).is_none() {
                continue;
            }
            native_mut(&mut self.core, node).happened.push(kind);
            self.touched.push(node);
            self.core.tree.mark_event(node);
        }
        let root = self.core.tree.root();
        self.core.open(root);
        let mount = self.mount.clone();
        let (page, _) = self.core.visit("page", ChildKind::Logical, || {
            Node::Native(NativeNode(DomNode::new(mount, Kind::Page)))
        });
        self.core.open(page);
    }

    /// Declares the element `id` of `tag`, listening for `events` from the
    /// first pass.
    fn element(
        &mut self,
        id: impl Hash,
        tag: &str,
        kind: Kind,
        events: &[EventKind],
    ) -> (NodeId, Visit) {
        let document = &self.document;
        let (node, visit) = self.core.visit(id, ChildKind::Visual, || {
            let element = document
                .create_element(tag)
                .expect("the tag is a valid element name");
            Node::Native(NativeNode(DomNode::new(element, kind)))
        });
        if visit == Visit::Created {
            let native = native_mut(&mut self.core, node);
            for &event in events {
                let queue = Rc::clone(&self.events);
                let listener = Closure::<dyn FnMut(Event)>::new(move |_: Event| {
                    queue.queue.borrow_mut().push((node, event));
                    (queue.schedule)();
                });
                native
                    .element
                    .add_event_listener_with_callback(
                        event.name(),
                        listener.as_ref().unchecked_ref(),
                    )
                    .expect("adding a listener cannot fail");
                native.listeners.push((event, listener));
            }
        }
        (node, visit)
    }

    /// A vertical stack of the controls `body` declares.
    pub fn stack(&mut self, id: impl Hash, body: impl FnOnce(&mut Ui)) {
        let (node, _) = self.element(id, "div", Kind::Stack, &[]);
        self.core.open(node);
        body(self);
        let Ok(()) = immediate_tree::close(self);
    }

    pub fn label(&mut self, id: impl Hash, text: &str) {
        let (node, _) = self.element(id, "span", Kind::Label, &[]);
        set_text(native_mut(&mut self.core, node), text);
    }

    /// Returns whether the button was clicked since the last pass.
    pub fn button(&mut self, id: impl Hash, text: &str) -> bool {
        let (node, _) = self.element(id, "button", Kind::Button, &[EventKind::Click]);
        let native = native_mut(&mut self.core, node);
        set_text(native, text);
        native.happened.contains(&EventKind::Click)
    }

    /// An input bound to `value`. Typing writes `value`; a new `value`
    /// from the app is written to the input only when it differs from what
    /// the input shows, so the caret stays where the user put it.
    pub fn text_input(&mut self, id: impl Hash, value: &mut String) -> TextInputResponse {
        let (node, _) = self.element(
            id,
            "input",
            Kind::TextInput,
            &[EventKind::Input, EventKind::Blur],
        );
        let native = native_mut(&mut self.core, node);
        let input = native
            .element
            .dyn_ref::<HtmlInputElement>()
            .expect("a text input is an input element");
        let changed = native.happened.contains(&EventKind::Input);
        if changed {
            *value = input.value();
        } else if input.value() != *value {
            input.set_value(value);
        }
        TextInputResponse {
            changed,
            blurred: native.happened.contains(&EventKind::Blur),
            element: input.clone().unchecked_into(),
        }
    }

    /// Asks for another pass soon after this one.
    pub fn request_pass(&mut self) {
        self.core.request_pass();
    }

    /// See [`Core::state`].
    pub fn state<T: 'static>(&mut self, id: impl Hash, init: impl FnOnce() -> T) -> &mut T {
        self.core.state(id, init)
    }

    /// See [`immediate_tree::cache`].
    pub fn cached<D: 'static + PartialEq>(
        &mut self,
        id: impl Hash,
        deps: D,
        body: impl FnOnce(&mut Ui),
    ) {
        let Ok(()) = immediate_tree::cache(self, id, deps, |ui| {
            body(ui);
            Ok(())
        });
    }

    /// See [`Core::task`]. The output is taken with
    /// [`TaskResponse::take`].
    pub fn task<F>(
        &mut self,
        id: impl Hash,
        start: impl FnOnce() -> F,
    ) -> TaskResponse<'_, F::Output>
    where
        F: 'static + Future,
        F::Output: 'static,
    {
        TaskResponse(self.core.task(id, start))
    }

    /// See [`immediate_tree::flow`].
    pub fn flow<'s, S, F>(
        &mut self,
        id: impl Hash,
        state: &mut S,
        start: impl FnOnce(Flow<S::Family>) -> F,
    ) -> FlowResponse<'_, F::Output>
    where
        S: Lent<'s>,
        F: 'static + Future,
        F::Output: 'static,
    {
        let Ok(response) = immediate_tree::flow(self, id, state, |flow| start(Flow(flow)));
        FlowResponse(response)
    }
}

impl DomNode {
    fn new(element: Element, kind: Kind) -> Self {
        DomNode {
            element,
            kind,
            visual: Vec::new(),
            text: String::new(),
            listeners: Vec::new(),
            happened: Vec::new(),
        }
    }
}

fn native_mut(core: &mut Core<NativeNode>, node: NodeId) -> &mut DomNode {
    match &mut core.tree.node_mut(node).payload {
        Node::Native(NativeNode(native)) => native,
        _ => unreachable!("widget keys hold DOM payloads"),
    }
}

/// Writes `text` when it differs from the text last written.
fn set_text(native: &mut DomNode, text: &str) {
    if native.text != text {
        native.element.set_text_content(Some(text));
        native.text.clear();
        native.text.push_str(text);
    }
}

/// What [`Ui::text_input`] returns.
pub struct TextInputResponse {
    pub changed: bool,
    /// The input lost focus since the last pass.
    pub blurred: bool,
    pub element: HtmlElement,
}

/// What [`Ui::task`] returns.
pub struct TaskResponse<'u, T>(immediate_tree::TaskResponse<'u, T>);

impl<T: 'static> TaskResponse<'_, T> {
    pub fn take(&mut self) -> Option<T> {
        self.0.take()
    }

    pub fn running(&mut self) -> bool {
        self.0.running()
    }
}

/// The future's handle to what [`Ui::flow`] lends.
pub struct Flow<L: Lend>(immediate_tree::Flow<L, Ui>);

impl<L: Lend> Clone for Flow<L> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<L: Lend> Copy for Flow<L> {}

impl<L: Lend> Flow<L> {
    /// See [`immediate_tree::Flow::frame`].
    pub fn frame<R>(
        self,
        mut draw: impl FnMut(&mut Ui, &mut L::Of<'_>) -> Option<R>,
    ) -> impl Future<Output = R> {
        self.0.frame(move |ui, state| Ok(draw(ui, state)))
    }
}

/// What [`Ui::flow`] returns.
pub struct FlowResponse<'u, T>(immediate_tree::FlowResponse<'u, T>);

impl<T: 'static> FlowResponse<'_, T> {
    pub fn take(&mut self) -> Option<T> {
        self.0.take()
    }
}

/// Resolves after `millis` milliseconds, through `setTimeout`. Dropping it
/// first clears the timeout.
pub fn sleep(millis: i32) -> Sleep {
    Sleep {
        millis,
        shared: Rc::new(RefCell::new(SleepState::default())),
        timer: None,
    }
}

pub struct Sleep {
    millis: i32,
    shared: Rc<RefCell<SleepState>>,
    /// The timeout's handle and callback, once polled.
    timer: Option<(i32, Closure<dyn FnMut()>)>,
}

#[derive(Default)]
struct SleepState {
    done: bool,
    waker: Option<Waker>,
}

impl Future for Sleep {
    type Output = ();

    fn poll(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<()> {
        let mut shared = self.shared.borrow_mut();
        if shared.done {
            return Poll::Ready(());
        }
        shared.waker = Some(context.waker().clone());
        drop(shared);
        if self.timer.is_none() {
            let shared = Rc::clone(&self.shared);
            let fire = Closure::<dyn FnMut()>::new(move || {
                let waker = {
                    let mut shared = shared.borrow_mut();
                    shared.done = true;
                    shared.waker.take()
                };
                if let Some(waker) = waker {
                    waker.wake();
                }
            });
            let handle = web_sys::window()
                .expect("the page has a window")
                .set_timeout_with_callback_and_timeout_and_arguments_0(
                    fire.as_ref().unchecked_ref(),
                    self.millis,
                )
                .expect("setTimeout accepts a function");
            self.timer = Some((handle, fire));
        }
        Poll::Pending
    }
}

impl Drop for Sleep {
    fn drop(&mut self) {
        if let Some((handle, _)) = &self.timer {
            web_sys::window()
                .expect("the page has a window")
                .clear_timeout_with_handle(*handle);
        }
    }
}
