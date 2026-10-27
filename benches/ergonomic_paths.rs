//! Cost of the ergonomic paths added on top of the borrow: the arithmetic
//! assignment operators, `Display`/`Hash`, comparison against a bare value,
//! `map_get`, and (with the feature on) the unchecked borrow.
//!
//! The point is the same as `compare.rs`: price the diagnostics, and report the
//! numbers as they are, including where `borrow_lite` is slower. Each new path
//! is measured against the `std` type it stands in for (`RefCell`/`RwLock`) and,
//! where the operator has a lock analogue, against `parking_lot::RwLock`.
//!
//! Run with `cargo bench --bench ergonomic_paths`. `cargo test --all-targets`
//! runs the same code in criterion's test mode as a smoke test.

#![allow(missing_docs)]

use std::cell::RefCell;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::hint::black_box;
use std::sync::RwLock;

use borrow_lite::{Local, Shared, inline, local, shared};
use criterion::{Criterion, criterion_group, criterion_main};

/// `cell += 1` on all three cells, next to the `std` and `parking_lot` forms.
///
/// The operator takes one exclusive borrow; the `std` columns take a
/// `borrow_mut`/`write` guard for the same statement.
fn add_assign(c: &mut Criterion) {
    let mut inline_cell = inline(0u64);
    let mut local_cell: Local<u64> = local(0);
    let mut shared_cell: Shared<u64> = shared(0);
    let std_inline = RefCell::new(0u64);
    let std_local = RefCell::new(0u64);
    let std_shared = RwLock::new(0u64);
    let pl_shared = parking_lot::RwLock::new(0u64);

    let mut group = c.benchmark_group("add_assign");
    group.bench_function("std/RefCell (inline)", |b| {
        b.iter(|| *std_inline.borrow_mut() += black_box(1));
    });
    group.bench_function("borrow_lite/Inline", |b| {
        b.iter(|| inline_cell += black_box(1));
    });
    group.bench_function("std/RefCell (local)", |b| {
        b.iter(|| *std_local.borrow_mut() += black_box(1));
    });
    group.bench_function("borrow_lite/Local", |b| {
        b.iter(|| local_cell += black_box(1));
    });
    group.bench_function("std/RwLock", |b| {
        b.iter(|| *std_shared.write().unwrap() += black_box(1));
    });
    group.bench_function("parking_lot/RwLock", |b| {
        b.iter(|| *pl_shared.write() += black_box(1));
    });
    group.bench_function("borrow_lite/Shared", |b| {
        b.iter(|| shared_cell += black_box(1));
    });
    group.finish();
}

/// `c == 5` (a bare value) next to `*c.borrow() == 5`.
fn compare_against_a_value(c: &mut Criterion) {
    let cell: Local<u64> = local(7);
    let std_cell = RefCell::new(7u64);

    let mut group = c.benchmark_group("eq_value");
    group.bench_function("std/RefCell guard", |b| {
        b.iter(|| black_box(*std_cell.borrow() == 7));
    });
    group.bench_function("borrow_lite/with", |b| {
        b.iter(|| black_box(cell.with(|v| *v == 7)));
    });
    group.bench_function("borrow_lite/PartialEq<T>", |b| {
        b.iter(|| black_box(cell == 7));
    });
    group.finish();

    assert!(cell == 7);
}

/// `map_get(|v| v.field.clone())` next to the `RefCell` guard form and the
/// closure form it wraps.
fn map_get(c: &mut Criterion) {
    let cell: Local<(u64, u64)> = local((7, 8));
    let std_cell = RefCell::new((7u64, 8u64));

    let mut group = c.benchmark_group("map_get");
    group.bench_function("std/RefCell guard", |b| {
        b.iter(|| black_box(std_cell.borrow().0));
    });
    group.bench_function("borrow_lite/with", |b| {
        b.iter(|| black_box(cell.with(|v| v.0)));
    });
    group.bench_function("borrow_lite/map_get", |b| {
        b.iter(|| black_box(cell.map_get(|v| v.0)));
    });
    group.finish();

    assert_eq!(cell.map_get(|v| v.0), 7);
}

/// `Hash` on a cell, next to hashing a `RefCell` (which also implements it) and
/// hashing the bare value.
fn hash(c: &mut Criterion) {
    let cell: Local<u64> = local(7);
    let std_cell = RefCell::new(7u64);

    let mut group = c.benchmark_group("hash");
    group.bench_function("bare value", |b| {
        b.iter(|| {
            let mut h = DefaultHasher::new();
            black_box(7u64).hash(&mut h);
            black_box(h.finish())
        });
    });
    group.bench_function("std/RefCell", |b| {
        b.iter(|| {
            let mut h = DefaultHasher::new();
            std_cell.borrow().hash(&mut h);
            black_box(h.finish())
        });
    });
    group.bench_function("borrow_lite/Local", |b| {
        b.iter(|| {
            let mut h = DefaultHasher::new();
            cell.hash(&mut h);
            black_box(h.finish())
        });
    });
    group.finish();
}

/// The unchecked borrow (feature `unsafe-unchecked`) against a raw
/// `UnsafeCell`. This is the mode that must add *nothing* over `UnsafeCell`,
/// because that is its entire reason to exist.
#[cfg(not(feature = "unsafe-unchecked"))]
fn unchecked(_c: &mut Criterion) {}

#[cfg(feature = "unsafe-unchecked")]
fn unchecked(c: &mut Criterion) {
    use borrow_lite::Unchecked;
    use std::cell::UnsafeCell;

    let cell: Unchecked<u64> = Unchecked::new(0);
    let raw = UnsafeCell::new(0u64);

    let mut group = c.benchmark_group("unchecked/read");
    group.bench_function("std/UnsafeCell::get", |b| {
        b.iter(|| {
            // SAFETY: single-threaded, no other reference alive.
            black_box(unsafe { *raw.get() })
        });
    });
    group.bench_function("borrow_lite/Unchecked", |b| {
        b.iter(|| {
            // SAFETY: single-threaded, no other reference alive.
            black_box(unsafe { *cell.borrow_unchecked() })
        });
    });
    group.finish();
}

/// The same *shape* as `Inline`, so the size difference is the only difference:
/// this is the yardstick the README's "cheapest cell" claim is measured against.
fn inline_vs_refcell(c: &mut Criterion) {
    let cell = inline(0u64);
    let std_cell = RefCell::new(0u64);

    let mut group = c.benchmark_group("read/inline");
    group.bench_function("std/RefCell", |b| {
        b.iter(|| black_box(*std_cell.borrow()));
    });
    group.bench_function("borrow_lite/Inline", |b| {
        b.iter(|| black_box(cell.with(|v| *v)));
    });
    group.finish();
}

criterion_group!(
    benches,
    add_assign,
    compare_against_a_value,
    map_get,
    hash,
    inline_vs_refcell,
    unchecked,
);
criterion_main!(benches);
