//! The behaviour the crate promises: one cell, shared handles, borrows that
//! are counted and released, and conflicts that carry their source locations.

use lite::{Conflict, ConflictKind, Local, Shared, local, shared};

#[test]
fn local_handles_see_the_same_value() {
    let a = local(String::from("hi"));
    let b = a.clone();
    b.with_mut(|s| s.push('!'));
    assert_eq!(a.with(String::clone), "hi!");
}

#[test]
fn shared_handles_see_the_same_value() {
    let a = shared(String::from("hi"));
    let b = a.clone();
    b.with_mut(|s| s.push('!'));
    assert_eq!(a.with(String::clone), "hi!");
}

#[test]
fn many_readers_coexist() {
    let cell = Local::new(vec![1, 2, 3]);
    let first = cell.borrow();
    let second = cell.borrow();
    assert_eq!(first.iter().sum::<i32>(), second.iter().sum::<i32>());
    assert_eq!(cell.try_borrow().unwrap().len(), 3);
}

#[test]
fn writer_is_refused_while_readers_are_alive() {
    let cell = Local::new(vec![1]);
    let guard = cell.borrow();
    let conflict = cell.try_borrow_mut().unwrap_err();
    assert_eq!(conflict.kind(), ConflictKind::Readers);
    assert_eq!(conflict.count(), Some(1));
    assert!(conflict.holder().is_some());
    assert!(conflict.wanted().is_some());
    drop(guard);

    // and once the reader goes away the writer is fine
    cell.try_borrow_mut().unwrap().push(2);
    assert_eq!(cell.with(|v| v.clone()), vec![1, 2]);
}

#[test]
fn reader_is_refused_while_a_writer_is_alive() {
    let cell = Local::new(vec![1]);
    let guard = cell.borrow_mut();
    assert_eq!(cell.try_borrow().unwrap_err().kind(), ConflictKind::Writer);
    assert_eq!(
        cell.try_borrow_mut().unwrap_err().kind(),
        ConflictKind::Writer
    );
    drop(guard);
    assert_eq!(cell.try_borrow().unwrap().len(), 1);
}

#[test]
fn conflict_reports_the_holder_location() {
    let cell = Local::new(0u8);
    let _guard = cell.borrow();
    let conflict = cell.try_borrow_mut().unwrap_err();
    let holder = conflict.holder().expect("holder recorded");
    let wanted = conflict.wanted().expect("wanted recorded");
    assert_eq!(holder.file(), file!());
    assert_eq!(wanted.file(), file!());
    assert_ne!(holder.line(), wanted.line());
}

#[test]
fn nested_with_mut_is_a_conflict_not_a_deadlock() {
    let cell = shared(vec![1u8]);
    let seen = shared(None);
    let outcome = cell.try_with_mut(|_| {
        // re-entering from inside `with_mut` must be refused, and the outer
        // borrow must stay valid
        seen.with_mut(|slot| *slot = Some(cell.try_with_mut(|_| ()).is_err()))
    });
    assert!(outcome.is_ok());
    assert_eq!(seen.with(|slot| *slot), Some(true));
}

/// The mirror of `nested_with_mut_is_a_conflict_not_a_deadlock`: two *shared*
/// borrows are compatible, so `with` inside `with` on the same cell must
/// succeed. Only the write case is a conflict. This was the untested half - the
/// suite only ever nested writes.
#[test]
fn nested_read_borrows_are_allowed() {
    let cell = Local::new(vec![1u8, 2]);
    let first = cell.with(|outer| {
        // A second shared borrow of the same cell, from inside the first.
        let inner_len = cell.with(|nested| nested.len());
        assert_eq!(inner_len, 2);
        outer[0]
    });
    assert_eq!(first, 1);

    // And on the threaded cell, where the reader count is an atomic.
    let shared_cell = shared(7u64);
    let doubled = shared_cell.with(|outer| shared_cell.with(|nested| *nested + *outer));
    assert_eq!(doubled, 14);
}

#[test]
fn guards_release_on_drop() {
    let cell = Local::new(0u8);
    for _ in 0..10 {
        let guard = cell.borrow();
        drop(guard);
        let mut guard = cell.borrow_mut();
        *guard += 1;
        drop(guard);
    }
    assert_eq!(cell.with(|v| *v), 10);
}

#[test]
fn replace_returns_the_previous_value() {
    let cell = Local::new(String::from("old"));
    let previous = cell.replace(String::from("new"));
    assert_eq!(previous, "old");
    assert_eq!(cell.with(|v| v.clone()), "new");
}

#[test]
fn read_guard_map_narrows_a_borrow() {
    let cell = Local::new((1u32, String::from("hello")));
    let guard = lite::ReadGuard::map(cell.borrow(), |t| t.1.as_str());
    assert_eq!(&*guard, "hello");
    // still a borrow of the whole cell
    assert!(cell.try_borrow_mut().is_err());
}

#[test]
fn write_guard_map_narrows_a_borrow() {
    let cell = Local::new((1u32, 2u32));
    {
        let mut guard = lite::WriteGuard::map(cell.borrow_mut(), |t| &mut t.0);
        *guard = 10;
    }
    assert_eq!(cell.with(|t| *t), (10, 2));
}

/// `count` is only meaningful when readers are what is in the way. A writer
/// conflict has nothing to count, so it must report `None` rather than `Some(1)`
/// - the distinction is in the docs and was never asserted.
#[test]
fn count_is_none_for_a_writer_conflict() {
    let cell = Local::new(0u8);
    let _guard = cell.borrow_mut();
    let conflict = cell.try_borrow().unwrap_err();
    assert_eq!(conflict.kind(), ConflictKind::Writer);
    assert_eq!(
        conflict.count(),
        None,
        "a writer conflict must not be counted"
    );
}

/// The same for the shared cell, whose writer conflict is a different code path
/// (the atomic state rather than the cell).
#[test]
fn count_is_none_for_a_writer_conflict_on_a_shared_cell() {
    let cell = shared(0u8);
    let _guard = cell.borrow_mut();
    let conflict = cell.try_borrow().unwrap_err();
    assert_eq!(conflict.kind(), ConflictKind::Writer);
    assert_eq!(conflict.count(), None);
}

/// `replace` takes a write borrow, so it must report a conflict like any other
/// write rather than panicking or writing anyway. It was only ever tested on an
/// idle cell.
#[test]
fn replace_reports_a_conflict_while_a_reader_is_alive() {
    let cell = Local::new(String::from("old"));
    let _guard = cell.borrow();
    let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        cell.replace(String::from("new"));
    }));
    assert!(panic.is_err(), "replace must panic while a reader is alive");
    assert_eq!(cell.with(|v| v.clone()), "old", "a refused replace wrote");
}

#[test]
fn conflict_is_a_std_error() {
    fn take(_: &dyn std::error::Error) {}
    let cell = Local::new(0u8);
    let _guard = cell.borrow_mut();
    let conflict: Conflict = cell.try_borrow().unwrap_err();
    take(&conflict);
    assert!(conflict.to_string().contains("already mutably borrowed"));
}

#[test]
fn shared_cell_is_usable_from_many_threads() {
    // Borrows do not block, so concurrent writers report a conflict rather
    // than waiting. Recoverable, and the retry loop below is all it takes.
    let counter: Shared<usize> = shared(0);
    std::thread::scope(|scope| {
        for _ in 0..8 {
            scope.spawn(|| {
                for _ in 0..100 {
                    while counter.try_with_mut(|c| *c += 1).is_err() {
                        std::thread::yield_now();
                    }
                }
            });
        }
    });
    assert_eq!(counter.with(|c| *c), 800);
}

#[test]
fn shared_cell_reads_concurrently() {
    let data: Shared<Vec<u8>> = shared((0..64).collect());
    std::thread::scope(|scope| {
        for _ in 0..8 {
            scope.spawn(|| assert_eq!(data.with(Vec::len), 64));
        }
    });
}

#[test]
fn a_cloned_read_guard_holds_its_own_borrow() {
    let cell = local(vec![1u8, 2, 3]);
    let first = cell.borrow();
    let second = first.clone();
    // Two guards are alive, so the cell is still read-borrowed.
    let conflict = cell.try_borrow_mut().unwrap_err();
    assert_eq!(conflict.kind(), ConflictKind::Readers);
    assert_eq!(conflict.count(), Some(2));
    // Dropping one leaves the other holding its borrow.
    drop(second);
    assert_eq!(cell.try_borrow_mut().unwrap_err().count(), Some(1));
    drop(first);
    // Only when the last clone is gone is the cell free.
    assert!(cell.try_borrow_mut().is_ok());
}

#[test]
fn a_cloned_shared_read_guard_holds_its_own_borrow() {
    // The same contract across threads: the count is atomic, and the borrow is
    // only released when every clone is dropped.
    let cell = shared(7u8);
    let first = cell.borrow();
    let second = first.clone();
    assert_eq!(cell.try_borrow_mut().unwrap_err().count(), Some(2));
    drop(second);
    assert_eq!(cell.try_borrow_mut().unwrap_err().count(), Some(1));
    drop(first);
    assert!(cell.try_borrow_mut().is_ok());
}

#[test]
fn shared_is_send_and_sync_when_the_payload_is() {
    // Compile-time assertions. This is the same contract as `Arc<RwLock<T>>`.
    fn is_send<T: Send>() {}
    fn is_sync<T: Sync>() {}
    is_send::<Shared<u8>>();
    is_sync::<Shared<u8>>();
}

#[test]
fn inline_is_send_but_not_sync_like_refcell() {
    // The same auto traits as `RefCell<T>`: `Send` when `T: Send`, never `Sync`.
    // The `!Sync` half is a `compile_fail` doctest on `Inline`; the `Send` half
    // is checkable here, and it is the one `Local` cannot satisfy.
    fn is_send<T: Send>() {}
    is_send::<lite::Inline<u8>>();
    is_send::<lite::Inline<String>>();
}

#[test]
fn a_shared_cell_moves_between_threads() {
    let cell = shared(String::from("start"));
    let moved = cell.clone();
    std::thread::spawn(move || moved.with_mut(|s| s.push_str("-done")))
        .join()
        .unwrap();
    assert_eq!(cell.with(String::clone), "start-done");
}

#[test]
fn a_shared_cell_with_a_non_sync_payload_still_works_in_one_thread() {
    // `Cell<u8>` is `Send` but not `Sync`, so such a cell cannot be shared
    // across threads; that is a property of the payload, not of the cell.
    let slot = shared(std::cell::Cell::new(0u8));
    slot.with_mut(|c| c.set(7));
    assert_eq!(slot.with_mut(|c| c.get()), 7);
}

/// The README's core claim is "the value is the same" as `Rc<RefCell<T>>` and
/// `Arc<RwLock<T>>` - only the behaviour on a refused borrow differs. That is a
/// differential property, so it is tested by running the *same* script of
/// operations against both and comparing the results, rather than by asserting
/// each side in isolation.
mod differential {
    use std::cell::RefCell;
    use std::rc::Rc;
    use std::sync::{Arc, RwLock};

    use lite::{Local, Shared, local, shared};

    /// Every operation is applied to both cells; the results must match.
    #[test]
    fn local_matches_rc_refcell_for_the_same_script() {
        let std_cell = Rc::new(RefCell::new(vec![1i32, 2, 3]));
        let lite: Local<Vec<i32>> = local(vec![1, 2, 3]);

        // read
        assert_eq!(*std_cell.borrow(), lite.with(|v| v.clone()));

        // write
        std_cell.borrow_mut().push(4);
        lite.with_mut(|v| v.push(4));
        assert_eq!(*std_cell.borrow(), lite.with(|v| v.clone()));

        // replace, which returns the previous value on both sides
        let std_old = std_cell.replace(vec![9]);
        let lite_old = lite.replace(vec![9]);
        assert_eq!(std_old, lite_old);
        assert_eq!(*std_cell.borrow(), lite.with(|v| v.clone()));

        // a second handle sees the same data
        let std_alias = Rc::clone(&std_cell);
        let lite_alias = lite.clone();
        std_alias.borrow_mut().push(10);
        lite_alias.with_mut(|v| v.push(10));
        assert_eq!(*std_cell.borrow(), lite.with(|v| v.clone()));

        // sum, the shape the doctests use
        assert_eq!(
            std_cell.borrow().iter().sum::<i32>(),
            lite.with(|v| v.iter().sum())
        );
    }

    /// `ptr_eq`, `From`, `Default`, `PartialEq` and `Eq` are the trait surface
    /// `Rc<RefCell<T>>` and `Arc<RwLock<T>>` have and this crate did not. Each
    /// is compared against the `std` type on the same script.
    #[test]
    fn the_trait_surface_matches_the_std_types() {
        // ptr_eq: clone shares, separately built does not - same as Rc/Arc.
        let a: Local<u8> = local(1);
        let b = a.clone();
        let c: Local<u8> = local(1);
        // Same script on Rc: a clone shares, a separately built cell does not.
        let rc_a = Rc::new(1u8);
        let rc_b = Rc::clone(&rc_a);
        let rc_c = Rc::new(1u8);
        assert_eq!(a.ptr_eq(&b), Rc::ptr_eq(&rc_a, &rc_b));
        assert_eq!(a.ptr_eq(&c), Rc::ptr_eq(&rc_a, &rc_c));

        // PartialEq compares contents for distinct cells, like Rc<RefCell>.
        let x: Local<i32> = local(5);
        let y: Local<i32> = local(5);
        let z: Local<i32> = local(6);
        let rc_x = Rc::new(RefCell::new(5i32));
        let rc_y = Rc::new(RefCell::new(5i32));
        let rc_z = Rc::new(RefCell::new(6i32));
        assert_eq!(x == y, rc_x == rc_y);
        assert_eq!(x == z, rc_x == rc_z);
        assert!(x == y);
        assert!(x != z);
        assert!(x == x.clone()); // same cell: equal via ptr short-circuit
        assert_eq!(x, y); // assert_eq! works, so Eq is satisfied

        // Shared side matches Arc<RwLock>.
        let p: Shared<i32> = shared(5);
        let q: Shared<i32> = shared(5);
        let r: Shared<i32> = shared(6);
        assert!(p == q);
        assert!(p != r);
        assert!(p.ptr_eq(&p.clone()));
        assert!(!p.ptr_eq(&q));

        // From<T> and Default, on both handle types.
        let from_local: Local<i32> = Local::from(7);
        let from_shared: Shared<i32> = Shared::from(7);
        assert_eq!(from_local.with(|v| *v), 7);
        assert_eq!(from_shared.with(|v| *v), 7);
        let default_local: Local<i32> = Local::default();
        let default_shared: Shared<i32> = Shared::default();
        assert_eq!(default_local.with(|v| *v), i32::default());
        assert_eq!(default_shared.with(|v| *v), i32::default());

        // The std types offer the same, for the record.
        let std_default: RefCell<i32> = RefCell::default();
        assert_eq!(*std_default.borrow(), 0);
    }

    /// `PartialEq` on distinct cells panics when one is borrowed, exactly as
    /// `Rc<RefCell<T>>` does - but a cell compared with *itself* short-circuits
    /// on the pointer and never borrows, so it does not panic. The safe twin
    /// `try_eq` returns the conflict instead.
    #[test]
    fn equality_on_a_borrowed_cell_panics_like_std_and_try_eq_does_not() {
        let a: Local<i32> = local(1);
        let b: Local<i32> = local(1);

        // Same cell, live mutable borrow: no panic (pointer short-circuit).
        let held = a.borrow_mut();
        assert!(a == a.clone(), "self-comparison must not borrow the value");
        // `try_eq` carries the same short-circuit, so a cell compared with
        // itself is `Ok(true)` even while borrowed - it must not report a
        // conflict that the panicking form does not have.
        assert_eq!(
            a.try_eq(&a.clone()),
            Ok(true),
            "try_eq on the same cell must not borrow the value"
        );
        // Distinct cell, live borrow on `a`: panics, like Rc<RefCell>.
        let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| a == b));
        assert!(panic.is_err(), "comparing a borrowed cell must panic");
        // The non-panicking twin reports it instead.
        assert!(a.try_eq(&b).is_err(), "try_eq must report the conflict");
        drop(held);

        // Idle: they agree.
        assert!(a == b);
        assert!(a.try_eq(&b).unwrap());
    }

    /// Ordering mirrors equality: distinct cells compare by value under a
    /// shared borrow, the same cell short-circuits to `Equal`, and the
    /// non-panicking twin `try_cmp` reports a conflict instead of panicking.
    #[test]
    fn ordering_on_a_borrowed_cell_panics_like_std_and_try_cmp_does_not() {
        use core::cmp::Ordering;

        let a: Local<i32> = local(1);
        let b: Local<i32> = local(2);

        // Ordering by value, like `Rc<RefCell<T>>`.
        assert_eq!(a.partial_cmp(&b), Some(Ordering::Less));
        assert!(a < b);
        assert_eq!(b.cmp(&a), Ordering::Greater);
        // Same cell short-circuits to `Equal` without borrowing.
        assert_eq!(a.partial_cmp(&a.clone()), Some(Ordering::Equal));
        assert_eq!(a.cmp(&a.clone()), Ordering::Equal);
        assert_eq!(a.try_cmp(&a.clone()), Ok(Some(Ordering::Equal)));

        // Same cell, live mutable borrow: still no panic (pointer short-circuit).
        let held = a.borrow_mut();
        assert_eq!(a.partial_cmp(&a.clone()), Some(Ordering::Equal));
        assert_eq!(a.cmp(&a.clone()), Ordering::Equal);
        assert_eq!(a.try_cmp(&a.clone()), Ok(Some(Ordering::Equal)));
        // Distinct cell, live borrow on `a`: panics, like Rc<RefCell>.
        let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| a < b));
        assert!(panic.is_err(), "comparing a borrowed cell must panic");
        // The non-panicking twin reports it instead.
        assert!(a.try_cmp(&b).is_err(), "try_cmp must report the conflict");
        drop(held);

        // Idle: they agree again.
        assert!(a < b);
        assert_eq!(a.try_cmp(&b), Ok(Some(Ordering::Less)));
    }

    /// The shared cell orders the same way, and `Arc<RwLock<T>>` offers no
    /// ordering at all - so the reference here is `RefCell`, not `RwLock`.
    #[test]
    fn shared_cells_compare_by_value_too() {
        use core::cmp::Ordering;

        let a: Shared<i32> = shared(1);
        let b: Shared<i32> = shared(2);
        assert_eq!(a.partial_cmp(&b), Some(Ordering::Less));
        assert!(a < b);
        assert_eq!(a.partial_cmp(&a.clone()), Some(Ordering::Equal));
        assert_eq!(a.try_cmp(&b), Ok(Some(Ordering::Less)));

        // The short-circuit holds under a live exclusive borrow for every
        // ordering entry point, so `partial_cmp`, `cmp` and `try_cmp` are each
        // pinned (a mutation in any one must not survive).
        let held = a.borrow_mut();
        assert_eq!(a.partial_cmp(&a.clone()), Some(Ordering::Equal));
        assert_eq!(a.cmp(&a.clone()), Ordering::Equal);
        assert_eq!(a.try_cmp(&a.clone()), Ok(Some(Ordering::Equal)));
        drop(held);

        // Sorting a slice of handles works, which is the reason `Ord` is here.
        let mut v = [shared(3), shared(1), shared(2)];
        v.sort();
        assert_eq!(
            v.iter().map(|c| c.with(|x| *x)).collect::<Vec<_>>(),
            [1, 2, 3]
        );
    }

    /// Both refuse a conflicting borrow, and both report it the same way when
    /// asked non-panicking. The *diagnostics* differ (that is the point of the
    /// crate), so only the boolean outcome is compared.
    #[test]
    fn local_and_rc_refcell_refuse_the_same_borrows() {
        let std_cell = Rc::new(RefCell::new(0u8));
        let lite: Local<u8> = local(0);

        {
            let _std_read = std_cell.borrow();
            let _lite_read = lite.borrow();
            assert!(std_cell.try_borrow_mut().is_err());
            assert!(lite.try_borrow_mut().is_err());
        }
        {
            let _std_write = std_cell.borrow_mut();
            let _lite_write = lite.borrow_mut();
            assert!(std_cell.try_borrow().is_err());
            assert!(lite.try_borrow().is_err());
        }

        // And both recover once the guard is gone.
        assert!(std_cell.try_borrow_mut().is_ok());
        assert!(lite.try_borrow_mut().is_ok());
    }

    /// The threaded pair. `RwLock` *waits* where `Shared` refuses, so the
    /// script only uses operations both complete: exclusive mutation under a
    /// write lock, and a read that must see the final value.
    #[test]
    fn shared_matches_arc_rwlock_for_the_same_script() {
        let std_cell = Arc::new(RwLock::new(0u64));
        let lite: Shared<u64> = shared(0);

        std::thread::scope(|scope| {
            for _ in 0..4 {
                let std_cell = Arc::clone(&std_cell);
                let lite = lite.clone();
                scope.spawn(move || {
                    for _ in 0..250 {
                        // `RwLock` blocks, so no retry is needed; `Shared`
                        // refuses, so retry. Both end up applying every write.
                        *std_cell.write().unwrap() += 1;
                        while lite.try_with_mut(|v| *v += 1).is_err() {
                            std::thread::yield_now();
                        }
                    }
                });
            }
        });

        assert_eq!(*std_cell.read().unwrap(), 1000);
        assert_eq!(lite.with(|v| *v), 1000);
    }
}

/// `get_mut` is the zero-cost path when the handle is unique. It must not
/// report `Some` while another handle exists, because that handle could hold a
/// borrow - the same contract as `Rc::get_mut`.
#[test]
fn get_mut_requires_a_unique_handle() {
    let mut cell = local(vec![1, 2, 3]);
    cell.get_mut().unwrap().push(4);
    assert_eq!(cell.with(|v| v.clone()), vec![1, 2, 3, 4]);

    // A second handle makes it `None`, even with no borrow actually alive.
    let other = cell.clone();
    assert!(cell.get_mut().is_none());
    drop(other);
    // And it comes back once the handle is gone.
    assert!(cell.get_mut().is_some());

    // Same for the atomic cell.
    let mut cell = shared(vec![1, 2, 3]);
    cell.get_mut().unwrap().push(4);
    assert_eq!(cell.with(|v| v.clone()), vec![1, 2, 3, 4]);
    let other = cell.clone();
    assert!(cell.get_mut().is_none());
    drop(other);
    assert!(cell.get_mut().is_some());
}

/// `into_inner` is the ownership-taking pair to `get_mut`, and it must follow
/// the same uniqueness contract as `Rc::try_unwrap`/`Arc::try_unwrap`: `Err`
/// while another handle exists, because that handle could hold a borrow.
#[test]
fn into_inner_requires_a_unique_handle() {
    let cell = local(String::from("bye"));
    let other = cell.clone();
    let cell = cell.into_inner().unwrap_err();
    assert_eq!(cell.with(|v| v.clone()), "bye"); // returned unchanged
    drop(other);
    assert_eq!(cell.into_inner().unwrap(), "bye");

    let cell = shared(String::from("bye"));
    let other = cell.clone();
    let cell = cell.into_inner().unwrap_err();
    drop(other);
    assert_eq!(cell.into_inner().unwrap(), "bye");
}

/// `take` must behave like `RefCell::take`: it empties the cell, and a live
/// borrow turns it into a located panic rather than losing the value.
#[test]
fn take_empties_the_cell_and_panics_when_borrowed() {
    let cell = local(vec![1, 2, 3]);
    assert_eq!(cell.take(), vec![1, 2, 3]);
    assert!(cell.with(Vec::is_empty));
    assert_eq!(cell.take(), Vec::<i32>::new()); // already empty, still fine

    let cell = shared(vec![9]);
    assert_eq!(cell.take(), vec![9]);
    assert!(cell.with(Vec::is_empty));

    // A held borrow refuses `take` with a panic that names the holder.
    let cell = local(vec![1]);
    let _held = cell.borrow();
    let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| cell.take()))
        .expect_err("take should have panicked");
    let message = panic.downcast_ref::<String>().unwrap();
    assert!(message.contains("already"), "unexpected message: {message}");
    assert_eq!(cell.with(|v| v.clone()), vec![1]); // value not lost
}

/// The value-extraction path must agree with the standard types it mirrors, the
/// same differential check the borrow paths get.
#[test]
fn into_inner_and_take_agree_with_std() {
    use std::cell::RefCell;
    use std::sync::{Arc, RwLock};

    // `take` vs `RefCell::take`.
    let std_cell = RefCell::new(vec![1i32, 2, 3]);
    let lite = local(vec![1i32, 2, 3]);
    assert_eq!(std_cell.take(), lite.take());

    // `into_inner` vs `Arc::try_unwrap` (both fail with a live second handle,
    // both succeed once it is gone).
    let std_cell = Arc::new(RwLock::new(5u8));
    let lite = shared(5u8);
    let std_clone = Arc::clone(&std_cell);
    let lite_clone = lite.clone();
    assert!(Arc::try_unwrap(std_cell).is_err());
    assert!(lite.into_inner().is_err());
    drop(std_clone);
    drop(lite_clone);
    let std_cell = Arc::new(RwLock::new(5u8));
    let lite = shared(5u8);
    assert_eq!(Arc::try_unwrap(std_cell).unwrap().into_inner().unwrap(), 5);
    assert_eq!(lite.into_inner().unwrap(), 5);
}

/// Every panicking method has a `try_` twin that reports the same conflict as
/// an error - except `replace`/`take`, which were the gap. `Conflict` is `Copy`,
/// and `try_replace` additionally hands the unwritten value back so a refused
/// replace never loses it.
#[test]
fn try_replace_and_try_take_report_the_conflict_and_keep_the_value() {
    let cell = Local::new(String::from("old"));

    // Idle: `try_replace` succeeds and returns the old value, matching `replace`.
    assert_eq!(cell.try_replace(String::from("new")).unwrap(), "old");
    assert_eq!(cell.with(|v| v.clone()), "new");

    // Borrowed: refused, value handed back, cell untouched.
    let _guard = cell.borrow();
    let (conflict, value) = cell.try_replace(String::from("lost")).unwrap_err();
    assert_eq!(value, "lost", "a refused try_replace swallowed the value");
    assert_eq!(conflict.kind(), ConflictKind::Readers);
    assert_eq!(cell.with(|v| v.clone()), "new", "a refused replace wrote");
    assert!(
        cell.try_take().is_err(),
        "try_take must report the conflict"
    );
    assert_eq!(
        cell.with(|v| v.clone()),
        "new",
        "a refused take emptied the cell"
    );
    drop(_guard);

    // Idle again: both work, and `try_take` matches `take`.
    assert_eq!(cell.try_take().unwrap(), "new");
    assert!(cell.with(String::is_empty));
    assert_eq!(cell.try_take().unwrap(), String::new());
}

/// The `try_` twins must be reachable on both handle types through `Deref`.
#[test]
fn the_try_twins_are_available_on_local_and_shared() {
    let local: Local<u32> = Local::new(1);
    assert_eq!(local.try_replace(2).unwrap(), 1);
    assert_eq!(local.try_take().unwrap(), 2);

    let shared: Shared<u32> = Shared::new(1);
    assert_eq!(shared.try_replace(2).unwrap(), 1);
    assert_eq!(shared.try_take().unwrap(), 2);
}

/// The cross-thread half of the pointer short-circuit. `Shared::eq` and
/// `Inner<AtomicState>::try_eq` each carry their own copy of it, and until now
/// only the `Local` copies were exercised: deleting the guard from either
/// `Shared` copy left the whole suite green. Each assertion below pins one copy.
mod shared_short_circuit {
    use lite::{Shared, shared};

    /// `Shared::eq` short-circuits on the pointer: a cell compared with itself
    /// is equal without borrowing the value, so a live exclusive borrow cannot
    /// turn `cell == cell.clone()` into a panic.
    #[test]
    fn shared_equality_with_itself_never_borrows() {
        let a: Shared<i32> = shared(1);
        let held = a.borrow_mut();
        assert!(
            a == a.clone(),
            "self-comparison must short-circuit on the pointer, not borrow"
        );
        // A distinct cell with a live borrow on `a` does conflict, exactly as
        // the `Local` side does.
        let b: Shared<i32> = shared(1);
        let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| a == b));
        assert!(panic.is_err(), "comparing a borrowed Shared must panic");
        drop(held);
        assert!(a == b);
    }

    /// `Inner<AtomicState>::try_eq` carries the same short-circuit, and it is a
    /// *separate* copy from `Shared::eq` - this test exists to fail if only one
    /// of the two is removed.
    #[test]
    fn shared_try_eq_with_itself_never_borrows() {
        let a: Shared<i32> = shared(1);
        let b: Shared<i32> = shared(1);
        let held = a.borrow_mut();
        assert_eq!(
            a.try_eq(&a.clone()),
            Ok(true),
            "try_eq on the same cell must short-circuit, not report a conflict"
        );
        assert!(
            a.try_eq(&b).is_err(),
            "try_eq on a distinct borrowed cell must report the conflict"
        );
        drop(held);
        assert_eq!(a.try_eq(&b), Ok(true));
    }

    /// The short-circuit under real concurrency: one thread holds the exclusive
    /// borrow while others compare the cell with a clone. A short-circuit that
    /// actually borrowed would race with the writer, so this is the shape the
    /// atomic state exists for.
    #[test]
    fn shared_self_comparison_is_safe_while_another_thread_writes() {
        let cell: Shared<u64> = shared(0);
        let writers: Shared<u64> = cell.clone();
        let readers: Shared<u64> = cell.clone();
        std::thread::scope(|scope| {
            scope.spawn(move || {
                for _ in 0..1_000 {
                    writers.with_mut(|v| *v += 1);
                }
            });
            for _ in 0..4 {
                let readers = readers.clone();
                scope.spawn(move || {
                    for _ in 0..1_000 {
                        // Never borrows, so it cannot conflict with the writer.
                        assert!(readers == readers.clone());
                        assert_eq!(readers.try_eq(&readers.clone()), Ok(true));
                    }
                });
            }
        });
        assert_eq!(cell.with(|v| *v), 1_000);
    }
}

/// The one place the crate's `PartialEq` deliberately differs from `Rc`/`Arc`.
///
/// `Rc`/`Arc` short-circuit on the pointer **only when `T: Eq`** - they
/// specialise on `Eq` precisely because "a `PartialEq` might be deliberately
/// irreflexive" (their own comment). So `Rc<RefCell<f64>>` compared with itself
/// still borrows the value, and `Rc::new(NAN) == Rc::clone(..)` is `false`.
///
/// `lite` has no `Eq` bound to specialise on at that point, so it always
/// short-circuits. The divergence is observable, so it is pinned here rather
/// than left to prose, and it is documented on the impls.
mod irreflexive_payload {
    use std::cell::RefCell;
    use std::rc::Rc;

    use lite::{Local, local};

    #[test]
    fn local_self_comparison_short_circuits_even_for_a_non_eq_payload() {
        // lite: the pointer short-circuit runs regardless of `T: Eq`, so
        // a cell with a live exclusive borrow still compares equal to itself.
        let cell: Local<f64> = local(1.0);
        let held = cell.borrow_mut();
        assert!(cell == cell.clone(), "lite short-circuits unconditionally");
        assert_eq!(cell.try_eq(&cell.clone()), Ok(true));
        drop(held);

        // The consequence, stated plainly: a self-comparison of an irreflexive
        // payload reports `true`, where a value comparison would report `false`.
        let nan: Local<f64> = local(f64::NAN);
        assert!(nan == nan.clone());
    }

    #[test]
    fn std_diverges_for_a_non_eq_payload_which_is_why_we_document_it() {
        // std: no `T: Eq`, so no pointer short-circuit - it borrows the value,
        // which panics under a live borrow, and NAN != NAN.
        let rc = Rc::new(RefCell::new(1.0f64));
        let held = rc.borrow_mut();
        let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let other = Rc::clone(&rc);
            rc == other
        }));
        assert!(
            panic.is_err(),
            "std Rc<RefCell<f64>> does not short-circuit"
        );
        drop(held);

        let rc_nan = Rc::new(RefCell::new(f64::NAN));
        assert!(
            rc_nan != Rc::clone(&rc_nan),
            "std compares the value, so NAN is not equal to itself"
        );

        // With `T: Eq` std *does* short-circuit, matching lite.
        let rc_i = Rc::new(RefCell::new(1i32));
        let held = rc_i.borrow_mut();
        assert!(
            rc_i == Rc::clone(&rc_i),
            "with T: Eq std short-circuits and never borrows"
        );
        drop(held);
    }
}

/// What a `Conflict` records, and where it is deliberately silent. The holder
/// is written only on the writer path, so a readers conflict on a `Shared` cell
/// names its count but no location; the `Local` cell can name the first reader
/// for free. Both halves are pinned here, because the docs promise them.
mod holder_locations {
    use lite::{ConflictKind, Local, Shared, local, shared};

    #[test]
    fn local_readers_conflict_names_the_first_reader() {
        let cell: Local<i32> = local(1);
        let _first = cell.borrow();
        let _second = cell.borrow();
        let conflict = cell.try_borrow_mut().unwrap_err();
        assert_eq!(conflict.kind(), ConflictKind::Readers);
        assert_eq!(conflict.count(), Some(2));
        assert!(
            conflict.holder().is_some(),
            "the single-threaded cell can name the first reader for free"
        );
    }

    /// The documented asymmetry: a `Shared` readers conflict has a count but no
    /// holder, because readers do not record their location (an atomic store on
    /// every read borrow, which would also race).
    #[test]
    fn shared_readers_conflict_has_a_count_but_no_holder() {
        let cell: Shared<i32> = shared(1);
        let _a = cell.borrow();
        let _b = cell.borrow();
        let _c = cell.borrow();
        let conflict = cell.try_borrow_mut().unwrap_err();
        assert_eq!(conflict.kind(), ConflictKind::Readers);
        assert_eq!(conflict.count(), Some(3));
        assert_eq!(
            conflict.holder(),
            None,
            "readers must not record a location"
        );
        // The refusal still names the attempt.
        assert!(conflict.wanted().is_some());
    }

    #[test]
    fn shared_writer_conflict_names_the_writer() {
        let cell: Shared<i32> = shared(1);
        let _writer = cell.borrow_mut();
        let conflict = cell.try_borrow().unwrap_err();
        assert_eq!(conflict.kind(), ConflictKind::Writer);
        let holder = conflict
            .holder()
            .expect("a writer conflict names the writer");
        assert_eq!(holder.file(), file!());
        assert!(conflict.wanted().is_some());
    }

    /// Cross-thread, the holder must still name the writer rather than be lost:
    /// this is the case the atomic `holder` pointer exists for. Channels order
    /// the two threads explicitly - a sleep would be flaky under Miri, which
    /// interleaves far more aggressively.
    #[test]
    fn shared_writer_conflict_names_the_writer_across_threads() {
        use std::sync::mpsc;

        let cell: Shared<u32> = shared(0);
        let (holding_tx, holding_rx) = mpsc::channel::<()>();
        let (release_tx, release_rx) = mpsc::channel::<()>();

        std::thread::scope(|scope| {
            let writer = cell.clone();
            scope.spawn(move || {
                let guard = writer.borrow_mut();
                // The borrow is definitely held from here until `release_rx`.
                holding_tx.send(()).unwrap();
                release_rx.recv().unwrap();
                drop(guard);
            });

            // Only attempt the conflicting borrow once the writer holds it.
            holding_rx.recv().unwrap();
            let conflict = cell.try_borrow().unwrap_err();
            assert_eq!(conflict.kind(), ConflictKind::Writer);
            let holder = conflict.holder().expect("the writer's location survived");
            assert_eq!(
                holder.file(),
                file!(),
                "the holder must point at the writing thread's call site"
            );
            release_tx.send(()).unwrap();
        });
    }

    #[test]
    fn replace_with_computes_from_the_old_value() {
        let cell = local(5u32);
        let old = cell.replace_with(|v| {
            *v += 10;
            *v
        });
        // `f` runs first and mutates in place; the value returned is what was
        // in the slot when the replacement was written, i.e. what `f` left.
        assert_eq!(old, 15);
        assert_eq!(cell.with(|v| *v), 15);
    }

    #[test]
    fn replace_with_can_move_out_of_the_old_value() {
        // The reason `replace_with` exists: `replace` would force a clone.
        let cell = local(vec![1, 2, 3]);
        let old = cell.replace_with(core::mem::take);
        assert!(old.is_empty(), "f took the vector before the swap");
        assert_eq!(cell.with(Vec::clone), vec![1, 2, 3]);
    }

    #[test]
    fn replace_with_panics_when_borrowed_and_try_reports_it() {
        let cell = local(1u8);
        let held = cell.borrow();
        let conflict = match cell.try_replace_with(|v| *v + 1) {
            Ok(_) => panic!("a borrowed cell must refuse the replace"),
            Err((conflict, _f)) => conflict,
        };
        assert_eq!(conflict.kind(), ConflictKind::Readers);
        assert_eq!(conflict.wanted().map(|l| l.file()), Some(file!()));
        let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            cell.replace_with(|v| *v + 1)
        }));
        assert!(panic.is_err(), "the panicking form must panic");
        drop(held);
        // The refused closure was handed back and can be retried.
        let old = cell.try_replace_with(|v| *v + 1);
        assert_eq!(old.ok(), Some(1));
        assert_eq!(cell.with(|v| *v), 2);
    }

    #[test]
    fn swap_exchanges_two_local_cells() {
        let a = local(1u8);
        let b = local(2u8);
        a.swap(&b);
        assert_eq!(a.with(|v| *v), 2);
        assert_eq!(b.with(|v| *v), 1);
    }

    #[test]
    fn swap_exchanges_two_shared_cells() {
        let a = shared(String::from("x"));
        let b = shared(String::from("y"));
        a.swap(&b);
        assert_eq!(a.with(String::clone), "y");
        assert_eq!(b.with(String::clone), "x");
    }

    #[test]
    fn swap_works_across_handles_to_the_same_pair() {
        // A swap through clones still swaps the cells, not the handles.
        let a = local(1u8);
        let a2 = a.clone();
        let b = local(2u8);
        a2.swap(&b);
        assert_eq!(a.with(|v| *v), 2);
        assert_eq!(b.with(|v| *v), 1);
    }

    #[test]
    fn try_swap_reports_the_borrowed_cell() {
        let a = local(1u8);
        let b = local(2u8);
        let held = a.borrow();
        let conflict = a.try_swap(&b).unwrap_err();
        assert_eq!(conflict.kind(), ConflictKind::Readers);
        // The cell that was in the way is `a`, whose borrow site is here.
        assert_eq!(conflict.holder().map(|l| l.file()), Some(file!()));
        drop(held);
        assert!(a.try_swap(&b).is_ok());
        assert_eq!(a.with(|v| *v), 2);
    }

    #[test]
    fn try_swap_reports_a_borrowed_other() {
        // The conflict can be on the *other* cell too, and must still be
        // reported rather than panicking.
        let a = local(1u8);
        let b = local(2u8);
        let held = b.borrow_mut();
        let conflict = a.try_swap(&b).unwrap_err();
        assert_eq!(conflict.kind(), ConflictKind::Writer);
        drop(held);
        assert_eq!(
            a.with(|v| *v),
            1,
            "a refused swap must not have moved anything"
        );
    }

    #[test]
    fn swapping_a_cell_with_itself_is_refused() {
        // `Rc<RefCell<T>>::swap` on the same cell double-borrows and reports a
        // generic second-borrow. lite detects it and points at the caller.
        let a = local(1u8);
        let conflict = a.try_swap(&a).unwrap_err();
        assert_eq!(conflict.kind(), ConflictKind::Writer);
        assert_eq!(conflict.wanted().map(|l| l.file()), Some(file!()));
        // The self-swap through clones is the same cell, so also refused.
        let alias = a.clone();
        assert!(a.try_swap(&alias).is_err());
        assert_eq!(a.with(|v| *v), 1, "the value must be untouched");
        let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| a.swap(&a)));
        assert!(panic.is_err());
    }
}
