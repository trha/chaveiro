//! Blocking work on a global dispatch queue.

use crate::completion::completion;
use dispatch2::{DispatchQoS, DispatchQueue, GlobalQueueIdentifier};
use std::panic::{AssertUnwindSafe, catch_unwind};

/// Runs `job` on the user-initiated global queue and resolves to its result, or to
/// [`None`] when it panicked (the panic message is then on stderr).
///
/// Dropping the future does not stop the job: it runs to the end and its
/// result drops on the pool thread.
pub async fn off_thread<T: 'static + Send>(job: impl 'static + Send + FnOnce() -> T) -> Option<T> {
    let (completer, completion) = completion();
    DispatchQueue::global_queue(GlobalQueueIdentifier::QualityOfService(
        DispatchQoS::UserInitiated,
    ))
    .exec_async(move || {
        // A panic must not unwind into libdispatch, which aborts. On a
        // panic the completer drops unused, so the future gets `None`.
        if let Ok(value) = catch_unwind(AssertUnwindSafe(job)) {
            completer.complete(value);
        }
    });
    completion.await
}
