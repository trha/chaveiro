//! A pass that declares what the last pass declared allocates nothing and
//! sends the toolkit no child edits.

use immediate_tree::{ChildKind, Core, Node, NodeId, Shell, cache, close, edits};
use std::{
    alloc::{GlobalAlloc, Layout, System},
    cell::Cell,
};

/// Counts the allocations of the thread that turns `COUNTING` on, so the
/// test harness's other threads do not show up.
struct CountingAllocator;

thread_local! {
    static COUNTING: Cell<bool> = const { Cell::new(false) };
    static ALLOCATIONS: Cell<u64> = const { Cell::new(0) };
}

fn count() {
    // `try_with` fails only while the thread's locals are torn down, when
    // nothing is counted anyway.
    let _ = COUNTING.try_with(|counting| {
        if counting.get() {
            ALLOCATIONS.with(|allocations| allocations.set(allocations.get() + 1));
        }
    });
}

unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        count();
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        unsafe { System.dealloc(pointer, layout) }
    }

    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        count();
        unsafe { System.realloc(pointer, layout, size) }
    }
}

#[global_allocator]
static GLOBAL: CountingAllocator = CountingAllocator;

/// The allocations `run` makes on this thread.
fn allocations_in(run: impl FnOnce()) -> u64 {
    ALLOCATIONS.with(|allocations| allocations.set(0));
    COUNTING.with(|counting| counting.set(true));
    run();
    COUNTING.with(|counting| counting.set(false));
    ALLOCATIONS.with(Cell::get)
}

/// A native node holds the child list its element last got.
struct Ui {
    core: Core<Vec<NodeId>>,
    /// Child edits sent to the toolkit.
    edits: u64,
}

impl Shell for Ui {
    type Native = Vec<NodeId>;
    type Error = !;

    fn core(&mut self) -> &mut Core<Vec<NodeId>> {
        &mut self.core
    }

    fn apply_visual(&mut self, node: NodeId, declared: &mut Vec<NodeId>) -> Result<(), !> {
        let Node::Native(applied) = &mut self.core.tree.node_mut(node).payload else {
            unreachable!("close applies only native nodes");
        };
        self.edits += edits(applied, declared).count() as u64;
        std::mem::swap(applied, declared);
        Ok(())
    }

    fn release(&mut self, _: Vec<NodeId>) {}
}

/// Declares `panels` panels of `leaves` leaves each, every panel in a cache
/// when `cached`. Panel 0 leaves out leaf `skip` when one is given.
fn pass(ui: &mut Ui, panels: usize, leaves: usize, cached: bool, skip: Option<usize>) {
    let panel = |ui: &mut Ui, index: usize| {
        let (node, _) = ui
            .core
            .visit(index, ChildKind::Visual, || Node::Native(Vec::new()));
        ui.core.open(node);
        for leaf in (0..leaves).filter(|&leaf| index != 0 || Some(leaf) != skip) {
            ui.core
                .visit(leaf, ChildKind::Visual, || Node::Native(Vec::new()));
        }
        close(ui)
    };
    ui.core.tree.begin_pass();
    let root = ui.core.tree.root();
    ui.core.open(root);
    for index in 0..panels {
        let built = match cached {
            false => panel(ui, index),
            true => cache(ui, ("cache", index), skip, |ui| panel(ui, index)),
        };
        let Ok(()) = built;
    }
    let Ok(()) = close(ui);
}

fn ui() -> Ui {
    Ui {
        core: Core::new(|| true),
        edits: 0,
    }
}

#[test]
fn steady_pass_allocates_nothing() {
    for cached in [false, true] {
        let mut ui = ui();
        // Two passes: the first builds, the second grows the reused lists.
        pass(&mut ui, 200, 99, cached, None);
        pass(&mut ui, 200, 99, cached, None);
        let allocations = allocations_in(|| pass(&mut ui, 200, 99, cached, None));
        assert_eq!(allocations, 0, "cached: {cached}");
    }
}

#[test]
fn steady_pass_sends_no_edits() {
    for cached in [false, true] {
        let mut ui = ui();
        pass(&mut ui, 200, 99, cached, None);
        ui.edits = 0;
        pass(&mut ui, 200, 99, cached, None);
        assert_eq!(ui.edits, 0, "cached: {cached}");
    }
}

#[test]
fn removing_one_leaf_sends_one_edit() {
    for cached in [false, true] {
        let mut ui = ui();
        pass(&mut ui, 200, 99, cached, None);
        ui.edits = 0;
        pass(&mut ui, 200, 99, cached, Some(40));
        assert_eq!(ui.edits, 1, "cached: {cached}");
    }
}

/// The counter sees allocations at all, so a zero above means something.
#[test]
fn first_pass_allocates() {
    let mut ui = ui();
    assert!(allocations_in(|| pass(&mut ui, 200, 99, false, None)) > 0);
}
