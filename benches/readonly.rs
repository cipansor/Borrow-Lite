//! Read-only contention: many readers, no writer at all.
//!
//! Every other contended benchmark here involves a writer, so the pure-read
//! case was never measured. It is the one case where a read-write lock is
//! supposed to be at its best, and it is worth knowing what it costs.
//!
//! The interesting detail is in `AtomicState::try_shared`: recording a shared
//! borrow is `fetch_add` on one shared counter, so readers contend on the same
//! cache line as each other. They do not conflict - the borrow is granted - but
//! they do serialise on that line. `Arc<T>` without any borrow bookkeeping is
//! the zero-cost floor to compare against.
//!
//! Groups:
//!
//! * `readonly/threads` - N threads reading as fast as they can, 1 to 8 threads
//! * `readonly/hold` - N readers each holding a shared borrow across a barrier,
//!   so all N are provably inside at once, then releasing
//!
//! Both assert that no read was ever refused: with no writer present, every
//! shared borrow must succeed. A refused read would be a bug in the cell.
//!
//! Run with `cargo bench --bench readonly`.

#![allow(missing_docs)]

use std::hint::black_box;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Barrier, RwLock};
use std::time::Duration;

use borrow_lite::{Shared, shared};
use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};

const READS_PER_THREAD: u64 = 500_000;
const THREAD_COUNTS: [u64; 4] = [1, 2, 4, 8];

fn tune(group: &mut criterion::BenchmarkGroup<'_, criterion::measurement::WallTime>) {
    group.sample_size(30);
    group.warm_up_time(Duration::from_secs(1));
    group.measurement_time(Duration::from_secs(3));
}

/// N threads read the same value as fast as they can. No writer exists, so
/// every borrow must be granted; the returned sum is checked against the
/// expected total, which also stops the optimiser removing the reads.
fn read_saturation(c: &mut Criterion) {
    let mut group = c.benchmark_group("readonly/threads");
    tune(&mut group);

    for &threads in &THREAD_COUNTS {
        group.throughput(Throughput::Elements(threads * READS_PER_THREAD));

        let borrow_lite: Shared<u64> = shared(7);
        group.bench_with_input(
            BenchmarkId::new("borrow_lite/Shared", threads),
            &threads,
            |b, &threads| {
                b.iter(|| {
                    // Shared, not captured by value: a `move` closure would give
                    // each thread its own copy and the assertion below would
                    // then pass without testing anything.
                    let refused = AtomicU64::new(0);
                    let refused = &refused;
                    let sum = std::thread::scope(|scope| {
                        let handles: Vec<_> = (0..threads)
                            .map(|_| {
                                let borrow_lite = borrow_lite.clone();
                                scope.spawn(move || {
                                    let mut total = 0u64;
                                    for _ in 0..READS_PER_THREAD {
                                        match borrow_lite.try_with(|v| *v) {
                                            Ok(value) => total += value,
                                            Err(_) => {
                                                refused.fetch_add(1, Ordering::Relaxed);
                                            }
                                        }
                                    }
                                    total
                                })
                            })
                            .collect();
                        handles.into_iter().map(|h| h.join().unwrap()).sum::<u64>()
                    });
                    assert_eq!(
                        refused.load(Ordering::Relaxed),
                        0,
                        "a read was refused with no writer present"
                    );
                    assert_eq!(sum, threads * READS_PER_THREAD * 7);
                    black_box(sum)
                });
            },
        );

        let std_cell = Arc::new(RwLock::new(7u64));
        group.bench_with_input(
            BenchmarkId::new("std/Arc<RwLock>", threads),
            &threads,
            |b, &threads| {
                b.iter(|| {
                    let sum = std::thread::scope(|scope| {
                        let handles: Vec<_> = (0..threads)
                            .map(|_| {
                                let std_cell = Arc::clone(&std_cell);
                                scope.spawn(move || {
                                    let mut total = 0u64;
                                    for _ in 0..READS_PER_THREAD {
                                        total += *std_cell.read().unwrap();
                                    }
                                    total
                                })
                            })
                            .collect();
                        handles.into_iter().map(|h| h.join().unwrap()).sum::<u64>()
                    });
                    assert_eq!(sum, threads * READS_PER_THREAD * 7);
                    black_box(sum)
                });
            },
        );

        // No borrow bookkeeping at all: this is the floor, and it shows how
        // much of the cost above is the cell rather than the threads.
        let plain = Arc::new(7u64);
        group.bench_with_input(
            BenchmarkId::new("std/Arc (no checking)", threads),
            &threads,
            |b, &threads| {
                b.iter(|| {
                    let sum = std::thread::scope(|scope| {
                        let handles: Vec<_> = (0..threads)
                            .map(|_| {
                                let plain = Arc::clone(&plain);
                                scope.spawn(move || {
                                    let mut total = 0u64;
                                    for _ in 0..READS_PER_THREAD {
                                        total += *plain;
                                    }
                                    total
                                })
                            })
                            .collect();
                        handles.into_iter().map(|h| h.join().unwrap()).sum::<u64>()
                    });
                    assert_eq!(sum, threads * READS_PER_THREAD * 7);
                    black_box(sum)
                });
            },
        );
    }
    group.finish();
}

/// N readers each hold a shared borrow while a barrier waits for all of them.
///
/// The barrier only releases once every thread has acquired, so if shared
/// borrows could not coexist this would hang rather than complete. It measures
/// the cost of a held shared borrow, not just an acquire/release pair.
fn held_readers(c: &mut Criterion) {
    let mut group = c.benchmark_group("readonly/hold");
    tune(&mut group);

    for &threads in &THREAD_COUNTS {
        group.throughput(Throughput::Elements(threads));

        let borrow_lite: Shared<u64> = shared(7);
        group.bench_with_input(
            BenchmarkId::new("borrow_lite/Shared", threads),
            &threads,
            |b, &threads| {
                b.iter(|| {
                    let barrier = Arc::new(Barrier::new(threads as usize));
                    let observed = AtomicU64::new(0);
                    std::thread::scope(|scope| {
                        for _ in 0..threads {
                            let borrow_lite = borrow_lite.clone();
                            let barrier = Arc::clone(&barrier);
                            let observed = &observed;
                            scope.spawn(move || {
                                let guard = borrow_lite.try_borrow().expect("no writer present");
                                // Every reader is inside the borrow here.
                                observed.fetch_add(*guard, Ordering::SeqCst);
                                barrier.wait();
                            });
                        }
                    });
                    assert_eq!(observed.load(Ordering::SeqCst), threads * 7);
                    black_box(observed.load(Ordering::SeqCst))
                });
            },
        );

        let std_cell = Arc::new(RwLock::new(7u64));
        group.bench_with_input(
            BenchmarkId::new("std/Arc<RwLock>", threads),
            &threads,
            |b, &threads| {
                b.iter(|| {
                    let barrier = Arc::new(Barrier::new(threads as usize));
                    let observed = AtomicU64::new(0);
                    std::thread::scope(|scope| {
                        for _ in 0..threads {
                            let std_cell = Arc::clone(&std_cell);
                            let barrier = Arc::clone(&barrier);
                            let observed = &observed;
                            scope.spawn(move || {
                                let guard = std_cell.read().unwrap();
                                observed.fetch_add(*guard, Ordering::SeqCst);
                                barrier.wait();
                            });
                        }
                    });
                    assert_eq!(observed.load(Ordering::SeqCst), threads * 7);
                    black_box(observed.load(Ordering::SeqCst))
                });
            },
        );
    }
    group.finish();
}

criterion_group!(benches, read_saturation, held_readers);
criterion_main!(benches);
