use super::*;
use std::sync::atomic::AtomicUsize;

const SENDERS: usize = 8;
const PER_SENDER: usize = 10_000;

#[test]
fn every_message_arrives_once_in_send_order_with_one_wake_at_a_time() {
    let outstanding = Arc::new(AtomicUsize::new(0));
    let mailbox = Arc::new(Mailbox::new({
        let outstanding = Arc::clone(&outstanding);
        move || {
            let before = outstanding.fetch_add(1, Ordering::AcqRel);
            assert_eq!(before, 0, "a second wake was posted before the first ran");
            true
        }
    }));
    let senders: Vec<_> = (0..SENDERS)
        .map(|sender| {
            let handle = Handle::new(Arc::clone(&mailbox));
            std::thread::spawn(move || {
                for sequence in 0..PER_SENDER {
                    assert!(handle.send((sender, sequence)));
                }
            })
        })
        .collect();

    // The UI thread: run a drain for each wake, as the dispatcher would.
    let mut received = vec![Vec::new(); SENDERS];
    let mut buffer = Vec::new();
    let mut total = 0;
    while total < SENDERS * PER_SENDER {
        if outstanding.load(Ordering::Acquire) == 0 {
            std::thread::yield_now();
            continue;
        }
        outstanding.store(0, Ordering::Release);
        mailbox.drain(&mut buffer);
        for (sender, sequence) in buffer.drain(..) {
            received[sender].push(sequence);
            total += 1;
        }
    }
    for sender in senders {
        sender.join().expect("senders do not panic");
    }
    for sequences in &received {
        assert!(sequences.iter().copied().eq(0..PER_SENDER));
    }
}

#[test]
fn a_send_after_close_fails() {
    let mailbox = Arc::new(Mailbox::new(|| true));
    let handle = Handle::new(Arc::clone(&mailbox));
    assert!(handle.send(1));
    mailbox.close();
    assert!(!handle.send(2));
    let mut buffer = Vec::new();
    mailbox.drain(&mut buffer);
    assert!(buffer.is_empty(), "close drops queued messages");
}

#[test]
fn a_refused_wake_closes_the_mailbox() {
    let mailbox = Arc::new(Mailbox::new(|| false));
    let handle = Handle::new(Arc::clone(&mailbox));
    assert!(!handle.send(1));
    assert!(!handle.send(2));
}
