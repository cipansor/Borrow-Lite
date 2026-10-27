//! Fuzz `Snapshot<T>`: whole-value replacement and the retirement contract.
//!
//! `Snapshot` is the one cell whose promise is a *memory* promise rather than a
//! borrow one: `load` hands out a `&T` that must stay valid even after a later
//! `store` replaces the current value, and every value ever published must be
//! dropped exactly once - by `into_inner` for the current one and by `Drop` for
//! the retired ones. Neither is something the borrow-bookkeeping fuzzer
//! exercises, and both are exactly where a hand-written reclamation scheme goes
//! wrong.
//!
//! Each byte is an operation on a `Snapshot<P>`, where `P` counts its own drops
//! in a process-global counter. The oracle is the value plus the number of
//! payloads created; at the end every payload must have been freed exactly once:
//!
//! * `load`/`with` see the value the last `store` published;
//! * a reference taken *before* a `store` still reads the old value afterwards
//!   (the value was retired, not mutated in place);
//! * `into_inner` returns the current value;
//! * `created == drops` at the end - no leak and no double free.
//!
//! The script runs twice: once with a payload that carries its value, and once
//! with a **zero-sized** payload. The second run is the one that guards the bug
//! this type actually had: for a ZST every `Box::into_raw` yields the same
//! dangling pointer, so removing the current value by *pointer equality* in
//! `into_inner` removes every retired entry and the retired destructors never
//! run. `created == drops` catches that (verified by reverting `into_inner`'s
//! `pop` to the old `retain`), which a sized payload cannot.
//!
//! Run with:
//!
//! ```text
//! cargo +nightly fuzz run snapshot fuzz/corpus/snapshot fuzz/seeds/snapshot
//! ```
//!
//! `fuzz/seeds/snapshot` is tracked and read as the starting corpus.

#![no_main]

use core::sync::atomic::{AtomicUsize, Ordering};

use borrow_lite::Snapshot;
use libfuzzer_sys::fuzz_target;

/// Counts every payload drop, so the target can assert `created == drops`.
static DROPS: AtomicUsize = AtomicUsize::new(0);

/// A payload whose drop is observable, plus whether the script can check values
/// through it (a zero-sized payload stores nothing).
trait Probe: Sized {
    /// Whether `read` returns the value the payload was built with.
    const CHECK_VALUE: bool;
    fn make(value: u64) -> Self;
    fn read(&self) -> u64;
}

struct WithValue(u64);

impl Probe for WithValue {
    const CHECK_VALUE: bool = true;

    fn make(value: u64) -> Self {
        WithValue(value)
    }

    fn read(&self) -> u64 {
        self.0
    }
}

impl Drop for WithValue {
    fn drop(&mut self) {
        DROPS.fetch_add(1, Ordering::SeqCst);
    }
}

struct ZeroSized;

impl Probe for ZeroSized {
    const CHECK_VALUE: bool = false;

    fn make(_value: u64) -> Self {
        ZeroSized
    }

    fn read(&self) -> u64 {
        0
    }
}

impl Drop for ZeroSized {
    fn drop(&mut self) {
        DROPS.fetch_add(1, Ordering::SeqCst);
    }
}

/// Four operations, so a short seed file still reaches each path.
fn decode(byte: u8) -> u8 {
    byte % 4
}

fn run_script<P: Probe>(data: &[u8], label: &str) {
    // The counter is process-global and the target runs many times per process.
    DROPS.store(0, Ordering::SeqCst);

    let cell = Snapshot::new(P::make(0));
    let mut created: u64 = 1;
    let mut value: u64 = 0;

    for &byte in data {
        match decode(byte) {
            // A plain read: one load, no bookkeeping.
            0 => {
                if P::CHECK_VALUE {
                    assert_eq!(cell.load().read(), value, "{label}: load saw the wrong value");
                } else {
                    let _ = cell.load();
                }
            }
            // The closure form reads the same thing under a different name.
            1 => {
                let seen = cell.with(|p| p.read());
                if P::CHECK_VALUE {
                    assert_eq!(seen, value, "{label}: with saw the wrong value");
                }
            }
            // Replace the whole value.
            2 => {
                value = value.wrapping_add(1);
                cell.store(P::make(value));
                created += 1;
                if P::CHECK_VALUE {
                    assert_eq!(
                        cell.load().read(),
                        value,
                        "{label}: store did not publish the value"
                    );
                }
            }
            // Take a reference, replace the value, then read the *old* one
            // through the reference. This is the whole reason the type exists:
            // the previous value is retired, not mutated, so the reference is
            // still valid and still shows what it showed when it was taken.
            _ => {
                let seen = cell.load();
                let old = seen.read();
                value = value.wrapping_add(1);
                cell.store(P::make(value));
                created += 1;
                if P::CHECK_VALUE {
                    assert_eq!(
                        seen.read(),
                        old,
                        "{label}: a retired value changed under a live reference"
                    );
                    assert_eq!(
                        cell.load().read(),
                        value,
                        "{label}: store did not publish the value"
                    );
                }
            }
        }
    }

    // Consume the cell: the current value comes back by value, `Drop` reclaims
    // the retired ones. Together they must free exactly what was created.
    let last = cell.into_inner();
    if P::CHECK_VALUE {
        assert_eq!(
            last.read(),
            value,
            "{label}: into_inner returned the wrong value"
        );
    }
    drop(last);

    let dropped = DROPS.load(Ordering::SeqCst) as u64;
    assert_eq!(
        dropped, created,
        "{label}: created {created} payloads but dropped {dropped}"
    );
}

fuzz_target!(|data: &[u8]| {
    run_script::<WithValue>(data, "WithValue");
    run_script::<ZeroSized>(data, "ZeroSized");
});
