//! The immediate-mode spike's vault on the crate: an unlock row, a filter, a
//! scrolling list of 1,000 entries, and a details window whose close the app
//! confirms. `tests/ui/vault.ps1` drives it.
//!
//! `RUST_LOG=winui_immediate=trace` logs turns, realized rows and event
//! registrations, with span times. `VAULT_BENCH=1` runs passes on a timer,
//! counts the Rust allocations of each steady pass on the UI thread, prints
//! a summary and exits.

use std::{
    alloc::{GlobalAlloc, Layout, System},
    cell::Cell,
    time::Duration,
};
use windows_core::Result;
use winui_immediate::{App, Length, ListId, Orientation, Ui, run};

fn main() -> Result<()> {
    if std::env::var_os("RUST_LOG").is_some() {
        tracing_subscriber::fmt()
            .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
            .with_span_events(tracing_subscriber::fmt::format::FmtSpan::CLOSE)
            .with_writer(std::io::stderr)
            .init();
    }
    let bench = std::env::var_os("VAULT_BENCH").map(|_| Bench::new());
    let ticking = bench.is_some();
    let entries = (1..=1000)
        .map(|id| Entry {
            id,
            name: format!("entry {id}"),
            password: format!("password {id}"),
        })
        .collect();
    let mut vault = Vault {
        open: true,
        entries,
        filter: String::new(),
        visible: Vec::new(),
        revision: 0,
        unlocked: false,
        details: Details::Closed,
        bench,
    };
    vault.refilter();
    run(move |handle| {
        if ticking {
            std::thread::spawn(move || {
                while handle.send(()) {
                    std::thread::sleep(Duration::from_millis(10));
                }
            });
        }
        Ok(vault)
    })?;
    eprintln!("run returned");
    Ok(())
}

struct Vault {
    open: bool,
    entries: Vec<Entry>,
    filter: String,
    /// Indexes into `entries` that match `filter`.
    visible: Vec<usize>,
    /// Changes whenever `visible` changes.
    revision: u64,
    unlocked: bool,
    details: Details,
    bench: Option<Bench>,
}

struct Entry {
    id: u64,
    name: String,
    password: String,
}

/// The details window: its close goes through the app, which asks once.
#[derive(Clone, Copy, PartialEq)]
enum Details {
    Closed,
    Open,
    /// The user asked to close once; the next request closes it.
    Confirming,
}

impl App for Vault {
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
        let main = ui.window("main", "winui-immediate vault", |ui| {
            ui.grid(
                "rows",
                &[Length::Auto, Length::Auto, Length::Auto, Length::Star(1.0)],
                |ui| {
                    ui.label(
                        "status",
                        format_args!(
                            "{} of {} entries, {}",
                            self.visible.len(),
                            self.entries.len(),
                            if self.unlocked { "unlocked" } else { "locked" }
                        ),
                    )?;
                    ui.stack("actions", Orientation::Horizontal, 8.0, |ui| {
                        let password = ui.password_box("password", "Master password")?.handle();
                        let label = if self.unlocked { "Lock" } else { "Unlock" };
                        if ui.button("submit", label)?.clicked()? {
                            let typed = ui.take_password(password)?;
                            self.unlocked = !self.unlocked && !typed.is_empty();
                        }
                        if ui.button("details", "Open details")?.clicked()? {
                            self.details = Details::Open;
                        }
                        Ok(())
                    })?;
                    if ui.text_box("filter", "Filter", &mut self.filter)?.changed() {
                        self.refilter();
                    }
                    ui.list("entries", self.visible.len(), self.revision, Vault::row)?;
                    Ok(())
                },
            )?;
            Ok(())
        })?;
        if main.close_requested() {
            self.open = false;
            return Ok(());
        }
        if self.details != Details::Closed {
            let details = ui.window("details", "vault details", |ui| {
                let text = match self.details {
                    Details::Confirming => "Close again to confirm",
                    _ => "1,000 entries in this vault",
                };
                ui.label("text", format_args!("{text}"))?;
                Ok(())
            })?;
            if details.close_requested() {
                eprintln!("details close requested");
                self.details = match self.details {
                    Details::Open => Details::Confirming,
                    _ => Details::Closed,
                };
            }
        }
        Ok(())
    }
}

impl Vault {
    fn refilter(&mut self) {
        self.revision += 1;
        self.visible.clear();
        let filter = self.filter.as_str();
        self.visible.extend(
            (0..self.entries.len()).filter(|&index| self.entries[index].name.contains(filter)),
        );
    }

    /// One row: the entry's name, its password or dots, and a Reveal button.
    /// Whether an entry is revealed is UI state, kept per entry by the list.
    fn row(&mut self, ui: &mut Ui, list: ListId, row: usize) -> Result<()> {
        let entry = &self.entries[self.visible[row]];
        let revealed = *ui.list_state(list, entry.id, || false);
        let shown = if revealed {
            entry.password.as_str()
        } else {
            "••••••"
        };
        let mut clicked = false;
        ui.stack("row", Orientation::Horizontal, 12.0, |ui| {
            ui.label("name", format_args!("{}", entry.name))?;
            ui.label("password", format_args!("{shown}"))?;
            clicked = ui.button("reveal", "Reveal")?.clicked()?;
            Ok(())
        })?;
        if clicked {
            *ui.list_state(list, entry.id, || false) = !revealed;
        }
        Ok(())
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
