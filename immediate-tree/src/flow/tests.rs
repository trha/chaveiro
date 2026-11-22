use crate::{
    Flow, Owned, OwnedFamily, owned,
    testing::{TestError, Ui, ui},
    tree::Key,
};
use proptest::prelude::*;
use std::{
    cell::Cell,
    future::{Future, pending},
    pin::Pin,
    rc::Rc,
    task::{Context, Poll},
};

/// The key of the state node `id` that a frame of the flow `"flow"`
/// declares at the root.
fn inner(id: &str) -> Key {
    Key::child(Key::child(Key::ROOT, "flow"), id)
}

/// Returns pending once, then ready, so a frame's next step runs in a
/// later poll without waking.
fn yield_once() -> impl Future<Output = ()> {
    let mut yielded = false;
    std::future::poll_fn(move |_: &mut Context<'_>| match yielded {
        true => Poll::Ready(()),
        false => {
            yielded = true;
            Poll::Pending
        }
    })
}

/// Sets its flag when dropped.
struct DropFlag(Rc<Cell<bool>>);

impl Drop for DropFlag {
    fn drop(&mut self) {
        self.0.set(true);
    }
}

#[test]
#[should_panic(expected = "Flow::frame is polled only inside Ui::flow")]
fn a_frame_outside_a_flow_panics() {
    let flow: Flow<OwnedFamily<()>, Ui> = Flow {
        family: std::marker::PhantomData,
    };
    let mut frame = flow.frame(|_, _| Ok(Some(())));
    let waker = std::task::Waker::noop();
    let _ = Pin::new(&mut frame).poll(&mut Context::from_waker(waker));
}

#[test]
#[should_panic(expected = "never inside another frame's closure")]
fn a_nested_frame_panics() {
    let mut ui = ui();
    let mut unit = ();
    ui.begin();
    let _ = ui.flow("flow", owned(&mut unit), |flow| async move {
        flow.frame(move |_, _| {
            // A frame awaited inside a frame's closure, as a nested
            // `block_on` would.
            let mut inner = flow.frame(|_, _| Ok(Some(())));
            let waker = std::task::Waker::noop();
            let _ = Pin::new(&mut inner).poll(&mut Context::from_waker(waker));
            Ok(Some(()))
        })
        .await;
    });
}

#[test]
fn a_poll_that_draws_nothing_keeps_the_last_controls() {
    let mut ui = ui();
    let mut unit = ();
    for pass in 0..3 {
        ui.begin();
        ui.flow("flow", owned(&mut unit), |flow| async move {
            flow.frame(|ui, _| {
                ui.state("inner", || 0u32)?;
                Ok(Some(()))
            })
            .await;
            pending::<()>().await;
        })
        .expect("no WinUI call fails here");
        ui.end().unwrap();
        assert!(
            ui.tree.find(inner("inner")).is_some(),
            "pass {pass}: the frame's node stays while the future waits"
        );
    }
}

#[test]
fn a_completed_frame_draws_the_next_step_in_the_next_pass() {
    let mut ui = ui();
    let mut unit = ();
    let start = |flow: Flow<OwnedFamily<()>, Ui>| async move {
        flow.frame(|ui, _| {
            ui.state("first", || ())?;
            Ok(Some(()))
        })
        .await;
        flow.frame(|ui, _| {
            ui.state("second", || ())?;
            Ok(None::<()>)
        })
        .await;
    };
    ui.begin();
    ui.flow("flow", owned(&mut unit), start).unwrap();
    assert!(ui.take_pass_request(), "the next step asks for a pass");
    ui.end().unwrap();
    assert!(ui.tree.find(inner("first")).is_some());
    assert!(
        ui.tree.find(inner("second")).is_none(),
        "one frame per pass"
    );

    ui.begin();
    ui.flow("flow", owned(&mut unit), start).unwrap();
    ui.end().unwrap();
    assert!(ui.tree.find(inner("second")).is_some());
    assert!(
        ui.tree.find(inner("first")).is_none(),
        "the first step's node is gone once the second draws"
    );
}

#[test]
fn leaving_a_flow_out_drops_its_future_and_controls() {
    let mut ui = ui();
    let mut unit = ();
    let dropped = Rc::new(Cell::new(false));
    ui.begin();
    let guard = DropFlag(Rc::clone(&dropped));
    ui.flow("flow", owned(&mut unit), move |flow| async move {
        let _guard = guard;
        flow.frame(|ui, _| {
            ui.state("inner", || ())?;
            Ok(None::<()>)
        })
        .await;
    })
    .unwrap();
    ui.end().unwrap();
    assert!(!dropped.get());

    ui.begin();
    ui.end().unwrap();
    assert!(dropped.get(), "the future is dropped");
    assert!(
        ui.tree.find(inner("inner")).is_none(),
        "its controls are dropped"
    );
}

#[test]
fn a_draw_error_comes_out_of_flow() {
    let mut ui = ui();
    let mut unit = ();
    ui.begin();
    let result = ui.flow("flow", owned(&mut unit), |flow| async move {
        flow.frame(|_, _| Err::<Option<()>, _>(TestError("draw failed")))
            .await;
    });
    assert_eq!(result.err(), Some(TestError("draw failed")));
}

#[test]
fn the_output_is_taken_once() {
    let mut ui = ui();
    let mut unit = ();
    let start =
        |flow: Flow<OwnedFamily<()>, Ui>| async move { flow.frame(|_, _| Ok(Some(7u32))).await };
    ui.begin();
    let mut response = ui.flow("flow", owned(&mut unit), start).unwrap();
    assert!(!response.running());
    assert_eq!((response.take(), response.take()), (Some(7), None));
    ui.end().unwrap();
}

/// A view type with a lifetime, lent through `lend!`.
struct Picks<'s> {
    names: &'s [String],
    picked: Option<&'s str>,
    frames: u32,
}
crate::lend!(PicksFamily = Picks<'_>);

proptest! {
    /// The spec: each frame sees the value the current pass lent, and its
    /// writes reach the caller when `Ui::flow` returns, for a value with a
    /// lifetime and for an owned one, across passes that change the value
    /// and the borrow between polls.
    #[test]
    fn each_frame_sees_the_value_its_pass_lent(
        lists in prop::collection::vec(prop::collection::vec("[a-z]{1,4}", 0..4), 1..12),
    ) {
        let mut ui = ui();
        let mut count = 0u32;
        for (pass, names) in lists.iter().enumerate() {
            // A fresh borrow each pass, as a view's would be.
            let mut picks = Picks { names, picked: None, frames: 0 };
            ui.begin();
            ui.flow("flow", &mut picks, |flow| async move {
                loop {
                    flow.frame(|_, picks| {
                        picks.picked = picks.names.first().map(String::as_str);
                        picks.frames += 1;
                        Ok(None::<()>)
                    })
                    .await;
                }
            })
            .unwrap();
            ui.flow("counter", owned(&mut count), |flow| async move {
                loop {
                    flow.frame(|_, count: &mut Owned<u32>| {
                        *count.get_mut() += 1;
                        Ok(None::<()>)
                    })
                    .await;
                    yield_once().await;
                }
            })
            .unwrap();
            ui.end().unwrap();
            prop_assert_eq!(picks.picked, names.first().map(String::as_str));
            prop_assert_eq!(picks.frames, 1, "one frame per pass");
            prop_assert_eq!(count as usize, pass + 1);
        }
    }
}

#[test]
#[should_panic(expected = "a frame is polled only inside a flow of its own family and shell")]
fn a_frame_carried_into_a_flow_of_another_family_panics() {
    let mut ui = ui();
    let mut unit = ();
    let mut count = 0u32;
    let carried: Rc<Cell<Option<Flow<OwnedFamily<()>, Ui>>>> = Rc::new(Cell::new(None));
    ui.begin();
    // The first flow hands its `Flow` out; the second polls a frame of it,
    // which would read the `u32` lent here as a `()`.
    ui.flow("unit", owned(&mut unit), {
        let carried = Rc::clone(&carried);
        move |flow| async move {
            carried.set(Some(flow));
            pending::<()>().await;
        }
    })
    .unwrap();
    ui.flow("count", owned(&mut count), move |_| async move {
        let flow = carried.take().expect("the first flow ran");
        flow.frame(|_, _| Ok(Some(()))).await;
    })
    .unwrap();
}
