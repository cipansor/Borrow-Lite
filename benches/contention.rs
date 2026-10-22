//! Throughput when several threads want the same cell at once.
//!
//! The single-thread numbers in `compare.rs` price the diagnostics. These
//! price the *conflict policy*. `lite` never blocks, so a contended cell
//! burns CPU in a retry loop, while `RwLock` parks the loser. There is a
//! crossover, and this is how you find it.
//!
//! Every measurement is self-checking: the counter is read before and after
//! each iteration and the delta must equal the increments that were asked for.
//! A lost update would fail the benchmark, so these double as a real
//! contention test at thread counts Miri is too slow to reach.
//!
//! This benchmark retries with `spin_loop()`, which is the *worst* of the
//! options in `retry.rs`. It is kept that way on purpose: it is what the first
//! version of this benchmark did, and it is the loop most people write. Read
//! `retry.rs` before concluding anything about the cell from these numbers -
//! both its strategy comparison and its `retry/section` sweep, which is the
//! yielding version of the `section` group below.
//!
//! Run with `cargo bench --bench contention`.

#![allow(missing_docs)]

use std::hint::{black_box, spin_loop};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};
use std::time::Duration;

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use lite::{Shared, shared};

/// Increments each thread performs. Deliberately large: spawning a thread costs
/// tens of microseconds, so a short run would measure thread creation rather
/// than contention. `spawn_only` still reports the overhead for reference.
const WRITES_PER_THREAD: u64 = 20_000;
const READS_PER_READER: u64 = 50_000;
const WRITES_PER_WRITER: u64 = 6_000;

/// Thread counts to sweep. The machine has 4 cores, so 8 oversubscribes.
const THREAD_COUNTS: [u64; 4] = [1, 2, 4, 8];

/// Keep the contended groups quick: thread scheduling is noisy anyway, and a
/// long measurement time buys precision this workload does not have.
fn tune(group: &mut criterion::BenchmarkGroup<'_, criterion::measurement::WallTime>) {
    group.sample_size(30);
    group.warm_up_time(Duration::from_secs(1));
    group.measurement_time(Duration::from_secs(3));
}

/// Every thread increments the same counter, across thread counts.
fn write_scaling(c: &mut Criterion) {
    let mut group = c.benchmark_group("contention/write");
    tune(&mut group);

    for &threads in &THREAD_COUNTS {
        let total = threads * WRITES_PER_THREAD;
        group.throughput(Throughput::Elements(total));

        let lite: Shared<u64> = shared(0);
        group.bench_with_input(
            BenchmarkId::new("lite/Shared retry", threads),
            &threads,
            |b, &threads| {
                b.iter(|| {
                    let before = lite.with(|v| *v);
                    std::thread::scope(|scope| {
                        for _ in 0..threads {
                            scope.spawn(|| {
                                for _ in 0..WRITES_PER_THREAD {
                                    while lite.try_with_mut(|v| *v += 1).is_err() {
                                        spin_loop();
                                    }
                                }
                            });
                        }
                    });
                    let after = lite.with(|v| *v);
                    assert_eq!(after - before, total, "lost update");
                    black_box(after)
                });
            },
        );

        let std_cell = Arc::new(RwLock::new(0u64));
        group.bench_with_input(
            BenchmarkId::new("std/Arc<RwLock>", threads),
            &threads,
            |b, &threads| {
                b.iter(|| {
                    let before = *std_cell.read().unwrap();
                    std::thread::scope(|scope| {
                        for _ in 0..threads {
                            scope.spawn(|| {
                                for _ in 0..WRITES_PER_THREAD {
                                    *std_cell.write().unwrap() += 1;
                                }
                            });
                        }
                    });
                    let after = *std_cell.read().unwrap();
                    assert_eq!(after - before, total, "lost update");
                    black_box(after)
                });
            },
        );

        // The floor: no checking at all, just one atomic add per increment.
        let atomic = AtomicU64::new(0);
        group.bench_with_input(
            BenchmarkId::new("std/AtomicU64", threads),
            &threads,
            |b, &threads| {
                b.iter(|| {
                    std::thread::scope(|scope| {
                        for _ in 0..threads {
                            scope.spawn(|| {
                                for _ in 0..WRITES_PER_THREAD {
                                    atomic.fetch_add(1, Ordering::Relaxed);
                                }
                            });
                        }
                    });
                    black_box(atomic.load(Ordering::Relaxed))
                });
            },
        );

        // The same loop as `lite/Shared retry`, but waiting with
        // `yield_now` instead of `spin_loop`. `retry.rs` showed spinning is the
        // worst strategy and yielding beats `RwLock` at every thread count, so
        // the spinning row above understates the cell. This row is the fair
        // comparison, and it is the one to read.
        let lite: Shared<u64> = shared(0);
        group.bench_with_input(
            BenchmarkId::new("lite/Shared yield", threads),
            &threads,
            |b, &threads| {
                b.iter(|| {
                    let before = lite.with(|v| *v);
                    std::thread::scope(|scope| {
                        for _ in 0..threads {
                            scope.spawn(|| {
                                for _ in 0..WRITES_PER_THREAD {
                                    while lite.try_with_mut(|v| *v += 1).is_err() {
                                        std::thread::yield_now();
                                    }
                                }
                            });
                        }
                    });
                    let after = lite.with(|v| *v);
                    assert_eq!(after - before, total, "lost update");
                    black_box(after)
                });
            },
        );

        // The real competitor. `parking_lot::RwLock` keeps its whole state in
        // one word and takes a single CAS on the fast path, so it is cheaper
        // than `std::sync::RwLock` - and it is what people reach for when a
        // contended lock is the bottleneck. Comparing only against `std` would
        // flatter `lite`.
        let pl_cell = Arc::new(parking_lot::RwLock::new(0u64));
        group.bench_with_input(
            BenchmarkId::new("parking_lot/RwLock", threads),
            &threads,
            |b, &threads| {
                b.iter(|| {
                    let before = *pl_cell.read();
                    std::thread::scope(|scope| {
                        for _ in 0..threads {
                            scope.spawn(|| {
                                for _ in 0..WRITES_PER_THREAD {
                                    *pl_cell.write() += 1;
                                }
                            });
                        }
                    });
                    let after = *pl_cell.read();
                    assert_eq!(after - before, total, "lost update");
                    black_box(after)
                });
            },
        );
    }
    group.finish();
}

/// Threads that do nothing, so the spawn and join cost can be subtracted.
fn spawn_overhead(c: &mut Criterion) {
    let mut group = c.benchmark_group("contention/spawn_only");
    tune(&mut group);

    for &threads in &THREAD_COUNTS {
        group.bench_with_input(
            BenchmarkId::from_parameter(threads),
            &threads,
            |b, &threads| {
                b.iter(|| {
                    std::thread::scope(|scope| {
                        for _ in 0..threads {
                            scope.spawn(|| black_box(0u64));
                        }
                    });
                });
            },
        );
    }
    group.finish();
}

/// Three readers and one writer on four threads. This is the mix where a
/// blocking read-write lock is supposed to shine, because readers never
/// conflict with each other.
fn mixed_read_heavy(c: &mut Criterion) {
    const READERS: u64 = 3;

    let mut group = c.benchmark_group("contention/mixed");
    tune(&mut group);
    group.throughput(Throughput::Elements(
        READERS * READS_PER_READER + WRITES_PER_WRITER,
    ));

    let lite: Shared<u64> = shared(0);
    group.bench_function("lite/Shared retry", |b| {
        b.iter(|| {
            let before = lite.with(|v| *v);
            std::thread::scope(|scope| {
                for _ in 0..READERS {
                    scope.spawn(|| {
                        for _ in 0..READS_PER_READER {
                            // Readers conflict only with the writer, but they
                            // still have to cope with being refused.
                            while lite.try_with(|v| *v).is_err() {
                                spin_loop();
                            }
                        }
                    });
                }
                scope.spawn(|| {
                    for _ in 0..WRITES_PER_WRITER {
                        while lite.try_with_mut(|v| *v += 1).is_err() {
                            spin_loop();
                        }
                    }
                });
            });
            let after = lite.with(|v| *v);
            assert_eq!(after - before, WRITES_PER_WRITER, "lost update");
            black_box(after)
        });
    });

    let std_cell = Arc::new(RwLock::new(0u64));
    group.bench_function("std/Arc<RwLock>", |b| {
        b.iter(|| {
            let before = *std_cell.read().unwrap();
            std::thread::scope(|scope| {
                for _ in 0..READERS {
                    scope.spawn(|| {
                        for _ in 0..READS_PER_READER {
                            black_box(*std_cell.read().unwrap());
                        }
                    });
                }
                scope.spawn(|| {
                    for _ in 0..WRITES_PER_WRITER {
                        *std_cell.write().unwrap() += 1;
                    }
                });
            });
            let after = *std_cell.read().unwrap();
            assert_eq!(after - before, WRITES_PER_WRITER, "lost update");
            black_box(after)
        });
    });
    group.finish();
}

/// Four threads, but the exclusive borrow does real work while it holds the
/// cell. A retry loop throws away the time it spends spinning; a parked thread
/// does not, which is where the blocking lock should win.
fn section_length(c: &mut Criterion) {
    const THREADS: u64 = 4;
    const WRITES: u64 = 6_000;
    const WORK_LEVELS: [u64; 4] = [0, 64, 256, 1024];

    let mut group = c.benchmark_group("contention/section");
    tune(&mut group);

    for &work in &WORK_LEVELS {
        // Each acquisition adds `sum(0..work)` and then one, so the value delta
        // grows with the work level even though the acquisition count does not.
        let per_acquisition: u64 = (0..work).sum::<u64>() + 1;
        let acquisitions = THREADS * WRITES;
        let expected_delta = acquisitions * per_acquisition;
        group.throughput(Throughput::Elements(acquisitions));

        let lite: Shared<u64> = shared(0);
        group.bench_with_input(
            BenchmarkId::new("lite/Shared retry", work),
            &work,
            |b, &work| {
                b.iter(|| {
                    let before = lite.with(|v| *v);
                    std::thread::scope(|scope| {
                        for _ in 0..THREADS {
                            scope.spawn(|| {
                                for _ in 0..WRITES {
                                    while lite
                                        .try_with_mut(|v| {
                                            for i in 0..work {
                                                *v = v.wrapping_add(i);
                                            }
                                            *v += 1;
                                        })
                                        .is_err()
                                    {
                                        spin_loop();
                                    }
                                }
                            });
                        }
                    });
                    let after = lite.with(|v| *v);
                    assert_eq!(after - before, expected_delta, "lost update");
                    black_box(after)
                });
            },
        );

        let std_cell = Arc::new(RwLock::new(0u64));
        group.bench_with_input(
            BenchmarkId::new("std/Arc<RwLock>", work),
            &work,
            |b, &work| {
                b.iter(|| {
                    let before = *std_cell.read().unwrap();
                    std::thread::scope(|scope| {
                        for _ in 0..THREADS {
                            scope.spawn(|| {
                                for _ in 0..WRITES {
                                    let mut guard = std_cell.write().unwrap();
                                    for i in 0..work {
                                        *guard = guard.wrapping_add(i);
                                    }
                                    *guard += 1;
                                }
                            });
                        }
                    });
                    let after = *std_cell.read().unwrap();
                    assert_eq!(after - before, expected_delta, "lost update");
                    black_box(after)
                });
            },
        );
    }
    group.finish();
}

criterion_group!(
    benches,
    write_scaling,
    spawn_overhead,
    mixed_read_heavy,
    section_length
);
criterion_main!(benches);
