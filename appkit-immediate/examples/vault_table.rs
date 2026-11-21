//! 10,000 rows in a table view that fills a star grid row, each with a
//! label and a button. The status line shows the last pick and how many row
//! views the table keeps, which `tools/smoke.sh` checks stays near one
//! screen while it scrolls.

// The program is macOS only. On other targets it is empty, so it checks but
// does not link.
#![cfg_attr(not(target_os = "macos"), no_main)]
#![cfg(target_os = "macos")]

use appkit_immediate::{App, Length, Orientation, TableViewId, Ui, run};

const ROWS: usize = 10_000;

fn main() {
    run(|_| Vault {
        open: true,
        picked: None,
    });
}

struct Vault {
    open: bool,
    picked: Option<usize>,
}

impl App for Vault {
    type Message = ();

    fn update(&mut self, (): ()) {}

    fn view(&mut self, ui: &mut Ui) {
        if ui.should_terminate() {
            self.open = false;
        }
        if !self.open {
            return;
        }
        let window = ui.window("main", "vault", |ui| {
            ui.grid("rows", &[Length::Star(1.0), Length::Auto], |ui| {
                let table = ui.table_view("entries", ROWS, 0, row);
                let slots = ui.table_view_slots(table);
                ui.label(
                    "status",
                    format_args!("picked: {:?}, slots: {slots}", self.picked),
                );
            });
        });
        if window.should_close() {
            self.open = false;
        }
    }
}

fn row(app: &mut Vault, ui: &mut Ui, _table: TableViewId, index: usize) {
    ui.stack_view("row", Orientation::Horizontal, 8.0, |ui| {
        ui.label("name", format_args!("Entry {index}"));
        if ui.button("pick", "Pick").clicked() {
            app.picked = Some(index);
        }
    });
}
