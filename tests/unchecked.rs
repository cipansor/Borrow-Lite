//! The `unsafe-unchecked` mode: a cell with no borrow flags, exactly the size
//! of `UnsafeCell<T>`.
//!
//! Gated on the `unsafe-unchecked` feature, which is off by default. The point
//! of the feature is that a caller who has proven no other borrow can exist
//! pays *nothing* for the check, so the size assertion is the headline test:
//! `Unchecked<T>` and `UnsafeCell<T>` must be the same size, or the mode has
//! bought nothing.
#![cfg(feature = "unsafe-unchecked")]

use std::cell::UnsafeCell;

use borrow_lite::{Unchecked, unchecked};

/// The mode's whole reason to exist: no flags, so the same size as the raw
/// `UnsafeCell` it wraps.
#[test]
fn unchecked_is_the_size_of_unsafe_cell() {
    assert_eq!(
        core::mem::size_of::<Unchecked<u64>>(),
        core::mem::size_of::<UnsafeCell<u64>>()
    );
    assert_eq!(
        core::mem::size_of::<Unchecked<[u8; 32]>>(),
        core::mem::size_of::<UnsafeCell<[u8; 32]>>()
    );
}

/// Reading and writing through the unchecked borrows round-trips.
#[test]
fn the_unchecked_borrows_read_and_write() {
    let cell = unchecked(1i32);
    // SAFETY: this cell is not borrowed anywhere else in the test.
    unsafe {
        assert_eq!(*cell.borrow_unchecked(), 1);
        *cell.borrow_mut_unchecked() += 41;
        assert_eq!(*cell.borrow_unchecked(), 42);
    }
}

/// `get_mut` is the safe path: `&mut self` proves nothing else aliases it.
#[test]
fn get_mut_needs_no_unsafe() {
    let mut cell = unchecked(vec![1, 2, 3]);
    cell.get_mut().push(4);
    // SAFETY: no other borrow is alive.
    assert_eq!(unsafe { cell.borrow_unchecked().len() }, 4);
}

/// `into_inner` and `From` build and tear down the cell.
#[test]
fn construction_and_teardown() {
    let cell = Unchecked::from(7u8);
    assert_eq!(cell.into_inner(), 7);

    let cell: Unchecked<i32> = 5.into();
    assert_eq!(cell.into_inner(), 5);
}

/// `Debug` prints the value.
#[test]
fn debug_prints_the_value() {
    assert_eq!(format!("{:?}", unchecked(7u8)), "Unchecked(7)");
}

/// A refused borrow is not possible here - there is no check - so the mode is
/// about the *absence* of the panic, which is what a hot path wants.
#[test]
fn a_borrow_never_panics() {
    let cell = unchecked(0u64);
    for _ in 0..1_000 {
        // SAFETY: the loop is single-threaded and each borrow ends before the
        // next begins.
        unsafe {
            *cell.borrow_mut_unchecked() += 1;
        }
    }
    assert_eq!(cell.into_inner(), 1_000);
}

/// The mode keeps the same auto traits as `UnsafeCell`: `Send` when `T` is,
/// and never `Sync`.
#[test]
fn auto_traits_match_unsafe_cell() {
    fn assert_send<T: Send>() {}
    assert_send::<Unchecked<u64>>();
    // `!Sync` is checked by a `compile_fail` doctest on the type itself.
}
