//! The `diagnostics` feature turned *off*: a conflict is a kind and a count,
//! and nothing records a location.
//!
//! The feature is on by default, so this file only runs under
//! `--no-default-features`. It is the regression for the point of the feature:
//! with it off, `Conflict` carries no location and `Inline<u64>` is the same
//! size as `RefCell<u64>`. Both are asserted here so that a change which
//! silently re-adds the holder (or the word it lives in) fails a test rather
//! than only a `const` assertion in `safety.rs`.
#![cfg(not(feature = "diagnostics"))]

use borrow_lite::{ConflictKind, Inline, Local, Shared, inline, local, shared};

/// A refused borrow is still a `Conflict` with the right kind - only the
/// location is gone.
#[test]
fn a_conflict_reports_the_kind_but_no_location() {
    let cell = local(vec![1u8]);
    let _held = cell.borrow();
    let conflict = cell.try_borrow_mut().unwrap_err();

    assert_eq!(conflict.kind(), ConflictKind::Readers);
    assert_eq!(conflict.count(), Some(1));
    assert_eq!(conflict.holder(), None, "nothing records a holder");
    assert_eq!(conflict.wanted(), None, "nothing records the attempt");
}

/// The panicking message keeps the kind and the count and drops the locations -
/// the `RefCell`-shaped message, which is what the feature exists to remove.
#[test]
fn the_panic_message_carries_no_location() {
    let cell = local(0u8);
    let _held = cell.borrow_mut();
    let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _ = cell.borrow();
    }))
    .expect_err("the borrow must be refused");
    let message = panic
        .downcast_ref::<String>()
        .expect("borrow_lite panics with a formatted String");

    assert!(message.contains("already mutably borrowed"), "{message}");
    assert!(!message.contains("held at"), "{message}");
    assert!(!message.contains("borrow attempted at"), "{message}");
}

/// The borrow protocol is unchanged: refusing, releasing, and re-borrowing all
/// behave the same, only the diagnosis is missing.
#[test]
fn the_value_semantics_are_unchanged() {
    let cell: Local<Vec<i32>> = local(vec![1, 2, 3]);
    {
        let held = cell.borrow();
        assert_eq!(held.len(), 3);
        assert!(cell.try_borrow_mut().is_err());
    }
    cell.with_mut(|v| v.push(4));
    assert_eq!(cell.with(Vec::len), 4);

    let shared: Shared<u8> = shared(0);
    shared.with_mut(|v| *v += 1);
    assert_eq!(shared.with(|v| *v), 1);
}

/// The point of the feature: `Inline<u64>` is the same size as `RefCell<u64>`.
/// A `const` assertion in `tests/safety.rs` pins this for the build too; this is
/// the runtime echo, and it names the feature so the failure is legible.
#[test]
fn the_inline_cell_shrinks_to_refcell_size() {
    assert_eq!(
        core::mem::size_of::<Inline<u64>>(),
        core::mem::size_of::<std::cell::RefCell<u64>>(),
        "without `diagnostics`, `Inline` must be `RefCell`-sized"
    );
    assert_eq!(core::mem::size_of::<Inline<u64>>(), 16);
    // And the cell still works at that size.
    let cell = inline(7u64);
    assert_eq!(cell.with(|v| *v), 7);
}
