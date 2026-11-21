//! Spike: a virtualized vault list with a live filter in windows-reactor.
//!
//! The root owns every entry and the filtered index list. The list is an
//! `ItemsRepeater` fed by a `VirtualSource`, so only realized rows get views.
//! A probe types a fixed filter sequence and prints, per filter change, the
//! allocations, the row renders, and the time from `update` to the end of
//! publication.

// The program is Windows only. On other targets it is empty, so it checks
// but does not link.
#![cfg_attr(not(windows), no_main)]
#![cfg(windows)]

use std::{
    rc::Rc,
    sync::atomic::{AtomicUsize, Ordering},
    time::{Duration, Instant},
};
use windows_reactor::*;

/// Spike probe: counts heap allocations on all threads.
struct CountingAllocator;

static ALLOCATIONS: AtomicUsize = AtomicUsize::new(0);
static ROW_RENDERS: AtomicUsize = AtomicUsize::new(0);
/// Spike probe: zero every block on free. Set once in `main` from
/// `SPIKE_WIPE`, before the UI starts.
static WIPE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Zeroes `size` bytes at `pointer` in a way the optimizer keeps.
///
/// # Safety
/// `pointer` must be valid for writes of `size` bytes.
unsafe fn wipe(pointer: *mut u8, size: usize) {
    // SAFETY: the caller guarantees `size` writable bytes.
    unsafe { std::ptr::write_bytes(pointer, 0, size) };
    std::sync::atomic::compiler_fence(Ordering::SeqCst);
}

unsafe impl std::alloc::GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: std::alloc::Layout) -> *mut u8 {
        ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        // SAFETY: same contract as the caller's.
        unsafe { std::alloc::System.alloc(layout) }
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: std::alloc::Layout) {
        if WIPE.load(Ordering::Relaxed) {
            // SAFETY: the block is live and `layout.size()` bytes long.
            unsafe { wipe(pointer, layout.size()) };
        }
        // SAFETY: same contract as the caller's.
        unsafe { std::alloc::System.dealloc(pointer, layout) }
    }

    unsafe fn realloc(
        &self,
        pointer: *mut u8,
        layout: std::alloc::Layout,
        new_size: usize,
    ) -> *mut u8 {
        if !WIPE.load(Ordering::Relaxed) {
            ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
            // SAFETY: same contract as the caller's.
            return unsafe { std::alloc::System.realloc(pointer, layout, new_size) };
        }
        // Move by hand so the old block is wiped before it is freed.
        let new_layout = std::alloc::Layout::from_size_align(new_size, layout.align())
            .expect("realloc keeps a valid alignment");
        // SAFETY: `new_layout` has a non-zero size because the caller's does.
        let new_pointer = unsafe { self.alloc(new_layout) };
        if !new_pointer.is_null() {
            // SAFETY: both blocks are valid for the smaller size and do not
            // overlap.
            unsafe {
                std::ptr::copy_nonoverlapping(pointer, new_pointer, layout.size().min(new_size));
                self.dealloc(pointer, layout);
            }
        }
        new_pointer
    }
}

#[global_allocator]
static GLOBAL: CountingAllocator = CountingAllocator;

/// The filter texts the probe types, one per step.
const PROBE_FILTERS: [&str; 8] = [
    "e", "en", "ent", "entry 4", "entry 42", "entry 4", "", "zzz",
];

struct Entry {
    id: u64,
    name: Rc<str>,
}

#[derive(Clone)]
enum VaultMessage {
    Filter(String),
    Select(u64),
    /// Spike probe: types the next filter in `PROBE_FILTERS`.
    ProbeStep,
}

struct Vault {
    entries: Rc<[Entry]>,
    filter: String,
    /// Indexes into `entries` that match `filter`, in display order.
    visible: Rc<Vec<u32>>,
    /// Bumped whenever `visible` changes, as `VirtualSource` requires.
    visible_revision: u64,
    selected: Option<u64>,
    probe_step: usize,
    /// Allocations, row renders and start time when the last filter changed.
    probe_start: Option<(usize, usize, Instant)>,
}

impl Vault {
    fn set_filter(&mut self, text: String) {
        let needle = text.to_lowercase();
        self.visible = Rc::new(
            self.entries
                .iter()
                .enumerate()
                .filter(|(_, entry)| entry.name.to_lowercase().contains(&needle))
                .map(|(index, _)| u32::try_from(index).expect("fewer than 2^32 entries"))
                .collect(),
        );
        self.visible_revision += 1;
        self.filter = text;
    }
}

fn probe_later(context: &ComponentContext<Vault>) {
    context.spawn_background(|_cancel| {
        std::thread::sleep(Duration::from_millis(700));
        VaultMessage::ProbeStep
    });
}

impl Component for Vault {
    type Input = ();
    type Message = VaultMessage;

    fn create(_input: &(), context: &ComponentContext<Self>) -> Self {
        let entry_count = std::env::var("SPIKE_ROWS")
            .ok()
            .and_then(|text| text.parse().ok())
            .unwrap_or(5000u64);
        let entries: Rc<[Entry]> = (1..=entry_count)
            .map(|id| Entry {
                id,
                name: format!("entry {id}").into(),
            })
            .collect();
        let mut vault = Self {
            entries,
            filter: String::new(),
            visible: Rc::default(),
            visible_revision: 0,
            selected: None,
            probe_step: 0,
            probe_start: None,
        };
        vault.set_filter(String::new());
        probe_later(context);
        vault
    }

    fn update(&mut self, message: VaultMessage, context: &ComponentContext<Self>) {
        match message {
            // WinUI raises TextChanged after the state sets new text, and
            // that echo reaches here with the text we already hold.
            VaultMessage::Filter(text) if text == self.filter => {}
            VaultMessage::Filter(text) => self.set_filter(text),
            VaultMessage::Select(id) => self.selected = Some(id),
            VaultMessage::ProbeStep => {
                let Some(text) = PROBE_FILTERS.get(self.probe_step) else {
                    return;
                };
                self.probe_step += 1;
                self.probe_start = Some((
                    ALLOCATIONS.load(Ordering::Relaxed),
                    ROW_RENDERS.load(Ordering::Relaxed),
                    Instant::now(),
                ));
                self.set_filter((*text).to_owned());
                probe_later(context);
            }
        }
    }

    fn view(&self, _input: &(), context: &mut ViewContext<Self>) -> View {
        context.window_title(format!(
            "Vault spike: {} of {}",
            self.visible.len(),
            self.entries.len()
        ));
        // Runs once, after the first publication.
        context.use_effect("mount", (), || {
            if let Some(started) = STARTED.get() {
                eprintln!(
                    "mount: publish {:?}, allocations {}",
                    started.elapsed(),
                    ALLOCATIONS.load(Ordering::Relaxed),
                );
            }
            None
        });
        if let Some((allocations, row_renders, started)) = self.probe_start {
            let filter = self.filter.clone();
            let visible = self.visible.len();
            // Effects run after a successful publication, so this measures
            // update + view + plan + native apply, but not WinUI layout.
            context.use_effect("probe", self.visible_revision, move || {
                eprintln!(
                    "filter {filter:?}: visible {visible}, publish {:?}, allocations {}, row renders {}",
                    started.elapsed(),
                    ALLOCATIONS.load(Ordering::Relaxed) - allocations,
                    ROW_RENDERS.load(Ordering::Relaxed) - row_renders,
                );
                None
            });
        }
        let source = {
            let entries = Rc::clone(&self.entries);
            let visible = Rc::clone(&self.visible);
            let row_entries = Rc::clone(&self.entries);
            let row_visible = Rc::clone(&self.visible);
            let selected = self.selected;
            // Captureless mapper, so every row gets an equal callback.
            let on_select = context.callback(VaultMessage::Select);
            VirtualSource::new(
                self.visible_revision,
                self.visible.len(),
                move |index| entries[visible[index] as usize].id,
                move |index| {
                    let entry = &row_entries[row_visible[index] as usize];
                    View::component::<EntryRow>(EntryRowInput {
                        id: entry.id,
                        name: Rc::clone(&entry.name),
                        selected: selected == Some(entry.id),
                        on_select: on_select.clone(),
                    })
                },
            )
        };
        Grid::new()
            .rows([GridLength::Auto, GridLength::Star(1.0)])
            .row_spacing(8.0)
            .children((
                TextBox::new()
                    .text(self.filter.clone())
                    .placeholder_text("Filter")
                    .on_text_changed(context.callback(VaultMessage::Filter))
                    .grid_row(0),
                ScrollViewer::new()
                    .grid_row(1)
                    .content(ItemsRepeater::new().virtual_source(source)),
            ))
    }
}

#[derive(Clone, PartialEq)]
struct EntryRowInput {
    id: u64,
    name: Rc<str>,
    selected: bool,
    /// Takes the entry id.
    on_select: Callback<u64>,
}

/// A row with no state of its own. It only renders its input.
struct EntryRow;

impl Component for EntryRow {
    type Input = EntryRowInput;
    type Message = ();

    fn create(_input: &EntryRowInput, _context: &ComponentContext<Self>) -> Self {
        Self
    }

    fn view(&self, input: &EntryRowInput, _context: &mut ViewContext<Self>) -> View {
        ROW_RENDERS.fetch_add(1, Ordering::Relaxed);
        let label = if input.selected {
            format!("> {}", input.name)
        } else {
            input.name.to_string()
        };
        Button::new()
            .on_click({
                let on_select = input.on_select.clone();
                let id = input.id;
                // `call` returns false when the root is gone or its queue is
                // full, and a lost click needs no handling.
                move || {
                    let _ = on_select.call(id);
                }
            })
            .content(label)
    }
}

/// Spike probe: when `main` started, for the mount timing.
static STARTED: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();

fn main() {
    WIPE.store(std::env::var_os("SPIKE_WIPE").is_some(), Ordering::Relaxed);
    STARTED.get_or_init(Instant::now);
    App::run_component::<Vault>(()).expect("WinUI app failed to start");
}
