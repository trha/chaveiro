//! The vault window on its own, over a made-up vault, so it can be seen and
//! driven without an account. A thread plays a script: it prints the
//! sidebar's rows and the other text after the first pass, picks the Work
//! folder through the table as a click would, shows each sync state, and
//! counts the Rust allocations of steady passes on the main thread, which
//! must be zero. Lock prints to stderr and closes the window.

// The program is macOS only. On other targets it is empty, so it checks but
// does not link.
#![cfg_attr(not(target_os = "macos"), no_main)]
#![cfg(target_os = "macos")]
#![allow(dead_code, reason = "shares modules with the app")]

#[path = "../src/vault.rs"]
mod vault;

use appkit_immediate::{App, Ui, run};
use chaveiro_core::{
    core::{UserAction, VaultScreen},
    crypto::SecretString,
    vault::{CipherKind, CipherSummary, FolderSummary, VaultIndex},
};
use objc2::{MainThreadMarker, Message, rc::Retained};
use objc2_app_kit::{NSApplication, NSTableView, NSTextField, NSView};
use objc2_foundation::NSIndexSet;
use std::{
    alloc::{GlobalAlloc, Layout, System},
    cell::Cell,
    thread,
    time::Duration,
};
use uuid::Uuid;
use vault::{VaultAction, VaultForm};

fn main() {
    run(|handle| {
        thread::spawn(move || {
            for step in SCRIPT {
                thread::sleep(Duration::from_millis(300));
                handle.send(*step);
            }
        });
        Preview {
            index: sample(),
            form: VaultForm::default(),
            open: true,
            sync: Sync::Synced,
            steady: None,
        }
    });
}

#[derive(Clone, Copy, Debug)]
enum Step {
    Dump,
    /// Selects the sidebar row with this label through the table.
    Pick(&'static str),
    Sync(Sync),
    /// Starts counting allocations.
    Measure,
    /// A message that changes nothing, so its pass is a steady one.
    Tick,
    Report,
    Quit,
}

#[derive(Clone, Copy, Debug)]
enum Sync {
    Synced,
    Syncing,
    Failed,
}

const SCRIPT: &[Step] = &[
    Step::Dump,
    Step::Pick("Work"),
    Step::Dump,
    Step::Sync(Sync::Syncing),
    Step::Dump,
    Step::Sync(Sync::Failed),
    Step::Dump,
    Step::Measure,
    Step::Tick,
    Step::Tick,
    Step::Tick,
    Step::Tick,
    Step::Tick,
    Step::Report,
    Step::Quit,
];

struct Preview {
    index: VaultIndex,
    form: VaultForm,
    open: bool,
    sync: Sync,
    /// The allocation count when measuring began.
    steady: Option<usize>,
}

impl App for Preview {
    type Message = Step;

    fn update(&mut self, step: Step) {
        let mtm = MainThreadMarker::new().expect("update runs on the main thread");
        let content = || {
            NSApplication::sharedApplication(mtm)
                .windows()
                .firstObject()
                .expect("the window is open")
                .contentView()
                .expect("a window has a content view")
        };
        match step {
            Step::Dump => {
                println!("-- filter {:?}", self.form.filter);
                dump(&content());
            }
            Step::Pick(label) => {
                let content = content();
                let table = find_table(&content).expect("the sidebar is a table");
                let row = (0..table.numberOfRows())
                    .find(|&row| {
                        table
                            .viewAtColumn_row_makeIfNecessary(0, row, false)
                            .is_some_and(|cell| {
                                texts(&cell).first().is_some_and(|text| text == label)
                            })
                    })
                    .expect("the sidebar lists the label");
                table.selectRowIndexes_byExtendingSelection(
                    &NSIndexSet::indexSetWithIndex(row as usize),
                    false,
                );
                println!("-- picked {label}");
            }
            Step::Sync(sync) => self.sync = sync,
            Step::Measure => self.steady = Some(ALLOCATIONS.get()),
            Step::Tick => {}
            Step::Report => {
                let start = self.steady.expect("measuring began");
                println!("-- steady passes allocated {}", ALLOCATIONS.get() - start);
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
        let now_ms = 1_800_000_000_000;
        let (syncing, sync_error) = match self.sync {
            Sync::Synced => (false, None),
            Sync::Syncing => (true, None),
            Sync::Failed => (false, Some("offline")),
        };
        let screen = VaultScreen {
            email: "alex@example.com",
            index: &self.index,
            index_revision: 1,
            open: None,
            syncing,
            sync_error,
            error: None,
            revision: None,
            synced_at: Some(now_ms - 2 * 60 * 1000),
            device_unlock: false,
            device_enrolling: false,
            device_error: None,
            ssh_approval: None,
        };
        match vault::view(ui, &mut self.form, &screen, now_ms) {
            None => {}
            Some(VaultAction::Close) => self.open = false,
            Some(VaultAction::User(UserAction::Lock)) => {
                eprintln!("lock");
                self.open = false;
            }
            Some(VaultAction::User(_)) => {}
        }
    }
}

fn find_table(view: &NSView) -> Option<Retained<NSTableView>> {
    if let Ok(table) = Retained::downcast::<NSTableView>(view.retain()) {
        return Some(table);
    }
    view.subviews().iter().find_map(|child| find_table(&child))
}

/// The text of every text field in `view`, depth first.
fn texts(view: &NSView) -> Vec<String> {
    let mut found = Vec::new();
    if let Ok(field) = Retained::downcast::<NSTextField>(view.retain()) {
        found.push(field.stringValue().to_string());
    }
    for child in view.subviews().iter() {
        found.extend(texts(&child));
    }
    found
}

/// Prints the sidebar's rows, with the selected one marked, then the
/// window's other text.
fn dump(content: &NSView) {
    let table = find_table(content).expect("the sidebar is a table");
    let mut rows = Vec::new();
    for row in 0..table.numberOfRows() {
        let mark = if table.selectedRow() == row { ">" } else { " " };
        let cell = table.viewAtColumn_row_makeIfNecessary(0, row, false);
        let text = cell.map(|cell| texts(&cell)).unwrap_or_default();
        println!("{mark} {}", text.join(" | "));
        rows.extend(text);
    }
    let mut rest = texts(content);
    for text in &rows {
        if let Some(position) = rest.iter().position(|other| other == text) {
            rest.remove(position);
        }
    }
    println!("  other: {rest:?}");
}

/// Two folders, one that did not decrypt, and items of every type: 9 live
/// (3 favorites, 2 with no folder, 1 of an unknown type) and 2 in the
/// trash.
fn sample() -> VaultIndex {
    let folder = |id, name: &str, readable| FolderSummary {
        id: Uuid::from_u128(id),
        name: SecretString::new(name.to_owned()),
        readable,
    };
    let item = |id, kind, name: &str, folder: u128, favorite, deleted| CipherSummary {
        id: Uuid::from_u128(id),
        kind,
        name: SecretString::new(name.to_owned()),
        subtitle: SecretString::default(),
        folder_id: (folder != 0).then(|| Uuid::from_u128(folder)),
        organization_id: None,
        favorite,
        deleted,
        readable: true,
    };
    VaultIndex {
        // Sorted as the core sorts them: by name, the unreadable one first.
        folders: vec![
            folder(3, "", false),
            folder(1, "Personal", true),
            folder(2, "Work", true),
        ],
        ciphers: vec![
            item(10, CipherKind::Login, "Amazon", 1, false, false),
            item(11, CipherKind::Login, "GitHub", 2, true, false),
            item(12, CipherKind::Login, "Mail server", 2, false, false),
            item(13, CipherKind::Card, "Personal Visa", 1, false, false),
            item(14, CipherKind::Identity, "Alex Lima", 0, false, false),
            item(15, CipherKind::SecureNote, "Home Wi-Fi", 1, true, false),
            item(16, CipherKind::SshKey, "Deploy key", 2, true, false),
            item(17, CipherKind::Unknown(7), "Shared drive", 3, false, false),
            item(18, CipherKind::Login, "Old forum", 0, false, false),
            item(19, CipherKind::Login, "Deleted login", 1, true, true),
            item(20, CipherKind::Card, "Expired card", 0, false, true),
        ],
        ssh_keys: Vec::new(),
    }
}

thread_local! {
    static ALLOCATIONS: Cell<usize> = const { Cell::new(0) };
}

struct CountingAllocator;

// SAFETY: forwards to `System` and only bumps a thread-local counter.
unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        ALLOCATIONS.set(ALLOCATIONS.get() + 1);
        // SAFETY: same contract as the caller's.
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        // SAFETY: same contract as the caller's.
        unsafe { System.dealloc(pointer, layout) }
    }

    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        ALLOCATIONS.set(ALLOCATIONS.get() + 1);
        // SAFETY: same contract as the caller's.
        unsafe { System.realloc(pointer, layout, size) }
    }
}

#[global_allocator]
static GLOBAL: CountingAllocator = CountingAllocator;
