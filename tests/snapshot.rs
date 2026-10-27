//! `Snapshot<T>`: the read-mostly cell.
//!
//! The claims to check are the ones the type exists for: a read never conflicts,
//! a stored value is visible to the next read, a reader that took `&T` before a
//! `store` still sees the old value (the retired values are kept alive), and
//! concurrent readers and a writer do not tear a value or lose a write.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use borrow_lite::Snapshot;

#[test]
fn a_stored_value_is_visible_to_the_next_read() {
    let cell = Snapshot::new(1u32);
    assert_eq!(*cell.load(), 1);
    cell.store(2);
    assert_eq!(*cell.load(), 2);
    cell.store(3);
    assert_eq!(*cell.load(), 3);
}

#[test]
fn a_read_taken_before_a_store_still_sees_the_old_value() {
    // This is the contract the retired list exists to keep: the reference handed
    // out by `load` must stay valid across a `store`.
    let cell = Snapshot::new(String::from("v1"));
    let seen = cell.load();
    cell.store(String::from("v2"));
    assert_eq!(seen, "v1");
    assert_eq!(cell.load(), "v2");
}

#[test]
fn a_reader_never_conflicts_with_a_writer() {
    // Unlike `Shared`, `load` has no failure mode at all - there is no `try_`
    // twin because there is nothing to refuse. The type simply does not compile
    // a refusal path; this test pins the observable half: a read taken while a
    // write is in flight still returns a whole value.
    let cell = Snapshot::new(0u64);
    for n in 1..=1000u64 {
        cell.store(n);
        assert_eq!(*cell.load(), n);
    }
}

#[test]
fn into_inner_returns_the_current_value() {
    let cell = Snapshot::new(vec![1, 2]);
    cell.store(vec![3]);
    cell.store(vec![4, 5]);
    assert_eq!(cell.into_inner(), vec![4, 5]);
}

#[test]
fn into_inner_drops_the_retired_values() {
    // The retired values are kept while the cell lives and released when it is
    // consumed. A drop counter proves the cleanup happens rather than leaking.
    struct Counted(Arc<AtomicUsize>);
    impl Drop for Counted {
        fn drop(&mut self) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }

    let drops = Arc::new(AtomicUsize::new(0));
    {
        let cell = Snapshot::new(Counted(Arc::clone(&drops)));
        cell.store(Counted(Arc::clone(&drops)));
        cell.store(Counted(Arc::clone(&drops)));
        // Three values were created; none may be freed while the cell lives, or
        // a reader's `&T` would dangle.
        assert_eq!(drops.load(Ordering::SeqCst), 0);
        let current = cell.into_inner();
        drop(current);
    }
    assert_eq!(drops.load(Ordering::SeqCst), 3);
}

#[test]
fn into_inner_drops_a_retired_zero_sized_value() {
    // A zero-sized `T` makes every `Box::into_raw` yield the *same* dangling
    // pointer, so removing the current value by pointer equality would remove
    // every entry and leak the destructors of the retired ones. The current
    // value is always the last entry `store` pushed, so removal is by position.
    struct Zst;
    static DROPS: AtomicUsize = AtomicUsize::new(0);
    impl Drop for Zst {
        fn drop(&mut self) {
            DROPS.fetch_add(1, Ordering::SeqCst);
        }
    }

    DROPS.store(0, Ordering::SeqCst);
    let cell = Snapshot::new(Zst);
    cell.store(Zst);
    cell.store(Zst);
    assert_eq!(DROPS.load(Ordering::SeqCst), 0);
    let current = cell.into_inner();
    drop(current);
    // All three values must be destroyed: the one handed back, and both retired.
    assert_eq!(DROPS.load(Ordering::SeqCst), 3);
}

#[test]
fn with_scopes_the_borrow_like_the_other_cells() {
    let cell = Snapshot::new(vec![1, 2, 3]);
    assert_eq!(cell.with(Vec::len), 3);
    assert_eq!(cell.with(|v| v.iter().sum::<i32>()), 6);
}

#[test]
fn deref_reads_the_current_value() {
    let cell = Snapshot::new(41u32);
    assert_eq!(*cell, 41);
    cell.store(42);
    assert_eq!(*cell, 42);
}

#[test]
fn concurrent_readers_and_one_writer_do_not_lose_a_write_or_tear_a_value() {
    // Many readers spin on `load` while one writer advances the value. Every
    // observed value must be one the writer actually published (no torn read),
    // and the final value must be the last one written (no lost update).
    const WRITES: u64 = 20_000;

    let cell = Arc::new(Snapshot::new((0u64, 0u64)));
    let done = Arc::new(AtomicBool::new(false));
    let torn = Arc::new(AtomicBool::new(false));

    std::thread::scope(|scope| {
        for _ in 0..4 {
            let cell = Arc::clone(&cell);
            let done = Arc::clone(&done);
            let torn = Arc::clone(&torn);
            scope.spawn(move || {
                while !done.load(Ordering::Acquire) {
                    let (a, b) = *cell.load();
                    // The writer always publishes a pair whose halves are equal.
                    if a != b {
                        torn.store(true, Ordering::Release);
                    }
                }
            });
        }

        for n in 1..=WRITES {
            cell.store((n, n));
        }
        done.store(true, Ordering::Release);
    });

    assert!(
        !torn.load(Ordering::Acquire),
        "a reader observed a torn value"
    );
    assert_eq!(*cell.load(), (WRITES, WRITES));
}

#[test]
fn is_send_and_sync_when_the_value_is() {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<Snapshot<u64>>();
    assert_send_sync::<Snapshot<String>>();
}
