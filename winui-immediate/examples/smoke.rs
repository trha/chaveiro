//! Two windows. The first counts clicks; closing it ends the app. The second
//! closes on its own when asked, and a message from a background thread
//! updates the first window's status line.

use winui_immediate::{App, Handle, Orientation, Ui, run};

fn main() -> windows_core::Result<()> {
    run(
        Smoke {
            clicks: 0,
            second_open: true,
            main_open: true,
            status: String::from("waiting for the background thread"),
        },
        |_, handle: Handle<String>| {
            std::thread::spawn(move || {
                std::thread::sleep(std::time::Duration::from_millis(500));
                // The window may already be gone, which needs no message.
                let _ = handle.send(String::from("hello from a background thread"));
            });
        },
    )?;
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
    type Message = String;

    fn update(&mut self, message: String) {
        self.status = message;
    }

    fn view(&mut self, ui: &mut Ui) -> windows_core::Result<()> {
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
