//! Does a better retry strategy recover the contention loss?
//!
//! `contention.rs` shows `borrow_lite` losing to `RwLock` at four and eight
//! threads, because the retry loop burns CPU that a parked thread would give
//! back. That raises an obvious question: how much of the loss is the cell, and
//! how much is the naive `spin_loop()` loop the earlier benchmarks use?
//!
//! This compares four ways of waiting for a busy cell to free up:
//!
//! * `spin` - `core::hint::spin_loop()`, what `contention.rs` does
//! * `yield` - `std::thread::yield_now()`, which hands the core back
//! * `backoff` - spin a few times, then yield, escalating on repeats
//! * `RwLock` - the blocking reference point
//!
//! A second group, `retry/section`, sweeps the length of the exclusive borrow
//! with `spin` and `yield` side by side, so the degradation `contention.rs`
//! measured with `spin` can be compared against a yielding loop instead of
//! assumed.
//!
//! Every run is self-checking: the counter is read before and after, and the
//! delta must equal the increments asked for. `readonly.rs` covers the one case
//! these groups do not: many readers and no writer at all.
//!
//! Run with `cargo bench --bench retry`.

#![allow(missing_docs)]

use std::hint::spin_loop;
use std::sync::{Arc, RwLock};
use std::time::Duration;

use borrow_lite::{Shared, shared};
use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};

const WRITES_PER_THREAD: u64 = 20_000;
const THREAD_COUNTS: [u64; 3] = [2, 4, 8];

/// How a thread waits for a busy cell. A plain `fn` keeps the benchmark
/// monomorphic, so the waiting itself is what gets measured.
type Wait = fn(&mut u32);

fn wait_spin(_: &mut u32) {
    spin_loop();
}

fn wait_yield(_: &mut u32) {
    std::thread::yield_now();
}

/// Spin briefly, then yield, and back off further on each repeat. The state is
/// per-thread and reset whenever a borrow succeeds, so a thread that is not
/// contended never yields at all.
fn wait_backoff(spins: &mut u32) {
    if *spins < 16 {
        for _ in 0..*spins {
            spin_loop();
        }
        *spins *= 2;
    } else {
        std::thread::yield_now();
    }
}

/// One contended run: `threads` threads each perform `WRITES_PER_THREAD`
/// increments through the cell, waiting with `wait` on conflict.
fn contended(borrow_lite: &Shared<u64>, threads: u64, wait: Wait) {
    let before = borrow_lite.with(|v| *v);
    std::thread::scope(|scope| {
        for _ in 0..threads {
            scope.spawn(|| {
                let mut spins = 1u32;
                for _ in 0..WRITES_PER_THREAD {
                    while borrow_lite.try_with_mut(|v| *v += 1).is_err() {
                        wait(&mut spins);
                    }
                    spins = 1;
                }
            });
        }
    });
    let after = borrow_lite.with(|v| *v);
    assert_eq!(after - before, threads * WRITES_PER_THREAD, "lost update");
}

fn retry_strategies(c: &mut Criterion) {
    let mut group = c.benchmark_group("retry");
    group.sample_size(30);
    group.warm_up_time(Duration::from_secs(1));
    group.measurement_time(Duration::from_secs(3));

    for &threads in &THREAD_COUNTS {
        group.throughput(Throughput::Elements(threads * WRITES_PER_THREAD));

        let borrow_lite: Shared<u64> = shared(0);
        group.bench_with_input(
            BenchmarkId::new("spin", threads),
            &threads,
            |b, &threads| {
                b.iter(|| contended(&borrow_lite, threads, wait_spin));
            },
        );

        let borrow_lite: Shared<u64> = shared(0);
        group.bench_with_input(
            BenchmarkId::new("yield", threads),
            &threads,
            |b, &threads| {
                b.iter(|| contended(&borrow_lite, threads, wait_yield));
            },
        );

        let borrow_lite: Shared<u64> = shared(0);
        group.bench_with_input(
            BenchmarkId::new("backoff", threads),
            &threads,
            |b, &threads| {
                b.iter(|| contended(&borrow_lite, threads, wait_backoff));
            },
        );

        let std_cell = Arc::new(RwLock::new(0u64));
        group.bench_with_input(
            BenchmarkId::new("RwLock", threads),
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
                    assert_eq!(after - before, threads * WRITES_PER_THREAD, "lost update");
                });
            },
        );
    }
    group.finish();
}

/// The exclusive borrow does `work` units of arithmetic, then increments. Same
/// shape as the `section` group in `contention.rs`, so the two are comparable.
fn section(
    borrow_lite: &Shared<u64>,
    threads: u64,
    writes: u64,
    work: u64,
    wait: Wait,
    expected_delta: u64,
) {
    let before = borrow_lite.with(|v| *v);
    std::thread::scope(|scope| {
        for _ in 0..threads {
            scope.spawn(|| {
                let mut spins = 1u32;
                for _ in 0..writes {
                    while borrow_lite
                        .try_with_mut(|v| {
                            for i in 0..work {
                                *v = v.wrapping_add(i);
                            }
                            *v += 1;
                        })
                        .is_err()
                    {
                        wait(&mut spins);
                    }
                    spins = 1;
                }
            });
        }
    });
    let after = borrow_lite.with(|v| *v);
    assert_eq!(after - before, expected_delta, "lost update");
}

/// Does a yielding retry survive a long critical section, where `contention.rs`
/// showed spinning collapsing? The borrow is held for 0 to 1024 units of work.
fn section_length_by_strategy(c: &mut Criterion) {
    const THREADS: u64 = 4;
    const WRITES: u64 = 6_000;
    const WORK_LEVELS: [u64; 4] = [0, 64, 256, 1024];

    let mut group = c.benchmark_group("retry/section");
    group.sample_size(30);
    group.warm_up_time(Duration::from_secs(1));
    group.measurement_time(Duration::from_secs(3));

    for &work in &WORK_LEVELS {
        // Each acquisition adds `sum(0..work)` and then one.
        let per_acquisition: u64 = (0..work).sum::<u64>() + 1;
        let acquisitions = THREADS * WRITES;
        let expected_delta = acquisitions * per_acquisition;
        group.throughput(Throughput::Elements(acquisitions));

        let borrow_lite: Shared<u64> = shared(0);
        group.bench_with_input(BenchmarkId::new("spin", work), &work, |b, &work| {
            b.iter(|| {
                section(
                    &borrow_lite,
                    THREADS,
                    WRITES,
                    work,
                    wait_spin,
                    expected_delta,
                )
            })
        });

        let borrow_lite: Shared<u64> = shared(0);
        group.bench_with_input(BenchmarkId::new("yield", work), &work, |b, &work| {
            b.iter(|| {
                section(
                    &borrow_lite,
                    THREADS,
                    WRITES,
                    work,
                    wait_yield,
                    expected_delta,
                )
            })
        });

        // `backoff` was only ever measured on short sections, where it wins.
        // The question this arm answers is whether it degrades on long ones: a
        // spinning wait is the wrong shape when the holder is off-CPU for a
        // while, so if backoff loses badly as `work` grows it is not a
        // candidate for the default.
        let borrow_lite: Shared<u64> = shared(0);
        group.bench_with_input(BenchmarkId::new("backoff", work), &work, |b, &work| {
            b.iter(|| {
                section(
                    &borrow_lite,
                    THREADS,
                    WRITES,
                    work,
                    wait_backoff,
                    expected_delta,
                )
            })
        });

        let std_cell = Arc::new(RwLock::new(0u64));
        group.bench_with_input(BenchmarkId::new("RwLock", work), &work, |b, &work| {
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
            });
        });
    }
    group.finish();
}

criterion_group!(benches, retry_strategies, section_length_by_strategy);
criterion_main!(benches);
