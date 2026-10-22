//! Waiting for a busy cell, with the strategy the benchmarks found best.
//!
//! The crate deliberately has no "wait until free" loop: an unbounded wait can
//! deadlock, which is the failure this crate exists to avoid. What it does have
//! is the *single step* to take between attempts, and a bounded retry built on
//! it, because the obvious loop to write is the wrong one.
//!
//! `benches/retry.rs` compares four ways of waiting for the same cell and
//! `benches/contention.rs` compares the winner against `parking_lot::RwLock`.
//! Yielding wins at every thread count, and a spinning loop is the *worst*
//! option - yet `spin_loop()` is what people reach for, because it is the
//! cheapest-looking one. At eight threads on four cores, spinning measured
//! 14.4 ms against 0.93 ms for yielding, on the same cell. So the helper here
//! yields, and the reason is in the numbers rather than in taste.

// The module is already gated on `std` where it is declared in `lib.rs`; a second
// `#![cfg]` here is a duplicate attribute, which clippy rejects.
use crate::error::Conflict;

/// Give the CPU up once before retrying a borrow.
///
/// This is `std::thread::yield_now()`, named and documented so that the retry
/// loop people write is the one that measured fastest. It is a no-op on a
/// single-threaded workload, so it is safe to call unconditionally.
///
/// ```
/// use lite::{Shared, shared, wait};
///
/// let counter = shared(0usize);
/// std::thread::scope(|scope| {
///     for _ in 0..4 {
///         scope.spawn(|| {
///             while counter.try_with_mut(|c| *c += 1).is_err() {
///                 wait();
///             }
///         });
///     }
/// });
/// assert_eq!(counter.with(|c| *c), 4);
/// ```
#[inline]
pub fn wait() {
    std::thread::yield_now();
}

/// Retry `op` until it succeeds, or until `attempts` have been made.
///
/// Bounded on purpose: an unbounded retry can spin forever if the cell is held
/// by something that will never release it, which is the deadlock the crate
/// avoids. Returning the last [`Conflict`] lets the caller decide whether to
/// give up, log, or try again.
///
/// # Errors
///
/// Returns the [`Conflict`] from the final attempt if every one of the
/// `attempts` calls to `op` failed.
///
/// ```
/// use lite::{Conflict, Local, local, retry};
///
/// let counter = local(0i32);
/// let held = counter.borrow(); // something else is borrowing
/// let result = retry(3, || counter.try_with_mut(|c| *c += 1));
/// assert!(result.is_err()); // still held, so every attempt failed
/// drop(held);
/// retry(3, || counter.try_with_mut(|c| *c += 1)).unwrap();
/// assert_eq!(counter.with(|c| *c), 1);
/// ```
#[inline]
pub fn retry<T>(
    attempts: usize,
    mut op: impl FnMut() -> Result<T, Conflict>,
) -> Result<T, Conflict> {
    // `max(1)` so a zero count still makes one attempt, which keeps the
    // function total: there is no "no attempt was made" result to invent.
    let mut remaining = attempts.max(1);
    loop {
        match op() {
            Ok(value) => return Ok(value),
            Err(conflict) => {
                remaining -= 1;
                if remaining == 0 {
                    return Err(conflict);
                }
                wait();
            }
        }
    }
}
