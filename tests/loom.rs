//! Model checking for the borrow protocol, with loom.
//!
//! Run with:
//!
//! ```text
//! LOOM_MAX_PREEMPTIONS=3 RUSTFLAGS="--cfg loom" cargo test --test loom --release
//! ```
//!
//! # What loom can and cannot check here
//!
//! The handshake in `AtomicState::try_shared`/`try_exclusive` is a Dekker-style
//! protocol, and its correctness rests on `SeqCst` being a *single total order
//! over all locations*. Loom does not model that. From loom's own README,
//! "Unsupported features":
//!
//! > `SeqCst` accesses ... are regarded as `AcqRel`. That is, they impose
//! > weaker synchronization, causing Loom to generate false alarms (not
//! > complete).
//!
//! That is easy to confirm: `loom_does_not_order_seqcst_across_locations` below
//! asserts that loom accepts the store-buffering outcome `r1 == 0 && r2 == 0`,
//! which a real `SeqCst` total order forbids. It passes, which is the point.
//!
//! So a loom run reporting "reader and writer both inside the cell" on this
//! protocol is a false alarm, not a bug. An earlier version of this file
//! reported exactly that, and a standalone copy of the same handshake - with no
//! borrow_lite code involved at all - reported it too, which is how the cause was
//! pinned down.
//!
//! What is therefore checked here:
//!
//! * the parts that do not need a global order, which is most of the state
//!   machine: a released borrow frees the cell, readers do not conflict with
//!   each other, a writer never loses an update against another writer, and the
//!   guard bookkeeping balances;
//! * that loom's limit is real, so nobody later reads a passing run as proof
//!   that `SeqCst` is unnecessary.
//!
//! The global-order property itself is covered by `tests/concurrency.rs` under
//! real threads, and by Miri. Neither is limited this way.

#![cfg(loom)]

use loom::sync::Arc;
use loom::sync::atomic::{AtomicUsize, Ordering};
use loom::thread;

use borrow_lite::Shared;

/// Loom does not order `SeqCst` accesses across locations, so this outcome is
/// reachable and the test is written to record it.
///
/// Store buffering: `r1 == 0 && r2 == 0` is forbidden under a real `SeqCst`
/// total order. Loom allows it, which is the documented `AcqRel`
/// approximation. This test documents a limitation rather than checking the
/// crate - and it will fail if a future loom becomes strong enough that the
/// module docs above are no longer true, which is worth knowing.
///
/// `loom::model` runs the closure once per explored interleaving, so the
/// observation is recorded in a plain `std` atomic outside the model and
/// asserted afterwards. Asserting inside the closure would demand one specific
/// outcome from every interleaving, which is not what this is testing.
static SAW_FORBIDDEN: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

#[test]
fn loom_does_not_order_seqcst_across_locations() {
    loom::model(|| {
        let x = Arc::new(AtomicUsize::new(0));
        let y = Arc::new(AtomicUsize::new(0));

        let x1 = x.clone();
        let y1 = y.clone();
        let a = thread::spawn(move || {
            x1.store(1, Ordering::SeqCst);
            y1.load(Ordering::SeqCst)
        });

        let x2 = x.clone();
        let y2 = y.clone();
        let b = thread::spawn(move || {
            y2.store(1, Ordering::SeqCst);
            x2.load(Ordering::SeqCst)
        });

        let r1 = a.join().unwrap();
        let r2 = b.join().unwrap();

        if r1 == 0 && r2 == 0 {
            SAW_FORBIDDEN.store(true, std::sync::atomic::Ordering::SeqCst);
        }
    });

    assert!(
        SAW_FORBIDDEN.load(std::sync::atomic::Ordering::SeqCst),
        "loom now enforces SeqCst across locations; the module docs about \
         loom's limits are out of date"
    );
}

/// Two writers, each holding the borrow across a yield. If exclusivity were
/// broken, both would read the same value and the total would end up 1 instead
/// of 2 - the classic lost update.
///
/// This does not need a global order: both writers `compare_exchange` the *same*
/// flag, and the join is what makes the outcome observable.
#[test]
fn two_writers_never_lose_an_update() {
    loom::model(|| {
        let cell: Shared<u64> = borrow_lite::shared(0);
        let first = cell.clone();
        let second = cell.clone();

        let a = thread::spawn(move || {
            let _ = first.try_with_mut(|v| {
                let seen = *v;
                thread::yield_now();
                *v = seen + 1;
            });
        });

        let b = thread::spawn(move || {
            let _ = second.try_with_mut(|v| {
                let seen = *v;
                thread::yield_now();
                *v = seen + 1;
            });
        });

        a.join().unwrap();
        b.join().unwrap();

        // Either both succeeded (2) or exactly one did (1); never a lost update.
        let final_value = cell.with(|v| *v);
        assert!(
            final_value == 1 || final_value == 2,
            "two writers overlapped: {final_value}"
        );
    });
}

/// Two readers must never conflict, because no writer exists. A conflict here
/// would be a false negative in the shared-borrow path.
#[test]
fn readers_never_conflict_with_each_other() {
    loom::model(|| {
        let cell: Shared<u64> = borrow_lite::shared(7);
        let first = cell.clone();
        let second = cell.clone();

        let refused = Arc::new(AtomicUsize::new(0));

        let a_refused = refused.clone();
        let a = thread::spawn(move || {
            if first.try_with(|v| *v).is_err() {
                a_refused.fetch_add(1, Ordering::SeqCst);
            }
        });

        let b_refused = refused.clone();
        let b = thread::spawn(move || {
            if second.try_with(|v| *v).is_err() {
                b_refused.fetch_add(1, Ordering::SeqCst);
            }
        });

        a.join().unwrap();
        b.join().unwrap();

        assert_eq!(refused.load(Ordering::SeqCst), 0, "two readers conflicted");
    });
}

/// A guard dropped on one thread must free the cell for another thread.
///
/// This is the release half of the handshake, and it does not depend on a global
/// order: the `join` below orders the release before the read. It is a handoff
/// rather than a retry race on purpose - with a bounded retry the reader can
/// legitimately starve for its whole window, which is exactly why the docs say
/// to retry, so racing here would test the scheduler, not the cell.
#[test]
fn a_released_borrow_becomes_available_to_the_other_thread() {
    loom::model(|| {
        let cell: Shared<u64> = borrow_lite::shared(0);
        let writer = cell.clone();
        let reader = cell.clone();

        let a = thread::spawn(move || {
            let guard = writer.borrow_mut();
            let value = *guard;
            drop(guard);
            value
        });

        let written = a.join().unwrap();
        assert_eq!(written, 0);

        let read = reader.try_with(|v| *v);
        assert_eq!(
            read.ok(),
            Some(0),
            "the cell was not readable after release"
        );

        // And the cell is usable again, which is the property a stuck cell
        // would break.
        assert!(cell.try_borrow_mut().is_ok(), "the cell stayed borrowed");
    });
}

/// The guard bookkeeping must balance: after a borrow is taken and released, the
/// internal counters are back to zero, so the cell is reusable. A leak here
/// would surface later as a conflict nothing explains.
#[test]
fn borrow_bookkeeping_is_balanced() {
    loom::model(|| {
        let cell: Shared<u64> = borrow_lite::shared(0);
        let other = cell.clone();

        let a = thread::spawn(move || {
            // Read, release, then take exclusive. If the reader's counter were
            // leaked, the exclusive borrow would be refused forever.
            let seen = other.with(|v| *v);
            let took_exclusive = other.try_borrow_mut().is_ok();
            (seen, took_exclusive)
        });

        let (seen, took_exclusive) = a.join().unwrap();
        assert_eq!(seen, 0);
        assert!(took_exclusive, "the reader's borrow was not released");

        // And the cell is free again.
        assert!(cell.try_borrow_mut().is_ok(), "the cell stayed borrowed");
    });
}

/// A reader and a writer must never both hold the cell.
///
/// # Why this is ignored
///
/// On the shipped two-location protocol this test **fails**, and the failure is
/// a false alarm, not a bug. The protocol's correctness rests on `SeqCst`'s
/// total order across the reader counter and the writer flag, and loom treats
/// `SeqCst` as `AcqRel` (see the module docs). Loom therefore explores
/// interleavings a real `SeqCst` machine forbids and reports an overlap that
/// cannot happen.
///
/// It is kept because it is the concrete evidence for that claim, and because
/// it *passes* against a single-word state where both sides contend for one
/// location. That design was implemented and measured during development: it
/// made this property model-checkable, but it was 7-22% slower on every
/// benchmark (single-threaded read +7.5%, write +22%, contention +7% to +21%),
/// so it was reverted. This test is the one thing the slower design bought.
///
/// Run it deliberately with:
///
/// ```text
/// LOOM_MAX_PREEMPTIONS=3 RUSTFLAGS="--cfg loom" cargo test --test loom --release -- --ignored
/// ```
///
/// and expect the overlap assertion below to fire.
#[test]
#[ignore = "loom false alarm on the two-location SeqCst protocol; passes on a single-word state"]
fn a_reader_and_a_writer_are_never_inside_together() {
    loom::model(|| {
        let cell: Shared<u64> = borrow_lite::shared(0);
        let reader = cell.clone();
        let writer = cell.clone();

        // Each side publishes that it is inside, then checks whether the
        // *other* side is also inside. The first version of this test checked a
        // flag that nothing ever set, so it passed even against a writer that
        // ignored readers entirely - a vacuous test, caught only by mutating
        // the cell. The counters below are what the mutation must trip.
        let overlap = Arc::new(AtomicUsize::new(0));
        let reader_inside = Arc::new(AtomicUsize::new(0));
        let writer_inside = Arc::new(AtomicUsize::new(0));

        let a_overlap = overlap.clone();
        let a_inside = reader_inside.clone();
        let a_writer_inside = writer_inside.clone();
        let a = thread::spawn(move || {
            if let Ok(guard) = reader.try_borrow() {
                let value = *guard;
                a_inside.fetch_add(1, Ordering::SeqCst);
                if a_writer_inside.load(Ordering::SeqCst) != 0 {
                    a_overlap.store(2, Ordering::SeqCst);
                }
                thread::yield_now();
                let _ = value;
                a_inside.fetch_sub(1, Ordering::SeqCst);
            }
        });

        let b_overlap = overlap.clone();
        let b_inside = writer_inside.clone();
        let b_reader_inside = reader_inside.clone();
        let b = thread::spawn(move || {
            if let Ok(mut guard) = writer.try_borrow_mut() {
                *guard += 1;
                b_inside.fetch_add(1, Ordering::SeqCst);
                if b_reader_inside.load(Ordering::SeqCst) != 0 {
                    b_overlap.store(2, Ordering::SeqCst);
                }
                thread::yield_now();
                b_inside.fetch_sub(1, Ordering::SeqCst);
            }
        });

        a.join().unwrap();
        b.join().unwrap();

        assert_eq!(
            overlap.load(Ordering::SeqCst),
            0,
            "a reader and a writer were both inside the cell"
        );
        assert_eq!(
            reader_inside.load(Ordering::SeqCst),
            0,
            "reader bookkeeping leaked"
        );
        assert_eq!(
            writer_inside.load(Ordering::SeqCst),
            0,
            "writer bookkeeping leaked"
        );
    });
}

/// A reader that is refused by a writer must not corrupt the count.
///
/// This is a regression test for a real trap in a single-word design. A reader
/// that claims its slot with `fetch_add` and *then* checks the writer bit must
/// undo the increment if it sees a writer - but the undo is a second atomic op,
/// and if a writer release could touch the same word in between, the undo would
/// run against the writer's value instead of the reader's own increment and the
/// count could wrap.
///
/// The shipped design avoids that by keeping the two pieces of bookkeeping in
/// separate words. `try_shared` claims its slot with `fetch_add` on `shared`,
/// re-checks the `writer` word, and on a writer undoes the increment with
/// `fetch_sub`. The writer never writes `shared` at all - it only claims and
/// releases `writer` - so the undo can only ever subtract the reader's own
/// increment and the count cannot wrap. This model is what guards the property:
/// a 200,000-iteration stress loop would not have caught it, because the window
/// is a few instructions wide and the interleaving has to be enumerated rather
/// than raced for.
#[test]
fn a_refused_reader_does_not_wrap_the_count() {
    loom::model(|| {
        let cell: Shared<u64> = borrow_lite::shared(0);
        let reader = cell.clone();
        let writer = cell.clone();

        let a = thread::spawn(move || {
            let _ = reader.try_borrow();
        });
        let b = thread::spawn(move || {
            let _ = writer.try_borrow_mut();
        });

        a.join().unwrap();
        b.join().unwrap();

        assert!(
            cell.try_borrow_mut().is_ok(),
            "the cell is stuck: the reader's undo wrapped the count"
        );
    });
}

/// A reader that is refused by a writer must always be told *where* the writer
/// is.
///
/// The writer used to claim the cell with a `compare_exchange` on a flag and
/// then publish its location in a second store, so a reader that ran in between
/// saw a claimed cell with no location. Folding the claim and the location into
/// one compare-exchange on a single pointer removes that window: a reader either
/// sees the null pointer (no writer) or the writer's location, never a claimed
/// cell without a location.
#[test]
fn a_writer_conflict_always_carries_a_location() {
    loom::model(|| {
        let cell: Shared<u32> = borrow_lite::shared(0);
        let writer = cell.clone();
        let reader = cell.clone();

        let a = thread::spawn(move || {
            if let Ok(mut guard) = writer.try_borrow_mut() {
                *guard += 1;
            }
        });

        let b = thread::spawn(move || {
            if let Err(conflict) = reader.try_borrow() {
                assert_eq!(conflict.kind(), borrow_lite::ConflictKind::Writer);
                assert!(
                    conflict.holder().is_some(),
                    "a writer conflict lost the writer's location"
                );
            }
        });

        a.join().unwrap();
        b.join().unwrap();
    });
}

/// A reader releasing its borrow must not erase a writer's location.
///
/// `end_shared` used to be two operations: `fetch_sub` the reader count, and -
/// when the count reached zero - `set_holder(None)`. Reaching zero is exactly
/// the moment a writer is allowed to win the handshake, so a writer could
/// acquire and store its location *between* those two operations, and the
/// reader's `set_holder(None)` would then wipe it. The next conflict would
/// report no holder, losing the diagnostic for a cell that was, at that moment,
/// definitely held by a writer.
///
/// This is a plain `Relaxed` race between two writes to the same atomic, so loom
/// models it directly - no `SeqCst` global order is involved, and the module's
/// limitation does not apply. The writer checks the holder *after* it holds the
/// cell, which is the window the wipe falls in.
#[test]
fn a_reader_release_never_erases_the_writers_location() {
    loom::model(|| {
        let cell: Shared<u32> = borrow_lite::shared(0);
        let reader = cell.clone();
        let writer = cell.clone();

        let a = thread::spawn(move || {
            // Take and release a shared borrow.
            let _ = reader.try_borrow();
        });

        let b = thread::spawn(move || {
            if let Ok(mut guard) = writer.try_borrow_mut() {
                // We hold the cell exclusively; a conflicting read must name us.
                let conflict = writer.try_borrow().unwrap_err();
                assert!(
                    conflict.holder().is_some(),
                    "a concurrent reader release erased the writer's location"
                );
                *guard += 1;
            }
        });

        a.join().unwrap();
        b.join().unwrap();
    });
}

/// A cloned guard must keep the borrow alive after the original is dropped, and
/// the cell must be free once the last clone goes. This is the balance property
/// `add_shared` has to satisfy, and unlike "a reader and a writer are never
/// inside together" it does not depend on `SeqCst` ordering across locations,
/// so loom can actually check it.
#[test]
fn a_cloned_guard_holds_the_borrow_after_the_original_is_dropped() {
    loom::model(|| {
        let cell: Shared<u64> = borrow_lite::shared(1);
        let reader_handle = cell.clone();
        let check_handle = cell.clone();

        let reader = thread::spawn(move || {
            let guard = reader_handle.borrow();
            let clone = guard.clone();
            drop(guard);
            // The clone is the only guard left, so the cell is still borrowed:
            // if `add_shared` had not incremented, dropping the original would
            // have freed the cell here.
            let still_borrowed = reader_handle.try_borrow_mut().is_err();
            let seen = *clone;
            drop(clone);
            (still_borrowed, seen)
        });

        let (still_borrowed, seen) = reader.join().unwrap();
        assert!(
            still_borrowed,
            "the clone did not keep the borrow alive after the original was dropped"
        );
        assert_eq!(seen, 1);
        // Every guard is gone, so the cell must be free again.
        assert!(
            check_handle.try_borrow_mut().is_ok(),
            "the cloned borrow was leaked"
        );
    });
}
