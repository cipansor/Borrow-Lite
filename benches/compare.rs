//! Cost of the two cells, next to the `std` types they stand in for.
//!
//! The point is not to win a microbenchmark. It is to price the diagnostics:
//! `borrow_lite` records a source location and a borrow count on every borrow,
//! and the numbers here say what that costs.
//!
//! Run with `cargo bench`. `cargo test --all-targets` runs the same code in
//! criterion's test mode, which is only a smoke test.

#![allow(missing_docs)]

use std::cell::{Cell, RefCell};
use std::hint::black_box;
use std::rc::Rc;
use std::sync::{Arc, RwLock};

use borrow_lite::{
    Conflict, Inline, Local, ReadGuard, Shared, Snapshot, cell, inline, local, shared,
};
use criterion::{BatchSize, Criterion, criterion_group, criterion_main};

/// Shared read borrow, then read the value through the guard.
fn read_local(c: &mut Criterion) {
    let std_cell = Rc::new(RefCell::new(0u64));
    let borrow_cell: Local<u64> = local(0);

    let mut group = c.benchmark_group("read/local");
    group.bench_function("std/Rc<RefCell>", |b| {
        b.iter(|| {
            let guard = std_cell.borrow();
            black_box(*guard)
        });
    });
    group.bench_function("borrow_lite/Local", |b| {
        b.iter(|| {
            let guard = borrow_cell.borrow();
            black_box(*guard)
        });
    });
    group.finish();
}

/// Exclusive borrow, then mutate.
fn write_local(c: &mut Criterion) {
    let std_cell = Rc::new(RefCell::new(0u64));
    let borrow_cell: Local<u64> = local(0);

    let mut group = c.benchmark_group("write/local");
    group.bench_function("std/Rc<RefCell>", |b| {
        b.iter(|| {
            let mut guard = std_cell.borrow_mut();
            *guard = guard.wrapping_add(1);
            black_box(*guard)
        });
    });
    group.bench_function("borrow_lite/Local", |b| {
        b.iter(|| {
            let mut guard = borrow_cell.borrow_mut();
            *guard = guard.wrapping_add(1);
            black_box(*guard)
        });
    });
    group.finish();
}

/// The closure form, which is what the crate recommends.
fn closure_form(c: &mut Criterion) {
    let std_cell = Rc::new(RefCell::new(0u64));
    let borrow_cell: Local<u64> = local(0);

    let mut group = c.benchmark_group("with/local");
    group.bench_function("std/Rc<RefCell>", |b| {
        b.iter(|| {
            let mut value = std_cell.borrow_mut();
            *value = value.wrapping_add(1);
            black_box(*value)
        });
    });
    group.bench_function("borrow_lite/with_mut", |b| {
        b.iter(|| borrow_cell.with_mut(|value| black_box(*value)));
    });
    group.finish();
}

/// Repeated small borrows from a single thread, the shape most application
/// code has. Each iteration does one read-modify-write through the cell.
fn churn_local(c: &mut Criterion) {
    let std_cell = Rc::new(RefCell::new(0u64));
    let borrow_cell: Local<u64> = local(0);

    let mut group = c.benchmark_group("churn/local");
    group.bench_function("std/Rc<RefCell>", |b| {
        b.iter(|| {
            for _ in 0..16 {
                *std_cell.borrow_mut() += 1;
            }
            black_box(*std_cell.borrow())
        });
    });
    group.bench_function("borrow_lite/Local", |b| {
        b.iter(|| {
            for _ in 0..16 {
                borrow_cell.with_mut(|value| *value += 1);
            }
            black_box(borrow_cell.with(|value| *value))
        });
    });
    group.finish();
}

/// Cross-thread read borrow, next to `RwLock::read`.
fn read_shared(c: &mut Criterion) {
    let std_cell = Arc::new(RwLock::new(0u64));
    let borrow_cell: Shared<u64> = shared(0);

    let mut group = c.benchmark_group("read/shared");
    group.bench_function("std/Arc<RwLock>", |b| {
        b.iter(|| {
            let guard = std_cell.read().unwrap();
            black_box(*guard)
        });
    });
    group.bench_function("borrow_lite/Shared", |b| {
        b.iter(|| {
            let guard = borrow_cell.borrow();
            black_box(*guard)
        });
    });
    // The read-mostly alternative: one `Acquire` load, no counter. It is not a
    // borrow - there is nothing to refuse - so it is the floor for "read a
    // shared value", next to the two cells that count.
    let snapshot = Snapshot::new(0u64);
    group.bench_function("borrow_lite/Snapshot", |b| {
        b.iter(|| black_box(*snapshot.load()));
    });
    group.finish();
}

/// Cross-thread exclusive borrow, next to `RwLock::write`.
fn write_shared(c: &mut Criterion) {
    let std_cell = Arc::new(RwLock::new(0u64));
    let borrow_cell: Shared<u64> = shared(0);

    let mut group = c.benchmark_group("write/shared");
    group.bench_function("std/Arc<RwLock>", |b| {
        b.iter(|| {
            let mut guard = std_cell.write().unwrap();
            *guard = guard.wrapping_add(1);
            black_box(*guard)
        });
    });
    group.bench_function("borrow_lite/Shared", |b| {
        b.iter(|| {
            let mut guard = borrow_cell.borrow_mut();
            *guard = guard.wrapping_add(1);
            black_box(*guard)
        });
    });
    group.finish();
}

/// The cost of a refused borrow: the success path, then the `Err` path.
fn conflict_path(c: &mut Criterion) {
    let std_cell = Rc::new(RefCell::new(0u64));
    let borrow_cell: Local<u64> = local(0);

    let mut group = c.benchmark_group("try/local");
    group.bench_function("std/Rc<RefCell> success", |b| {
        b.iter(|| black_box(std_cell.try_borrow_mut().is_ok()));
    });
    group.bench_function("borrow_lite/Local success", |b| {
        b.iter(|| black_box(borrow_cell.try_borrow_mut().is_ok()));
    });
    group.bench_function("std/Rc<RefCell> conflict", |b| {
        let held = std_cell.borrow_mut();
        b.iter(|| black_box(std_cell.try_borrow_mut().is_err()));
        drop(held);
    });
    group.bench_function("borrow_lite/Local conflict", |b| {
        let held = borrow_cell.borrow_mut();
        b.iter(|| black_box(borrow_cell.try_borrow_mut().is_err()));
        drop(held);
    });
    group.finish();
}

/// Cloning a handle: `Rc::clone`/`Arc::clone` against the newtype wrapper.
fn clone_handle(c: &mut Criterion) {
    let std_rc = Rc::new(RefCell::new(0u64));
    let borrow_local: Local<u64> = local(0);
    let std_arc = Arc::new(RwLock::new(0u64));
    let borrow_shared: Shared<u64> = shared(0);

    let mut group = c.benchmark_group("clone");
    group.bench_function("std/Rc::clone", |b| {
        b.iter(|| black_box(Rc::clone(&std_rc)));
    });
    group.bench_function("borrow_lite/Local::clone", |b| {
        b.iter(|| black_box(borrow_local.clone()));
    });
    group.bench_function("std/Arc::clone", |b| {
        b.iter(|| black_box(Arc::clone(&std_arc)));
    });
    group.bench_function("borrow_lite/Shared::clone", |b| {
        b.iter(|| black_box(borrow_shared.clone()));
    });
    group.finish();
}

/// Allocation of a fresh cell, which is where the extra bookkeeping fields
/// show up.
fn allocation(c: &mut Criterion) {
    let mut group = c.benchmark_group("alloc");
    group.bench_function("std/Rc<RefCell<u64>>", |b| {
        b.iter(|| black_box(Rc::new(RefCell::new(black_box(0u64)))));
    });
    group.bench_function("borrow_lite/Local<u64>", |b| {
        b.iter(|| black_box(local(black_box(0u64))));
    });
    group.bench_function("std/Arc<RwLock<u64>>", |b| {
        b.iter(|| black_box(Arc::new(RwLock::new(black_box(0u64)))));
    });
    group.bench_function("borrow_lite/Shared<u64>", |b| {
        b.iter(|| black_box(shared(black_box(0u64))));
    });
    group.finish();
}

/// Narrowing an existing guard, which is the cheap alternative to re-borrowing.
fn guard_map(c: &mut Criterion) {
    let borrow_cell: Local<Vec<u8>> = local(vec![0; 64]);

    let mut group = c.benchmark_group("guard");
    group.bench_function("borrow_lite/map", |b| {
        b.iter(|| {
            let guard = borrow_cell.borrow();
            let narrowed: ReadGuard<'_, [u8], _> = ReadGuard::map(guard, |values| &values[..8]);
            black_box(narrowed.len())
        });
    });
    group.finish();
}

/// The `Cell`-only baseline: what the borrow bookkeeping costs over a type that
/// does no checking at all.
fn cell_baseline(c: &mut Criterion) {
    let plain = black_box(Cell::new(0u64));
    let borrow_cell: Local<u64> = local(0);

    let mut group = c.benchmark_group("baseline");
    group.bench_function("std/Cell", |b| {
        b.iter(|| {
            plain.set(plain.get().wrapping_add(1));
            black_box(plain.get())
        });
    });
    group.bench_function("borrow_lite/with_mut", |b| {
        b.iter(|| borrow_cell.with_mut(|value| black_box(*value)));
    });
    group.finish();
}

/// The inline cell against the `std` type it stands in for.
///
/// `Inline<T>` keeps the value in the struct, exactly as `RefCell<T>` does, so
/// the claim to check is that the borrow bookkeeping plus the recorded source
/// location costs nothing measurable over `RefCell`. `Local` is included to
/// show the one thing that does cost: the `Rc` pointer hop.
fn inline_cell(c: &mut Criterion) {
    let std_cell = RefCell::new(0u64);
    let borrow_cell: Inline<u64> = inline(0);
    let shared_cell: Local<u64> = local(0);

    let mut group = c.benchmark_group("inline");
    group.bench_function("std/RefCell read", |b| {
        b.iter(|| black_box(*black_box(&std_cell).borrow()));
    });
    group.bench_function("borrow_lite/Inline read", |b| {
        b.iter(|| black_box(*black_box(&borrow_cell).borrow()));
    });
    group.bench_function("borrow_lite/Local read", |b| {
        b.iter(|| black_box(*black_box(&shared_cell).borrow()));
    });
    group.bench_function("std/RefCell write", |b| {
        b.iter(|| *black_box(&std_cell).borrow_mut() += 1);
    });
    group.bench_function("borrow_lite/Inline write", |b| {
        b.iter(|| *black_box(&borrow_cell).borrow_mut() += 1);
    });
    group.finish();
}

/// The `cell!` accessors against the closure form they replace.
///
/// The accessor is `ReadGuard::map`/`WriteGuard::map` under the hood, so the
/// claim to check is that naming a field costs nothing over the closure: the
/// numbers should be indistinguishable. If they are not, the macro is hiding a
/// borrow the closure did not take.
fn cell_accessors(c: &mut Criterion) {
    cell! {
        shared Stats, StatsData { count: u64 => count_mut }
    }

    let stats = Stats::new(0);

    let mut group = c.benchmark_group("cell");
    group.bench_function("macro/accessor", |b| {
        b.iter(|| {
            let guard = stats.count();
            black_box(*guard)
        });
    });
    group.bench_function("macro/with", |b| {
        b.iter(|| stats.handle().with(|d| black_box(d.count)));
    });
    group.bench_function("macro/borrow", |b| {
        b.iter(|| {
            let guard = stats.handle().borrow();
            black_box(guard.count)
        });
    });
    group.bench_function("macro/accessor_mut", |b| {
        b.iter(|| {
            let mut guard = stats.count_mut();
            *guard = guard.wrapping_add(1);
            black_box(*guard)
        });
    });
    group.finish();
}

/// A batch of cells created and dropped, which exercises the drop path.
fn create_and_drop(c: &mut Criterion) {
    let mut group = c.benchmark_group("lifecycle");
    group.bench_function("std/Rc<RefCell>", |b| {
        b.iter_batched(
            || {
                (0..32)
                    .map(|_| Rc::new(RefCell::new(0u64)))
                    .collect::<Vec<_>>()
            },
            |cells| drop(black_box(cells)),
            BatchSize::SmallInput,
        );
    });
    group.bench_function("borrow_lite/Local", |b| {
        b.iter_batched(
            || (0..32).map(|_| local(0u64)).collect::<Vec<_>>(),
            |cells| drop(black_box(cells)),
            BatchSize::SmallInput,
        );
    });
    group.finish();
}

/// The paths that skip the borrow flags entirely: `get_mut` while the handle is
/// unique, `into_inner` to consume it, and `take` to empty it.
///
/// These are the "no bookkeeping at all" paths, so the claim to check is that
/// they cost what the equivalent `std` call costs and not a flag check more.
/// `RefCell::get_mut` and `Rc::try_unwrap` are the comparisons: `into_inner`
/// builds on `Rc::try_unwrap`, so it should track it closely.
fn unique_handle_paths(c: &mut Criterion) {
    let mut group = c.benchmark_group("unique");
    group.bench_function("std/RefCell::get_mut", |b| {
        let mut cell = RefCell::new(0u64);
        b.iter(|| *black_box(cell.get_mut()) += 1);
    });
    group.bench_function("borrow_lite/Local::get_mut", |b| {
        let mut cell: Local<u64> = local(0);
        b.iter(|| *black_box(cell.get_mut().unwrap()) += 1);
    });
    group.bench_function("std/RefCell::take", |b| {
        let cell = RefCell::new(0u64);
        b.iter(|| black_box(cell.take()));
    });
    group.bench_function("borrow_lite/Local::take", |b| {
        let cell: Local<u64> = local(0);
        b.iter(|| black_box(cell.take()));
    });
    group.bench_function("std/Rc::try_unwrap", |b| {
        b.iter_batched(
            || Rc::new(0u64),
            |cell| black_box(Rc::try_unwrap(cell).unwrap()),
            BatchSize::SmallInput,
        );
    });
    group.bench_function("borrow_lite/Local::into_inner", |b| {
        b.iter_batched(
            || local(0u64),
            |cell| black_box(cell.into_inner().unwrap()),
            BatchSize::SmallInput,
        );
    });
    group.finish();
}

/// Turning a `Conflict` into a message, which is the part the crate actually
/// sells. The borrow paths above only build the `Conflict` (a `Copy` struct
/// with a few words, free to construct); formatting it allocates and runs
/// several `write!` calls, and nothing measured that until now.
///
/// `to_string` is the `Display` path a user reaches for. The `borrow_mut` case
/// measures the full panicking path a refused *panicking* borrow pays for -
/// building the message and unwinding - which is why it is orders of magnitude
/// more expensive than formatting alone, and why `try_` is the recommended
/// form on any path that can be refused.
fn diagnostics(c: &mut Criterion) {
    // Produce a real conflict with both locations and a reader count, so the
    // formatting has every field to write.
    let cell: Local<u64> = local(0);
    let _read = cell.borrow();
    let conflict: Conflict = cell.try_borrow_mut().unwrap_err();
    drop(_read);

    let mut group = c.benchmark_group("diagnostics");
    group.bench_function("std/BorrowMutError::to_string", |b| {
        let std_cell = Rc::new(RefCell::new(0u64));
        let _held = std_cell.borrow_mut();
        b.iter(|| black_box(std_cell.try_borrow_mut().unwrap_err().to_string()));
    });
    group.bench_function("borrow_lite/Conflict::to_string", |b| {
        b.iter(|| black_box(conflict.to_string()));
    });
    // The default panic hook prints every caught panic to stderr. That would
    // make this benchmark measure stderr writes - hundreds of megabytes of
    // them - instead of the unwind, so the hook is silenced for these two and
    // restored afterwards.
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));

    group.bench_function("std/RefCell::borrow_mut panic path", |b| {
        let std_cell = Rc::new(RefCell::new(0u64));
        let _held = std_cell.borrow();
        b.iter(|| {
            let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let _ = std_cell.borrow_mut();
            }));
            black_box(caught.is_err())
        });
    });
    group.bench_function("borrow_lite/borrow_mut panic path", |b| {
        let cell: Local<u64> = local(0);
        let _held = cell.borrow();
        b.iter(|| {
            let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let _ = cell.borrow_mut();
            }));
            black_box(caught.is_err())
        });
    });

    std::panic::set_hook(previous);
    group.bench_function("borrow_lite/kind+holder (no format)", |b| {
        b.iter(|| black_box((conflict.kind(), conflict.holder(), conflict.count())));
    });
    group.finish();
}

/// Comparing two cells. `PartialEq` on distinct cells borrows both values, so
/// it costs what a read borrow costs plus the comparison; `ptr_eq` touches only
/// the handles. The `std` columns are `Rc<RefCell>` and `Arc<RwLock>` doing the
/// same comparison, so the price of the diagnostics is visible on this path too.
fn equality(c: &mut Criterion) {
    let borrow_a: Local<u64> = local(7);
    let borrow_b: Local<u64> = local(7);
    let std_a = Rc::new(RefCell::new(7u64));
    let std_b = Rc::new(RefCell::new(7u64));

    let mut group = c.benchmark_group("eq/local");
    group.bench_function("std/Rc<RefCell>", |b| {
        b.iter(|| black_box(std_a == std_b));
    });
    group.bench_function("borrow_lite/Local", |b| {
        b.iter(|| black_box(borrow_a == borrow_b));
    });
    group.bench_function("borrow_lite/Local::ptr_eq", |b| {
        b.iter(|| black_box(borrow_a.ptr_eq(&borrow_b)));
    });
    group.bench_function("borrow_lite/Local::try_eq", |b| {
        b.iter(|| black_box(borrow_a.try_eq(&borrow_b).unwrap()));
    });
    group.finish();

    // The values agree with the `std` comparison, checked once at setup rather
    // than per iteration.
    assert_eq!(borrow_a == borrow_b, std_a == std_b, "Local::eq diverged");
    assert_eq!(borrow_a.try_eq(&borrow_b), Ok(true));
    assert!(
        !borrow_a.ptr_eq(&borrow_b),
        "distinct cells must not be ptr-equal"
    );
    assert!(borrow_a.ptr_eq(&borrow_a.clone()));

    let shared_a: Shared<u64> = shared(7);
    let shared_b: Shared<u64> = shared(7);
    let arc_a = Arc::new(RwLock::new(7u64));
    let arc_b = Arc::new(RwLock::new(7u64));
    let mut group = c.benchmark_group("eq/shared");
    group.bench_function("std/Arc<RwLock>", |b| {
        b.iter(|| black_box(*arc_a.read().unwrap() == *arc_b.read().unwrap()));
    });
    group.bench_function("borrow_lite/Shared", |b| {
        b.iter(|| black_box(shared_a == shared_b));
    });
    group.bench_function("borrow_lite/Shared::ptr_eq", |b| {
        b.iter(|| black_box(shared_a.ptr_eq(&shared_b)));
    });
    group.bench_function("borrow_lite/Shared::try_eq", |b| {
        b.iter(|| black_box(shared_a.try_eq(&shared_b).unwrap()));
    });
    group.finish();

    assert_eq!(
        shared_a == shared_b,
        *arc_a.read().unwrap() == *arc_b.read().unwrap()
    );
    assert_eq!(shared_a.try_eq(&shared_b), Ok(true));
    assert!(!shared_a.ptr_eq(&shared_b));
    assert!(shared_a.ptr_eq(&shared_a.clone()));

    // The self-comparison short-circuit is the cheap path - it must not pay for
    // a borrow, on either handle type. `try_eq` and `==` on the same cell cost
    // the pointer check, not a read borrow.
    let mut self_group = c.benchmark_group("eq/self");
    self_group.bench_function("borrow_lite/Local self ==", |b| {
        let other = borrow_a.clone();
        b.iter(|| black_box(borrow_a == other));
    });
    self_group.bench_function("borrow_lite/Shared self ==", |b| {
        let other = shared_a.clone();
        b.iter(|| black_box(shared_a == other));
    });
    self_group.bench_function("borrow_lite/Shared self try_eq", |b| {
        let other = shared_a.clone();
        b.iter(|| black_box(shared_a.try_eq(&other).unwrap()));
    });
    self_group.finish();
}

/// The closure-free forms, which are the shortest way to read and write.
///
/// `get`/`set` are thin wrappers over the same borrow the closure forms take,
/// so the claim that they cost the same is measured rather than asserted: the
/// `std` column is `Cell::get`/`Cell::set`, the construct they are shaped
/// after, and the `with` row is the closure form they replace.
fn closure_free_forms(c: &mut Criterion) {
    let std_cell = Cell::new(0u64);
    let borrow_cell: Local<u64> = local(0);

    let mut read = c.benchmark_group("get/local");
    read.bench_function("std/Cell::get", |b| {
        b.iter(|| black_box(std_cell.get()));
    });
    read.bench_function("borrow_lite/Local::get", |b| {
        b.iter(|| black_box(borrow_cell.get()));
    });
    read.bench_function("borrow_lite/Local::with", |b| {
        b.iter(|| borrow_cell.with(|value| black_box(*value)));
    });
    read.finish();

    let mut write = c.benchmark_group("set/local");
    write.bench_function("std/Cell::set", |b| {
        b.iter(|| std_cell.set(black_box(1)));
    });
    write.bench_function("borrow_lite/Local::set", |b| {
        b.iter(|| borrow_cell.set(black_box(1)));
    });
    // The value is materialised *before* the closure, so both sides receive it
    // the same way. Black-boxing inside the closure instead - the obvious form
    // - measures the closure-capture of a black box, not the cell, and reports
    // a ~20% penalty that `set` does not actually pay.
    write.bench_function("borrow_lite/Local::with_mut", |b| {
        b.iter(|| {
            let value = black_box(1);
            borrow_cell.with_mut(|slot| *slot = value);
        });
    });
    write.finish();
}

criterion_group!(
    benches,
    read_local,
    write_local,
    closure_form,
    closure_free_forms,
    churn_local,
    read_shared,
    write_shared,
    conflict_path,
    clone_handle,
    allocation,
    guard_map,
    cell_accessors,
    cell_baseline,
    inline_cell,
    create_and_drop,
    unique_handle_paths,
    diagnostics,
    equality,
);
criterion_main!(benches);
