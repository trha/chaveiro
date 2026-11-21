use crate::{pass::Ui, tree::Key};
use objc2::MainThreadMarker;
use proptest::prelude::*;
use std::{
    cell::Cell,
    panic::{AssertUnwindSafe, catch_unwind},
    rc::Rc,
};

/// One pass of the model: whether the app declares the cache, its deps, and
/// whether an event reaches a node inside it.
#[derive(Clone, Copy, Debug)]
struct Step {
    declared: bool,
    deps: u8,
    event: bool,
}

proptest! {
    #[test]
    fn the_body_runs_exactly_when_deps_change_the_node_is_new_or_an_event_arrives(
        steps in prop::collection::vec(
            (any::<bool>(), 0u8..3, any::<bool>()).prop_map(|(declared, deps, event)| Step { declared, deps, event }),
            1..20,
        )
    ) {
        let mut ui = test_ui();
        let runs = Rc::new(Cell::new(0));
        // The model: the deps of the last run, or None when the node is gone.
        let mut alive: Option<u8> = None;
        for step in steps {
            ui.begin();
            let event = step.event && alive.is_some();
            if event {
                let inner = ui.tree.find(inner_key()).expect("a live cache holds its state");
                ui.tree.mark_event(inner);
            }
            let before = runs.get();
            if step.declared {
                let runs = Rc::clone(&runs);
                ui.cached("cache", step.deps, |ui| {
                    runs.set(runs.get() + 1);
                    ui.state("inner", || 0u32);
                });
            }
            ui.end();
            let expected = step.declared && (alive != Some(step.deps) || event);
            prop_assert_eq!(runs.get() - before, u32::from(expected));
            alive = step.declared.then_some(step.deps);
        }
    }
}

#[test]
fn state_inside_a_cache_survives_skipped_runs_and_dies_with_the_cache() {
    let mut ui = test_ui();
    let pass = |ui: &mut Ui, declared: bool, deps: u8| -> u32 {
        let mut seen = 0;
        ui.begin();
        if declared {
            ui.cached("cache", deps, |ui| {
                let count = ui.state("count", || 0u32);
                *count += 1;
                seen = *count;
            });
        }
        ui.end();
        seen
    };
    assert_eq!(pass(&mut ui, true, 1), 1);
    assert_eq!(
        pass(&mut ui, true, 1),
        0,
        "a skipped run does not touch the state"
    );
    assert_eq!(
        pass(&mut ui, true, 2),
        2,
        "the state outlived the skipped run"
    );
    pass(&mut ui, false, 2);
    assert_eq!(
        pass(&mut ui, true, 2),
        1,
        "an undeclared cache drops its state"
    );
}

#[test]
fn state_keyed_by_its_inputs_recomputes_only_when_they_change() {
    let mut ui = test_ui();
    let computed = Cell::new(0);
    let pass = |ui: &mut Ui, input: u32| -> u32 {
        ui.begin();
        let value = *ui.state(("double", input), || {
            computed.set(computed.get() + 1);
            input * 2
        });
        ui.end();
        value
    };
    assert_eq!(pass(&mut ui, 3), 6);
    assert_eq!(pass(&mut ui, 3), 6);
    assert_eq!(pass(&mut ui, 4), 8);
    assert_eq!(computed.get(), 2);
    assert!(
        ui.tree
            .find(Key::child(Key::ROOT, ("double", 3u32)))
            .is_none(),
        "the state for the old input is dropped"
    );
}
#[test]
fn a_panicking_build_leaves_no_node_behind() {
    let mut ui = test_ui();
    ui.begin();
    let failed = catch_unwind(AssertUnwindSafe(|| {
        ui.cached("cache", 1u8, |ui| {
            ui.cached("inner", 2u8, |ui| {
                ui.state("count", || 0u32);
                panic!("a planned failure inside two open caches");
            });
        });
    }));
    assert!(failed.is_err());
    ui.close_all();
    ui.begin();
    ui.end();
    assert_eq!(ui.tree.live().len(), 1, "only the root is left");
}

/// The key of the state node inside the cache in the model test.
fn inner_key() -> Key {
    Key::child(Key::child(Key::ROOT, "cache"), "inner")
}

/// A `Ui` for tests that touch only the tree and cache payloads.
fn test_ui() -> Ui {
    // The tests create no AppKit object; the shared target is a plain
    // NSObject subclass, which any thread may create.
    let mtm = unsafe { MainThreadMarker::new_unchecked() };
    Ui::new(mtm, Rc::new(|| {}))
}
