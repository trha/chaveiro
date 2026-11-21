//! Runs `!Send` futures on the WinUI `DispatcherQueue` of the current thread.
//!
//! [`spawn`] starts a task and returns a [`SpawnHandle`]. A woken task is
//! polled in its own dispatcher callback, so a waker may run on any thread,
//! such as the thread pool thread that completes a WinRT operation.

mod bindings;

use bindings::{DispatcherQueue, DispatcherQueueHandler};
use std::{
    cell::RefCell,
    future::Future,
    marker::PhantomData,
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    task::{Context, Wake, Waker},
};

/// Runs `future` on this thread's `DispatcherQueue`. The first poll happens
/// in a later dispatcher callback, never inside this call. The future may
/// hold `!Send` state.
///
/// A task still alive when the thread ends is dropped with the thread's
/// locals, after the dispatcher has stopped.
///
/// Panics if this thread has no `DispatcherQueue`.
pub fn spawn(future: impl 'static + Future<Output = ()>) -> SpawnHandle {
    let queue = DispatcherQueue::GetForCurrentThread()
        .ok()
        .filter(|queue| !windows_core::Interface::as_raw(queue).is_null())
        .expect("winui_executor::spawn needs a thread with a DispatcherQueue");
    let (index, generation, wake) = TASKS.with(|tasks| {
        let mut tasks = tasks.borrow_mut();
        let index = match tasks.free.pop() {
            Some(index) => index,
            None => {
                tasks.slots.push(Slot {
                    generation: 0,
                    state: State::Vacant,
                });
                u32::try_from(tasks.slots.len() - 1).expect("fewer than 2^32 live tasks")
            }
        };
        let slot = &mut tasks.slots[index as usize];
        let wake = Arc::new(TaskWake {
            queue,
            index,
            generation: slot.generation,
            queued: AtomicBool::new(false),
        });
        slot.state = State::Idle {
            future: Box::pin(future),
            wake: Arc::clone(&wake),
        };
        (index, slot.generation, wake)
    });
    wake.wake_by_ref();
    SpawnHandle {
        index,
        generation,
        thread_bound: PhantomData,
    }
}

/// Owns one spawned task. Dropping it aborts the task.
#[must_use = "dropping a SpawnHandle aborts the task; call detach to keep it running"]
pub struct SpawnHandle {
    index: u32,
    generation: u32,
    /// Keeps the handle on the task's thread, where the task table lives.
    thread_bound: PhantomData<*const ()>,
}

impl SpawnHandle {
    /// Drops the task's future now. Aborting a finished task does nothing.
    /// If the task aborts itself from inside its own poll, the future is
    /// dropped as soon as that poll returns.
    pub fn abort(self) {
        // Drop does the work.
    }

    /// Lets the task run to the end without a handle.
    pub fn detach(self) {
        // The handle holds only plain ids, so forgetting it leaks nothing.
        std::mem::forget(self);
    }
}

impl Drop for SpawnHandle {
    fn drop(&mut self) {
        let future = TASKS.with(|tasks| {
            let mut tasks = tasks.borrow_mut();
            let slot = &mut tasks.slots[self.index as usize];
            if slot.generation != self.generation {
                return None;
            }
            match std::mem::replace(&mut slot.state, State::Vacant) {
                State::Idle { future, .. } => {
                    tasks.release(self.index);
                    Some(future)
                }
                State::Polling { .. } => {
                    slot.state = State::Polling { aborted: true };
                    None
                }
                State::Vacant => unreachable!("a vacant slot has a new generation"),
            }
        });
        // Dropped with no borrow held, because the future's drop may spawn or
        // abort other tasks.
        drop(future);
    }
}

/// The task table of one thread. A slot's generation changes each time it is
/// freed, so a stale handle or waker never reaches the next task in it.
struct Tasks {
    slots: Vec<Slot>,
    /// Indexes of vacant slots.
    free: Vec<u32>,
}

impl Tasks {
    /// Marks the slot at `index` vacant and gives it a new generation.
    fn release(&mut self, index: u32) {
        let slot = &mut self.slots[index as usize];
        slot.state = State::Vacant;
        slot.generation = slot.generation.wrapping_add(1);
        self.free.push(index);
    }
}

struct Slot {
    generation: u32,
    state: State,
}

enum State {
    Vacant,
    Idle {
        future: Pin<Box<dyn Future<Output = ()>>>,
        wake: Arc<TaskWake>,
    },
    /// The future is out of the table while it runs, so the poll can spawn
    /// and abort tasks. A handle meets this state only when the task drops its
    /// own handle during its poll, which sets `aborted`.
    Polling {
        aborted: bool,
    },
}

/// The waker of one task.
struct TaskWake {
    queue: DispatcherQueue,
    index: u32,
    generation: u32,
    /// Whether a poll callback is queued and has not started. Holds the
    /// dispatcher queue to one callback per task for a burst of wakes.
    queued: AtomicBool,
}

impl Wake for TaskWake {
    fn wake(self: Arc<Self>) {
        self.wake_by_ref();
    }

    fn wake_by_ref(self: &Arc<Self>) {
        if self.queued.swap(true, Ordering::AcqRel) {
            return;
        }
        // Copied out, because the closure would capture all of `self`: precise
        // capture stops at the `Arc` deref, and the handler must be 'static.
        let (index, generation) = (self.index, self.generation);
        let handler = DispatcherQueueHandler::new(move || {
            poll(index, generation);
        });
        // A refused enqueue means the dispatcher is shutting down, so the
        // task will never run again and its future drops with the thread.
        let _ = self.queue.TryEnqueue(&handler);
    }
}

/// Polls the task at `index` once, if it is still the task of `generation`.
fn poll(index: u32, generation: u32) {
    let taken = TASKS.with(|tasks| {
        let mut tasks = tasks.borrow_mut();
        let slot = tasks.slots.get_mut(index as usize)?;
        if slot.generation != generation {
            return None;
        }
        match std::mem::replace(&mut slot.state, State::Polling { aborted: false }) {
            State::Idle { future, wake } => Some((future, wake)),
            other => {
                slot.state = other;
                None
            }
        }
    });
    let Some((mut future, wake)) = taken else {
        return;
    };
    // Cleared before the poll, so a wake during the poll queues another.
    wake.queued.store(false, Ordering::Release);
    let waker = Waker::from(Arc::clone(&wake));
    let ready = future
        .as_mut()
        .poll(&mut Context::from_waker(&waker))
        .is_ready();
    let finished = TASKS.with(|tasks| {
        let mut tasks = tasks.borrow_mut();
        let slot = &mut tasks.slots[index as usize];
        let State::Polling { aborted } = slot.state else {
            unreachable!("only the poll leaves the Polling state");
        };
        if ready || aborted {
            tasks.release(index);
            Some(future)
        } else {
            slot.state = State::Idle { future, wake };
            None
        }
    });
    // Dropped with no borrow held, as in `SpawnHandle::drop`.
    drop(finished);
}

thread_local! {
    static TASKS: RefCell<Tasks> = const {
        RefCell::new(Tasks { slots: Vec::new(), free: Vec::new() })
    };
}
