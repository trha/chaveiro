//! A grid with an Auto, a 40pt, a 1-star and a 2-star row, each holding a
//! label, so `tools/smoke.sh` can measure the rows through Accessibility.

// The program is macOS only. On other targets it is empty, so it checks but
// does not link.
#![cfg_attr(not(target_os = "macos"), no_main)]
#![cfg(target_os = "macos")]

use appkit_immediate::{App, Length, Ui, run};

fn main() {
    run(|_| Grid { open: true });
}

struct Grid {
    open: bool,
}

impl App for Grid {
    type Message = ();

    fn update(&mut self, (): ()) {}

    fn view(&mut self, ui: &mut Ui) {
        if ui.should_terminate() {
            self.open = false;
        }
        if !self.open {
            return;
        }
        let rows = [
            Length::Auto,
            Length::Pixels(40.0),
            Length::Star(1.0),
            Length::Star(2.0),
        ];
        let window = ui.window("main", "grid", |ui| {
            ui.grid("rows", &rows, |ui| {
                ui.label("auto", format_args!("auto"));
                ui.label("pixels", format_args!("pixels"));
                ui.label("one", format_args!("one star"));
                ui.label("two", format_args!("two stars"));
            });
        });
        if window.should_close() {
            self.open = false;
        }
    }
}
