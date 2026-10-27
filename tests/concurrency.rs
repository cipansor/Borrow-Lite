//! Concurrency: the atomic cell must never hand the same value to two
//! exclusive borrows at once, and a conflict must be reported rather than
//! producing two live `&mut`.
//!
//! These are the tests that would catch a race in `AtomicState`. Run them
//! under Miri (`cargo +nightly miri test --test concurrency`) to check the
//! atomic handshake, not just the outcome on one machine.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use borrow_lite::{Conflict, Shared, shared};

#[test]
fn exclusive_borrows_are_never_held_by_two_threads_at_once() {
    let cell: Shared<Vec<u32>> = shared(Vec::new());
    let live = Arc::new(AtomicUsize::new(0));
    let worst = Arc::new(AtomicUsize::new(0));

    std::thread::scope(|scope| {
        for thread in 0..4u32 {
            let cell = cell.clone();
            let live = Arc::clone(&live);
            let worst = Arc::clone(&worst);
            scope.spawn(move || {
                for i in 0..250u32 {
                    // Borrows do not block, so retry until this thread wins.
                    loop {
                        match cell.try_borrow_mut() {
                            Ok(mut values) => {
                                // If two threads ever got here together, the
                                // tracker would observe a count above one.
                                let now = live.fetch_add(1, Ordering::SeqCst) + 1;
                                worst.fetch_max(now, Ordering::SeqCst);
                                values.push(thread * 1000 + i);
                                assert!(live.fetch_sub(1, Ordering::SeqCst) >= 1);
                                break;
                            }
                            Err(_) => std::thread::yield_now(),
                        }
                    }
                }
            });
        }
    });

    assert_eq!(worst.load(Ordering::SeqCst), 1);
    assert_eq!(cell.with(Vec::len), 1000);
}

#[test]
fn readers_and_writers_never_overlap() {
    let cell: Shared<usize> = shared(0);
    let readers = Arc::new(AtomicUsize::new(0));
    let writer_live = Arc::new(AtomicUsize::new(0));
    let overlaps = Arc::new(AtomicUsize::new(0));

    std::thread::scope(|scope| {
        for _ in 0..3 {
            let cell = cell.clone();
            let readers = Arc::clone(&readers);
            let writer_live = Arc::clone(&writer_live);
            let overlaps = Arc::clone(&overlaps);
            scope.spawn(move || {
                for _ in 0..300 {
                    if let Ok(value) = cell.try_borrow() {
                        readers.fetch_add(1, Ordering::SeqCst);
                        if writer_live.load(Ordering::SeqCst) != 0 {
                            overlaps.fetch_add(1, Ordering::SeqCst);
                        }
                        let _ = *value;
                        readers.fetch_sub(1, Ordering::SeqCst);
                    }
                    std::thread::yield_now();
                }
            });
        }
        for _ in 0..2 {
            let cell = cell.clone();
            let readers = Arc::clone(&readers);
            let writer_live = Arc::clone(&writer_live);
            let overlaps = Arc::clone(&overlaps);
            scope.spawn(move || {
                for _ in 0..300 {
                    if let Ok(mut value) = cell.try_borrow_mut() {
                        writer_live.fetch_add(1, Ordering::SeqCst);
                        if readers.load(Ordering::SeqCst) != 0 {
                            overlaps.fetch_add(1, Ordering::SeqCst);
                        }
                        *value += 1;
                        writer_live.fetch_sub(1, Ordering::SeqCst);
                    }
                    std::thread::yield_now();
                }
            });
        }
    });

    assert_eq!(overlaps.load(Ordering::SeqCst), 0);
    assert!(cell.with(|v| *v) > 0);
}

#[test]
fn a_conflict_from_another_thread_is_an_error_not_a_panic() {
    let cell: Shared<u8> = shared(0);
    let (ready_tx, ready_rx) = std::sync::mpsc::channel();
    let (go_tx, go_rx) = std::sync::mpsc::channel();

    std::thread::scope(|scope| {
        let cell_in_worker = cell.clone();
        let worker = scope.spawn(move || {
            let _guard = cell_in_worker.borrow_mut();
            ready_tx.send(()).unwrap();
            go_rx.recv().unwrap();
        });

        ready_rx.recv().unwrap();
        let conflict: Conflict = cell.try_borrow().unwrap_err();
        assert_eq!(conflict.kind(), borrow_lite::ConflictKind::Writer);
        go_tx.send(()).unwrap();
        worker.join().unwrap();
    });

    assert!(cell.try_borrow().is_ok());
}

/// The recommended way to cope with contention: retry with `yield_now`.
///
/// `benches/retry.rs` measures this pattern beating both a spin loop and
/// `RwLock`, so it is worth pinning down as behaviour rather than leaving it to
/// a benchmark. Two things must hold: no update is lost, and the borrow is
/// always released, even when a thread is refused many times in a row.
#[test]
fn yield_based_retry_never_loses_an_update() {
    // Miri is orders of magnitude slower, so shrink the workload there; the
    // point under Miri is the data race, not the throughput.
    const THREADS: u64 = 4;
    const WRITES: u64 = if cfg!(miri) { 50 } else { 5_000 };

    let cell: Shared<u64> = shared(0);
    std::thread::scope(|scope| {
        for _ in 0..THREADS {
            let cell = cell.clone();
            scope.spawn(move || {
                for _ in 0..WRITES {
                    while cell.try_with_mut(|value| *value += 1).is_err() {
                        std::thread::yield_now();
                    }
                }
            });
        }
    });

    assert_eq!(cell.with(|v| *v), THREADS * WRITES);
}

/// The same, for readers: a refused read must not observe a torn or stale
/// value once it finally gets in, and the writer must not be starved forever.
#[test]
fn yield_based_retry_keeps_readers_and_writer_consistent() {
    const READERS: u64 = 3;
    const READS: u64 = if cfg!(miri) { 50 } else { 20_000 };
    const WRITES: u64 = if cfg!(miri) { 25 } else { 2_000 };

    let cell: Shared<u64> = shared(0);
    let monotonic = std::sync::atomic::AtomicBool::new(true);

    std::thread::scope(|scope| {
        for _ in 0..READERS {
            let monotonic = &monotonic;
            let cell = cell.clone();
            scope.spawn(move || {
                let mut last = 0;
                for _ in 0..READS {
                    let mut seen = None;
                    while seen.is_none() {
                        seen = cell.try_with(|value| *value).ok();
                        if seen.is_none() {
                            std::thread::yield_now();
                        }
                    }
                    let seen = seen.unwrap();
                    // The value only ever increases, so a reader must never see
                    // it go backwards.
                    if seen < last {
                        monotonic.store(false, std::sync::atomic::Ordering::SeqCst);
                    }
                    last = seen;
                }
            });
        }
        let cell = cell.clone();
        scope.spawn(move || {
            for _ in 0..WRITES {
                while cell.try_with_mut(|value| *value += 1).is_err() {
                    std::thread::yield_now();
                }
            }
        });
    });

    assert!(monotonic.load(std::sync::atomic::Ordering::SeqCst));
    assert_eq!(cell.with(|v| *v), WRITES);
}

/// With no writer present, shared borrows from many threads must all succeed,
/// and they must genuinely coexist rather than queue.
///
/// Every reader acquires before the barrier releases, so the run only completes
/// if all of them are inside their borrow at the same time. If shared borrows
/// were serialised as exclusive ones, this would hang.
#[test]
fn shared_borrows_coexist_across_threads() {
    const READERS: usize = 8;

    let cell: Shared<u64> = shared(7);
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(READERS));
    let inside = std::sync::atomic::AtomicUsize::new(0);
    let worst = std::sync::atomic::AtomicUsize::new(0);
    let refused = std::sync::atomic::AtomicUsize::new(0);

    std::thread::scope(|scope| {
        for _ in 0..READERS {
            let cell = cell.clone();
            let barrier = std::sync::Arc::clone(&barrier);
            let inside = &inside;
            let worst = &worst;
            let refused = &refused;
            scope.spawn(move || {
                match cell.try_borrow() {
                    Ok(guard) => {
                        let now = inside.fetch_add(1, Ordering::SeqCst) + 1;
                        worst.fetch_max(now, Ordering::SeqCst);
                        assert_eq!(*guard, 7);
                        // Blocks until every reader is holding a borrow.
                        barrier.wait();
                        inside.fetch_sub(1, Ordering::SeqCst);
                    }
                    Err(_) => {
                        refused.fetch_add(1, Ordering::SeqCst);
                    }
                }
            });
        }
    });

    assert_eq!(refused.load(Ordering::SeqCst), 0);
    assert_eq!(
        worst.load(Ordering::SeqCst),
        READERS,
        "readers did not all hold a borrow at once"
    );
    assert!(cell.try_borrow_mut().is_ok(), "borrows were not released");
}

/// A refused reader must always be told where the writer is.
///
/// The writer holds the cell for the whole scope, so every `Err` is a genuine
/// writer conflict and must carry a location. This is the runtime counterpart of
/// `a_writer_conflict_always_carries_a_location` in `tests/loom.rs`: the loom
/// model explores the interleavings, this pins the outcome on a real machine.
#[cfg(feature = "diagnostics")]
#[test]
fn a_writer_conflict_always_names_the_writer() {
    let cell: Shared<u32> = shared(0);
    let writer = cell.clone();
    let reader = cell.clone();

    let guard = writer.borrow_mut();

    std::thread::scope(|scope| {
        scope.spawn(|| {
            for _ in 0..64 {
                let conflict = reader.try_borrow().expect_err("the writer holds the cell");
                assert_eq!(conflict.kind(), borrow_lite::ConflictKind::Writer);
                assert!(
                    conflict.holder().is_some(),
                    "a writer conflict did not name the writer"
                );
            }
        });
    });

    drop(guard);
    assert!(cell.try_borrow().is_ok(), "the borrow was not released");
}
