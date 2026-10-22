//! What does the observer hook cost?
//!
//! `Observable<T>` is a public module that had no performance data at all: the
//! other suites measure borrows, and observing is a different path. `update`
//! takes an exclusive borrow, runs the closure, then calls every observer
//! before releasing, so the cost is the borrow plus one call per observer.
//!
//! Groups:
//!
//! * `observable/read` - `with`, with and without observers registered
//! * `observable/update` - `update` against 0, 1, 4 and 16 observers, which is
//!   where the per-observer cost shows up
//! * `observable/churn` - `observe` then `unobserve`, the subscription path
//! * `observable/reentrant` - `notify` from inside an observer, the path with
//!   the round cap
//!
//! Run with `cargo bench --bench observable`.

#![allow(missing_docs)]

use std::hint::black_box;
use std::time::Duration;

use criterion::{BatchSize, BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use lite::{Observable, local};

const OBSERVER_COUNTS: [usize; 4] = [0, 1, 4, 16];
const ITERS: u64 = 200_000;

/// An observer that reads the value, so it cannot be optimised away.
fn register(observable: &Observable<u64>, count: usize) {
    for _ in 0..count {
        observable.observe(|value| {
            black_box(*value);
        });
    }
}

fn read_path(c: &mut Criterion) {
    let mut group = c.benchmark_group("observable/read");
    group.sample_size(50);
    group.warm_up_time(Duration::from_secs(1));
    group.measurement_time(Duration::from_secs(3));

    let bare: Observable<u64> = Observable::new(7);
    group.bench_function("with/no observers", |b| {
        b.iter(|| {
            let mut sum = 0u64;
            for _ in 0..ITERS {
                sum += bare.with(|v| *v);
            }
            black_box(sum)
        });
    });

    // Observers must not slow down a plain read: `with` only takes a borrow.
    let watched: Observable<u64> = Observable::new(7);
    register(&watched, 16);
    group.bench_function("with/16 observers", |b| {
        b.iter(|| {
            let mut sum = 0u64;
            for _ in 0..ITERS {
                sum += watched.with(|v| *v);
            }
            black_box(sum)
        });
    });

    // The reference point: a plain local cell with no observer machinery.
    let plain = local(7u64);
    group.bench_function("Local::with (reference)", |b| {
        b.iter(|| {
            let mut sum = 0u64;
            for _ in 0..ITERS {
                sum += plain.with(|v| *v);
            }
            black_box(sum)
        });
    });
    group.finish();
}

fn update_path(c: &mut Criterion) {
    let mut group = c.benchmark_group("observable/update");
    group.sample_size(50);
    group.warm_up_time(Duration::from_secs(1));
    group.measurement_time(Duration::from_secs(3));

    // `black_box` on the stored value in every closure below. Without it the
    // optimiser collapses the plain-cell loop to a single assignment - it
    // measured 0.01 ns per iteration, which is not a measurement of anything -
    // while `Observable::update` cannot be collapsed because `notify` touches a
    // heap-allocated list. The comparison would then be meaningless.
    for &observers in &OBSERVER_COUNTS {
        group.throughput(Throughput::Elements(ITERS));

        let observable: Observable<u64> = Observable::new(0);
        register(&observable, observers);
        group.bench_with_input(BenchmarkId::new("update", observers), &observers, |b, _| {
            b.iter(|| {
                for _ in 0..ITERS {
                    observable.update(|v| {
                        *v += 1;
                        black_box(*v);
                    });
                }
                black_box(observable.with(|v| *v))
            });
        });
    }

    // How much of `update` is the observer walk and how much is the borrow?
    let plain = local(0u64);
    group.bench_function("Local::with_mut (reference)", |b| {
        b.iter(|| {
            for _ in 0..ITERS {
                plain.with_mut(|v| {
                    *v += 1;
                    black_box(*v);
                });
            }
            black_box(plain.with(|v| *v))
        });
    });
    group.finish();
}

fn churn_path(c: &mut Criterion) {
    let mut group = c.benchmark_group("observable/churn");
    group.sample_size(50);
    group.warm_up_time(Duration::from_secs(1));
    group.measurement_time(Duration::from_secs(3));
    group.throughput(Throughput::Elements(ITERS));

    let observable: Observable<u64> = Observable::new(0);
    group.bench_function("observe+unobserve", |b| {
        b.iter(|| {
            for _ in 0..ITERS {
                let id = observable.observe(|value| {
                    black_box(*value);
                });
                black_box(observable.unobserve(id));
            }
            black_box(observable.observer_count())
        });
    });

    // Reusing one slot rather than growing the list: one long-lived observer
    // plus one that comes and goes.
    let observable: Observable<u64> = Observable::new(0);
    observable.observe(|value| {
        black_box(*value);
    });
    group.bench_function("observe+unobserve/1 resident", |b| {
        b.iter(|| {
            for _ in 0..ITERS {
                let id = observable.observe(|value| {
                    black_box(*value);
                });
                black_box(observable.unobserve(id));
            }
            black_box(observable.observer_count())
        });
    });
    group.finish();
}

fn reentrant_path(c: &mut Criterion) {
    let mut group = c.benchmark_group("observable/reentrant");
    group.sample_size(50);
    group.warm_up_time(Duration::from_secs(1));
    group.measurement_time(Duration::from_secs(3));
    group.throughput(Throughput::Elements(ITERS));

    // An observer that calls `notify` on the same cell: the deferred,
    // round-capped path, which is the interesting one for cost. The observer
    // needs an owned handle, so it holds a clone, which makes a reference cycle
    // that `clear_observers` breaks below.
    let observable: Observable<u64> = Observable::new(0);
    let inner = observable.clone();
    observable.observe(move |_| {
        inner.notify();
    });
    group.bench_function("notify from an observer", |b| {
        b.iter(|| {
            for _ in 0..ITERS {
                observable.notify();
            }
            black_box(observable.with(|v| *v))
        });
    });
    observable.clear_observers();

    // Plain notify with observers, for comparison.
    let observable: Observable<u64> = Observable::new(0);
    register(&observable, 4);
    group.bench_function("notify/4 observers", |b| {
        b.iter(|| {
            for _ in 0..ITERS {
                observable.notify();
            }
            black_box(observable.with(|v| *v))
        });
    });

    // `try_notify` is the twin of `notify`, and the two must cost the same: the
    // panicking form is `try_notify` plus a branch on the `Ok` it returns on
    // every healthy call. If this ever measures faster than `notify`, the twin
    // has stopped being the same code path.
    group.bench_function("try_notify/4 observers", |b| {
        b.iter(|| {
            for _ in 0..ITERS {
                observable.try_notify().unwrap();
            }
            black_box(observable.with(|v| *v))
        });
    });
    group.finish();
}

/// The refused re-entrant path: an observer that tries to mutate the value it
/// is being notified about, which must be told *no* rather than aliasing.
///
/// The deferred path above measures an observer that re-enters `notify`, which
/// is cheap (a flag check). This one measures the conflict: the observer's
/// `try_update` has to detect the borrow that the notification holds, build a
/// `Conflict`, and hand it back. That is the safety property the type exists
/// for, so it is worth pricing - and it was the one observable path with no
/// numbers.
fn refused_path(c: &mut Criterion) {
    let mut group = c.benchmark_group("observable/refused");
    group.sample_size(50);
    group.warm_up_time(Duration::from_secs(1));
    group.measurement_time(Duration::from_secs(3));
    group.throughput(Throughput::Elements(ITERS));

    // An observer that tries to update its own source and records that it was
    // refused, so the work cannot be optimised away.
    let observable: Observable<u64> = Observable::new(0);
    let refused: Observable<bool> = Observable::new(false);
    let inner = observable.clone();
    let refused_in_observer = refused.clone();
    observable.observe(move |_| {
        let outcome = inner.try_update(|v| *v += 1).is_err();
        refused_in_observer.update(|slot| *slot = outcome);
    });

    group.bench_function("observer try_update refused", |b| {
        b.iter(|| {
            for _ in 0..ITERS {
                observable.notify();
            }
            black_box(refused.with(|r| *r))
        });
    });

    // The same observer reading its source instead, which must be allowed.
    let observable: Observable<u64> = Observable::new(7);
    let observed: Observable<u64> = Observable::new(0);
    let inner = observable.clone();
    let observed_in_observer = observed.clone();
    observable.observe(move |_| {
        let value = inner.with(|v| *v);
        observed_in_observer.update(|slot| *slot = value);
    });

    group.bench_function("observer with (allowed)", |b| {
        b.iter(|| {
            for _ in 0..ITERS {
                observable.notify();
            }
            black_box(observed.with(|v| *v))
        });
    });
    group.finish();
}

/// The value-lifetime paths, which `Observable` shares with `Local` except
/// that `take` also notifies. The point is to price that difference: `get_mut`
/// and `into_inner` should cost exactly what `Local` costs, and `take` should
/// cost `Local::take` plus one notification round.
fn lifecycle_path(c: &mut Criterion) {
    let mut group = c.benchmark_group("observable/lifecycle");
    group.sample_size(50);

    group.bench_function("Local::get_mut (reference)", |b| {
        let mut cell = local(0u64);
        b.iter(|| *black_box(cell.get_mut().unwrap()) += 1);
    });
    group.bench_function("Observable::get_mut", |b| {
        let mut observable = Observable::new(0u64);
        b.iter(|| *black_box(observable.get_mut().unwrap()) += 1);
    });
    group.bench_function("Local::into_inner (reference)", |b| {
        b.iter_batched(
            || local(0u64),
            |cell| black_box(cell.into_inner().unwrap()),
            BatchSize::SmallInput,
        );
    });
    group.bench_function("Observable::into_inner", |b| {
        b.iter_batched(
            || Observable::new(0u64),
            |observable| black_box(observable.into_inner().unwrap()),
            BatchSize::SmallInput,
        );
    });

    group.bench_function("Local::take (reference)", |b| {
        let cell = local(0u64);
        b.iter(|| black_box(cell.take()));
    });
    for &observers in &OBSERVER_COUNTS {
        let observable: Observable<u64> = Observable::new(0);
        register(&observable, observers);
        group.bench_with_input(
            BenchmarkId::new("Observable::take", observers),
            &observers,
            |b, _| b.iter(|| black_box(observable.take())),
        );
    }
    // `replace` returns the old value *and* notifies - the union of
    // `Local::replace` (no notify) and `set` (notify, no return), so it should
    // cost roughly the sum.
    group.bench_function("Local::replace (reference)", |b| {
        let cell = local(0u64);
        b.iter(|| black_box(cell.replace(1)));
    });
    group.bench_function("Observable::set (reference)", |b| {
        let observable: Observable<u64> = Observable::new(0);
        b.iter(|| observable.set(1));
    });
    group.bench_function("Observable::replace", |b| {
        let observable: Observable<u64> = Observable::new(0);
        b.iter(|| black_box(observable.replace(1)));
    });
    group.bench_function("Observable::try_replace", |b| {
        let observable: Observable<u64> = Observable::new(0);
        b.iter(|| black_box(observable.try_replace(1).unwrap()));
    });
    group.finish();
}

criterion_group!(
    benches,
    read_path,
    update_path,
    churn_path,
    reentrant_path,
    refused_path,
    lifecycle_path
);
criterion_main!(benches);
