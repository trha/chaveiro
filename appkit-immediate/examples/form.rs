//! A text field, a text field the app rewrites in upper case while the user
//! types, and a secure text field with a button that takes its password, so
//! `tools/smoke.sh` can check reads, app writes during an edit, Return and
//! `take`.

use appkit_immediate::{App, Orientation, Ui, run};

fn main() {
    run(|_| Form {
        open: true,
        name: String::new(),
        upper: String::new(),
        returns: 0,
        took: None,
    });
}

struct Form {
    open: bool,
    name: String,
    upper: String,
    returns: u32,
    /// The length of the last password taken.
    took: Option<usize>,
}

impl App for Form {
    type Message = ();

    fn update(&mut self, (): ()) {}

    fn view(&mut self, ui: &mut Ui) {
        if ui.should_terminate() {
            self.open = false;
        }
        if !self.open {
            return;
        }
        let window = ui.window("main", "form", |ui| {
            ui.stack_view("fields", Orientation::Vertical, 8.0, |ui| {
                if ui
                    .text_field("name", "Name", &mut self.name)
                    .return_pressed()
                {
                    self.returns += 1;
                }
                ui.label("echo", format_args!("name: {}", self.name));
                ui.label("returns", format_args!("returns: {}", self.returns));
                if ui.text_field("upper", "Upper", &mut self.upper).changed() {
                    self.upper = self.upper.to_uppercase();
                }
                let password = ui.secure_text_field("password", "Password").handle();
                if ui.button("take", "Take").clicked() {
                    self.took = Some(ui.take_password(password).chars().count());
                }
                let empty = ui.password_is_empty(password);
                ui.label(
                    "took",
                    format_args!("took: {:?}, empty: {empty}", self.took),
                );
            });
        });
        if window.should_close() {
            self.open = false;
        }
    }
}
