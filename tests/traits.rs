//! The trait surface the three cells carry, so a cell can be used where the
//! bare value is: arithmetic assignment, `Display`/`Debug`, `Hash`, `From`,
//! `Default`, and comparison against a bare `T`.
//!
//! Every one of these borrows the cell for the duration of the call, and every
//! borrowing method here carries `#[track_caller]`, so a conflict names the
//! caller's line. That is the property this file pins: not just that the trait
//! is implemented, but that a refusal points at the user's code and not at the
//! crate.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

use borrow_lite::{Inline, Local, Shared, inline, local, shared};

fn hash_of<T: Hash>(value: &T) -> u64 {
    let mut hasher = DefaultHasher::new();
    value.hash(&mut hasher);
    hasher.finish()
}

/// The arithmetic assignment operators on all three cells: `c += 1` and friends
/// are one exclusive borrow, released before the statement ends.
#[test]
fn arithmetic_assignment_operators_work_on_all_three_cells() {
    let mut i = inline(10i32);
    i += 5;
    i -= 3;
    i *= 2;
    i /= 4;
    assert_eq!(i.get(), 6); // ((10 + 5 - 3) * 2) / 4

    let mut l = local(10i32);
    l += 5;
    l -= 3;
    l *= 2;
    l /= 4;
    assert_eq!(l.get(), 6);

    let mut s: Shared<i32> = shared(10i32);
    s += 5;
    s -= 3;
    s *= 2;
    s /= 4;
    assert_eq!(s.get(), 6);
}

/// A refused assignment names the caller's line, because the operator methods
/// carry `#[track_caller]`.
///
/// `+=` takes `&mut self`, so the borrow checker rules out a conflict with a
/// guard on the *same* variable. A clone of the handle is a different variable
/// borrowing the same cell, which is the case that can still conflict.
#[cfg(feature = "diagnostics")]
#[test]
fn a_refused_assignment_blames_the_caller() {
    let mut cell = local(1i32);
    let other = cell.clone();
    let _held = other.borrow_mut();
    let err = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| cell += 1))
        .expect_err("+= while a clone holds a write borrow must panic");
    let message = err
        .downcast_ref::<String>()
        .expect("borrow_lite panics with a formatted String");
    assert!(
        message.contains("borrow attempted at tests/traits.rs:"),
        "the refused location should be the caller: {message}"
    );
}

/// `Display` prints the value under a shared borrow, without a guard.
#[test]
fn display_prints_the_value() {
    assert_eq!(format!("{}", inline(7u8)), "7");
    assert_eq!(format!("{}", local(7u8)), "7");
    let s: Shared<u8> = shared(7u8);
    assert_eq!(format!("{s}"), "7");
}

/// `Debug` keeps the wrapper name, so the cell is visible in a debug print.
#[test]
fn debug_shows_the_wrapper() {
    assert_eq!(format!("{:?}", inline(7u8)), "Inline(7)");
    assert_eq!(format!("{:?}", local(7u8)), "Local(7)");
    let s: Shared<u8> = shared(7u8);
    assert_eq!(format!("{s:?}"), "Shared(7)");
}

/// A cell hashes by value, so it can key a `HashMap` without unwrapping.
#[test]
fn hash_hashes_the_value() {
    assert_eq!(hash_of(&inline(7u8)), hash_of(&7u8));
    assert_eq!(hash_of(&local(7u8)), hash_of(&7u8));
    let s: Shared<u8> = shared(7u8);
    assert_eq!(hash_of(&s), hash_of(&7u8));
}

/// A refused `Display` still panics with a located conflict; the refused
/// location is the `Display` impl rather than the caller, because `format!`
/// reaches `Display::fmt` through `core::fmt`'s non-`track_caller` machinery.
/// The *holder* is still named at the caller's line, which is the useful half.
#[cfg(feature = "diagnostics")]
#[test]
fn a_refused_display_reports_the_holder() {
    let cell = local(7u8);
    let _held = cell.borrow_mut();
    let err = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| format!("{cell}")))
        .expect_err("formatting under an exclusive borrow must panic");
    let message = err
        .downcast_ref::<String>()
        .expect("borrow_lite panics with a formatted String");
    assert!(message.contains("already mutably borrowed"), "{message}");
    assert!(
        message.contains("held at tests/traits.rs:"),
        "the holder should be named at the caller's line: {message}"
    );
}

/// `From<T>` and `Default` build a cell the same way `new` does.
#[test]
fn from_and_default_build_a_cell() {
    let i: Inline<i32> = Inline::from(5);
    assert_eq!(i.get(), 5);
    let l: Local<i32> = 5.into();
    assert_eq!(l.get(), 5);
    let s: Shared<i32> = Shared::from(5);
    assert_eq!(s.get(), 5);

    assert_eq!(Inline::<i32>::default().get(), 0);
    assert_eq!(Local::<i32>::default().get(), 0);
    assert_eq!(Shared::<i32>::default().get(), 0);
}

/// `PartialEq<T>` and `PartialOrd<T>` compare the cell with a bare value, so
/// `c == 5` and `c < 6` need no guard.
#[test]
fn a_cell_compares_against_a_bare_value() {
    let i = inline(5i32);
    assert!(i == 5);
    assert!(i != 6);
    assert!(i < 6);
    assert!(i >= 5);
    assert_eq!(i.partial_cmp(&5), Some(std::cmp::Ordering::Equal));

    let l = local(5i32);
    assert!(l == 5);
    assert!(l != 6);
    assert!(l < 6);
    assert_eq!(l.partial_cmp(&6), Some(std::cmp::Ordering::Less));

    let s: Shared<i32> = shared(5i32);
    assert!(s == 5);
    assert!(s != 6);
    assert!(s < 6);
    assert_eq!(s.partial_cmp(&6), Some(std::cmp::Ordering::Less));
}

/// Comparing against a bare value under an exclusive borrow names the caller.
#[cfg(feature = "diagnostics")]
#[test]
fn a_refused_bare_comparison_blames_the_caller() {
    let cell = local(5i32);
    let _held = cell.borrow_mut();
    let err = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| cell == 5))
        .expect_err("comparing a borrowed cell must panic");
    let message = err
        .downcast_ref::<String>()
        .expect("borrow_lite panics with a formatted String");
    assert!(
        message.contains("borrow attempted at tests/traits.rs:"),
        "the refused location should be the caller: {message}"
    );
}

/// A cell and its bare value hash and compare consistently, so a `HashMap`
/// keyed by `Inline<T>` finds the entry keyed by the value.
#[test]
fn a_cell_can_key_a_map_like_the_value() {
    use std::collections::HashMap;

    let mut by_value: HashMap<i32, &str> = HashMap::new();
    by_value.insert(5, "five");
    by_value.insert(6, "six");

    let key = inline(5i32);
    // The map is keyed by the bare value; look it up through the cell by
    // hashing the cell and comparing it to the value.
    let found = by_value
        .iter()
        .find(|(k, _)| hash_of(&key) == hash_of(*k) && key == **k)
        .map(|(_, v)| *v);
    assert_eq!(found, Some("five"));
}
