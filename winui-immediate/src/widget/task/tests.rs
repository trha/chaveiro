use crate::pass::Ui;
use proptest::prelude::*;
use std::{
    future::Future,
    pin::Pin,
    rc::Rc,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    task::{Context, Poll, Waker},
};

/// A future the test completes by hand, from any thread, as a WinRT
/// completion does.
#[derive(Clone, Default)]
struct Remote {
    inner: Arc<Mutex<RemoteState>>,
    /// Futures made and not yet dropped.
    live: Arc<AtomicUsize>,
}

#[derive(Default)]
struct RemoteState {
    value: Option<u32>,
    waker: Option<Waker>,
}

impl Remote {
    fn future(&self) -> RemoteFuture {
        self.live.fetch_add(1, Ordering::SeqCst);
        RemoteFuture(self.clone())
    }

    /// Completes with `value` and wakes, on another thread.
    fn complete(&self, value: u32) {
        let inner = Arc::clone(&self.inner);
        std::thread::spawn(move || {
            let waker = {
                let mut state = inner.lock().unwrap();
                state.value = Some(value);
                state.waker.take()
            };
            if let Some(waker) = waker {
                waker.wake();
            }
        })
        .join()
        .unwrap();
    }
}

struct RemoteFuture(Remote);

impl Future for RemoteFuture {
    type Output = u32;

    fn poll(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<u32> {
        let mut state = self.0.inner.lock().unwrap();
        match state.value {
            Some(value) => Poll::Ready(value),
            None => {
                state.waker = Some(context.waker().clone());
                Poll::Pending
            }
        }
    }
}

impl Drop for RemoteFuture {
    fn drop(&mut self) {
        self.0.live.fetch_sub(1, Ordering::SeqCst);
    }
}

/// A UI that counts the turns its tasks post.
fn ui_counting_posts() -> (Ui, Arc<AtomicUsize>) {
    let posts = Arc::new(AtomicUsize::new(0));
    let ui = Ui::with_task_post(Rc::new(|| {}), {
        let posts = Arc::clone(&posts);
        move || {
            posts.fetch_add(1, Ordering::SeqCst);
            true
        }
    });
    (ui, posts)
}

/// One pass that declares the task `id` when `declared`, and returns
/// what `take` gave.
fn pass(ui: &mut Ui, declared: bool, start: impl FnOnce() -> RemoteFuture) -> Option<u32> {
    ui.begin();
    let taken = match declared {
        true => ui.task("task", start).take(),
        false => None,
    };
    ui.end().expect("no window, so no WinUI call");
    taken
}

#[test]
fn start_runs_only_on_the_first_declaring_pass() {
    let (mut ui, _) = ui_counting_posts();
    let remote = Remote::default();
    let starts = std::cell::Cell::new(0);
    for _ in 0..3 {
        pass(&mut ui, true, || {
            starts.set(starts.get() + 1);
            remote.future()
        });
    }
    assert_eq!(starts.get(), 1);
}

#[test]
fn a_future_ready_at_once_gives_its_output_in_its_first_pass() {
    let (mut ui, _) = ui_counting_posts();
    ui.begin();
    let taken = ui.task("task", || async { 7u32 }).take();
    ui.end().unwrap();
    assert_eq!(taken, Some(7));
}

#[test]
fn a_running_task_says_so_and_a_finished_one_does_not() {
    let (mut ui, _) = ui_counting_posts();
    let remote = Remote::default();
    ui.begin();
    assert!(ui.task("task", || remote.future()).running());
    ui.end().unwrap();
    remote.complete(1);
    ui.poll_woken_tasks();
    ui.begin();
    assert!(!ui.task("task", || remote.future()).running());
    ui.end().unwrap();
}

#[test]
fn a_burst_of_wakes_posts_one_turn() {
    let (mut ui, posts) = ui_counting_posts();
    let first = Remote::default();
    let second = Remote::default();
    ui.begin();
    ui.task("first", || first.future());
    ui.task("second", || second.future());
    ui.end().unwrap();
    first.complete(1);
    second.complete(2);
    assert_eq!(posts.load(Ordering::SeqCst), 1);
    ui.poll_woken_tasks();
    ui.begin();
    assert_eq!(ui.task("first", || first.future()).take(), Some(1));
    assert_eq!(ui.task("second", || second.future()).take(), Some(2));
    ui.end().unwrap();
}

#[test]
fn a_wake_after_the_turn_took_the_list_posts_a_new_turn() {
    let (mut ui, posts) = ui_counting_posts();
    let first = Remote::default();
    let second = Remote::default();
    ui.begin();
    ui.task("first", || first.future());
    ui.task("second", || second.future());
    ui.end().unwrap();
    first.complete(1);
    ui.poll_woken_tasks();
    second.complete(2);
    assert_eq!(posts.load(Ordering::SeqCst), 2);
}

#[test]
fn a_wake_of_a_dropped_task_is_skipped() {
    let (mut ui, _) = ui_counting_posts();
    let remote = Remote::default();
    pass(&mut ui, true, || remote.future());
    // Keeps the waker past the drop, as a late WinRT completion does.
    let waker = remote.inner.lock().unwrap().waker.clone().unwrap();
    pass(&mut ui, false, || remote.future());
    waker.wake();
    ui.poll_woken_tasks();
}

#[test]
fn tasks_at_the_root_are_not_counted_as_windows() {
    let (mut ui, _) = ui_counting_posts();
    let remote = Remote::default();
    ui.begin();
    ui.task("task", || remote.future());
    assert_eq!(ui.end().unwrap(), 0);
}

/// One step of the model: declare the task or not, complete it from
/// another thread or not, then run a turn's poll or not.
#[derive(Clone, Copy, Debug)]
struct Step {
    declared: bool,
    complete: bool,
    turn: bool,
}

proptest! {
    /// The spec: a task starts when first declared, runs until a pass
    /// leaves it out, and its output is taken exactly once, in a pass after
    /// a turn polled it following the completion. A dropped task never
    /// gives an output, and a declared task after a drop starts again.
    #[test]
    fn the_output_arrives_once_after_completion_and_a_drop_restarts(
        steps in prop::collection::vec(
            (any::<bool>(), any::<bool>(), any::<bool>())
                .prop_map(|(declared, complete, turn)| Step { declared, complete, turn }),
            1..30,
        )
    ) {
        let (mut ui, _) = ui_counting_posts();
        // A new remote per start, so a completion reaches only the live run.
        let mut remote: Option<Remote> = None;
        let mut next_value = 0u32;
        // The model.
        let mut alive = false;
        let mut completed: Option<u32> = None;
        let mut polled = false;
        let mut taken = false;
        for step in steps {
            let starting = step.declared && !alive;
            let started = std::cell::Cell::new(false);
            let fresh = Remote::default();
            let got = pass(&mut ui, step.declared, || {
                started.set(true);
                fresh.future()
            });
            prop_assert_eq!(started.get(), starting, "start runs exactly when the task is new");
            if starting {
                remote = Some(fresh);
                alive = true;
                completed = None;
                polled = false;
                taken = false;
            }
            if !step.declared {
                alive = false;
                if let Some(remote) = &remote {
                    prop_assert_eq!(remote.live.load(Ordering::SeqCst), 0, "a dropped task drops its future");
                }
            }
            match got {
                Some(value) => {
                    prop_assert!(alive && polled && !taken, "the output comes only once, after a polled completion");
                    prop_assert_eq!(Some(value), completed);
                    taken = true;
                }
                None => prop_assert!(!(step.declared && polled && !taken && !starting), "a polled output is taken in the next pass"),
            }
            if step.complete && alive && completed.is_none() {
                next_value += 1;
                remote.as_ref().unwrap().complete(next_value);
                completed = Some(next_value);
            }
            if step.turn {
                ui.poll_woken_tasks();
                if completed.is_some() {
                    polled = true;
                }
            }
        }
    }
}

/// A stream another thread feeds, as a pipe or a WinRT event would.
#[derive(Clone, Default)]
struct Feed {
    inner: Arc<Mutex<FeedState>>,
    /// Streams made and not yet dropped.
    live: Arc<AtomicUsize>,
}

#[derive(Default)]
struct FeedState {
    items: std::collections::VecDeque<u32>,
    ended: bool,
    waker: Option<Waker>,
}

impl Feed {
    fn stream(&self) -> FeedStream {
        self.live.fetch_add(1, Ordering::SeqCst);
        FeedStream(self.clone())
    }

    /// A start for a stream that already exists, so it must not run.
    fn stream_unused(self) -> impl FnOnce() -> FeedStream {
        move || panic!("the stream exists, so start does not run")
    }

    /// Sends `items`, then ends the stream when `end`, on another thread.
    fn send(&self, items: Vec<u32>, end: bool) {
        let inner = Arc::clone(&self.inner);
        std::thread::spawn(move || {
            let waker = {
                let mut state = inner.lock().unwrap();
                state.items.extend(items);
                state.ended |= end;
                state.waker.take()
            };
            if let Some(waker) = waker {
                waker.wake();
            }
        })
        .join()
        .unwrap();
    }
}

struct FeedStream(Feed);

impl futures_core::Stream for FeedStream {
    type Item = u32;

    fn poll_next(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Option<u32>> {
        let mut state = self.0.inner.lock().unwrap();
        match state.items.pop_front() {
            Some(item) => Poll::Ready(Some(item)),
            None if state.ended => Poll::Ready(None),
            None => {
                state.waker = Some(context.waker().clone());
                Poll::Pending
            }
        }
    }
}

impl Drop for FeedStream {
    fn drop(&mut self) {
        self.0.live.fetch_sub(1, Ordering::SeqCst);
    }
}

#[test]
fn items_ready_at_once_are_in_the_first_pass() {
    let (mut ui, _) = ui_counting_posts();
    let feed = Feed::default();
    feed.send(vec![1, 2], false);
    ui.begin();
    let mut stream = ui.stream("feed", || feed.stream());
    assert_eq!((stream.next(), stream.next(), stream.next()), (Some(1), Some(2), None));
    ui.end().unwrap();
}

#[test]
fn an_ended_stream_is_finished_once_its_items_are_taken() {
    let (mut ui, _) = ui_counting_posts();
    let feed = Feed::default();
    feed.send(vec![1], true);
    ui.begin();
    let mut stream = ui.stream("feed", || feed.stream());
    assert!(!stream.finished(), "an item waits");
    stream.next();
    assert!(stream.finished());
    ui.end().unwrap();
    assert_eq!(feed.live.load(Ordering::SeqCst), 0, "an ended stream is dropped");
}

/// One step of the stream model.
#[derive(Clone, Debug)]
enum FeedStep {
    /// Another thread sends this many items.
    Send(u8),
    /// A turn polls the woken nodes.
    Turn,
    /// A pass declares the stream and takes up to this many items.
    Take(u8),
    /// A pass leaves the stream out.
    Leave,
}

proptest! {
    /// The spec: a declared stream gives every item sent while it lives,
    /// each once, in send order, starting from the first item sent after
    /// it was made. Leaving it out drops it, and items not taken go with it.
    #[test]
    fn a_stream_gives_each_item_once_in_order(
        steps in prop::collection::vec(
            prop_oneof![
                (1u8..4).prop_map(FeedStep::Send),
                Just(FeedStep::Turn),
                (0u8..4).prop_map(FeedStep::Take),
                Just(FeedStep::Leave),
            ],
            1..40,
        )
    ) {
        let (mut ui, _) = ui_counting_posts();
        let mut feed: Option<Feed> = None;
        let mut next_item = 0u32;
        // Every item sent to the live stream, and those taken from it.
        let mut sent: Vec<u32> = Vec::new();
        let mut taken: Vec<u32> = Vec::new();
        for step in steps {
            match step {
                FeedStep::Send(count) => if let Some(feed) = &feed {
                    let items: Vec<u32> = (0..count).map(|_| { next_item += 1; next_item }).collect();
                    sent.extend(&items);
                    feed.send(items, false);
                },
                FeedStep::Turn => ui.poll_woken_tasks(),
                FeedStep::Take(count) => {
                    let fresh = Feed::default();
                    ui.begin();
                    let mut stream = ui.stream("feed", || fresh.stream());
                    for _ in 0..count {
                        match stream.next() {
                            Some(item) => taken.push(item),
                            None => break,
                        }
                    }
                    ui.end().unwrap();
                    if feed.is_none() {
                        feed = Some(fresh);
                    }
                }
                FeedStep::Leave => {
                    ui.begin();
                    ui.end().unwrap();
                    if let Some(old) = feed.take() {
                        prop_assert_eq!(old.live.load(Ordering::SeqCst), 0, "a stream left out is dropped");
                        // The next stream starts over.
                        sent.clear();
                        taken.clear();
                    }
                }
            }
            prop_assert!(sent.starts_with(&taken), "taken {:?} is not a prefix of sent {:?}", taken, sent);
        }
        // Nothing is lost: one more turn and a pass that takes everything
        // give the rest.
        if feed.is_some() {
            ui.poll_woken_tasks();
            ui.begin();
            let mut stream = ui.stream("feed", Feed::default().stream_unused());
            while let Some(item) = stream.next() {
                taken.push(item);
            }
            ui.end().unwrap();
            prop_assert_eq!(taken, sent);
        }
    }
}
