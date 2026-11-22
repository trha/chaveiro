//! A list of 200 entries, each row with a Delete button that removes its
//! entry. Prints `deleted <id>` to stderr per removal. One click must
//! delete exactly one entry, even though the new length makes WinUI
//! rebuild rows during layout. `tests/ui/list_delete.ps1` drives it.

// The program is Windows only. On other targets it is empty, so it checks
// but does not link.
#![cfg_attr(not(windows), no_main)]
#![cfg(windows)]

use windows_core::Result;
use winui_immediate::{App, ListId, Orientation, Ui, run};

fn main() -> Result<()> {
    run(|| {
        Ok(Entries {
            open: true,
            ids: (1..=200).collect(),
        })
    })
}

struct Entries {
    open: bool,
    ids: Vec<u32>,
}

impl App for Entries {
    fn view(&mut self, ui: &mut Ui) -> Result<()> {
        if !self.open {
            return Ok(());
        }
        let window = ui.window("main", "winui-immediate list delete", |ui| {
            ui.list("entries", self.ids.len(), 0, Entries::row)?;
            Ok(())
        })?;
        if window.close_requested() {
            self.open = false;
        }
        Ok(())
    }
}

impl Entries {
    fn row(&mut self, ui: &mut Ui, _: ListId, index: usize) -> Result<()> {
        let id = self.ids[index];
        let mut delete = false;
        ui.stack("row", Orientation::Horizontal, 8.0, |ui| {
            ui.label("name", format_args!("Entry {id}"))?;
            delete = ui.button("delete", &format!("Delete {id}"))?.clicked()?;
            Ok(())
        })?;
        if delete {
            eprintln!("deleted {id}");
            self.ids.retain(|kept| *kept != id);
        }
        Ok(())
    }
}
