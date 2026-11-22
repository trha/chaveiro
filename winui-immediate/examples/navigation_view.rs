//! A navigation view in a resizable window with its own title bar. The
//! content names the selected entry, with its glyph as a font icon. Add
//! adds an item, so the pane rebuilds and keeps its selection; Count bumps
//! the first count, which updates in place; Pick Trash selects from code.
//! Picks print to stderr. `tests/ui/navigation_view.ps1` drives it.

// The program is Windows only. On other targets it is empty, so it checks
// but does not link.
#![cfg_attr(not(windows), no_main)]
#![cfg(windows)]

use winui_immediate::{
    App, Length, NavigationEntry, NavigationSelection, Orientation, Ui, run,
};

fn main() -> windows_core::Result<()> {
    run(|| {
        Ok(Preview {
            open: true,
            // Favorites; entry 0 is the Vault header.
            selected: Some(NavigationSelection::Entry(2)),
            extra: 0,
            count: 42,
        })
    })
}

struct Preview {
    open: bool,
    selected: Option<NavigationSelection>,
    /// Folders added with Add.
    extra: usize,
    count: usize,
}

const TRASH: char = '\u{E74D}';

impl App for Preview {
    fn view(&mut self, ui: &mut Ui) -> windows_core::Result<()> {
        if !self.open {
            return Ok(());
        }
        let names: Vec<String> = (0..self.extra)
            .map(|index| format!("Folder {index}"))
            .collect();
        let mut entries = vec![
            NavigationEntry::Header("Vault"),
            NavigationEntry::Item {
                glyph: '\u{E8A9}',
                label: "All items",
                count: Some(self.count),
            },
            NavigationEntry::Item {
                glyph: '\u{E734}',
                label: "Favorites",
                count: Some(6),
            },
            NavigationEntry::Header("Folders"),
        ];
        entries.extend(names.iter().map(|name| NavigationEntry::Item {
            glyph: '\u{E8B7}',
            label: name,
            count: None,
        }));
        entries.push(NavigationEntry::Separator);
        entries.push(NavigationEntry::Item {
            glyph: TRASH,
            label: "Trash",
            count: Some(2),
        });
        let trash = entries.len() - 1;

        let mut add = false;
        let mut bump = false;
        let mut pick_trash = false;
        let mut picked = false;
        // Add and Count are the only changes to the entries.
        let revision = (self.extra as u64) << 32 | self.count as u64;
        let mut main = ui.window("main", "winui-immediate navigation view", |ui| {
            ui.grid("window", &[Length::Auto, Length::Star(1.0)], |ui| {
                ui.title_bar("title bar", "Navigation view", false, |_| Ok(()))?;
                let push = |pane: &mut Vec<_>| pane.extend_from_slice(&entries);
                picked = ui
                    .navigation_view("nav", revision, push, &mut self.selected, |ui, selected| {
                        ui.stack("content", Orientation::Vertical, 8.0, |ui| {
                            let (glyph, name) = match selected {
                                Some(NavigationSelection::Settings) => ('\u{E713}', "Settings"),
                                Some(NavigationSelection::Entry(index)) => match entries[index] {
                                    NavigationEntry::Item { glyph, label, .. } => (glyph, label),
                                    _ => ('?', "?"),
                                },
                                None => (' ', "Nothing"),
                            };
                            ui.font_icon("icon", glyph)?.size(32.0)?.secondary(true)?;
                            ui.label("selected", format_args!("Selected: {name}"))?;
                            add = ui.button("add", "Add")?.clicked()?;
                            bump = ui.button("count", "Count")?.clicked()?;
                            pick_trash = ui.button("trash", "Pick Trash")?.clicked()?;
                            Ok(())
                        })?
                        .padding([24.0, 24.0, 24.0, 24.0])?;
                        Ok(())
                    })?
                    .picked();
                Ok(())
            })?;
            Ok(())
        })?;
        main.mica()?
            .extends_into_title_bar()?
            .initial_size(1100.0, 700.0)?;
        if main.close_requested() {
            self.open = false;
        }
        if picked {
            eprintln!("picked {:?}", self.selected);
        }
        if add {
            // Trash moves down one, so a selected Trash follows it.
            if self.selected == Some(NavigationSelection::Entry(trash)) {
                self.selected = Some(NavigationSelection::Entry(trash + 1));
            }
            self.extra += 1;
        }
        if bump {
            self.count += 1;
        }
        if pick_trash {
            self.selected = Some(NavigationSelection::Entry(trash));
        }
        Ok(())
    }
}
