//! Counts this thread's calls into WinUI objects by method, so a test can
//! check that a pass with no changes makes none. With the `count-calls`
//! feature, `build.rs` puts a call to [`count`] at the top of every generated
//! method that takes `&self`.

use std::cell::RefCell;

thread_local! {
    /// [`None`] while not counting.
    static CALLS: RefCell<Option<Vec<(&'static str, u64)>>> = const { RefCell::new(None) };
}

pub(crate) fn count(method: &'static str) {
    CALLS.with_borrow_mut(|calls| {
        if let Some(calls) = calls {
            match calls.iter_mut().find(|(name, _)| *name == method) {
                Some((_, count)) => *count += 1,
                None => calls.push((method, 1)),
            }
        }
    });
}

/// Starts counting this thread's WinUI calls, from zero.
pub fn start() {
    CALLS.set(Some(Vec::new()));
}

/// Stops counting. Returns each method called since [`start`] as
/// `Type::Method`, with its number of calls, most called first. Empty when
/// counting was not started.
pub fn take() -> Vec<(&'static str, u64)> {
    let mut calls = CALLS.take().unwrap_or_default();
    calls.sort_by(|left, right| right.1.cmp(&left.1).then(left.0.cmp(right.0)));
    calls
}
