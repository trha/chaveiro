//! Two windows. The first counts clicks; closing it, or Cmd+Q, ends the app.
//! The second closes on its own when asked, and a task that waits on a
//! background thread updates the first window's status line.
//!
//! With `SMOKE_PANIC=1`, the pass that takes the task's answer panics, to check that `run` closes
//! the windows and resumes the panic.

// The program is macOS only. On other targets it is empty, so it checks but
// does not link.
#![cfg_attr(not(target_os = "macos"), no_main)]
#![cfg(target_os = "macos")]

use appkit_immediate::{App, Orientation, Ui, run};
use objc2_app_kit::NSTextField;
use objc2_foundation::NSString;

fn main() {
    if std::env::var_os("OBJC_DEBUG_MISSING_POOLS").is_some() {
        // AppKit's own threads autorelease without pools, so the smoke
        // script checks only lines for this thread.
        eprintln!("main thread {:#x}", unsafe { pthread_self() });
    }
    run(|| Smoke {
        clicks: 0,
        panic: std::env::var_os("SMOKE_PANIC").is_some(),
        second_open: true,
        main_open: true,
        status: String::from("waiting for the background thread"),
    });
    eprintln!("run returned");
}

unsafe extern "C" {
    fn pthread_self() -> usize;
}

struct Smoke {
    clicks: u32,
    panic: bool,
    second_open: bool,
    main_open: bool,
    status: String,
}

impl Drop for Smoke {
    fn drop(&mut self) {
        eprintln!("app dropped");
    }
}

impl App for Smoke {
    fn view(&mut self, ui: &mut Ui) {
        // Taken before the status line is declared, so this pass shows it.
        // Declared at the root, so closing a window keeps it.
        let answer = ui
            .task("background", || {
                appkit_executor::off_thread(|| {
                    std::thread::sleep(std::time::Duration::from_millis(500));
                    String::from("hello from a background thread")
                })
            })
            .take();
        if let Some(Some(status)) = answer {
            assert!(!self.panic, "planned panic");
            self.status = status;
        }
        if ui.should_terminate() {
            eprintln!("should terminate");
            self.main_open = false;
            self.second_open = false;
        }
        if self.main_open {
            let main = ui.window("main", "appkit-immediate smoke", |ui| {
                ui.stack_view("content", Orientation::Vertical, 8.0, |ui| {
                    ui.label("count", format_args!("{} clicks", self.clicks));
                    ui.label("status", format_args!("{}", self.status));
                    if ui.button("increment", "Click me").clicked() {
                        self.clicks += 1;
                        eprintln!("clicks {}", self.clicks);
                    }
                    if self.clicks % 2 == 1 {
                        ui.label("odd", format_args!("odd count"));
                    }
                    // Rebuilt only when the click count's parity changes.
                    ui.cached("parity", self.clicks % 2, |ui| {
                        let parity = if self.clicks.is_multiple_of(2) {
                            "even"
                        } else {
                            "odd"
                        };
                        ui.label("parity", format_args!("cached {parity}"));
                    });
                    let clicks = self.clicks;
                    ui.native(
                        "native",
                        |mtm| {
                            NSTextField::labelWithString(&NSString::new(), mtm)
                                .into_super()
                                .into_super()
                        },
                        |view| {
                            let label = view
                                .downcast_ref::<NSTextField>()
                                .expect("the native view is a text field");
                            label.setStringValue(&NSString::from_str(&format!("native {clicks}")));
                        },
                    );
                });
            });
            if main.should_close() {
                eprintln!("main should close");
                self.main_open = false;
                self.second_open = false;
            }
        }
        if self.second_open {
            let second = ui.window("second", "second window", |ui| {
                ui.label("text", format_args!("Close me"));
            });
            if second.should_close() {
                eprintln!("second should close");
                self.second_open = false;
            }
        }
    }
}
