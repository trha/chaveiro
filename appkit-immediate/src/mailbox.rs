//! Messages from any thread to the UI thread, with one wake per burst.

use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};

/// Sends messages to the UI thread from any thread. Cheap to clone.
pub struct Handle<M> {
    mailbox: Arc<Mailbox<M>>,
}

impl<M> Handle<M> {
    /// Queues `message` and wakes the UI thread, which applies it with
    /// [`App::update`](crate::App::update) before the next pass. Returns false,
    /// and drops the message, once the last window has closed.
    pub fn send(&self, message: M) -> bool {
        self.mailbox.send(message)
    }

    pub(crate) fn new(mailbox: Arc<Mailbox<M>>) -> Self {
        Handle { mailbox }
    }
}

impl<M> Clone for Handle<M> {
    fn clone(&self) -> Self {
        Handle {
            mailbox: Arc::clone(&self.mailbox),
        }
    }
}

/// Messages queued for the UI thread.
///
/// Contract: [`wake_posted`](Self::wake_posted) is true from the moment a send
/// posts a wake until the UI thread starts draining, so a burst of sends posts
/// one wake, and a send that lands after the drain started posts a new one.
pub(crate) struct Mailbox<M> {
    queue: Mutex<Vec<M>>,
    wake_posted: AtomicBool,
    /// Cleared when the last window closes; sends then fail.
    open: AtomicBool,
    /// Asks the UI thread to drain. Returns false when the UI thread can no
    /// longer run work.
    wake: Box<dyn 'static + Send + Sync + Fn() -> bool>,
}

impl<M> Mailbox<M> {
    pub(crate) fn new(wake: impl 'static + Send + Sync + Fn() -> bool) -> Self {
        Mailbox {
            queue: Mutex::new(Vec::new()),
            wake_posted: AtomicBool::new(false),
            open: AtomicBool::new(true),
            wake: Box::new(wake),
        }
    }

    fn send(&self, message: M) -> bool {
        if !self.open.load(Ordering::Acquire) {
            return false;
        }
        self.queue
            .lock()
            .expect("no code panics while holding the queue lock")
            .push(message);
        if self.wake_posted.swap(true, Ordering::AcqRel) {
            return true;
        }
        if (self.wake)() {
            return true;
        }
        // The UI thread is gone, so nothing will ever drain the queue.
        self.close();
        false
    }

    /// Moves every queued message into `into`, in send order. Call on the UI
    /// thread, from the wake.
    pub(crate) fn drain(&self, into: &mut Vec<M>) {
        // Clear the flag first: a send after this point posts its own wake,
        // and a send before it is in the queue taken below.
        self.wake_posted.store(false, Ordering::Release);
        let mut queue = self
            .queue
            .lock()
            .expect("no code panics while holding the queue lock");
        std::mem::swap(&mut *queue, into);
    }

    /// Fails every later send and drops the queued messages.
    pub(crate) fn close(&self) {
        self.open.store(false, Ordering::Release);
        self.queue
            .lock()
            .expect("no code panics while holding the queue lock")
            .clear();
    }
}

#[cfg(test)]
mod tests;
