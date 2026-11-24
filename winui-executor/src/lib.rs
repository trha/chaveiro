//! Runs `!Send` futures on the WinUI `DispatcherQueue` of the current thread.
//!
//! [`spawn`] starts a task and returns a [`SpawnHandle`]. A woken task is
//! polled in its own dispatcher callback, so a waker may run on any thread,
//! such as the thread pool thread that completes a WinRT operation.

// The crate is Windows only and empty on other targets.
#![cfg(windows)]

mod bindings;

use bindings::{DispatcherQueue, DispatcherQueueHandler, DispatcherQueueTimer, TimeSpan};
use std::{
    cell::{Cell, RefCell},
    future::Future,
    marker::PhantomData,
    pin::Pin,
    rc::Rc,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    task::{Context, Poll, Wake, Waker},
    time::Duration,
};
use windows_future::{
    AsyncActionCompletedHandler, AsyncActionWithProgressCompletedHandler,
    AsyncOperationCompletedHandler, AsyncOperationWithProgressCompletedHandler, AsyncStatus,
    IAsyncAction, IAsyncActionWithProgress, IAsyncInfo, IAsyncOperation,
    IAsyncOperationWithProgress,
};

/// Runs `future` on this thread's `DispatcherQueue`. The first poll happens
/// in a later dispatcher callback, never inside this call. The future may
/// hold `!Send` state.
///
/// A task still alive when the thread ends is dropped with the thread's
/// locals, after the dispatcher has stopped.
///
/// Panics if this thread has no `DispatcherQueue`.
pub fn spawn(future: impl 'static + Future<Output = ()>) -> SpawnHandle {
    let queue = DispatcherQueue::GetForCurrentThread()
        .ok()
        .filter(|queue| !windows_core::Interface::as_raw(queue).is_null())
        .expect("winui_executor::spawn needs a thread with a DispatcherQueue");
    let (index, generation, wake) = TASKS.with(|tasks| {
        let mut tasks = tasks.borrow_mut();
        let index = match tasks.free.pop() {
            Some(index) => index,
            None => {
                tasks.slots.push(Slot {
                    generation: 0,
                    state: State::Vacant,
                });
                u32::try_from(tasks.slots.len() - 1).expect("fewer than 2^32 live tasks")
            }
        };
        let slot = &mut tasks.slots[index as usize];
        let wake = Arc::new(TaskWake {
            queue,
            index,
            generation: slot.generation,
            queued: AtomicBool::new(false),
        });
        slot.state = State::Idle {
            future: Box::pin(future),
            wake: Arc::clone(&wake),
        };
        (index, slot.generation, wake)
    });
    wake.wake_by_ref();
    SpawnHandle {
        index,
        generation,
        thread_bound: PhantomData,
    }
}

/// A future that is ready once `duration` has passed, timed by this
/// thread's `DispatcherQueue`, so no thread waits. The time starts at the
/// first poll. Dropping it stops the timer.
///
/// Poll it only on a thread with a `DispatcherQueue`, such as inside a
/// task from [`spawn`]. A timer that fails to start makes it ready at once.
pub fn sleep(duration: Duration) -> Sleep {
    Sleep {
        duration,
        timer: None,
    }
}

/// Ready once `Instant::now()` has reached `deadline`, never before, even
/// when the timer fires early. A deadline in the past makes it ready at
/// once. Poll it as [`sleep`].
pub async fn sleep_until(deadline: std::time::Instant) {
    loop {
        let left = deadline.saturating_duration_since(std::time::Instant::now());
        if left.is_zero() {
            return;
        }
        sleep(left).await;
    }
}

/// Ready once the system clock has reached `deadline`, never before, for
/// state that follows the wall clock, such as a TOTP code. A clock that
/// jumps back makes it wait longer. Poll it as [`sleep`].
pub async fn sleep_until_clock(deadline: std::time::SystemTime) {
    loop {
        let Ok(left) = deadline.duration_since(std::time::SystemTime::now()) else {
            return;
        };
        if left.is_zero() {
            return;
        }
        sleep(left).await;
    }
}

/// A stream that yields `()` every `period`, timed by one repeating timer
/// of this thread's `DispatcherQueue`, so it allocates nothing per tick.
/// The first tick comes one `period` after the first poll. Ticks that pass
/// while nothing polls are counted and yielded together. Dropping it stops
/// the timer.
///
/// Poll it as [`sleep`]. A timer that fails to start ends the stream.
pub fn interval(period: Duration) -> Interval {
    Interval {
        period,
        timer: None,
        started: false,
    }
}

/// The stream of [`interval`].
#[must_use = "an interval does nothing unless polled"]
pub struct Interval {
    period: Duration,
    /// Made at the first poll.
    timer: Option<RepeatingTimer>,
    /// The first poll tried to start the timer.
    started: bool,
}

struct RepeatingTimer {
    timer: DispatcherQueueTimer,
    /// Removes the tick handler when dropped.
    _tick: windows_core::EventRevoker,
    /// Ticks not yielded yet.
    ticks: Rc<Cell<u32>>,
    /// The waker of the last poll, for the tick.
    waker: Rc<RefCell<Option<Waker>>>,
}

impl Interval {
    fn start(&self) -> windows_core::Result<RepeatingTimer> {
        let queue = DispatcherQueue::GetForCurrentThread()?;
        let timer = queue.CreateTimer()?;
        timer.SetInterval(TimeSpan {
            // A TimeSpan counts 100 ns units.
            duration: i64::try_from(self.period.as_nanos() / 100).unwrap_or(i64::MAX),
        })?;
        timer.SetIsRepeating(true)?;
        let ticks = Rc::new(Cell::new(0u32));
        let waker = Rc::new(RefCell::new(None::<Waker>));
        let tick = timer.Tick({
            let (ticks, waker) = (Rc::clone(&ticks), Rc::clone(&waker));
            move |_, _| {
                ticks.set(ticks.get().saturating_add(1));
                if let Some(waker) = waker.borrow_mut().take() {
                    waker.wake();
                }
            }
        })?;
        timer.Start()?;
        Ok(RepeatingTimer {
            timer,
            _tick: tick,
            ticks,
            waker,
        })
    }
}

impl futures_core::Stream for Interval {
    type Item = ();

    fn poll_next(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Option<()>> {
        if !self.started {
            self.started = true;
            match self.start() {
                Ok(timer) => self.timer = Some(timer),
                Err(error) => {
                    // Ticking never would hang the poller, so the stream ends.
                    eprintln!("winui_executor::interval: the timer did not start: {error}");
                }
            }
        }
        let Some(timer) = self.timer.as_ref() else {
            return Poll::Ready(None);
        };
        match timer.ticks.get() {
            0 => {
                // The waker is reused while it would wake the same task, so
                // a steady poller allocates nothing here.
                let mut slot = timer.waker.borrow_mut();
                match slot.as_ref() {
                    Some(waker) if waker.will_wake(context.waker()) => {}
                    _ => *slot = Some(context.waker().clone()),
                }
                Poll::Pending
            }
            ticks => {
                timer.ticks.set(ticks - 1);
                Poll::Ready(Some(()))
            }
        }
    }
}

impl Drop for RepeatingTimer {
    fn drop(&mut self) {
        // As for `Timer`: stopping fails only while the dispatcher shuts
        // down, and then the timer never fires anyway.
        let _ = self.timer.Stop();
    }
}

/// The future of [`sleep`].
#[must_use = "a sleep does nothing unless awaited"]
pub struct Sleep {
    duration: Duration,
    /// Made at the first poll.
    timer: Option<Timer>,
}

struct Timer {
    timer: DispatcherQueueTimer,
    /// Removes the tick handler when dropped.
    _tick: windows_core::EventRevoker,
    /// The tick fired.
    fired: Rc<Cell<bool>>,
    /// The waker of the last poll, for the tick.
    waker: Rc<RefCell<Option<Waker>>>,
}

impl Sleep {
    fn start(&self) -> windows_core::Result<Timer> {
        let queue = DispatcherQueue::GetForCurrentThread()?;
        let timer = queue.CreateTimer()?;
        timer.SetInterval(TimeSpan {
            // A TimeSpan counts 100 ns units.
            duration: i64::try_from(self.duration.as_nanos() / 100).unwrap_or(i64::MAX),
        })?;
        timer.SetIsRepeating(false)?;
        let fired = Rc::new(Cell::new(false));
        let waker = Rc::new(RefCell::new(None::<Waker>));
        let tick = timer.Tick({
            let (fired, waker) = (Rc::clone(&fired), Rc::clone(&waker));
            move |_, _| {
                fired.set(true);
                if let Some(waker) = waker.borrow_mut().take() {
                    waker.wake();
                }
            }
        })?;
        timer.Start()?;
        Ok(Timer {
            timer,
            _tick: tick,
            fired,
            waker,
        })
    }
}

impl Future for Sleep {
    type Output = ();

    fn poll(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<()> {
        if self.timer.is_none() {
            match self.start() {
                Ok(timer) => self.timer = Some(timer),
                Err(error) => {
                    // Waiting forever would hang the task, so it goes on now.
                    eprintln!("winui_executor::sleep: the timer did not start: {error}");
                    return Poll::Ready(());
                }
            }
        }
        let timer = self.timer.as_ref().expect("started above");
        if timer.fired.get() {
            return Poll::Ready(());
        }
        *timer.waker.borrow_mut() = Some(context.waker().clone());
        Poll::Pending
    }
}

impl Drop for Timer {
    fn drop(&mut self) {
        // Stopping fails only when the dispatcher is shutting down, and then
        // the timer never fires anyway.
        let _ = self.timer.Stop();
    }
}

/// Awaits the WinRT operation `operation` and returns its results.
///
/// Dropping the future before the operation ends calls `Cancel` on it, and
/// this thread keeps the operation until it reports an end, so
/// [`shutdown`] can wait for it. A bare `.await` on the operation lacks both:
/// a drop leaves it running with nothing to wait for it.
///
/// The future owns the operation's `Completed` handler, so the operation
/// must not have one yet, and nothing may set one later. A cancel that
/// Windows reports while the future runs shows up as the error of
/// `GetResults`.
///
/// Call it on the thread that will poll the future, because the handler
/// posts to that thread's message queue.
pub fn winrt<A: WinRtOperation>(operation: A) -> WinRt<A> {
    let shared = Arc::new(Completion {
        done: AtomicBool::new(false),
        waker: std::sync::Mutex::new(None),
        // SAFETY: takes no arguments and cannot fail.
        thread: unsafe { windows_sys::Win32::System::Threading::GetCurrentThreadId() },
    });
    let installed = operation.on_completed({
        let shared = Arc::clone(&shared);
        Box::new(move || shared.complete())
    });
    WinRt {
        operation,
        shared,
        installed,
        settled: false,
    }
}

/// A WinRT async type: `IAsyncAction`, `IAsyncOperation` or one of their
/// `WithProgress` forms. Sealed: this crate implements it for those four.
pub trait WinRtOperation: windows_core::Interface + sealed::Sealed {
    type Output;

    #[doc(hidden)]
    fn on_completed(&self, handler: Box<dyn Send + Fn()>) -> windows_core::Result<()>;

    #[doc(hidden)]
    fn results(&self) -> windows_core::Result<Self::Output>;
}

mod sealed {
    pub trait Sealed {}
}

macro_rules! winrt_operation {
    ([$($generic:ident),*] $type:ty, $handler:ty, $output:ty) => {
        impl<$($generic: windows_core::RuntimeType + 'static),*> sealed::Sealed for $type {}

        impl<$($generic: windows_core::RuntimeType + 'static),*> WinRtOperation for $type {
            type Output = $output;

            fn on_completed(&self, handler: Box<dyn Send + Fn()>) -> windows_core::Result<()> {
                self.SetCompleted(&<$handler>::new(move |_, _| {
                    handler();
                    Ok(())
                }))
            }

            fn results(&self) -> windows_core::Result<$output> {
                self.GetResults()
            }
        }
    };
}

winrt_operation!([] IAsyncAction, AsyncActionCompletedHandler, ());
winrt_operation!(
    [P]
    IAsyncActionWithProgress<P>,
    AsyncActionWithProgressCompletedHandler<P>,
    ()
);
winrt_operation!(
    [T]
    IAsyncOperation<T>,
    AsyncOperationCompletedHandler<T>,
    T
);
winrt_operation!(
    [T, P]
    IAsyncOperationWithProgress<T, P>,
    AsyncOperationWithProgressCompletedHandler<T, P>,
    T
);

/// The future of [`winrt`].
#[must_use = "a WinRT operation runs anyway; await it, or drop it to cancel"]
pub struct WinRt<A: WinRtOperation> {
    operation: A,
    shared: Arc<Completion>,
    /// Whether `Completed` was set. A failed set makes the first poll
    /// return that error.
    installed: windows_core::Result<()>,
    /// The poll returned, so the drop cancels nothing.
    settled: bool,
}

// No field is pinned, so the future moves freely.
impl<A: WinRtOperation> Unpin for WinRt<A> {}

/// What the `Completed` handler shares with the future.
struct Completion {
    done: AtomicBool,
    waker: std::sync::Mutex<Option<Waker>>,
    /// The thread whose queue gets a `WM_NULL` on completion, so a
    /// `GetMessage` there in [`shutdown`] returns.
    thread: u32,
}

impl Completion {
    /// Runs on any thread, once.
    fn complete(&self) {
        self.done.store(true, Ordering::Release);
        if let Some(waker) = self
            .waker
            .lock()
            .expect("no code panics while holding the waker lock")
            .take()
        {
            waker.wake();
        }
        // A failed post means the thread has ended or has no queue, and
        // then nothing waits in `shutdown`.
        // SAFETY: posts a message with no pointers.
        let _ = unsafe {
            windows_sys::Win32::UI::WindowsAndMessaging::PostThreadMessageW(
                self.thread,
                windows_sys::Win32::UI::WindowsAndMessaging::WM_NULL,
                0,
                0,
            )
        };
    }
}

impl<A: WinRtOperation> Future for WinRt<A> {
    type Output = windows_core::Result<A::Output>;

    fn poll(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Self::Output> {
        if let Err(error) = &self.installed {
            let error = error.clone();
            self.settled = true;
            return Poll::Ready(Err(error));
        }
        if !self.shared.done.load(Ordering::Acquire) {
            *self
                .shared
                .waker
                .lock()
                .expect("no code panics while holding the waker lock") =
                Some(context.waker().clone());
            // The handler may have run before the waker was stored.
            if !self.shared.done.load(Ordering::Acquire) {
                return Poll::Pending;
            }
        }
        self.settled = true;
        Poll::Ready(self.operation.results())
    }
}

impl<A: WinRtOperation> Drop for WinRt<A> {
    fn drop(&mut self) {
        if self.settled || self.shared.done.load(Ordering::Acquire) {
            return;
        }
        let info = self
            .operation
            .cast::<IAsyncInfo>()
            .expect("every WinRT async type implements IAsyncInfo");
        // Cancel fails only when the operation ended in the meantime, and
        // then the status check in `shutdown` drops it at once.
        let _ = info.Cancel();
        SETTLING.with(|settling| {
            let mut settling = settling.borrow_mut();
            settling.retain(still_running);
            settling.push(info);
        });
    }
}

/// Ends this thread's async work: drops every task still spawned, which
/// cancels the WinRT operations under them, then runs this thread's
/// messages until every operation that [`WinRt`] cancelled has ended, or
/// until `limit` has passed. Returns how many operations were still running
/// at the limit.
///
/// Call it once on the UI thread, after the message loop has ended and
/// nothing will spawn again. A task that a dropped future spawns is dropped
/// too. A `WM_QUIT` in the queue ends the wait early. Panics when called
/// from inside a task's poll.
pub fn shutdown(limit: Duration) -> usize {
    loop {
        let futures = TASKS.with(|tasks| {
            let mut tasks = tasks.borrow_mut();
            let mut futures = Vec::new();
            for index in 0..tasks.slots.len() {
                match std::mem::replace(&mut tasks.slots[index].state, State::Vacant) {
                    State::Idle { future, .. } => {
                        futures.push(future);
                        tasks.release(u32::try_from(index).expect("fewer than 2^32 tasks"));
                    }
                    State::Polling { .. } => {
                        panic!("winui_executor::shutdown ran inside a task's poll")
                    }
                    State::Vacant => {}
                }
            }
            futures
        });
        if futures.is_empty() {
            break;
        }
        // Dropped with no borrow held, as in `SpawnHandle::drop`.
        drop(futures);
    }
    let running = || {
        SETTLING.with(|settling| {
            let mut settling = settling.borrow_mut();
            settling.retain(still_running);
            settling.len()
        })
    };
    if running() == 0 {
        return 0;
    }
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        DispatchMessageW, GetMessageW, KillTimer, MSG, SetTimer, TranslateMessage,
        USER_TIMER_MAXIMUM, WM_TIMER,
    };
    // Each completion posts a WM_NULL, and the timer posts WM_TIMER at the
    // limit, so GetMessage returns whenever the list may have changed. An
    // operation may also end through a message, as a prompt owned by one of
    // this thread's windows does, so every other message is dispatched.
    let after = u32::try_from(limit.as_millis())
        .unwrap_or(u32::MAX)
        .min(USER_TIMER_MAXIMUM);
    // SAFETY: a thread timer with no window and no callback.
    let timer = unsafe { SetTimer(std::ptr::null_mut(), 0, after, None) };
    loop {
        let left = running();
        if left == 0 {
            break;
        }
        // SAFETY: `message` is a valid out pointer.
        let mut message = unsafe { std::mem::zeroed::<MSG>() };
        let got = unsafe { GetMessageW(&mut message, std::ptr::null_mut(), 0, 0) };
        // 0 is WM_QUIT and -1 an error; with no timer, waiting on is a hang.
        if got <= 0 || timer == 0 {
            break;
        }
        if message.message == WM_TIMER && message.hwnd.is_null() && message.wParam == timer {
            break;
        }
        // SAFETY: `message` came from GetMessage.
        unsafe {
            TranslateMessage(&message);
            DispatchMessageW(&message);
        }
    }
    if timer != 0 {
        // SAFETY: the timer made above, on this thread.
        unsafe { KillTimer(std::ptr::null_mut(), timer) };
    }
    running()
}

/// Whether a cancelled operation has yet to report its end. An operation
/// whose status cannot be read counts as ended, because nothing can wait on
/// it.
fn still_running(info: &IAsyncInfo) -> bool {
    matches!(info.Status(), Ok(AsyncStatus::Started))
}

/// Owns one spawned task. Dropping it aborts the task.
#[must_use = "dropping a SpawnHandle aborts the task; call detach to keep it running"]
pub struct SpawnHandle {
    index: u32,
    generation: u32,
    /// Keeps the handle on the task's thread, where the task table lives.
    thread_bound: PhantomData<*const ()>,
}

impl SpawnHandle {
    /// Drops the task's future now. Aborting a finished task does nothing.
    /// If the task aborts itself from inside its own poll, the future is
    /// dropped as soon as that poll returns.
    pub fn abort(self) {
        // Drop does the work.
    }

    /// Lets the task run to the end without a handle.
    pub fn detach(self) {
        // The handle holds only plain ids, so forgetting it leaks nothing.
        std::mem::forget(self);
    }
}

impl Drop for SpawnHandle {
    fn drop(&mut self) {
        let future = TASKS.with(|tasks| {
            let mut tasks = tasks.borrow_mut();
            let slot = &mut tasks.slots[self.index as usize];
            if slot.generation != self.generation {
                return None;
            }
            match std::mem::replace(&mut slot.state, State::Vacant) {
                State::Idle { future, .. } => {
                    tasks.release(self.index);
                    Some(future)
                }
                State::Polling { .. } => {
                    slot.state = State::Polling { aborted: true };
                    None
                }
                State::Vacant => unreachable!("a vacant slot has a new generation"),
            }
        });
        // Dropped with no borrow held, because the future's drop may spawn or
        // abort other tasks.
        drop(future);
    }
}

/// The task table of one thread. A slot's generation changes each time it is
/// freed, so a stale handle or waker never reaches the next task in it.
struct Tasks {
    slots: Vec<Slot>,
    /// Indexes of vacant slots.
    free: Vec<u32>,
}

impl Tasks {
    /// Marks the slot at `index` vacant and gives it a new generation.
    fn release(&mut self, index: u32) {
        let slot = &mut self.slots[index as usize];
        slot.state = State::Vacant;
        slot.generation = slot.generation.wrapping_add(1);
        self.free.push(index);
    }
}

struct Slot {
    generation: u32,
    state: State,
}

enum State {
    Vacant,
    Idle {
        future: Pin<Box<dyn Future<Output = ()>>>,
        wake: Arc<TaskWake>,
    },
    /// The future is out of the table while it runs, so the poll can spawn
    /// and abort tasks. A handle meets this state only when the task drops its
    /// own handle during its poll, which sets `aborted`.
    Polling {
        aborted: bool,
    },
}

/// The waker of one task.
struct TaskWake {
    queue: DispatcherQueue,
    index: u32,
    generation: u32,
    /// Whether a poll callback is queued and has not started. Holds the
    /// dispatcher queue to one callback per task for a burst of wakes.
    queued: AtomicBool,
}

impl Wake for TaskWake {
    fn wake(self: Arc<Self>) {
        self.wake_by_ref();
    }

    fn wake_by_ref(self: &Arc<Self>) {
        // Release publishes the caller's writes to the poll, which acquires
        // them in its swap of `queued`.
        if self.queued.swap(true, Ordering::Release) {
            return;
        }
        // Copied out, because the closure would capture all of `self`: precise
        // capture stops at the `Arc` deref, and the handler must be 'static.
        let (index, generation) = (self.index, self.generation);
        let handler = DispatcherQueueHandler::new(move || {
            poll(index, generation);
        });
        // A refused enqueue means the dispatcher is shutting down, so the
        // task will never run again and its future drops with the thread.
        let _ = self.queue.TryEnqueue(&handler);
    }
}

/// Polls the task at `index` once, if it is still the task of `generation`.
fn poll(index: u32, generation: u32) {
    let taken = TASKS.with(|tasks| {
        let mut tasks = tasks.borrow_mut();
        let slot = tasks.slots.get_mut(index as usize)?;
        if slot.generation != generation {
            return None;
        }
        match std::mem::replace(&mut slot.state, State::Polling { aborted: false }) {
            State::Idle { future, wake } => Some((future, wake)),
            other => {
                slot.state = other;
                None
            }
        }
    });
    let Some((mut future, wake)) = taken else {
        return;
    };
    // Cleared before the poll, so a wake during the poll queues another. A
    // swap, because a plain store would read no wake: the swap reads the
    // last one and acquires the writes before it.
    wake.queued.swap(false, Ordering::Acquire);
    let waker = Waker::from(Arc::clone(&wake));
    let ready = future
        .as_mut()
        .poll(&mut Context::from_waker(&waker))
        .is_ready();
    let finished = TASKS.with(|tasks| {
        let mut tasks = tasks.borrow_mut();
        let slot = &mut tasks.slots[index as usize];
        let State::Polling { aborted } = slot.state else {
            unreachable!("only the poll leaves the Polling state");
        };
        if ready || aborted {
            tasks.release(index);
            Some(future)
        } else {
            slot.state = State::Idle { future, wake };
            None
        }
    });
    // Dropped with no borrow held, as in `SpawnHandle::drop`.
    drop(finished);
}

thread_local! {
    static TASKS: RefCell<Tasks> = const {
        RefCell::new(Tasks { slots: Vec::new(), free: Vec::new() })
    };
    /// Operations that [`WinRt`] cancelled and that may not have ended yet.
    static SETTLING: RefCell<Vec<IAsyncInfo>> = const { RefCell::new(Vec::new()) };
}
