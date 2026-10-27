//! Fuzz the observer path: subscription bookkeeping, notification counts, and
//! the re-entrancy rules.
//!
//! Each pair of input bytes is an operation on an `Observable<u64>`. Every
//! observer is backed by its own counter, so the oracle checks exact
//! notification counts rather than just "did not crash":
//!
//! * a live observer fires exactly once per notification;
//! * `observer_count` matches the number of live observers, including after
//!   `unobserve` and `clear_observers`;
//! * re-entering `update` from inside an observer is refused with a `Conflict`,
//!   not a panic and not a second write;
//! * an observer that removes itself is gone afterwards and does not fire
//!   again;
//! * an observer added during a notification is picked up by that same
//!   notification (see `tests/observers.rs`
//!   `notification_handles_a_round_of_new_subscribers`), not deferred to the
//!   next one;
//! * `get` reads without notifying, and `set` notifies exactly once like `update`.
//!
//! # What is deliberately not here
//!
//! There is no panicking-observer case. `libfuzzer-sys` installs a panic hook
//! that calls `std::process::abort()` before unwinding (see its `initialize`),
//! so a panic in a cargo-fuzz target aborts the process and `catch_unwind` can
//! never see it. A target that injects panics cannot pass regardless of what the
//! crate does, so the recovery logic is not testable this way.
//!
//! Panic recovery is covered by `tests/safety.rs`, which runs under plain
//! `cargo test` and under Miri, and includes the bug this path once had (a
//! panicking observer used to drop the rest of the round). Loom would be the
//! wrong tool for it too: none of this is about the memory model.
//!
//! Run with:
//!
//! ```text
//! cargo +nightly fuzz run observers fuzz/corpus/observers fuzz/seeds/observers
//! ```
//!
//! `fuzz/seeds/observers` is tracked and read as the starting corpus, and is
//! where a once-failing input is kept as a regression file.

#![no_main]

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use borrow_lite::{Observable, ObserverId};
use libfuzzer_sys::fuzz_target;

struct Live {
    id: ObserverId,
    count: Arc<AtomicUsize>,
}

/// Register a plain counting observer and return its handle.
fn add_counter(cell: &Observable<u64>, live: &mut Vec<Live>) {
    let count = Arc::new(AtomicUsize::new(0));
    let counter = count.clone();
    let id = cell.observe(move |_| {
        counter.fetch_add(1, Ordering::SeqCst);
    });
    live.push(Live { id, count });
}

fuzz_target!(|data: &[u8]| {
    let cell: Observable<u64> = Observable::new(0);
    let mut live: Vec<Live> = Vec::new();

    for pair in data.chunks(2) {
        let op = pair[0] % 13;
        let arg = pair.get(1).copied().unwrap_or(0);

        match op {
            // Register a counting observer.
            0 | 1 => add_counter(&cell, &mut live),

            // Notify: every live observer fires exactly once.
            2 => {
                let before: Vec<usize> = live
                    .iter()
                    .map(|o| o.count.load(Ordering::SeqCst))
                    .collect();
                cell.notify();
                for (index, observer) in live.iter().enumerate() {
                    assert_eq!(
                        observer.count.load(Ordering::SeqCst),
                        before[index] + 1,
                        "observer {index} was not notified exactly once"
                    );
                }
                assert_eq!(
                    cell.observer_count(),
                    live.len(),
                    "the observer list changed during a plain notification"
                );
            }

            // Remove a specific observer.
            3 => {
                if !live.is_empty() {
                    let index = (arg as usize) % live.len();
                    let removed = live.remove(index);
                    assert!(
                        cell.unobserve(removed.id),
                        "unobserve said a live observer was not there"
                    );
                    assert!(
                        !cell.unobserve(removed.id),
                        "unobserve claimed to remove the same observer twice"
                    );
                    assert_eq!(
                        cell.observer_count(),
                        live.len(),
                        "observer_count is wrong after unobserve"
                    );
                }
            }

            // update writes the value, then notifies.
            4 => {
                let before = cell.with(|v| *v);
                let counts: Vec<usize> = live
                    .iter()
                    .map(|o| o.count.load(Ordering::SeqCst))
                    .collect();
                cell.update(|v| *v = v.wrapping_add(1));
                assert_eq!(cell.with(|v| *v), before.wrapping_add(1));
                for (index, observer) in live.iter().enumerate() {
                    assert_eq!(
                        observer.count.load(Ordering::SeqCst),
                        counts[index] + 1,
                        "observer {index} missed an update"
                    );
                }
            }

            // An observer that removes itself.
            5 => {
                let count = Arc::new(AtomicUsize::new(0));
                let counter = count.clone();
                let my_id: Arc<std::sync::Mutex<Option<ObserverId>>> =
                    Arc::new(std::sync::Mutex::new(None));
                let id_slot = my_id.clone();
                let handle = cell.clone();
                let id = cell.observe(move |_| {
                    counter.fetch_add(1, Ordering::SeqCst);
                    // An id that was never subscribed is unknown even mid-
                    // notification, when live observers may sit in the guard's
                    // hands rather than the list. `u64::MAX` cannot be a real
                    // id: ids are handed out from zero.
                    assert!(
                        !handle.unobserve(u64::MAX),
                        "an unknown id must be false inside a notification too"
                    );
                    if let Some(id) = *id_slot.lock().unwrap() {
                        let _ = handle.unobserve(id);
                    }
                });
                *my_id.lock().unwrap() = Some(id);
                // Keep our own handle on the counter: `live` takes the other
                // one, and the check below still needs this one.
                live.push(Live {
                    id,
                    count: count.clone(),
                });

                cell.notify();
                assert_eq!(
                    cell.observer_count(),
                    live.len() - 1,
                    "a self-removing observer was not removed"
                );
                live.retain(|o| o.id != id);

                // It must not fire again.
                let fired = count.load(Ordering::SeqCst);
                cell.notify();
                assert_eq!(
                    count.load(Ordering::SeqCst),
                    fired,
                    "a removed observer fired again"
                );
            }

            // `take` empties the value and notifies, so every observer fires
            // exactly once and the value becomes the default. The observer list
            // itself is untouched.
            6 => {
                let before = cell.with(|v| *v);
                let counts: Vec<usize> = live
                    .iter()
                    .map(|o| o.count.load(Ordering::SeqCst))
                    .collect();
                let taken = cell.take();
                assert_eq!(taken, before, "take returned the wrong value");
                assert_eq!(cell.with(|v| *v), 0, "take did not leave the default");
                for (index, observer) in live.iter().enumerate() {
                    assert_eq!(
                        observer.count.load(Ordering::SeqCst),
                        counts[index] + 1,
                        "observer {index} missed a take notification"
                    );
                }
                assert_eq!(
                    cell.observer_count(),
                    live.len(),
                    "take changed the observer list"
                );
            }

            // `replace` returns the old value and notifies, so it is `take`
            // without the default: observers fire once, the old value comes
            // back, and the new one is in place.
            7 => {
                let before = cell.with(|v| *v);
                let counts: Vec<usize> = live
                    .iter()
                    .map(|o| o.count.load(Ordering::SeqCst))
                    .collect();
                let new = before.wrapping_add(arg as u64);
                let old = cell.replace(new);
                assert_eq!(old, before, "replace returned the wrong old value");
                assert_eq!(cell.with(|v| *v), new, "replace did not store the value");
                for (index, observer) in live.iter().enumerate() {
                    assert_eq!(
                        observer.count.load(Ordering::SeqCst),
                        counts[index] + 1,
                        "observer {index} missed a replace notification"
                    );
                }
                assert_eq!(
                    cell.observer_count(),
                    live.len(),
                    "replace changed the observer list"
                );
            }

            // `clear_observers` from inside an observer must actually clear,
            // including the observers of the round in flight, and none of them
            // may fire again. This is the state-machine case that a plain
            // `observers.clear()` got wrong, because the round is held by the
            // notification guard rather than by the list.
            8 => {
                let handle = cell.clone();
                cell.observe(move |_| handle.clear_observers());
                // Register a few more so there is a round to clear.
                add_counter(&cell, &mut live);
                add_counter(&cell, &mut live);

                cell.notify();
                assert_eq!(
                    cell.observer_count(),
                    0,
                    "clear_observers from inside an observer left observers"
                );
                // Every counter must be frozen from here on.
                let frozen: Vec<usize> = live
                    .iter()
                    .map(|o| o.count.load(Ordering::SeqCst))
                    .collect();
                cell.notify();
                for (index, observer) in live.iter().enumerate() {
                    assert_eq!(
                        observer.count.load(Ordering::SeqCst),
                        frozen[index],
                        "a cleared observer {index} fired again"
                    );
                }
                live.clear();
            }

            // `try_notify` from inside an observer: the value is already
            // shared-borrowed for the round, so a nested notification is a
            // no-op success (the `notifying` flag is set), NOT a conflict and
            // NOT a second run of the observers.
            9 => {
                let handle = cell.clone();
                let before = cell.with(|v| *v);
                let counts: Vec<usize> = live
                    .iter()
                    .map(|o| o.count.load(Ordering::SeqCst))
                    .collect();
                let outcome = Observable::new(None);
                let outcome_in_observer = outcome.clone();
                let id = cell.observe(move |_| {
                    let result = handle.try_notify();
                    outcome_in_observer.update(|o| *o = Some(result.is_ok()));
                });
                cell.notify();
                assert_eq!(
                    outcome.with(|o| *o),
                    Some(true),
                    "a nested notification from an observer must be an ignored success"
                );
                // The nested call must not have run anyone a second time.
                for (index, observer) in live.iter().enumerate() {
                    assert_eq!(
                        observer.count.load(Ordering::SeqCst),
                        counts[index] + 1,
                        "observer {index} ran more than once for one notification"
                    );
                }
                assert_eq!(cell.with(|v| *v), before, "notify changed the value");
                let _ = cell.unobserve(id);
            }

            // An observer that re-enters `update`; that must be refused.
            10 => {
                let handle = cell.clone();
                let id = cell.observe(move |_| {
                    assert!(
                        handle.try_update(|v| *v += 1000).is_err(),
                        "a re-entrant update from an observer was allowed"
                    );
                });
                let before = cell.with(|v| *v);
                cell.notify();
                // The refused update must not have written: the value is
                // exactly what it was, not merely "small". Comparing against a
                // fixed bound was wrong once `replace` could add a large `arg`.
                assert_eq!(
                    cell.with(|v| *v),
                    before,
                    "a refused re-entrant update still wrote the value"
                );
                let _ = cell.unobserve(id);
            }

            // `get` is the closure-free read, and it must not notify: reading
            // is not a change, so no observer may fire.
            11 => {
                let before = cell.get();
                let counts: Vec<usize> = live
                    .iter()
                    .map(|o| o.count.load(Ordering::SeqCst))
                    .collect();
                assert_eq!(cell.get(), before, "get returned a different value");
                for (index, observer) in live.iter().enumerate() {
                    assert_eq!(
                        observer.count.load(Ordering::SeqCst),
                        counts[index],
                        "a read notified observer {index}"
                    );
                }
            }

            // `set` is the closure-free write, and it must notify exactly once,
            // like `update`. `get` then sees the new value.
            _ => {
                let counts: Vec<usize> = live
                    .iter()
                    .map(|o| o.count.load(Ordering::SeqCst))
                    .collect();
                let new = cell.get().wrapping_add(arg as u64);
                cell.set(new);
                assert_eq!(cell.get(), new, "set did not store the value");
                for (index, observer) in live.iter().enumerate() {
                    assert_eq!(
                        observer.count.load(Ordering::SeqCst),
                        counts[index] + 1,
                        "observer {index} missed a set notification"
                    );
                }
                assert_eq!(
                    cell.observer_count(),
                    live.len(),
                    "set changed the observer list"
                );
            }
        }
    }

    // The cell must be usable at the end.
    cell.clear_observers();
    assert_eq!(cell.observer_count(), 0, "clear_observers left observers");
    cell.notify();
    assert_eq!(cell.observer_count(), 0);
});
