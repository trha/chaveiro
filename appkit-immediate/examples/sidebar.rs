//! A split view with a source list sidebar over a footer, and a content
//! pane that names the pick. A thread plays a script: a user pick (sent
//! through the table, so the delegate reports it), a selection from code, a
//! rebuild of the entries that keeps the selection, and a pick of a header
//! from code, which shows no selection. After each step it prints the app's selection
//! and the sidebar material, every text field and the table with its frame
//! in the window, and checks
//! the layout is not ambiguous.

// The program is macOS only. On other targets it is empty, so it checks but
// does not link.
#![cfg_attr(not(target_os = "macos"), no_main)]
#![cfg(target_os = "macos")]

use appkit_immediate::{App, Length, Orientation, Style, Ui, run};
use objc2::{MainThreadMarker, Message, rc::Retained};
use objc2_app_kit::{NSApplication, NSTableView, NSTextField, NSView, NSVisualEffectView};
use objc2_foundation::NSIndexSet;
use std::{thread, time::Duration};

fn main() {
    run(|handle| {
        thread::spawn(move || {
            for step in SCRIPT {
                thread::sleep(Duration::from_millis(400));
                handle.send(*step);
            }
        });
        Sidebar {
            open: true,
            selected: Some(1),
            folders: vec!["Personal", "Work"],
            revision: 0,
            picks: 0,
        }
    });
}

#[derive(Clone, Copy, Debug)]
enum Step {
    Dump,
    /// Selects a row through the table, as a click would.
    UserPick(usize),
    /// Sets the app's selection.
    Select(usize),
    /// Adds a folder, so the entries change.
    AddFolder,
    Quit,
}

const SCRIPT: &[Step] = &[
    Step::Dump,
    Step::UserPick(4),
    Step::Dump,
    Step::Select(5),
    Step::Dump,
    Step::AddFolder,
    Step::Dump,
    Step::UserPick(0),
    Step::Dump,
    Step::Quit,
];

struct Sidebar {
    open: bool,
    selected: Option<usize>,
    folders: Vec<&'static str>,
    revision: u64,
    picks: usize,
}

impl App for Sidebar {
    type Message = Step;

    fn update(&mut self, step: Step) {
        let mtm = MainThreadMarker::new().expect("update runs on the main thread");
        let window = NSApplication::sharedApplication(mtm)
            .windows()
            .firstObject()
            .expect("the window is open");
        let content = window.contentView().expect("a window has a content view");
        match step {
            Step::Dump => {
                println!(
                    "window content {:.0}x{:.0}, selected {:?}, picks {}, ambiguous {}",
                    content.frame().size.width,
                    content.frame().size.height,
                    self.selected,
                    self.picks,
                    ambiguous(&content)
                );
                dump(&content, &content, 0);
            }
            Step::UserPick(row) => {
                let table = find_table(&content).expect("the sidebar is a table");
                table.selectRowIndexes_byExtendingSelection(
                    &NSIndexSet::indexSetWithIndex(row),
                    false,
                );
                println!("user picks row {row}");
            }
            Step::Select(position) => {
                self.selected = Some(position);
                println!("app selects {position}");
            }
            Step::AddFolder => {
                self.folders.insert(0, "Archive");
                self.revision += 1;
                // The selected folder moved down one row.
                self.selected = self.selected.map(|position| position + 1);
                println!("added a folder");
            }
            Step::Quit => self.open = false,
        }
    }

    fn view(&mut self, ui: &mut Ui) {
        if ui.should_terminate() {
            self.open = false;
        }
        if !self.open {
            return;
        }
        let folders = &self.folders;
        let mut window = ui.window("main", "sidebar", |ui| {
            ui.split_view("split", |ui| {
                ui.grid("sidebar", &[Length::Star(1.0), Length::Auto], |ui| {
                    let list = ui.source_list("list", self.revision, &mut self.selected, |list| {
                        list.header("Vault");
                        list.item("tray.full", "All Items", Some(42));
                        list.item("star", "Favorites", Some(6));
                        list.header("Folders");
                        for folder in folders {
                            list.item("folder", folder, Some(3));
                        }
                        list.spacer();
                        list.item("trash", "Trash", None);
                    });
                    if list.picked() {
                        self.picks += 1;
                    }
                    ui.label("footer", format_args!("Synced 2 minutes ago"));
                });
                ui.stack_view("content", Orientation::Vertical, 8.0, |ui| {
                    ui.label("title", format_args!("Entry {:?}", self.selected))
                        .style(Style::Title2);
                });
            });
        });
        window.full_size_content().initial_size(800.0, 500.0);
        if window.should_close() {
            self.open = false;
        }
    }
}

fn ambiguous(view: &NSView) -> bool {
    view.hasAmbiguousLayout() || view.subviews().iter().any(|child| ambiguous(&child))
}

fn find_table(view: &NSView) -> Option<Retained<NSTableView>> {
    if let Ok(table) = Retained::downcast::<NSTableView>(view.retain()) {
        return Some(table);
    }
    view.subviews().iter().find_map(|child| find_table(&child))
}

/// Prints each text field's text and each table's selection, with frames in
/// `root`'s coordinates.
fn dump(root: &NSView, view: &NSView, depth: usize) {
    let frame = view.convertRect_toView(view.bounds(), Some(root));
    let place = format!(
        "{:.0},{:.0} {:.0}x{:.0}",
        frame.origin.x, frame.origin.y, frame.size.width, frame.size.height
    );
    if let Ok(field) = Retained::downcast::<NSTextField>(view.retain()) {
        println!(
            "{:depth$}text {:?} at {place}",
            "",
            field.stringValue().to_string()
        );
    } else if Retained::downcast::<NSVisualEffectView>(view.retain()).is_ok() {
        println!("{:depth$}sidebar material at {place}", "");
    } else if let Ok(table) = Retained::downcast::<NSTableView>(view.retain()) {
        println!(
            "{:depth$}table rows {} selected {} at {place}",
            "",
            table.numberOfRows(),
            table.selectedRow()
        );
    }
    for child in view.subviews().iter() {
        dump(root, &child, depth + 1);
    }
}
