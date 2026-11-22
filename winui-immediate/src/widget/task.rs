//! Futures and streams owned by tree nodes: each runs while passes declare
//! it.

use crate::{
    pass::{ChildKind, Payload, Ui},
    tree::NodeId,
};
use futures_core::Stream;
use std::{
    any::Any,
    collections::VecDeque,
    future::Future,
    hash::Hash,
    marker::PhantomData,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll, Wake, Waker},
};
// Loom checks the wake protocol with its own atomics and lock; `Arc` stays
// std's, because `Waker::from` needs it.
#[cfg(loom)]
use loom::sync::{
    Mutex,
    atomic::{AtomicBool, Ordering},
};
#[cfg(not(loom))]
use std::sync::{
    Mutex,
    atomic::{AtomicBool, Ordering},
};

impl Ui {
    /// Declares the task `id`. The first pass that declares it calls `start`
    /// and polls the future once, so a future that is ready at once has its
    /// output in that pass. Later passes do not call `start`, so it may move
    /// its inputs in.
    ///
    /// The future runs on the UI thread, and its waker may be called from
    /// any thread. A woken future is polled at the start of the next turn,
    /// before its passes, so the turn's first pass can take the output.
    ///
    /// The task lives while each pass declares it. The first pass that
    /// leaves it out drops the future, which cancels whatever it awaits.
    /// Declared outside every window, it lives across window changes.
    ///
    /// Panics when `id` held a task with another output type.
    pub fn task<F>(
        &mut self,
        id: impl Hash,
        start: impl FnOnce() -> F,
    ) -> TaskResponse<'_, F::Output>
    where
        F: 'static + Future,
        F::Output: 'static,
    {
        let (node, visit) = self
            .visit(id, ChildKind::Logical, || {
                let future = start();
                let future: TaskFuture =
                    Box::pin(async move { Box::new(future.await) as Box<dyn Any> });
                Ok(Payload::Task(TaskNode {
                    future: Some(future),
                    output: None,
                    wake: None,
                }))
            })
            .expect("creating a task makes no WinUI call");
        if visit == crate::tree::Visit::Created {
            let wake = Arc::new(TaskWake {
                node,
                queued: AtomicBool::new(false),
                shared: Arc::clone(&self.tasks),
            });
            self.task_mut(node).wake = Some(wake);
            self.poll_task(node);
        }
        TaskResponse {
            ui: self,
            node,
            output: PhantomData,
        }
    }

    /// Declares the stream `id`. The first pass that declares it calls
    /// `start` and polls the stream until it returns `Pending`, so items
    /// ready at once are in that pass. Later passes do not call `start`.
    ///
    /// A woken stream is polled at the start of the next turn, again until
    /// `Pending`, and the node keeps every item it yielded, in order, until
    /// a pass takes it with [`StreamResponse::next`]. Items wait in a queue
    /// that the node reuses, so a steady stream allocates no memory per item.
    ///
    /// The stream lives while each pass declares it, as a task does: the
    /// first pass that leaves it out drops the stream and its untaken items.
    ///
    /// Panics when `id` held a stream with another item type.
    pub fn stream<S>(
        &mut self,
        id: impl Hash,
        start: impl FnOnce() -> S,
    ) -> StreamResponse<'_, S::Item>
    where
        S: 'static + Stream,
        S::Item: 'static,
    {
        let (node, visit) = self
            .visit(id, ChildKind::Logical, || {
                Ok(Payload::Stream(StreamNode {
                    stream: Some(Box::pin(start())),
                    items: Box::new(VecDeque::<S::Item>::new()),
                    wake: None,
                }))
            })
            .expect("creating a stream makes no WinUI call");
        if visit == crate::tree::Visit::Created {
            let wake = Arc::new(TaskWake {
                node,
                queued: AtomicBool::new(false),
                shared: Arc::clone(&self.tasks),
            });
            self.stream_mut(node).wake = Some(wake);
            self.poll_stream(node);
        }
        StreamResponse {
            ui: self,
            node,
            item: PhantomData,
        }
    }

    /// Polls every task and stream woken since the last turn. Called by the
    /// runner at the start of a turn. A woken node that has dropped is
    /// skipped.
    pub(crate) fn poll_woken_tasks(&mut self) {
        let mut woken = self.tasks.take_woken();
        for node in woken.drain(..) {
            // A slot reused by another node has a new generation, so a live
            // id still names the node that was woken.
            match self.tree.get(node).map(|node| &node.payload) {
                Some(Payload::Task(_)) => self.poll_task(node),
                Some(Payload::Stream(_)) => self.poll_stream(node),
                // A flow draws, so it runs only where the view declares it;
                // its wake only asked for this turn.
                Some(Payload::Flow(_)) => {}
                // The node dropped after its wake.
                None => {}
                Some(
                    Payload::Root
                    | Payload::Window(_)
                    | Payload::Control(_)
                    | Payload::Cache(_)
                    | Payload::State(_)
                    | Payload::Menu(_)
                    | Payload::Shortcut(_)
                    | Payload::Flyout(_),
                ) => unreachable!("only task, stream and flow nodes have wakers"),
            }
        }
        // Handed back to reuse its capacity, unless a wake already put a
        // new list there.
        let mut slot = self
            .tasks
            .woken
            .lock()
            .expect("no code panics while holding the woken lock");
        if slot.is_empty() {
            *slot = woken;
        }
    }

    /// Polls the task `node` once, if it still runs.
    fn poll_task(&mut self, node: NodeId) {
        let task = self.task_mut(node);
        let Some(mut future) = task.future.take() else {
            return;
        };
        let wake = Arc::clone(task.wake.as_ref().expect("set when the task was created"));
        wake.begin_poll();
        let waker = Waker::from(wake);
        // Polled with the future out of the tree, so its code holds no
        // borrow of the node.
        let polled = future.as_mut().poll(&mut Context::from_waker(&waker));
        let task = self.task_mut(node);
        match polled {
            Poll::Ready(output) => task.output = Some(output),
            Poll::Pending => task.future = Some(future),
        }
    }

    fn task_mut(&mut self, node: NodeId) -> &mut TaskNode {
        match &mut self.tree.node_mut(node).payload {
            Payload::Task(task) => task,
            _ => unreachable!("task keys hold tasks"),
        }
    }

    /// Polls the stream `node` until it returns `Pending` or ends, if it
    /// still runs.
    fn poll_stream(&mut self, node: NodeId) {
        let stream = self.stream_mut(node);
        let Some(mut polled) = stream.stream.take() else {
            return;
        };
        let wake = Arc::clone(
            stream
                .wake
                .as_ref()
                .expect("set when the stream was created"),
        );
        wake.begin_poll();
        let waker = Waker::from(wake);
        // The stream is out of the node while it runs, as a task's future
        // is; its items go straight into the node's queue.
        let stream = self.stream_mut(node);
        let ended = polled
            .as_mut()
            .poll_into(&mut *stream.items, &mut Context::from_waker(&waker))
            .is_ready();
        if !ended {
            stream.stream = Some(polled);
        }
    }

    fn stream_mut(&mut self, node: NodeId) -> &mut StreamNode {
        match &mut self.tree.node_mut(node).payload {
            Payload::Stream(stream) => stream,
            _ => unreachable!("stream keys hold streams"),
        }
    }
}

/// What [`Ui::stream`] returns.
pub struct StreamResponse<'u, T> {
    ui: &'u mut Ui,
    node: NodeId,
    item: PhantomData<fn() -> T>,
}

impl<T: 'static> StreamResponse<'_, T> {
    /// The oldest item not taken yet. Items yielded since the last take
    /// come out in the order the stream yielded them.
    ///
    /// Widgets declared before this call show the state from before the
    /// item, as with [`TaskResponse::take`].
    pub fn next(&mut self) -> Option<T> {
        self.items().pop_front()
    }

    /// Whether the stream has ended and every item was taken.
    pub fn finished(&mut self) -> bool {
        self.ui.stream_mut(self.node).stream.is_none() && self.items().is_empty()
    }

    fn items(&mut self) -> &mut VecDeque<T> {
        self.ui
            .stream_mut(self.node)
            .items
            .downcast_mut::<VecDeque<T>>()
            .expect("a stream id keeps one item type")
    }
}

/// Contract: `items` is a `VecDeque<S::Item>` for the `S` that `stream`
/// was made from, and `stream` is [`None`] once that stream ended.
pub(crate) struct StreamNode {
    stream: Option<Pin<Box<dyn PollInto>>>,
    items: Box<dyn Any>,
    /// Set right after the node is made, because it names the node.
    wake: Option<Arc<TaskWake>>,
}

/// A stream whose items go into a queue it knows the type of, so a node
/// holds any stream without boxing each item.
trait PollInto {
    /// Polls until `Pending` or the end, pushing each item onto `items`,
    /// which is the `VecDeque` of this stream's item type. Ready means the
    /// stream ended.
    fn poll_into(self: Pin<&mut Self>, items: &mut dyn Any, context: &mut Context<'_>) -> Poll<()>;
}

impl<S: Stream> PollInto for S
where
    S::Item: 'static,
{
    fn poll_into(
        mut self: Pin<&mut Self>,
        items: &mut dyn Any,
        context: &mut Context<'_>,
    ) -> Poll<()> {
        let items = items
            .downcast_mut::<VecDeque<S::Item>>()
            .expect("the node made the queue for this stream's items");
        loop {
            match self.as_mut().poll_next(context) {
                Poll::Ready(Some(item)) => items.push_back(item),
                Poll::Ready(None) => return Poll::Ready(()),
                Poll::Pending => return Poll::Pending,
            }
        }
    }
}

/// What [`Ui::task`] returns.
pub struct TaskResponse<'u, T> {
    ui: &'u mut Ui,
    node: NodeId,
    output: PhantomData<fn() -> T>,
}

impl<T: 'static> TaskResponse<'_, T> {
    /// The output, once, in the first pass after the future finished.
    /// After a take the node holds nothing; stop declaring it to remove it.
    ///
    /// Widgets declared before this call show the state from before the
    /// output. Take outputs before reading the state they change, or call
    /// [`Ui::request_pass`] when that is not possible.
    pub fn take(&mut self) -> Option<T> {
        let output = self.ui.task_mut(self.node).output.take()?;
        Some(
            *output
                .downcast::<T>()
                .unwrap_or_else(|_| panic!("a task id keeps one output type")),
        )
    }

    /// Whether the future has not finished yet.
    pub fn running(&mut self) -> bool {
        self.ui.task_mut(self.node).future.is_some()
    }
}

/// The future of a task, with its output boxed so nodes hold one type.
type TaskFuture = Pin<Box<dyn Future<Output = Box<dyn Any>>>>;

/// Contract: at most one of `future` and `output` is set. Both are empty
/// after the output was taken.
pub(crate) struct TaskNode {
    future: Option<TaskFuture>,
    output: Option<Box<dyn Any>>,
    /// Set right after the node is made, because it names the node.
    wake: Option<Arc<TaskWake>>,
}

/// The wakes of every task of one [`Ui`], shared with their wakers.
///
/// Contract: `posted` is true from the moment a wake posts a turn until the
/// turn takes `woken`, so a burst of wakes posts one turn.
pub(crate) struct TaskWakes {
    woken: Mutex<Vec<NodeId>>,
    posted: AtomicBool,
    /// Asks the UI thread for a turn. Returns false when it can no longer
    /// run one.
    post: Box<dyn 'static + Send + Sync + Fn() -> bool>,
}

impl TaskWakes {
    pub(crate) fn new(post: impl 'static + Send + Sync + Fn() -> bool) -> Arc<Self> {
        Arc::new(TaskWakes {
            woken: Mutex::new(Vec::new()),
            posted: AtomicBool::new(false),
            post: Box::new(post),
        })
    }

    /// Takes the tasks woken so far, in wake order. Call it on the UI
    /// thread at the start of a turn.
    fn take_woken(&self) -> Vec<NodeId> {
        // Cleared before the list is taken: a wake after this point posts
        // a new turn, and a wake before it is in the list taken below.
        // Relaxed is enough, because the woken lock orders both sides.
        self.posted.store(false, Ordering::Relaxed);
        std::mem::take(
            &mut *self
                .woken
                .lock()
                .expect("no code panics while holding the woken lock"),
        )
    }
}

/// The waker of one task.
pub(crate) struct TaskWake {
    node: NodeId,
    /// The task is in `shared.woken` and has not been polled since.
    queued: AtomicBool,
    shared: Arc<TaskWakes>,
}

impl Wake for TaskWake {
    fn wake(self: Arc<Self>) {
        self.wake_by_ref();
    }

    fn wake_by_ref(self: &Arc<Self>) {
        self.signal();
    }
}

impl TaskWake {
    /// The waker of the node `node`, which wakes through `shared`.
    pub(crate) fn new(node: NodeId, shared: &Arc<TaskWakes>) -> Arc<Self> {
        Arc::new(TaskWake {
            node,
            queued: AtomicBool::new(false),
            shared: Arc::clone(shared),
        })
    }

    /// Lists the task for the next turn's poll, unless it is listed and
    /// not polled yet. Runs on any thread.
    fn signal(&self) {
        // Release publishes the caller's writes. A wake that returns here
        // takes no lock, so it reaches the poll only through this flag,
        // which `begin_poll` acquires.
        if self.queued.swap(true, Ordering::Release) {
            return;
        }
        self.shared
            .woken
            .lock()
            .expect("no code panics while holding the woken lock")
            .push(self.node);
        // Relaxed is enough, because the woken lock orders it with
        // `take_woken`.
        if !self.shared.posted.swap(true, Ordering::Relaxed) {
            // A refused post means the UI thread is shutting down, so the
            // task never runs again and drops with the tree.
            let _ = (self.shared.post)();
        }
    }

    /// Unlists the task right before a poll, so a wake during the poll
    /// lists it again. A swap, because a plain store would read no wake:
    /// the swap reads the last one and acquires the writes before it.
    pub(crate) fn begin_poll(&self) {
        self.queued.swap(false, Ordering::Acquire);
    }
}

#[cfg(all(test, not(loom)))]
mod tests;

#[cfg(all(test, loom))]
mod loom_tests;
