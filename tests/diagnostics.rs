//! The part that makes the crate worth using: what the panic actually says.

use lite::Local;

#[test]
fn panic_names_both_sides_and_the_count() {
    let cell = Local::new(vec![1, 2, 3]);
    let _guard = cell.borrow();

    let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        cell.borrow_mut().push(4);
    }))
    .expect_err("borrow_mut must panic while a reader is alive");

    let message = panic
        .downcast_ref::<String>()
        .expect("panic payload is a String");

    assert!(
        message.contains("cell is already immutably borrowed"),
        "unexpected message: {message}"
    );
    assert!(
        message.contains("borrow attempted at"),
        "message does not name the refused borrow: {message}"
    );
    assert!(
        message.contains("held at"),
        "message does not name the holder: {message}"
    );
    assert!(
        message.contains("1 active borrow"),
        "message does not count the borrows: {message}"
    );
    assert!(
        message.contains(file!()),
        "message does not point at this test file: {message}"
    );
    assert!(
        message.contains("with`/`with_mut"),
        "message does not suggest the fix: {message}"
    );
}

#[test]
fn panic_for_a_held_writer_says_so() {
    let cell = Local::new(0u8);
    let _guard = cell.borrow_mut();

    let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _ = cell.borrow();
    }))
    .expect_err("borrow must panic while a writer is alive");

    let message = panic.downcast_ref::<String>().unwrap();
    assert!(
        message.contains("cell is already mutably borrowed"),
        "unexpected message: {message}"
    );
}

#[test]
fn multiple_readers_are_counted() {
    let cell = Local::new(0u8);
    let first = cell.borrow();
    let second = cell.borrow();
    let conflict = cell.try_borrow_mut().unwrap_err();
    assert_eq!(conflict.count(), Some(2));
    assert!(conflict.to_string().contains("held at"));
    drop(first);
    drop(second);
    assert!(cell.try_borrow_mut().is_ok());
}

#[test]
fn display_and_debug_carry_the_locations() {
    let cell = Local::new(0u8);
    let _guard = cell.borrow();
    let conflict = cell.try_borrow_mut().unwrap_err();
    let display = conflict.to_string();
    let debug = format!("{conflict:?}");
    assert!(display.contains(file!()));
    assert!(debug.contains("Conflict"));
    assert!(debug.contains("Readers"));
}

/// A released reader must not be blamed for a later conflict.
///
/// The single-threaded cell remembers one reader's location. If that reader goes
/// while another is still alive, the holder must stop naming it - otherwise a
/// refused write points at a borrow that is already gone. The cell does not
/// track *every* reader's location (that would be O(borrows) of state), so the
/// honest outcome is that the holder becomes unknown: the write is still
/// correctly refused, and `count` still says how many readers block it.
#[test]
fn a_released_reader_is_not_blamed() {
    let cell = Local::new(0u8);
    let first = cell.borrow();
    let first_line = line!() - 1;
    let second = cell.borrow();

    drop(first);
    let conflict = cell.try_borrow_mut().unwrap_err();

    assert_eq!(
        conflict.count(),
        Some(1),
        "one reader still blocks the write"
    );
    if let Some(holder) = conflict.holder() {
        assert_ne!(
            holder.line(),
            first_line,
            "the conflict blamed reader A, which was already dropped"
        );
    }
    drop(second);
}

/// The recorder flag must survive a clone: when the recording reader goes but a
/// clone stays, the cell must stay borrowed and the holder must be cleared
/// rather than left pointing at the dropped original.
#[test]
fn a_clone_keeps_the_borrow_and_clears_the_holder() {
    let cell = Local::new(0u8);
    let original = cell.borrow();
    let clone = original.clone();
    drop(original);

    // Still borrowed by the clone, so the write is refused...
    let conflict = cell.try_borrow_mut().unwrap_err();
    assert_eq!(conflict.count(), Some(1));
    assert!(
        conflict.holder().is_none(),
        "the dropped original should not be named as the holder"
    );
    drop(clone);
    assert!(cell.try_borrow_mut().is_ok());
}
