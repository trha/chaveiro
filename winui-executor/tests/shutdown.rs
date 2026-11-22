//! `shutdown` waits for operations that a dropped `winrt` future cancelled.
//!
//! The operations are thread pool actions from `IAsyncAction::spawn`, whose
//! cancel does nothing, so each runs for its whole sleep, as an operation
//! that ignores a cancel does.

#![cfg(windows)]

use std::time::{Duration, Instant};
use windows_future::IAsyncAction;

fn action(runs: Duration) -> IAsyncAction {
    IAsyncAction::spawn(move || {
        std::thread::sleep(runs);
        Ok(())
    })
}

#[test]
fn waits_until_the_operation_ends_and_wakes_on_its_end() {
    drop(winui_executor::winrt(action(Duration::from_millis(400))));
    let started = Instant::now();
    let left = winui_executor::shutdown(Duration::from_secs(5));
    let took = started.elapsed();
    assert_eq!(left, 0);
    assert!(took >= Duration::from_millis(300), "returned before the end: {took:?}");
    // A wake on the completion, not on the limit.
    assert!(took < Duration::from_secs(2), "woke late: {took:?}");
}

#[test]
fn stops_at_the_limit_and_counts_what_still_runs() {
    drop(winui_executor::winrt(action(Duration::from_secs(3))));
    drop(winui_executor::winrt(action(Duration::from_secs(3))));
    let started = Instant::now();
    let left = winui_executor::shutdown(Duration::from_millis(300));
    let took = started.elapsed();
    assert_eq!(left, 2);
    assert!(took >= Duration::from_millis(250), "returned before the limit: {took:?}");
    assert!(took < Duration::from_secs(2), "missed the limit: {took:?}");
}

#[test]
fn returns_at_once_when_nothing_was_cancelled() {
    let started = Instant::now();
    assert_eq!(winui_executor::shutdown(Duration::from_secs(5)), 0);
    assert!(started.elapsed() < Duration::from_millis(100));
}

#[test]
fn a_finished_operation_is_not_kept() {
    let operation = action(Duration::ZERO);
    let future = winui_executor::winrt(operation.clone());
    while operation.Status().expect("status reads") == windows_future::AsyncStatus::Started {
        std::thread::sleep(Duration::from_millis(5));
    }
    std::thread::sleep(Duration::from_millis(20));
    drop(future);
    let started = Instant::now();
    assert_eq!(winui_executor::shutdown(Duration::from_secs(5)), 0);
    assert!(started.elapsed() < Duration::from_millis(100));
}
