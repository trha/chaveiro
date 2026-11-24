use crate::{
    testing::{TestError, Ui, ui},
    tree::Key,
};
use futures_core::Stream;
use proptest::prelude::*;
use std::{
    cell::{Cell, RefCell},
    collections::VecDeque,
    future::Future,
    pin::Pin,
    rc::Rc,
    task::{Context, Poll, Waker},
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
        let mut ui = ui();
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
                    ui.state("inner", || 0u32)?;
                    Ok(())
                }).expect("no WinUI call can fail here");
            }
            ui.end().expect("no WinUI call can fail here");
            let expected = step.declared && (alive != Some(step.deps) || event);
            prop_assert_eq!(runs.get() - before, u32::from(expected));
            alive = step.declared.then_some(step.deps);
        }
    }
}

#[test]
fn state_inside_a_cache_survives_skipped_runs_and_dies_with_the_cache() {
    let mut ui = ui();
    let pass = |ui: &mut Ui, declared: bool, deps: u8| -> u32 {
        let mut seen = 0;
        ui.begin();
        if declared {
            ui.cached("cache", deps, |ui| {
                let count = ui.state("count", || 0u32)?;
                *count += 1;
                seen = *count;
                Ok(())
            })
            .expect("no WinUI call can fail here");
        }
        ui.end().expect("no WinUI call can fail here");
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
    let mut ui = ui();
    let computed = Cell::new(0);
    let pass = |ui: &mut Ui, input: u32| -> u32 {
        ui.begin();
        let value = *ui
            .state(("double", input), || {
                computed.set(computed.get() + 1);
                input * 2
            })
            .expect("no WinUI call can fail here");
        ui.end().expect("no WinUI call can fail here");
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
fn a_failed_build_leaves_no_node_behind() {
    let mut ui = ui();
    ui.begin();
    let failed = ui.cached("cache", 1u8, |ui| {
        ui.cached("inner", 2u8, |ui| {
            ui.state("count", || 0u32)?;
            Err(TestError("inner failed"))
        })
    });
    assert!(failed.is_err());
    ui.close_all();
    ui.begin();
    ui.end().expect("no WinUI call can fail here");
    assert_eq!(ui.tree.live().len(), 1, "only the root is left");
}

/// A future that a test finishes by hand.
struct Remote(Rc<RefCell<(Option<u32>, Option<Waker>)>>);

impl Future for Remote {
    type Output = u32;

    fn poll(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<u32> {
        let mut shared = self.0.borrow_mut();
        match shared.0.take() {
            Some(value) => Poll::Ready(value),
            None => {
                shared.1 = Some(context.waker().clone());
                Poll::Pending
            }
        }
    }
}

/// A stream that yields the values a test pushes by hand, and never ends.
struct Feed(Rc<RefCell<(VecDeque<u32>, Option<Waker>)>>);

impl Stream for Feed {
    type Item = u32;

    fn poll_next(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Option<u32>> {
        let mut shared = self.0.borrow_mut();
        match shared.0.pop_front() {
            Some(value) => Poll::Ready(Some(value)),
            None => {
                shared.1 = Some(context.waker().clone());
                Poll::Pending
            }
        }
    }
}

#[test]
fn a_task_that_finishes_under_an_unchanged_cache_reaches_its_body() {
    let mut ui = ui();
    let shared = Rc::new(RefCell::new((None, None)));
    let pass = |ui: &mut Ui| -> Option<u32> {
        let mut seen = None;
        ui.begin();
        ui.cached("cache", 0u8, |ui| {
            seen = ui.task("task", || Remote(Rc::clone(&shared))).take();
            Ok(())
        })
        .expect("no toolkit call can fail here");
        ui.end().expect("no toolkit call can fail here");
        seen
    };
    assert_eq!(pass(&mut ui), None, "the task is still running");
    let waker = {
        let mut state = shared.borrow_mut();
        state.0 = Some(7);
        state.1.take().expect("the pending task left its waker")
    };
    waker.wake();
    ui.poll_woken_tasks();
    assert_eq!(pass(&mut ui), Some(7), "the woken task reruns its cache");
    assert_eq!(
        pass(&mut ui),
        None,
        "with no new wake the cache skips again"
    );
}

#[test]
fn a_stream_that_yields_under_an_unchanged_cache_reaches_its_body() {
    let mut ui = ui();
    let shared = Rc::new(RefCell::new((VecDeque::new(), None)));
    let pass = |ui: &mut Ui| -> Vec<u32> {
        let mut seen = Vec::new();
        ui.begin();
        ui.cached("outer", 0u8, |ui| {
            ui.cached("inner", 0u8, |ui| {
                let mut stream = ui.stream("stream", || Feed(Rc::clone(&shared)));
                while let Some(value) = stream.next() {
                    seen.push(value);
                }
                Ok(())
            })
        })
        .expect("no toolkit call can fail here");
        ui.end().expect("no toolkit call can fail here");
        seen
    };
    assert_eq!(pass(&mut ui), Vec::<u32>::new());
    for round in [vec![1, 2], vec![3]] {
        let waker = {
            let mut state = shared.borrow_mut();
            state.0.extend(round.iter().copied());
            state.1.take().expect("the pending stream left its waker")
        };
        waker.wake();
        ui.poll_woken_tasks();
        assert_eq!(pass(&mut ui), round, "both nested caches rerun");
    }
}

/// The key of the state node inside the cache in the model test.
fn inner_key() -> Key {
    Key::child(Key::child(Key::ROOT, "cache"), "inner")
}

#[test]
fn a_panicking_build_leaves_no_node_behind() {
    let mut ui = ui();
    ui.begin();
    let failed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        ui.cached("cache", 1u8, |ui| {
            ui.cached("inner", 2u8, |ui| {
                ui.state("count", || 0u32)?;
                panic!("a planned failure inside two open caches");
            })
        })
    }));
    assert!(failed.is_err());
    ui.close_all();
    ui.begin();
    ui.end().expect("no toolkit call can fail here");
    assert_eq!(ui.tree.live().len(), 1, "only the root is left");
}
