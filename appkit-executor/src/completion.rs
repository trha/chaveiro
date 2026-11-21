//! One-shot completion from any thread to one future, for Foundation
//! completion handlers and pool jobs.

use std::{
    future::Future,
    pin::Pin,
    sync::{Arc, Mutex, MutexGuard},
    task::{Context, Poll, Waker},
};

/// Makes a linked pair. The completer may move to any thread, and the
/// future may run on any executor.
pub fn completion<T: Send>() -> (Completer<T>, Completion<T>) {
    let slot = Arc::new(Mutex::new(Slot::Waiting(None)));
    (
        Completer {
            slot: Arc::clone(&slot),
        },
        Completion { slot, cancel: None },
    )
}

/// Completes one [`Completion`]. [`complete`](Self::complete) takes `&self`,
/// so a completer fits a `Fn` block; the first call wins.
pub struct Completer<T> {
    slot: Arc<Mutex<Slot<T>>>,
}

impl<T: Send> Completer<T> {
    /// Stores `value` and wakes the future. A call after the first, or after
    /// the future dropped, drops `value`.
    pub fn complete(&self, value: T) {
        let waker = {
            let mut slot = lock(&self.slot);
            match &mut *slot {
                Slot::Waiting(waker) => {
                    let waker = waker.take();
                    *slot = Slot::Done(value);
                    waker
                }
                // The future already has its answer or is gone, so nothing
                // can read a second value.
                Slot::Done(_) | Slot::Abandoned | Slot::Taken => None,
            }
        };
        // Woken with no lock held, because the waker may poll inline.
        if let Some(waker) = waker {
            waker.wake();
        }
    }
}

impl<T> Drop for Completer<T> {
    fn drop(&mut self) {
        let waker = {
            let mut slot = lock(&self.slot);
            match &mut *slot {
                Slot::Waiting(waker) => {
                    let waker = waker.take();
                    *slot = Slot::Abandoned;
                    waker
                }
                Slot::Done(_) | Slot::Abandoned | Slot::Taken => None,
            }
        };
        if let Some(waker) = waker {
            waker.wake();
        }
    }
}

/// Resolves to `Some(value)`, or to [`None`] when the completer dropped
/// without completing, such as a block that Foundation freed without
/// calling it.
#[must_use = "a completion does nothing unless awaited"]
pub struct Completion<T> {
    slot: Arc<Mutex<Slot<T>>>,
    /// Runs if the future drops before it resolves.
    cancel: Option<Box<dyn FnOnce()>>,
}

impl<T> Completion<T> {
    /// Runs `cancel` if this future drops before it resolves, such as when
    /// its task is aborted. `cancel` runs on the dropping thread. A later
    /// call replaces an earlier `cancel`.
    pub fn cancel_with(mut self, cancel: impl 'static + FnOnce()) -> Self {
        self.cancel = Some(Box::new(cancel));
        self
    }
}

impl<T> Future for Completion<T> {
    type Output = Option<T>;

    fn poll(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Option<T>> {
        let this = self.get_mut();
        let mut slot = lock(&this.slot);
        let output = match std::mem::replace(&mut *slot, Slot::Taken) {
            Slot::Done(value) => Some(value),
            Slot::Abandoned => None,
            Slot::Taken => panic!("Completion polled after it resolved"),
            Slot::Waiting(waker) => {
                let waker = match waker {
                    Some(waker) if waker.will_wake(context.waker()) => waker,
                    _ => context.waker().clone(),
                };
                *slot = Slot::Waiting(Some(waker));
                return Poll::Pending;
            }
        };
        drop(slot);
        this.cancel = None;
        Poll::Ready(output)
    }
}

impl<T> Drop for Completion<T> {
    fn drop(&mut self) {
        let Some(cancel) = self.cancel.take() else {
            return;
        };
        // A value that arrived but was never polled still counts as
        // resolved, so the work it cancels is already done.
        let done = matches!(
            std::mem::replace(&mut *lock(&self.slot), Slot::Taken),
            Slot::Done(_)
        );
        if !done {
            cancel();
        }
    }
}

fn lock<T>(slot: &Mutex<Slot<T>>) -> MutexGuard<'_, Slot<T>> {
    slot.lock()
        .expect("no code panics while holding the completion lock")
}

enum Slot<T> {
    /// No value yet; holds the waker of the last pending poll.
    Waiting(Option<Waker>),
    Done(T),
    /// The completer dropped without a value.
    Abandoned,
    /// The future resolved or dropped.
    Taken,
}

#[cfg(test)]
mod tests;
