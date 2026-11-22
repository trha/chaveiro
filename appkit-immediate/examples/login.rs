//! A login form in a box view, in a compact window with a visual effect
//! background sized to fit. `tools/smoke.sh` checks a pop-up pick, the
//! combo box, the error line, Return, and the busy state that disables the
//! fields.

// The program is macOS only. On other targets it is empty, so it checks but
// does not link.
#![cfg_attr(not(target_os = "macos"), no_main)]
#![cfg(target_os = "macos")]

use appkit_immediate::{Align, App, Style, Ui, run};
use std::time::Duration;

fn main() {
    run(|| Login {
        open: true,
        server: 0,
        email: String::new(),
        error: None,
        busy: false,
        status: String::from("signed out"),
        signing_in: None,
    });
}

const SERVERS: [&str; 3] = ["bitwarden.com", "bitwarden.eu", "Self-hosted"];

struct Login {
    open: bool,
    server: usize,
    email: String,
    error: Option<&'static str>,
    busy: bool,
    status: String,
    /// The password length of the pretend sign-in that runs, as a task.
    signing_in: Option<usize>,
}

impl App for Login {
    fn view(&mut self, ui: &mut Ui) {
        // Taken before the form reads `busy`, so this pass shows the end.
        if let Some(length) = self.signing_in {
            let mut task = ui.task("sign in", || {
                appkit_executor::sleep(Duration::from_millis(1500))
            });
            if task.take().is_some() {
                self.signing_in = None;
                self.busy = false;
                self.status = format!("signed in with {length} characters");
            }
        }
        if ui.should_terminate() {
            self.open = false;
        }
        if !self.open {
            return;
        }
        let mut window = ui.window("main", "Chaveiro", |ui| {
            ui.box_view("card", 320.0, |ui| {
                ui.label("title", format_args!("Log in"))
                    .style(Style::Title);
                ui.pop_up_button("server", &SERVERS, &mut self.server)
                    .header("Server")
                    .enabled(!self.busy);
                ui.combo_box("email", &["me@example.com"], &mut self.email)
                    .header("Email")
                    .required(true)
                    .enabled(!self.busy);
                let mut password = ui.secure_text_field("password", "Password");
                password
                    .header("Master password")
                    .required(true)
                    .hint("Your vault's master password")
                    .error(self.error)
                    .enabled(!self.busy);
                let submitted = password.return_pressed();
                let password = password.handle();
                let mut button = ui.button("log in", "Log in");
                button
                    .default_button(true)
                    .busy(self.busy)
                    .align(Align::End);
                if (button.clicked() || submitted) && !self.busy {
                    if ui.password_is_empty(password) {
                        self.error = Some("Enter your master password");
                    } else {
                        self.error = None;
                        self.busy = true;
                        let length = ui.take_password(password).chars().count();
                        self.signing_in = Some(length);
                        // The task is declared before the form, so it
                        // starts next pass.
                        ui.request_pass();
                    }
                }
                ui.label(
                    "status",
                    format_args!(
                        "server: {}, email: {}, {}",
                        self.server, self.email, self.status
                    ),
                );
            });
        });
        window.compact().glass().size_to_fit();
        if window.should_close() {
            self.open = false;
        }
    }
}
