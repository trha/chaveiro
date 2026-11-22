//! Two windows. The first counts clicks; closing it ends the app. The second
//! closes on its own when asked, and a task that a background thread
//! completes updates the first window's status line.

// The program is Windows only. On other targets it is empty, so it checks
// but does not link.
#![cfg_attr(not(windows), no_main)]
#![cfg(windows)]

use std::{
    future::Future,
    pin::Pin,
    sync::{Arc, Mutex},
    task::{Context, Poll, Waker},
};
use winui_immediate::{App, Orientation, Ui, run};

fn main() -> windows_core::Result<()> {
    run(|| {
        Ok(Smoke {
            clicks: 0,
            second_open: true,
            main_open: true,
            status: String::from("waiting for the background thread"),
        })
    })?;
    eprintln!("run returned");
    Ok(())
}

struct Smoke {
    clicks: u32,
    second_open: bool,
    main_open: bool,
    status: String,
}

impl App for Smoke {
    fn view(&mut self, ui: &mut Ui) -> windows_core::Result<()> {
        // Outside every window, so it runs while either is open.
        if let Some(status) = ui.task("background", from_thread).take() {
            self.status = status;
        }
        if self.main_open {
            let main = ui.window("main", "winui-immediate smoke", |ui| {
                ui.stack("content", Orientation::Vertical, 8.0, |ui| {
                    ui.label("count", format_args!("{} clicks", self.clicks))?;
                    ui.label("status", format_args!("{}", self.status))?;
                    if ui.button("increment", "Click me")?.clicked()? {
                        self.clicks += 1;
                        eprintln!("clicks {}", self.clicks);
                    }
                    Ok(())
                })?;
                Ok(())
            })?;
            if main.close_requested() {
                eprintln!("main close requested");
                self.main_open = false;
                self.second_open = false;
            }
        }
        if self.second_open {
            let second = ui.window("second", "second window", |ui| {
                ui.label("text", format_args!("Close me"))?;
                Ok(())
            })?;
            if second.close_requested() {
                eprintln!("second close requested");
                self.second_open = false;
            }
        }
        Ok(())
    }
}

/// A future that a background thread completes after 500 ms. Its waker is
/// called on that thread, as a WinRT completion's would be.
fn from_thread() -> FromThread {
    let shared = Arc::new(Mutex::new((None, None::<Waker>)));
    std::thread::spawn({
        let shared = Arc::clone(&shared);
        move || {
            std::thread::sleep(std::time::Duration::from_millis(500));
            let waker = {
                let mut shared = shared.lock().unwrap();
                shared.0 = Some(String::from("hello from a background thread"));
                shared.1.take()
            };
            if let Some(waker) = waker {
                waker.wake();
            }
        }
    });
    FromThread(shared)
}

struct FromThread(Arc<Mutex<(Option<String>, Option<Waker>)>>);

impl Future for FromThread {
    type Output = String;

    fn poll(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<String> {
        let mut shared = self.0.lock().unwrap();
        match shared.0.take() {
            Some(status) => Poll::Ready(status),
            None => {
                shared.1 = Some(context.waker().clone());
                Poll::Pending
            }
        }
    }
}