use super::*;
use proptest::prelude::*;
use std::{
    cell::Cell,
    rc::Rc,
    sync::atomic::{AtomicUsize, Ordering},
    task::Wake,
};

/// Counts its wakes.
struct CountWake(AtomicUsize);

impl Wake for CountWake {
    fn wake(self: Arc<Self>) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

fn poll_once<T>(future: &mut Completion<T>, wake: &Arc<CountWake>) -> Poll<Option<T>> {
    let waker = Waker::from(Arc::clone(wake));
    Pin::new(future).poll(&mut Context::from_waker(&waker))
}

#[derive(Clone, Debug)]
enum Op {
    Complete(u8),
    DropCompleter,
    Poll,
}

fn op() -> impl Strategy<Value = Op> {
    prop_oneof![
        any::<u8>().prop_map(Op::Complete),
        Just(Op::DropCompleter),
        Just(Op::Poll),
    ]
}

proptest! {
    /// The spec: the future yields the first completed value; it yields
    /// `None` only when no value came and the completer is gone; while it
    /// waits, the event that settles it wakes it.
    #[test]
    fn resolves_to_the_first_value_or_none_when_abandoned(ops in prop::collection::vec(op(), 0..20)) {
        let (completer, mut future) = completion::<u8>();
        let mut completer = Some(completer);
        let wake = Arc::new(CountWake(AtomicUsize::new(0)));
        let mut first: Option<u8> = None;
        let mut waiting = false;
        for op in ops {
            let wakes = wake.0.load(Ordering::SeqCst);
            match op {
                Op::Complete(value) => {
                    let Some(completer) = &completer else { continue };
                    completer.complete(value);
                    if first.is_none() {
                        first = Some(value);
                        if waiting {
                            prop_assert_eq!(wake.0.load(Ordering::SeqCst), wakes + 1, "the first value wakes");
                        }
                    }
                }
                Op::DropCompleter => {
                    if completer.take().is_some() && waiting && first.is_none() {
                        prop_assert_eq!(wake.0.load(Ordering::SeqCst), wakes + 1, "the drop wakes");
                    }
                }
                Op::Poll => match poll_once(&mut future, &wake) {
                    Poll::Ready(output) => {
                        match first {
                            Some(value) => prop_assert_eq!(output, Some(value)),
                            None => {
                                prop_assert!(completer.is_none(), "None while the completer lives");
                                prop_assert_eq!(output, None);
                            }
                        }
                        break;
                    }
                    Poll::Pending => {
                        prop_assert!(first.is_none(), "pending after a value came");
                        prop_assert!(completer.is_some(), "pending with the completer gone");
                        waiting = true;
                    }
                },
            }
        }
    }
}

#[test]
fn a_value_from_another_thread_arrives() {
    let (completer, mut future) = completion::<u8>();
    let thread = std::thread::spawn(move || completer.complete(7));
    let wake = Arc::new(CountWake(AtomicUsize::new(0)));
    let output = loop {
        if let Poll::Ready(output) = poll_once(&mut future, &wake) {
            break output;
        }
        std::thread::yield_now();
    };
    thread.join().expect("the completer does not panic");
    assert_eq!(output, Some(7));
}

fn cancel_counter() -> (Rc<Cell<usize>>, impl 'static + FnOnce()) {
    let count = Rc::new(Cell::new(0));
    let cancel = {
        let count = Rc::clone(&count);
        move || count.set(count.get() + 1)
    };
    (count, cancel)
}

#[test]
fn dropping_a_pending_completion_cancels_once() {
    let (count, cancel) = cancel_counter();
    let (completer, future) = completion::<u8>();
    drop(future.cancel_with(cancel));
    assert_eq!(count.get(), 1);
    // A value after the cancel goes nowhere and cancels nothing more.
    completer.complete(1);
    assert_eq!(count.get(), 1);
}

#[test]
fn a_resolved_completion_never_cancels() {
    let (count, cancel) = cancel_counter();
    let (completer, future) = completion::<u8>();
    let mut future = future.cancel_with(cancel);
    completer.complete(1);
    let wake = Arc::new(CountWake(AtomicUsize::new(0)));
    assert_eq!(poll_once(&mut future, &wake), Poll::Ready(Some(1)));
    drop(future);
    assert_eq!(count.get(), 0);
}

#[test]
fn a_value_that_arrived_unpolled_does_not_cancel() {
    let (count, cancel) = cancel_counter();
    let (completer, future) = completion::<u8>();
    let future = future.cancel_with(cancel);
    completer.complete(1);
    drop(future);
    assert_eq!(count.get(), 0);
}
