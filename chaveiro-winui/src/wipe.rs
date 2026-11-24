//! Wipes freed memory and dead stack, so secrets do not linger after lock
//! (threat model option C).

/// Overwrites [`STACK_WIPE`] bytes of dead stack below the caller with
/// zeros. Moves of a key struct leave copies in old stack frames that no
/// destructor sees; a memory scan after lock found two copies of the user
/// key there (WP-06), and none after this runs.
#[inline(never)]
pub fn wipe_stack() {
    let area = [0u8; STACK_WIPE];
    // `black_box` makes the zeros real writes, since the area's address
    // escapes.
    std::hint::black_box(&area);
}

/// Deeper than the core's unlock path goes below the UI thread's message
/// loop, and well inside the 1 MiB main-thread stack.
const STACK_WIPE: usize = 192 * 1024;
