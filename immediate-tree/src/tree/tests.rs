use super::*;
use proptest::prelude::*;
use std::{
    alloc::{GlobalAlloc, Layout, System},
    cell::Cell,
    collections::HashSet,
};

proptest! {
    #[test]
    fn live_nodes_are_exactly_the_reachable_ones(passes in prop::collection::vec(specs(3), 1..6)) {
        let mut tree = Tree::new(0u8);
        for specs in &passes {
            let before = tree.live().len();
            let dropped = pass(&mut tree, specs);
            let live: HashSet<NodeId> = tree.live().into_iter().collect();
            prop_assert_eq!(&live, &reachable(&tree));
            prop_assert_eq!(tree.key_count(), live.len());
            prop_assert!(live.len() + dropped >= before);
            for id in &live {
                prop_assert_eq!(tree.find(tree.node(*id).key()), Some(*id));
                if let Some(parent) = tree.parent(*id) {
                    prop_assert_eq!(tree.children(parent).iter().filter(|child| *child == id).count(), 1);
                }
            }
        }
    }

    #[test]
    fn an_uncached_pass_holds_exactly_the_declared_nodes(specs in specs(3)) {
        fn count(specs: &[Spec]) -> usize {
            specs.iter().map(|spec| 1 + count(&spec.children)).sum()
        }
        let uncached = |specs: &[Spec]| -> Vec<Spec> {
            fn strip(specs: &[Spec]) -> Vec<Spec> {
                specs.iter().map(|spec| Spec { id: spec.id, cached: false, children: strip(&spec.children) }).collect()
            }
            strip(specs)
        };
        let mut tree = Tree::new(0u8);
        pass(&mut tree, &uncached(&specs));
        // Plus one for the root.
        prop_assert_eq!(tree.live().len(), count(&specs) + 1);
    }
}

#[test]
#[should_panic(expected = "duplicate id")]
fn two_siblings_with_one_id_panic() {
    let mut tree = Tree::new(0u8);
    tree.begin_pass();
    let root = tree.root();
    let key = Key::child(Key::ROOT, "same");
    tree.visit(root, key, || 1);
    tree.visit(root, key, || 2);
}

#[test]
fn a_dropped_id_reads_nothing_after_its_slot_is_reused() {
    let mut tree = Tree::new(0u8);
    let first = Spec {
        id: 1,
        cached: false,
        children: Vec::new(),
    };
    pass(&mut tree, std::slice::from_ref(&first));
    let old = tree
        .find(Key::child(Key::ROOT, 1u8))
        .expect("declared above");
    pass(&mut tree, &[]);
    assert!(tree.get(old).is_none());
    let second = Spec {
        id: 2,
        cached: false,
        children: Vec::new(),
    };
    pass(&mut tree, &[second]);
    let new = tree
        .find(Key::child(Key::ROOT, 2u8))
        .expect("declared above");
    assert!(tree.get(old).is_none());
    assert_eq!(tree.get(new).map(|node| node.payload), Some(2));
}

#[test]
fn an_event_marks_its_ancestors_only() {
    let mut tree = Tree::new(0u8);
    let leaf = Spec {
        id: 2,
        cached: false,
        children: Vec::new(),
    };
    let sibling = Spec {
        id: 3,
        cached: false,
        children: Vec::new(),
    };
    let parent = Spec {
        id: 1,
        cached: false,
        children: vec![leaf],
    };
    pass(&mut tree, &[parent.clone(), sibling.clone()]);
    pass(&mut tree, &[parent, sibling]);
    let parent_key = Key::child(Key::ROOT, 1u8);
    let parent_id = tree.find(parent_key).expect("declared");
    let leaf_id = tree.find(Key::child(parent_key, 2u8)).expect("declared");
    let sibling_id = tree.find(Key::child(Key::ROOT, 3u8)).expect("declared");
    tree.mark_event(leaf_id);
    assert!(tree.has_event(leaf_id));
    assert!(tree.has_event(parent_id));
    assert!(tree.has_event(tree.root()));
    assert!(!tree.has_event(sibling_id));
    tree.begin_pass();
    assert!(!tree.has_event(leaf_id), "an event lasts one pass");
}

#[test]
fn a_steady_pass_allocates_nothing() {
    let row = |id| Spec {
        id,
        cached: false,
        children: vec![Spec {
            id: 0,
            cached: false,
            children: Vec::new(),
        }],
    };
    let specs = vec![
        Spec {
            id: 0,
            cached: false,
            children: (0..4).map(row).collect(),
        },
        row(1),
    ];
    let mut tree = Tree::new(0u8);
    // Warm-up fills the hash map and lets the spare-list pool settle: the
    // third pass still allocates one list (measured), every later pass none.
    for _ in 0..3 {
        pass(&mut tree, &specs);
    }
    let mut counts = Vec::with_capacity(6);
    for _ in 0..6 {
        ALLOCATIONS.set(0);
        COUNTING.set(true);
        let dropped = pass(&mut tree, &specs);
        COUNTING.set(false);
        assert_eq!(dropped, 0);
        counts.push(ALLOCATIONS.get());
    }
    assert_eq!(counts, [0; 6]);
}

#[derive(Clone, Debug)]
struct Spec {
    /// Unique among its siblings.
    id: u8,
    /// When the node already exists, keep it without building its children,
    /// as a cache hit does.
    cached: bool,
    children: Vec<Spec>,
}

fn specs(depth: u32) -> impl Strategy<Value = Vec<Spec>> {
    let leaf = (0u8..4, any::<bool>()).prop_map(|(id, cached)| Spec {
        id,
        cached,
        children: Vec::new(),
    });
    let node = leaf.prop_recursive(depth, 40, 4, |inner| {
        (0u8..4, any::<bool>(), prop::collection::vec(inner, 0..4)).prop_map(
            |(id, cached, children)| Spec {
                id,
                cached,
                children: unique(children),
            },
        )
    });
    prop::collection::vec(node, 0..4).prop_map(unique)
}

/// Keeps the first spec of each id, as an app must give siblings distinct ids.
fn unique(specs: Vec<Spec>) -> Vec<Spec> {
    let mut seen = HashSet::new();
    specs
        .into_iter()
        .filter(|spec| seen.insert(spec.id))
        .collect()
}

/// Builds `specs` under `parent` as a pass does, and returns the number of
/// payloads dropped.
fn build(tree: &mut Tree<u8>, parent: NodeId, parent_key: Key, specs: &[Spec]) -> usize {
    let mut visited = tree.take_list();
    let mut dropped = 0;
    for spec in specs {
        let key = Key::child(parent_key, spec.id);
        let (id, visit) = tree.visit(parent, key, || spec.id);
        visited.push(id);
        if spec.cached && visit == Visit::Existing {
            continue;
        }
        dropped += build(tree, id, key, &spec.children);
    }
    tree.close(parent, &mut visited, |_| dropped += 1);
    tree.recycle_list(visited);
    dropped
}

fn pass(tree: &mut Tree<u8>, specs: &[Spec]) -> usize {
    tree.begin_pass();
    let root = tree.root();
    build(tree, root, Key::ROOT, specs)
}

/// Every node reachable from the root through child lists.
fn reachable(tree: &Tree<u8>) -> HashSet<NodeId> {
    let mut seen = HashSet::new();
    let mut stack = vec![tree.root()];
    while let Some(id) = stack.pop() {
        assert!(seen.insert(id), "node {id:?} is reachable twice");
        stack.extend_from_slice(tree.children(id));
    }
    seen
}

struct CountingAllocator;

thread_local! {
    static COUNTING: Cell<bool> = const { Cell::new(false) };
    static ALLOCATIONS: Cell<usize> = const { Cell::new(0) };
}

// SAFETY: forwards to `System` and only bumps a thread-local counter.
unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if COUNTING.get() {
            ALLOCATIONS.set(ALLOCATIONS.get() + 1);
        }
        // SAFETY: same contract as the caller's.
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        // SAFETY: same contract as the caller's.
        unsafe { System.dealloc(pointer, layout) }
    }

    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        if COUNTING.get() {
            ALLOCATIONS.set(ALLOCATIONS.get() + 1);
        }
        // SAFETY: same contract as the caller's.
        unsafe { System.realloc(pointer, layout, size) }
    }
}

#[global_allocator]
static GLOBAL: CountingAllocator = CountingAllocator;
