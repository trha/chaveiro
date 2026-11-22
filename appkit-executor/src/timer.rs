//! Sleeps and intervals timed by dispatch timer sources on the main queue.

use dispatch2::{
    _dispatch_source_type_timer, DispatchObject, DispatchQueue, DispatchRetained, DispatchSource,
    DispatchTime,
};
use std::{
    future::Future,
    pin::Pin,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU32, Ordering},
    },
    task::{Context, Poll, Waker},
    time::{Duration, Instant, SystemTime},
};

/// Ready `duration` after its first poll, timed by a dispatch timer on the
/// main queue, so no thread waits. Dropping it cancels the timer.
pub fn sleep(duration: Duration) -> Sleep {
    Sleep {
        duration,
        timer: None,
    }
}

/// Ready once `Instant::now()` has reached `deadline`, never before, even
/// when the timer fires early. A deadline in the past makes it ready at
/// once.
pub async fn sleep_until(deadline: Instant) {
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return;
        }
        sleep(left).await;
    }
}

/// Ready once the system clock has reached `deadline`, never before, for
/// state that follows the wall clock, such as a TOTP code. A clock that
/// jumps back makes it wait longer.
pub async fn sleep_until_clock(deadline: SystemTime) {
    loop {
        let Ok(left) = deadline.duration_since(SystemTime::now()) else {
            return;
        };
        if left.is_zero() {
            return;
        }
        sleep(left).await;
    }
}

/// A stream that yields `()` every `period`, timed by one repeating dispatch
/// timer, so it allocates nothing per tick. The first tick comes one
/// `period` after the first poll. Ticks that pass while nothing polls are
/// counted and yielded one per poll. Dropping it cancels the timer.
pub fn interval(period: Duration) -> Interval {
    Interval {
        period,
        timer: None,
    }
}

/// The future of [`sleep`].
#[must_use = "a sleep does nothing unless awaited"]
pub struct Sleep {
    duration: Duration,
    /// Made at the first poll.
    timer: Option<Timer>,
}

impl Future for Sleep {
    type Output = ();

    fn poll(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<()> {
        let duration = self.duration;
        let timer = self
            .timer
            .get_or_insert_with(|| Timer::start(duration, None));
        match timer.take_tick(context.waker()) {
            true => Poll::Ready(()),
            false => Poll::Pending,
        }
    }
}

/// The stream of [`interval`].
#[must_use = "an interval does nothing unless polled"]
pub struct Interval {
    period: Duration,
    /// Made at the first poll.
    timer: Option<Timer>,
}

impl futures_core::Stream for Interval {
    type Item = ();

    fn poll_next(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Option<()>> {
        let period = self.period;
        let timer = self
            .timer
            .get_or_insert_with(|| Timer::start(period, Some(period)));
        match timer.take_tick(context.waker()) {
            true => Poll::Ready(Some(())),
            false => Poll::Pending,
        }
    }
}

/// A dispatch timer source whose handler counts ticks and wakes the last
/// poller. Dropping it cancels the source.
struct Timer {
    source: DispatchRetained<DispatchSource>,
    shared: Arc<TimerShared>,
}

struct TimerShared {
    /// Ticks not taken yet.
    ticks: AtomicU32,
    waker: Mutex<Option<Waker>>,
}

impl Timer {
    /// Fires first after `first`, then every `repeat`, or once when `repeat`
    /// is [`None`].
    fn start(first: Duration, repeat: Option<Duration>) -> Timer {
        let nanos = |duration: Duration| u64::try_from(duration.as_nanos()).unwrap_or(u64::MAX);
        let shared = Arc::new(TimerShared {
            ticks: AtomicU32::new(0),
            waker: Mutex::new(None),
        });
        // SAFETY: the timer type takes handle and mask 0, and the main queue
        // lives as long as the process.
        let source = unsafe {
            DispatchSource::new(
                (&raw const _dispatch_source_type_timer).cast_mut(),
                0,
                0,
                Some(DispatchQueue::main()),
            )
        };
        let handler = block2::RcBlock::new({
            let shared = Arc::clone(&shared);
            move || {
                shared.ticks.fetch_add(1, Ordering::Relaxed);
                let waker = shared
                    .waker
                    .lock()
                    .expect("no code panics while holding the waker lock")
                    .take();
                if let Some(waker) = waker {
                    waker.wake();
                }
            }
        });
        // SAFETY: dispatch copies the block, which holds only `Send` state,
        // so it may run and drop on any thread.
        unsafe { source.set_event_handler_with_block(block2::RcBlock::as_ptr(&handler)) };
        let start = DispatchTime::NOW.time(i64::try_from(nanos(first)).unwrap_or(i64::MAX));
        // An interval of FOREVER makes a one-shot timer.
        let interval = repeat.map_or(DispatchTime::FOREVER.0, nanos);
        source.set_timer(start, interval, 0);
        source.activate();
        Timer { source, shared }
    }

    /// Takes one tick if any fired; otherwise keeps `waker` for the next.
    fn take_tick(&self, waker: &Waker) -> bool {
        let mut slot = self
            .shared
            .waker
            .lock()
            .expect("no code panics while holding the waker lock");
        // Read under the lock, so a tick that lands after this read finds
        // the waker stored below.
        let taken = self
            .shared
            .ticks
            .try_update(Ordering::Relaxed, Ordering::Relaxed, |ticks| {
                ticks.checked_sub(1)
            })
            .is_ok();
        if !taken {
            // Reused while it wakes the same task, so a steady poller
            // allocates nothing here.
            match slot.as_ref() {
                Some(stored) if stored.will_wake(waker) => {}
                _ => *slot = Some(waker.clone()),
            }
        }
        taken
    }
}

impl Drop for Timer {
    fn drop(&mut self) {
        self.source.cancel();
    }
}
