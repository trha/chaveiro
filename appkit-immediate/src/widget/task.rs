//! Futures and streams owned by tree nodes: each runs while passes declare
//! it.

use crate::pass::Ui;
use futures_core::Stream;
use std::{future::Future, hash::Hash};

impl Ui {
    /// Declares the task `id`. The first pass that declares it calls `start`
    /// and polls the future once, so a future that is ready at once has its
    /// output in that pass. Later passes do not call `start`, so it may move
    /// its inputs in.
    ///
    /// The future runs on the main thread, and its waker may be called from
    /// any thread. A woken future is polled at the start of the next turn,
    /// before its pass, so the turn's first pass can take the output.
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
        TaskResponse(self.core.task(id, start))
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
        StreamResponse(self.core.stream(id, start))
    }

    /// Polls every task and stream woken since the last turn. Called by the
    /// runner at the start of a turn. Returns whether any node was woken.
    pub(crate) fn poll_woken_tasks(&mut self) -> bool {
        self.core.poll_woken_tasks()
    }
}

/// What [`Ui::stream`] returns.
pub struct StreamResponse<'u, T>(immediate_tree::StreamResponse<'u, T>);

impl<T: 'static> StreamResponse<'_, T> {
    /// The oldest item not taken yet. Items yielded since the last take
    /// come out in the order the stream yielded them.
    ///
    /// Widgets declared before this call show the state from before the
    /// item, as with [`TaskResponse::take`].
    pub fn next(&mut self) -> Option<T> {
        self.0.next()
    }

    /// Whether the stream has ended and every item was taken.
    pub fn finished(&mut self) -> bool {
        self.0.finished()
    }
}

/// What [`Ui::task`] returns.
pub struct TaskResponse<'u, T>(immediate_tree::TaskResponse<'u, T>);

impl<T: 'static> TaskResponse<'_, T> {
    /// The output, once, in the first pass after the future finished.
    /// After a take the node holds nothing; stop declaring it to remove it.
    ///
    /// Widgets declared before this call show the state from before the
    /// output. Take outputs before reading the state they change, or call
    /// [`Ui::request_pass`] when that is not possible.
    pub fn take(&mut self) -> Option<T> {
        self.0.take()
    }

    /// Whether the future has not finished yet.
    pub fn running(&mut self) -> bool {
        self.0.running()
    }
}
