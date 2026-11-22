//! Runs `!Send` futures on the macOS main dispatch queue.
//!
//! [`spawn`] starts a task and returns a [`SpawnHandle`]. A woken task is
//! polled in its own main-queue block, so a waker may run on any thread,
//! such as the session thread that calls a Foundation completion handler.
//! Under `NSApplication::run` the main queue drains between run-loop turns,
//! so a poll never lands inside an AppKit callback.
//!
//! Drain the main queue with `NSApplication::run` or `CFRunLoopRun`.
//! `dispatch_main` exits the main thread, which destroys the task table
//! before any task polls.

// The crate is macOS only and empty on other targets.
#![cfg(target_os = "macos")]

mod completion;
mod off_thread;
mod timer;

pub use completion::{Completer, Completion, completion};
pub use off_thread::off_thread;
pub use timer::{Interval, Sleep, interval, sleep, sleep_until, sleep_until_clock};

use dispatch2::DispatchQueue;
use objc2::MainThreadMarker;
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

/// Runs `future` on the main dispatch queue. The first poll happens in a
/// later main-queue block, never inside this call. The future may hold
/// `!Send` state.
///
/// The main thread's locals are not always destroyed at process exit, so a
/// task still alive then may never drop. Abort tasks before the app ends.
///
/// Panics when called off the main thread.
pub fn spawn(future: impl 'static + Future<Output = ()>) -> SpawnHandle {
    MainThreadMarker::new().expect("appkit_executor::spawn runs on the main thread");
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
#[must_use = "dropping a SpawnHandle aborts the task"]
pub struct SpawnHandle {
    index: u32,
    generation: u32,
    /// Keeps the handle on the main thread, where the task table lives.
    thread_bound: PhantomData<*const ()>,
}

impl SpawnHandle {
    /// Drops the task's future now. Aborting a finished task does nothing.
    /// If the task aborts itself from inside its own poll, the future is
    /// dropped as soon as that poll returns.
    pub fn abort(self) {
        // Drop does the work.
    }

    /// True once the future returned `Ready` or was dropped.
    pub fn is_finished(&self) -> bool {
        TASKS.with(|tasks| tasks.borrow().slots[self.index as usize].generation != self.generation)
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

/// The task table of the main thread. A slot's generation changes each time
/// it is freed, so a stale handle or waker never reaches the next task in it.
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
    index: u32,
    generation: u32,
    /// Whether a poll block is queued and has not started. Holds the main
    /// queue to one block per task for a burst of wakes.
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
        // Copied out, so the block holds plain ids and not the `Arc`.
        let (index, generation) = (self.index, self.generation);
        DispatchQueue::main().exec_async(move || poll(index, generation));
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
