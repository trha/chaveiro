//! Checks what a cancel does to a Windows Hello prompt. It opens a window,
//! starts a Hello key creation for it through `winui_executor::winrt`, then
//! aborts the task after 3 s and watches whether the prompt goes away.
//!
//! The prompt runs in `CredentialUIBroker.exe`, so the check looks for that
//! process. Leave the prompt alone while it runs. It prints one line per
//! step and exits with status 1 when the prompt outlives the cancel.

#![cfg_attr(not(windows), no_main)]
#![cfg(windows)]

// windows-bindgen makes the bindings `pub(crate)`, so the example compiles
// its own copy.
#[path = "../src/bindings.rs"]
mod bindings;

use bindings::{KeyCredentialCreationOption, KeyCredentialManager, WindowId};
use std::{
    cell::RefCell,
    rc::Rc,
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};
use windows_core::{Interface, Result};
use windows_future::{AsyncStatus, IAsyncInfo};
use winui_executor::SpawnHandle;
use winui_immediate::{App, Ui, run};

static FAILED: AtomicBool = AtomicBool::new(false);

fn main() -> Result<()> {
    println!("broker before start: {}", broker_running());
    run(|| Ok(Probe::default()))?;
    let started = Instant::now();
    let left = winui_executor::shutdown(Duration::from_secs(5));
    println!(
        "shutdown: {left} operation(s) left after {} ms",
        started.elapsed().as_millis()
    );
    if FAILED.load(Ordering::Relaxed) {
        std::process::exit(1);
    }
    Ok(())
}

#[derive(Default)]
struct Probe {
    /// The task that awaits the prompt, then the task that cancels it.
    prompt: Option<SpawnHandle>,
    watcher: Option<SpawnHandle>,
    started: bool,
    /// Set by the watcher when the prompt's task should be dropped.
    cancel: Option<Rc<RefCell<bool>>>,
    done: Rc<RefCell<bool>>,
}

impl App for Probe {
    fn view(&mut self, ui: &mut Ui) -> Result<()> {
        if *self.done.borrow() {
            self.prompt = None;
            self.watcher = None;
            return Ok(());
        }
        let window = ui.window("probe", "Hello cancel probe", |ui| {
            ui.label(
                "text",
                format_args!("A Hello prompt opens, then is cancelled. Do not touch it."),
            )?;
            Ok(())
        })?;
        let window = window.id()?;
        if !self.started {
            self.started = true;
            self.start(window)?;
        }
        if self.cancel.as_ref().is_some_and(|cancel| *cancel.borrow()) && self.prompt.is_some() {
            println!("cancel: dropping the prompt's task");
            self.prompt = None;
        }
        // Looks at the watcher's flags every 200 ms.
        let mut looks = ui.stream("look", || {
            winui_executor::interval(Duration::from_millis(200))
        });
        while looks.next().is_some() {}
        Ok(())
    }
}

impl Probe {
    fn start(&mut self, window: u64) -> Result<()> {
        let operation = KeyCredentialManager::RequestCreateForWindowAsync(
            WindowId { value: window },
            "chaveiro-cancel-probe",
            KeyCredentialCreationOption::ReplaceExisting,
        )?;
        let info = operation.cast::<IAsyncInfo>()?;
        self.prompt = Some(winui_executor::spawn(async move {
            match winui_executor::winrt(operation).await {
                Ok(result) => println!("prompt ended before the cancel: {:?}", result.Status()),
                Err(error) => println!("prompt failed before the cancel: {error}"),
            }
        }));
        // The cancel needs the prompt's handle, which the view owns, so the
        // watcher asks the view to drop it through `cancel`.
        let cancel = Rc::new(RefCell::new(false));
        let done = Rc::clone(&self.done);
        self.watcher = Some(winui_executor::spawn({
            let cancel = Rc::clone(&cancel);
            async move {
                winui_executor::sleep(Duration::from_secs(3)).await;
                println!(
                    "before cancel: broker {}, status {:?}",
                    broker_running(),
                    info.Status().map(|status| status.0)
                );
                *cancel.borrow_mut() = true;
                let cancelled = Instant::now();
                // Watch for 5 s whether the prompt goes.
                let mut gone = None;
                while cancelled.elapsed() < Duration::from_secs(5) {
                    winui_executor::sleep(Duration::from_millis(100)).await;
                    if gone.is_none() && !broker_running() {
                        gone = Some(cancelled.elapsed());
                    }
                }
                let status = info.Status();
                println!(
                    "after cancel: broker gone after {:?}, status {:?} (Canceled is {})",
                    gone.map(|gone| gone.as_millis()),
                    status.as_ref().map(|status| status.0),
                    AsyncStatus::Canceled.0
                );
                if gone.is_none() {
                    FAILED.store(true, Ordering::Relaxed);
                }
                *done.borrow_mut() = true;
            }
        }));
        self.cancel = Some(cancel);
        Ok(())
    }
}

/// Whether `CredentialUIBroker.exe` shows a window, which is the prompt. The
/// process can outlive its window.
fn broker_running() -> bool {
    std::process::Command::new("pwsh")
        .args([
            "-NoProfile",
            "-Command",
            "@(Get-Process CredentialUIBroker -ErrorAction SilentlyContinue | Where-Object MainWindowHandle -ne 0).Count",
        ])
        .output()
        .map(|output| String::from_utf8_lossy(&output.stdout).trim() != "0")
        .unwrap_or(false)
}
