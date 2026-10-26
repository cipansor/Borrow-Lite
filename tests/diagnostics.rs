//! The part that makes the crate worth using: what the panic actually says.

use lite::{ConflictKind, Local, Shared};

/// The panicking comparison operators must blame *this* file, not a line inside
/// the crate.
///
/// `eq`/`partial_cmp`/`cmp` borrow through `with`, and `with` through `borrow`,
/// so the location only reaches the caller if the chain of `#[track_caller]`
/// functions is unbroken. Without the attribute on the trait impl the chain
/// stops inside `src/lib.rs`, and the "borrow attempted at" location points at
/// the crate rather than at the comparison the user wrote.
#[test]
fn comparison_panics_name_the_caller() {
    fn assert_names_caller(result: std::thread::Result<()>, what: &str) {
        let panic = result.expect_err(what);
        let message = panic
            .downcast_ref::<String>()
            .unwrap_or_else(|| panic!("{what} panicked with a non-String payload"));
        assert!(
            message.contains("borrow attempted at tests/diagnostics.rs:"),
            "{what} did not name the caller: {message}"
        );
    }

    let a = lite::inline(1u8);
    let b = lite::inline(2u8);
    let held = a.borrow_mut();
    assert_names_caller(
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _ = a == b;
        })),
        "Inline::eq",
    );
    assert_names_caller(
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _ = a.partial_cmp(&b);
        })),
        "Inline::partial_cmp",
    );
    assert_names_caller(
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _ = a.cmp(&b);
        })),
        "Inline::cmp",
    );
    drop(held);

    let a: Local<u8> = Local::new(1);
    let b: Local<u8> = Local::new(2);
    let held = a.borrow_mut();
    assert_names_caller(
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _ = a == b;
        })),
        "Local::eq",
    );
    assert_names_caller(
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _ = a.partial_cmp(&b);
        })),
        "Local::partial_cmp",
    );
    assert_names_caller(
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _ = a.cmp(&b);
        })),
        "Local::cmp",
    );
    drop(held);

    let a: Shared<u8> = Shared::new(1);
    let b: Shared<u8> = Shared::new(2);
    let held = a.borrow_mut();
    assert_names_caller(
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _ = a == b;
        })),
        "Shared::eq",
    );
    assert_names_caller(
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _ = a.partial_cmp(&b);
        })),
        "Shared::partial_cmp",
    );
    assert_names_caller(
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _ = a.cmp(&b);
        })),
        "Shared::cmp",
    );
    drop(held);
}

/// `try_eq`/`try_cmp` must record the *caller* as the refused borrow.
///
/// They take both shared borrows themselves, in their own `#[track_caller]`
/// frame. Taking them inside a nested `try_with` closure instead would record a
/// `wanted` location on a line inside the crate, pointing the reader at the
/// crate rather than at the comparison they wrote.
#[test]
fn try_eq_and_try_cmp_name_the_caller_as_the_refused_borrow() {
    let a: Local<i32> = Local::new(1);
    let b: Local<i32> = Local::new(2);
    let held = a.borrow_mut();
    let eq = a.try_eq(&b).unwrap_err();
    assert_eq!(eq.kind(), ConflictKind::Writer);
    assert_eq!(eq.wanted().map(|l| l.file()), Some(file!()));
    assert_eq!(
        a.try_cmp(&b).unwrap_err().wanted().map(|l| l.file()),
        Some(file!())
    );
    drop(held);

    let a: Shared<i32> = Shared::new(1);
    let b: Shared<i32> = Shared::new(2);
    let held = a.borrow_mut();
    assert_eq!(
        a.try_eq(&b).unwrap_err().wanted().map(|l| l.file()),
        Some(file!())
    );
    assert_eq!(
        a.try_cmp(&b).unwrap_err().wanted().map(|l| l.file()),
        Some(file!())
    );
    drop(held);

    // A conflict on the *other* cell is reported the same way.
    let a: Local<i32> = Local::new(1);
    let b: Local<i32> = Local::new(2);
    let held = b.borrow_mut();
    assert_eq!(
        a.try_eq(&b).unwrap_err().wanted().map(|l| l.file()),
        Some(file!())
    );
    drop(held);
}

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

/// The recorder flag must survive `ReadGuard::map` too.
///
/// `map` consumes the guard and rebuilds it around the narrowed pointer, so the
/// recorder flag has to be carried across by hand - and if it is dropped, the
/// mapped guard becomes a reader that will not clear the holder it recorded.
/// With a second reader alive the last-reader safety net does not fire, so a
/// later refused write names a borrow that has already been dropped. This is the
/// `map`-shaped twin of `a_released_reader_is_not_blamed`, and it is the only
/// test that fails if `map` forgets the flag.
#[test]
fn a_mapped_recorder_clears_the_holder_it_recorded() {
    use lite::ReadGuard;

    let cell = Local::new((1u32, 2u32));

    // The borrow records the holder; `map` must carry that responsibility
    // across to the guard it rebuilds.
    let mapped = ReadGuard::map(cell.borrow(), |t| &t.0);

    // A second reader: shared == 2, so when `mapped` drops the count does not
    // reach zero and the safety net does not clear the holder for it.
    let other = cell.borrow();
    drop(mapped);

    let conflict = cell.try_borrow_mut().unwrap_err();
    assert_eq!(
        conflict.count(),
        Some(1),
        "one reader still blocks the write"
    );
    assert_eq!(
        conflict.holder().map(|h| h.line()),
        None,
        "the conflict blamed the mapped reader that was already dropped"
    );
    drop(other);
    assert!(cell.try_borrow_mut().is_ok(), "the borrow leaked");
}

/// The mirror of `a_clone_keeps_the_borrow_and_clears_the_holder`: when the
/// *clone* goes and the recording reader stays, the holder must still name that
/// live reader.
///
/// A clone that wrongly claims the recorder flag clears the holder as it drops,
/// so a later refused write reports no holder at all even though the borrow it
/// should blame is still alive. Only the borrow that recorded the holder may
/// clear it, and this pins the other direction from the existing test.
#[test]
fn a_dropped_clone_does_not_clear_a_live_recorders_holder() {
    let cell = Local::new(0u8);
    let original = cell.borrow();
    let original_line = line!() - 1;
    let clone = original.clone();

    // The clone - not the recorder - is the one that goes.
    drop(clone);

    let conflict = cell.try_borrow_mut().unwrap_err();
    assert_eq!(
        conflict.count(),
        Some(1),
        "one reader still blocks the write"
    );
    assert_eq!(
        conflict.holder().map(|h| h.line()),
        Some(original_line),
        "the live recording reader should still be named as the holder"
    );
    drop(original);
    assert!(cell.try_borrow_mut().is_ok(), "the borrow leaked");
}
