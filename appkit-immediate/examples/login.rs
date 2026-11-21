//! A login form in a box view, in a compact window with a visual effect
//! background sized to fit. `tools/smoke.sh` checks a pop-up pick, the
//! combo box, the error line, Return, and the busy state that disables the
//! fields.

use appkit_immediate::{Align, App, Handle, Style, Ui, run};
use std::thread::JoinHandle;

fn main() {
    run(|handle| Login {
        handle,
        open: true,
        server: 0,
        email: String::new(),
        error: None,
        busy: false,
        status: String::from("signed out"),
        worker: None,
    });
}

const SERVERS: [&str; 3] = ["bitwarden.com", "bitwarden.eu", "Self-hosted"];

struct Login {
    handle: Handle<Done>,
    open: bool,
    server: usize,
    email: String,
    error: Option<&'static str>,
    busy: bool,
    status: String,
    /// The pretend sign-in, joined when its result arrives or the app drops.
    worker: Option<JoinHandle<()>>,
}

/// The pretend sign-in finished.
struct Done(usize);

impl Drop for Login {
    fn drop(&mut self) {
        if let Some(worker) = self.worker.take() {
            worker.join().expect("the worker does not panic");
        }
    }
}

impl App for Login {
    type Message = Done;

    fn update(&mut self, Done(length): Done) {
        if let Some(worker) = self.worker.take() {
            worker.join().expect("the worker does not panic");
        }
        self.busy = false;
        self.status = format!("signed in with {length} characters");
    }

    fn view(&mut self, ui: &mut Ui) {
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
                        let handle = self.handle.clone();
                        self.worker = Some(std::thread::spawn(move || {
                            std::thread::sleep(std::time::Duration::from_millis(1500));
                            // The window may be gone, which needs no message.
                            let _ = handle.send(Done(length));
                        }));
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
        window.compact().visual_effect().size_to_fit();
        if window.should_close() {
            self.open = false;
        }
    }
}
