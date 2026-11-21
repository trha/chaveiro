//! Drives winui-executor on a real WinUI dispatcher and checks its contracts.
//! Prints one line per check and exits with status 1 if any failed.
//!
//! Runs under winui-immediate only to get a WinUI thread; the window closes
//! itself once every check has reported.

// The program is Windows only. On other targets it is empty, so it checks
// but does not link.
#![cfg_attr(not(windows), no_main)]
#![cfg(windows)]

use std::{
    cell::{Cell, RefCell},
    rc::Rc,
    sync::atomic::{AtomicBool, Ordering},
    thread,
    time::Duration,
};
use windows_future::IAsyncOperation;
use winui_executor::{SpawnHandle, spawn};
use winui_immediate::{App, Handle, Ui, run};

/// The checks, each reported once through a message.
const CHECKS: usize = 5;

static FAILED: AtomicBool = AtomicBool::new(false);

fn main() -> windows_core::Result<()> {
    run(|handle: Handle<Report>| {
        start(handle);
        Ok(Check { reported: 0 })
    })?;
    if FAILED.load(Ordering::Relaxed) {
        std::process::exit(1);
    }
    Ok(())
}

struct Report {
    name: &'static str,
    passed: bool,
}

fn report(handle: &Handle<Report>, name: &'static str, passed: bool) {
    // The window stays open until every check reports, so the send lands.
    let _ = handle.send(Report { name, passed });
}

/// Sets `flag` when dropped.
struct DropFlag(Rc<Cell<bool>>);

impl Drop for DropFlag {
    fn drop(&mut self) {
        self.0.set(true);
    }
}

fn start(handle: Handle<Report>) {
    let ui_thread = thread::current().id();

    // A WinRT operation completes on a thread pool thread, and the task
    // resumes on the UI thread.
    {
        let handle = handle.clone();
        spawn(async move {
            let value = IAsyncOperation::<i32>::spawn(|| {
                thread::sleep(Duration::from_millis(50));
                Ok(7)
            })
            .await;
            let passed = matches!(value, Ok(7)) && thread::current().id() == ui_thread;
            report(&handle, "winrt await resumes on the UI thread", passed);
        })
        .detach();
    }

    // The first poll happens after spawn returns.
    {
        let polled = Rc::new(Cell::new(false));
        let task = {
            let polled = Rc::clone(&polled);
            spawn(async move { polled.set(true) })
        };
        let not_inline = !polled.get();
        task.detach();
        let handle = handle.clone();
        spawn(async move {
            yield_once().await;
            report(
                &handle,
                "first poll is not inside spawn",
                not_inline && polled.get(),
            );
        })
        .detach();
    }

    // Abort before the first poll drops the future at once and never runs it.
    {
        let ran = Rc::new(Cell::new(false));
        let dropped = Rc::new(Cell::new(false));
        let task = {
            let ran = Rc::clone(&ran);
            let guard = DropFlag(Rc::clone(&dropped));
            spawn(async move {
                let _guard = guard;
                ran.set(true);
            })
        };
        task.abort();
        let dropped_now = dropped.get();
        let handle = handle.clone();
        spawn(async move {
            yield_once().await;
            report(
                &handle,
                "abort drops the future and it never runs",
                dropped_now && !ran.get(),
            );
        })
        .detach();
    }

    // A task that aborts itself during its poll stops there, and its future
    // drops right after that poll.
    {
        let own: Rc<RefCell<Option<SpawnHandle>>> = Rc::new(RefCell::new(None));
        let dropped = Rc::new(Cell::new(false));
        let resumed = Rc::new(Cell::new(false));
        let task = {
            let own = Rc::clone(&own);
            let guard = DropFlag(Rc::clone(&dropped));
            let resumed = Rc::clone(&resumed);
            spawn(async move {
                let _guard = guard;
                if let Some(task) = own.borrow_mut().take() {
                    task.abort();
                }
                yield_once().await;
                resumed.set(true);
            })
        };
        *own.borrow_mut() = Some(task);
        let handle = handle.clone();
        spawn(async move {
            yield_once().await;
            yield_once().await;
            report(
                &handle,
                "self abort stops the task",
                dropped.get() && !resumed.get(),
            );
        })
        .detach();
    }

    // A task can spawn another, and a dropped handle aborts.
    {
        let handle = handle.clone();
        spawn(async move {
            let ran = Rc::new(Cell::new(false));
            let inner = {
                let ran = Rc::clone(&ran);
                spawn(async move { ran.set(true) })
            };
            let kept = Rc::new(Cell::new(false));
            drop(spawn({
                let kept = Rc::clone(&kept);
                async move { kept.set(true) }
            }));
            yield_once().await;
            yield_once().await;
            report(
                &handle,
                "nested spawn runs, dropped handle aborts",
                ran.get() && !kept.get(),
            );
            drop(inner);
        })
        .detach();
    }
}

/// Returns pending once after waking itself, so the task goes back through
/// the dispatcher.
async fn yield_once() {
    let mut yielded = false;
    std::future::poll_fn(|context| {
        if yielded {
            std::task::Poll::Ready(())
        } else {
            yielded = true;
            context.waker().wake_by_ref();
            std::task::Poll::Pending
        }
    })
    .await
}

struct Check {
    reported: usize,
}

impl App for Check {
    type Message = Report;

    fn update(&mut self, report: Report) {
        self.reported += 1;
        if !report.passed {
            FAILED.store(true, Ordering::Relaxed);
        }
        println!(
            "{} {}",
            if report.passed { "PASS" } else { "FAIL" },
            report.name
        );
    }

    fn view(&mut self, ui: &mut Ui) -> windows_core::Result<()> {
        if self.reported < CHECKS {
            ui.window("main", "winui-executor check", |ui| {
                ui.label(
                    "status",
                    format_args!("{} of {CHECKS} checks", self.reported),
                )?;
                Ok(())
            })?;
        }
        Ok(())
    }
}
