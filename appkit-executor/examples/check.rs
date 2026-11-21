//! Drives appkit-executor on the real main dispatch queue and checks its
//! contracts. Prints one line per check and exits with status 1 if any
//! failed.
//!
//! Drains the main queue with `CFRunLoopRun`, so it needs no window.
//! `dispatch_main` does not work here: it exits the main thread, which
//! destroys the task table before any block runs.

// The program is macOS only. On other targets it is empty, so it checks but
// does not link.
#![cfg_attr(not(target_os = "macos"), no_main)]
#![cfg(target_os = "macos")]

use appkit_executor::{SpawnHandle, off_thread, spawn};
use dispatch2::DispatchQueue;
use std::{
    cell::{Cell, RefCell},
    future::poll_fn,
    rc::Rc,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    task::Poll,
    thread,
    time::Duration,
};

/// The checks, each reported once.
const CHECKS: usize = 8;

static FAILED: AtomicBool = AtomicBool::new(false);

thread_local! {
    static REPORTED: Cell<usize> = const { Cell::new(0) };
    /// Owns the tasks that must outlive `start`.
    static TASKS: RefCell<Vec<SpawnHandle>> = const { RefCell::new(Vec::new()) };
}

fn main() {
    DispatchQueue::main().exec_async(start);
    // SAFETY: called on the main thread, whose run loop drains the main
    // queue; it returns only when the run loop stops, which nothing asks.
    unsafe { CFRunLoopRun() };
}

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFRunLoopRun();
}

fn report(name: &'static str, passed: bool) {
    println!("{} {name}", if passed { "pass" } else { "FAIL" });
    if !passed {
        FAILED.store(true, Ordering::Relaxed);
    }
    let reported = REPORTED.get() + 1;
    REPORTED.set(reported);
    if reported == CHECKS {
        TASKS.with_borrow_mut(Vec::clear);
        std::process::exit(i32::from(FAILED.load(Ordering::Relaxed)));
    }
}

fn keep(task: SpawnHandle) {
    TASKS.with_borrow_mut(|tasks| tasks.push(task));
}

/// Sets `flag` when dropped.
struct DropFlag(Rc<Cell<bool>>);

impl Drop for DropFlag {
    fn drop(&mut self) {
        self.0.set(true);
    }
}

/// Resolves after the main queue has run the blocks queued before it.
async fn yield_now() {
    let mut yielded = false;
    poll_fn(|context| {
        if yielded {
            return Poll::Ready(());
        }
        yielded = true;
        context.waker().wake_by_ref();
        Poll::Pending
    })
    .await;
}

fn start() {
    let main_thread = thread::current().id();

    // The first poll happens after spawn returns.
    {
        let polled = Rc::new(Cell::new(false));
        keep(spawn({
            let polled = Rc::clone(&polled);
            async move { polled.set(true) }
        }));
        let early = polled.get();
        keep(spawn(async move {
            report(
                "first poll comes after spawn returns",
                !early && polled.get(),
            );
        }));
    }

    // A wake from another thread resumes the task on the main thread.
    {
        let done = Arc::new(AtomicBool::new(false));
        keep(spawn(async move {
            let mut started = false;
            poll_fn(|context| {
                if done.load(Ordering::Acquire) {
                    return Poll::Ready(());
                }
                if !started {
                    started = true;
                    let (done, waker) = (Arc::clone(&done), context.waker().clone());
                    thread::spawn(move || {
                        thread::sleep(Duration::from_millis(20));
                        done.store(true, Ordering::Release);
                        waker.wake();
                    });
                }
                Poll::Pending
            })
            .await;
            report(
                "wake from another thread polls on the main thread",
                thread::current().id() == main_thread,
            );
        }));
    }

    // A burst of wakes queues one poll.
    {
        let polls = Rc::new(Cell::new(0));
        keep(spawn({
            let polls = Rc::clone(&polls);
            async move {
                let mut burst = false;
                poll_fn(|context| {
                    polls.set(polls.get() + 1);
                    if burst {
                        return Poll::Ready(());
                    }
                    burst = true;
                    for _ in 0..1000 {
                        context.waker().wake_by_ref();
                    }
                    Poll::Pending
                })
                .await;
                // Polls: the first, and one for the whole burst.
                report("a burst of wakes polls once", polls.get() == 2);
            }
        }));
    }

    // Dropping the handle drops the future before its first poll.
    {
        let dropped = Rc::new(Cell::new(false));
        let ran = Rc::new(Cell::new(false));
        let guard = DropFlag(Rc::clone(&dropped));
        let task = spawn({
            let ran = Rc::clone(&ran);
            async move {
                let _guard = guard;
                ran.set(true);
            }
        });
        drop(task);
        let dropped_now = dropped.get();
        keep(spawn(async move {
            yield_now().await;
            report(
                "dropping the handle aborts the task",
                dropped_now && !ran.get(),
            );
        }));
    }

    // A task that drops its own handle during its poll ends after that poll.
    {
        let slot: Rc<RefCell<Option<SpawnHandle>>> = Rc::new(RefCell::new(None));
        let dropped = Rc::new(Cell::new(false));
        let polls = Rc::new(AtomicUsize::new(0));
        let task = spawn({
            let (slot, polls) = (Rc::clone(&slot), Rc::clone(&polls));
            let guard = DropFlag(Rc::clone(&dropped));
            async move {
                let _guard = guard;
                polls.fetch_add(1, Ordering::Relaxed);
                drop(slot.borrow_mut().take());
                yield_now().await;
                polls.fetch_add(1, Ordering::Relaxed);
            }
        });
        *slot.borrow_mut() = Some(task);
        keep(spawn(async move {
            yield_now().await;
            yield_now().await;
            report(
                "self abort ends the task after its poll",
                dropped.get() && polls.load(Ordering::Relaxed) == 1,
            );
        }));
    }

    // A stale handle aborts nothing after its slot is reused.
    {
        let first = spawn(async {});
        keep(spawn(async move {
            yield_now().await;
            let finished = first.is_finished();
            let ran = Rc::new(Cell::new(false));
            let second = spawn({
                let ran = Rc::clone(&ran);
                async move { ran.set(true) }
            });
            drop(first);
            yield_now().await;
            report(
                "a stale handle aborts nothing",
                finished && ran.get() && second.is_finished(),
            );
        }));
    }

    // off_thread runs its job on another thread and resumes on this one.
    keep(spawn(async move {
        let job_thread = off_thread(|| thread::current().id()).await;
        report(
            "off_thread runs off the main thread and resumes on it",
            job_thread.is_some_and(|id| id != main_thread) && thread::current().id() == main_thread,
        );
    }));

    // A panicking job yields None and the app keeps running.
    keep(spawn(async move {
        let result = off_thread(|| -> u8 { panic!("expected panic from the check") }).await;
        report("a panicking off_thread job yields None", result.is_none());
    }));
}
