//! Times one pass over a tree of `PANELS` containers with `LEAVES` controls
//! each, against a shell that counts its calls instead of calling a toolkit.
//! Reports time per node, heap allocations per pass, and the child edits
//! the shell would send to the toolkit.

use immediate_tree::{ChildKind, Core, Node, NodeId, Shell, cache, close, edits};
use std::{
    alloc::{GlobalAlloc, Layout, System},
    hint::black_box,
    sync::atomic::{AtomicU64, Ordering},
    time::Instant,
};

/// Counts every allocation and reallocation the process makes.
struct CountingAllocator;

static ALLOCATIONS: AtomicU64 = AtomicU64::new(0);

unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        unsafe { System.dealloc(pointer, layout) }
    }

    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        unsafe { System.realloc(pointer, layout, size) }
    }
}

#[global_allocator]
static GLOBAL: CountingAllocator = CountingAllocator;

/// A native node holds the child list its toolkit element last got.
struct Ui {
    core: Core<Vec<NodeId>>,
    /// Calls to `apply_visual`.
    applies: u64,
    /// Child edits those calls would send: the toolkit calls.
    edits: u64,
}

impl Shell for Ui {
    type Native = Vec<NodeId>;
    type Error = !;

    fn core(&mut self) -> &mut Core<Vec<NodeId>> {
        &mut self.core
    }

    fn apply_visual(&mut self, node: NodeId, declared: &mut Vec<NodeId>) -> Result<(), !> {
        self.applies += 1;
        let Node::Native(applied) = &mut self.core.tree.node_mut(node).payload else {
            unreachable!("close applies only native nodes");
        };
        self.edits += edits(applied, declared).count() as u64;
        std::mem::swap(applied, declared);
        Ok(())
    }

    fn release(&mut self, _: Vec<NodeId>) {}
}

const PANELS: usize = 200;
const LEAVES: usize = 99;
/// Panels plus leaves.
const NODES: usize = PANELS * (LEAVES + 1);

/// What one pass declares. `skip_leaf` leaves out one leaf of panel 0, and
/// `cached` wraps each panel in a cache keyed on `revision`.
#[derive(Clone, Copy)]
struct Shape {
    skip_leaf: Option<usize>,
    cached: bool,
    revision: u64,
}

fn panel(ui: &mut Ui, index: usize, shape: Shape) -> Result<(), !> {
    let (node, _) = ui
        .core
        .visit(index, ChildKind::Visual, || Node::Native(Vec::new()));
    ui.core.open(node);
    for leaf in 0..LEAVES {
        if index == 0 && shape.skip_leaf == Some(leaf) {
            continue;
        }
        ui.core
            .visit(leaf, ChildKind::Visual, || Node::Native(Vec::new()));
    }
    close(ui)
}

fn pass(ui: &mut Ui, shape: Shape) {
    ui.core.tree.begin_pass();
    let root = ui.core.tree.root();
    ui.core.open(root);
    for index in 0..PANELS {
        let built = match shape.cached {
            false => panel(ui, index, shape),
            true => {
                // Panel 0 depends on the revision; the rest never change.
                let deps = if index == 0 { shape.revision } else { 0 };
                cache(ui, ("cache", index), deps, |ui| panel(ui, index, shape))
            }
        };
        let Ok(()) = built;
    }
    let Ok(()) = close(ui);
}

/// Runs `passes` passes of `shape` after one warm-up pass, and prints the
/// mean time per pass and per node, and the allocations and edits per pass.
fn measure(name: &str, ui: &mut Ui, passes: u64, mut shape: impl FnMut(u64) -> Shape) {
    pass(ui, shape(0));
    ui.applies = 0;
    ui.edits = 0;
    // With `PASS_BENCH_COLD=1`, every pass starts with cold caches, as in an
    // app, where the toolkit's layout and render run between passes. The
    // buffer is twice a large last-level cache.
    let mut evict = std::env::var_os("PASS_BENCH_COLD").map(|_| vec![0u8; 64 << 20]);
    let mut elapsed = std::time::Duration::ZERO;
    let mut allocations = 0;
    for index in 1..=passes {
        if let Some(evict) = &mut evict {
            for byte in evict.iter_mut().step_by(64) {
                *byte = byte.wrapping_add(1);
            }
            black_box(&evict);
        }
        let before = ALLOCATIONS.load(Ordering::Relaxed);
        let start = Instant::now();
        pass(ui, shape(index));
        elapsed += start.elapsed();
        allocations += ALLOCATIONS.load(Ordering::Relaxed) - before;
    }
    let per_pass = elapsed.as_secs_f64() / passes as f64;
    println!(
        "{name:<34} {:>8.1} us/pass {:>6.1} ns/node {:>7.2} allocs/pass {:>6.1} applies/pass {:>6.1} edits/pass",
        per_pass * 1e6,
        per_pass * 1e9 / NODES as f64,
        allocations as f64 / passes as f64,
        ui.applies as f64 / passes as f64,
        ui.edits as f64 / passes as f64,
    );
    black_box(&ui.core);
}

fn main() {
    let new_ui = || Ui {
        core: Core::new(|| true),
        applies: 0,
        edits: 0,
    };
    let passes = 2_000;
    println!("{PANELS} panels x {LEAVES} leaves = {NODES} nodes, {passes} passes each");
    let plain = |skip_leaf| Shape {
        skip_leaf,
        cached: false,
        revision: 0,
    };

    let mut ui = new_ui();
    measure("steady, no cache", &mut ui, passes, |_| plain(None));

    let mut ui = new_ui();
    measure("one leaf toggles each pass", &mut ui, passes, |index| {
        plain((index % 2 == 1).then_some(40))
    });

    let mut ui = new_ui();
    measure("steady, every panel cached", &mut ui, passes, |_| Shape {
        skip_leaf: None,
        cached: true,
        revision: 0,
    });

    let mut ui = new_ui();
    measure(
        "cached, panel 0 rebuilds each pass",
        &mut ui,
        passes,
        |index| Shape {
            skip_leaf: None,
            cached: true,
            revision: index,
        },
    );

    // The first pass builds every node from nothing.
    let allocations = ALLOCATIONS.load(Ordering::Relaxed);
    let start = Instant::now();
    let mut ui = new_ui();
    pass(&mut ui, plain(None));
    println!(
        "{:<34} {:>8.1} us      {:>6.1} ns/node {:>7} allocs",
        "first pass, cold",
        start.elapsed().as_secs_f64() * 1e6,
        start.elapsed().as_secs_f64() * 1e9 / NODES as f64,
        ALLOCATIONS.load(Ordering::Relaxed) - allocations,
    );
}
