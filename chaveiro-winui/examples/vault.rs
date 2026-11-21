//! The vault window on its own, over a made-up vault, so it can be seen and
//! driven without an account. Lock prints to stderr and closes the window.
//! `tests/ui/vault.ps1` drives it.
//!
//! `VAULT_BENCH=1` runs passes on a timer, counts the Rust allocations of
//! each steady pass on the UI thread, prints a summary and exits.

// The program is Windows only. On other targets it is empty, so it checks
// but does not link.
#![cfg_attr(not(windows), no_main)]
#![cfg(windows)]
#![allow(dead_code, reason = "shares modules with the app")]

#[path = "../src/vault.rs"]
mod vault;

use chaveiro_core::{
    core::{UserAction, VaultScreen},
    crypto::SecretString,
    vault::{CipherKind, CipherSummary, FolderSummary, VaultIndex},
};
use std::{
    alloc::{GlobalAlloc, Layout, System},
    cell::Cell,
};
use uuid::Uuid;
use vault::{VaultAction, VaultForm};
use windows_core::Result;
use winui_immediate::{App, Handle, Ui, run};

fn main() -> Result<()> {
    let bench = std::env::var_os("VAULT_BENCH").map(|_| Bench::new());
    let ticking = bench.is_some();
    run(move |handle: Handle<()>| {
        if ticking {
            std::thread::spawn(move || {
                while handle.send(()) {
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
            });
        }
        Ok(Preview {
            index: sample(),
            form: VaultForm::default(),
            open: true,
            bench,
        })
    })
}

struct Preview {
    index: VaultIndex,
    form: VaultForm,
    open: bool,
    bench: Option<Bench>,
}

impl App for Preview {
    type Message = ();

    fn update(&mut self, (): ()) {}

    fn view(&mut self, ui: &mut Ui) -> Result<()> {
        if let Some(bench) = &mut self.bench
            && bench.measure()
        {
            self.open = false;
        }
        if !self.open {
            return Ok(());
        }
        let screen = VaultScreen {
            email: "alex@example.com",
            index: &self.index,
            index_revision: 1,
            open: None,
            syncing: false,
            sync_error: None,
            error: None,
            revision: None,
            synced_at: None,
            device_unlock: false,
            device_enrolling: false,
            device_error: None,
            ssh_approval: None,
        };
        let pass = vault::view(ui, &mut self.form, &screen)?;
        match pass.action {
            None => {}
            Some(VaultAction::Close) => self.open = false,
            Some(VaultAction::User(UserAction::Lock)) => {
                eprintln!("lock");
                self.open = false;
            }
            Some(VaultAction::User(_)) => {}
        }
        Ok(())
    }
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

/// Counts the UI thread's allocations between the starts of two passes.
struct Bench {
    passes: u32,
    /// The allocation count at the start of the last pass.
    last: usize,
    allocations: Vec<usize>,
}

impl Bench {
    /// Passes before measuring: the window opens and the rows are realized.
    const WARM_UP: u32 = 30;
    const MEASURED: usize = 200;

    /// Reserves every buffer up front, so the bench itself allocates nothing
    /// while it measures.
    fn new() -> Self {
        Bench {
            passes: 0,
            last: 0,
            allocations: Vec::with_capacity(Self::MEASURED),
        }
    }

    /// Records the pass that starts now. Returns true when the bench is done.
    fn measure(&mut self) -> bool {
        if self.allocations.len() == Self::MEASURED {
            return true;
        }
        let count = ALLOCATIONS.get();
        self.passes += 1;
        if self.passes > Self::WARM_UP {
            self.allocations.push(count - self.last);
        }
        self.last = count;
        if self.allocations.len() < Self::MEASURED {
            return false;
        }
        let total: usize = self.allocations.iter().sum();
        let most = self.allocations.iter().max().copied().unwrap_or(0);
        eprintln!(
            "bench: {} steady passes, {total} UI-thread allocations in all, at most {most} in one",
            self.allocations.len()
        );
        true
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
