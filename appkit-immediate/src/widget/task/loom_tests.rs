//! Checks the wake protocol under every interleaving loom finds. Run with
//! `RUSTFLAGS="--cfg loom" cargo test -p appkit-immediate --lib loom`.

use super::{TaskWake, TaskWakes};
use crate::tree::NodeId;
use loom::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

/// The spec: a wake after the future's state changed leads to a poll that
/// sees the change. Here the task is already listed by an earlier wake, so
/// the second wake may take the early return. Either the turn's poll sees
/// the state, or the wake listed the task again for the next turn.
///
/// The state is read with Relaxed, so any order the poll gets comes from
/// the wake protocol alone.
#[test]
fn a_wake_during_a_turn_is_never_lost() {
    loom::model(|| {
        let shared = TaskWakes::new(|| true);
        let wake = Arc::new(TaskWake {
            node: NodeId::for_test(0),
            queued: AtomicBool::new(false),
            shared: Arc::clone(&shared),
        });
        let done = Arc::new(AtomicBool::new(false));
        wake.signal();
        let completer = loom::thread::spawn({
            let (wake, done) = (Arc::clone(&wake), Arc::clone(&done));
            move || {
                done.store(true, Ordering::Relaxed);
                wake.signal();
            }
        });
        // One turn: take the list, then poll the listed task.
        let listed = shared.take_woken();
        assert_eq!(listed.len(), 1, "the first wake listed the task");
        wake.begin_poll();
        let seen = done.load(Ordering::Relaxed);
        completer.join().unwrap();
        let listed_again = !shared.take_woken().is_empty();
        assert!(
            seen || listed_again,
            "the poll missed the state and nothing listed the task again"
        );
    });
}

/// The spec: a burst of wakes from two threads posts at most one turn
/// before the turn takes the list.
#[test]
fn two_wakes_before_a_turn_post_one_turn() {
    loom::model(|| {
        let posts = Arc::new(loom::sync::atomic::AtomicUsize::new(0));
        let shared = TaskWakes::new({
            let posts = Arc::clone(&posts);
            move || {
                posts.fetch_add(1, Ordering::Relaxed);
                true
            }
        });
        let wake = |index| {
            Arc::new(TaskWake {
                node: NodeId::for_test(index),
                queued: AtomicBool::new(false),
                shared: Arc::clone(&shared),
            })
        };
        let (first, second) = (wake(0), wake(1));
        let other = loom::thread::spawn(move || second.signal());
        first.signal();
        other.join().unwrap();
        assert_eq!(posts.load(Ordering::Relaxed), 1);
        assert_eq!(shared.take_woken().len(), 2);
    });
}
