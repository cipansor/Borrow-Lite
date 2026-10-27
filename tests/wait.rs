//! The `std`-gated retry helper.
//!
//! `wait` and `retry` exist because the benchmarks found that yielding is the
//! winning strategy under contention and that spinning - the loop most people
//! write - is the worst one. These tests check the contract: `retry` is
//! bounded, returns the last conflict, and actually succeeds once the cell is
//! free.

#![cfg(feature = "std")]

use borrow_lite::{Conflict, Local, Shared, local, retry, shared, wait};

#[test]
fn wait_is_callable_and_cheap_when_uncontended() {
    wait();
}

#[test]
fn retry_succeeds_once_the_cell_is_free() {
    let cell = local(0i32);
    let held = cell.borrow();
    assert!(retry(2, || cell.try_with_mut(|c| *c += 1)).is_err());
    drop(held);
    retry(2, || cell.try_with_mut(|c| *c += 1)).unwrap();
    assert_eq!(cell.with(|c| *c), 1);
}

/// Bounded on purpose: an unbounded wait can deadlock, which is the failure the
/// crate exists to avoid. A held guard must make `retry` give up, not hang.
#[test]
fn retry_gives_up_instead_of_hanging() {
    let cell = local(0i32);
    let _held = cell.borrow();
    let result = retry(1, || cell.try_with_mut(|c| *c += 1));
    assert!(matches!(result, Err(Conflict { .. })));
    assert_eq!(cell.with(|c| *c), 0);
}

/// `retry(0)` still makes one attempt, so it cannot return a `None` conflict.
#[test]
fn retry_with_zero_attempts_still_tries_once() {
    let cell = local(5i32);
    assert_eq!(retry(0, || cell.try_with_mut(|c| *c)).unwrap(), 5);
}

/// The helper is for the contended case, so prove it works there: many threads
/// incrementing one cell, each retrying with `retry` + `wait`. If the helper
/// lost updates the assertion below fails, which is the same self-check the
/// contention benchmark uses.
#[test]
fn retry_is_correct_under_contention() {
    const THREADS: usize = 4;
    const WRITES: usize = 2_000;

    let counter: Shared<usize> = shared(0);
    std::thread::scope(|scope| {
        for _ in 0..THREADS {
            scope.spawn(|| {
                for _ in 0..WRITES {
                    retry(usize::MAX, || counter.try_with_mut(|c| *c += 1)).unwrap();
                }
            });
        }
    });
    assert_eq!(counter.with(|c| *c), THREADS * WRITES);
}

/// A `Local` used from one thread with a `retry` loop: the helper must not
/// change single-threaded semantics.
#[test]
fn retry_is_a_noop_when_the_cell_is_never_busy() {
    let cell = Local::new(String::new());
    for _ in 0..100 {
        retry(1, || cell.try_with_mut(|s| s.push('x'))).unwrap();
    }
    assert_eq!(cell.with(String::len), 100);
}
