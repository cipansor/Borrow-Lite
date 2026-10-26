//! Soundness properties that the other suites do not cover: what happens when
//! user code panics while a borrow is alive, and whether the handle types cost
//! anything over the pointer they wrap.
//!
//! Panic safety is the interesting half. A cell that leaks a borrow when a
//! closure panics is not memory-unsafe, but it is permanently broken - every
//! later borrow fails - so it deserves a test rather than a comment.

use std::panic::{AssertUnwindSafe, catch_unwind};

use borrow_lite::{Local, Observable, Shared, shared};

/// A panic inside `with_mut` must not leave the cell borrowed.
#[test]
fn a_panic_inside_with_mut_releases_the_borrow() {
    let cell: Local<u8> = Local::new(0);

    let panic = catch_unwind(AssertUnwindSafe(|| {
        cell.with_mut(|_| panic!("boom"));
    }));
    assert!(panic.is_err(), "the panic should have propagated");

    // The guard was dropped while unwinding, so the cell is free again.
    assert!(cell.try_borrow_mut().is_ok(), "borrow leaked on panic");
    assert!(cell.try_borrow().is_ok(), "borrow leaked on panic");
}

/// The same for an explicitly held guard that is alive when a panic unwinds.
#[test]
fn a_panic_while_a_guard_is_alive_releases_it() {
    let cell: Shared<u8> = shared(0);

    let panic = catch_unwind(AssertUnwindSafe(|| {
        let _guard = cell.borrow_mut();
        panic!("boom");
    }));
    assert!(panic.is_err());
    assert!(cell.try_borrow_mut().is_ok(), "borrow leaked on panic");
}

/// Several guards held across a panic must all be released, not just the last.
#[test]
fn a_panic_releases_every_live_reader() {
    let cell: Local<u8> = Local::new(0);

    let panic = catch_unwind(AssertUnwindSafe(|| {
        let _first = cell.borrow();
        let _second = cell.borrow();
        let _third = cell.borrow();
        panic!("boom");
    }));
    assert!(panic.is_err());
    assert!(cell.try_borrow_mut().is_ok(), "readers leaked on panic");
}

/// `Observable::notify` documents that a panicking observer leaves the cell
/// usable. That is the only claim in the crate about state after a panic, so it
/// is worth pinning down.
#[test]
fn a_panicking_observer_leaves_the_cell_usable() {
    let source = Observable::new(0u8);
    // The observer captures nothing, so there is no reference cycle here and
    // `clear_observers` below is only tidying up.
    source.observe(|_| panic!("observer boom"));

    let panic = catch_unwind(AssertUnwindSafe(|| source.notify()));
    assert!(panic.is_err(), "the observer panic should propagate");

    // Documented behaviour: the observer list and the notifying flag are
    // restored on the way out. A read must work, and the observer must still be
    // registered - which is what a second `notify` panicking proves.
    assert_eq!(source.with(|v| *v), 0, "cell unusable after a panic");
    assert_eq!(
        source.observer_count(),
        1,
        "the observer was dropped, not restored"
    );

    let again = catch_unwind(AssertUnwindSafe(|| source.notify()));
    assert!(again.is_err(), "the observer was lost, not restored");

    // And the notifying flag was cleared, so the value can still be updated
    // once the panicking observer is gone.
    source.clear_observers();
    assert!(
        source.try_update(|v| *v += 1).is_ok(),
        "cell left notifying"
    );
    assert_eq!(source.with(|v| *v), 1);
}

/// A panic in the update closure, before any observer runs, must still leave
/// the cell usable.
#[test]
fn a_panic_inside_update_leaves_the_cell_usable() {
    let source = Observable::new(0u8);

    let panic = catch_unwind(AssertUnwindSafe(|| {
        source.update(|_| panic!("update boom"));
    }));
    assert!(panic.is_err());
    assert!(source.try_update(|v| *v += 1).is_ok(), "cell left borrowed");
    assert_eq!(source.with(|v| *v), 1);
}

/// The handles are one pointer wide, like the `Rc`/`Arc` they replace. This is
/// a compile-time property, so a `const` assertion is the honest test: it
/// cannot silently start passing.
const _: () = {
    assert!(core::mem::size_of::<Local<u64>>() == core::mem::size_of::<usize>());
    assert!(core::mem::size_of::<Shared<u64>>() == core::mem::size_of::<usize>());
    // The single-threaded handle must not have grown a `Send` marker either.
    assert!(core::mem::size_of::<Local<u64>>() == core::mem::size_of::<*const u64>());
};

/// The exact byte counts the README quotes, pinned so they cannot drift.
///
/// These are the numbers `examples/sizes.rs` prints and the README repeats, and
/// a printed number cannot fail a build - if `LocalState` grows a field the cell
/// would get wider and the prose would quietly become wrong. A `const`
/// assertion turns that into a compile error.
///
/// They are 64-bit layout facts rather than portable guarantees, so they are
/// gated on the pointer width; the guard sizes depend on the handle, the raw
/// pointer and the recorder flag, which is what makes a `ReadGuard` wider than
/// a `WriteGuard`.
///
/// The `Inline` size is the whole point of the `diagnostics` feature, so it is
/// asserted for *both* modes here rather than left to the default build:
/// `Inline<u64>` is 24 bytes with the holder recorded and 16 without it - the
/// same 16 as `RefCell<u64>`, which is the claim the feature exists to make.
/// The `ReadGuard` shrinks with it (the recorder flag is dropped), so its size
/// is asserted per mode too.
#[cfg(target_pointer_width = "64")]
const _: () = {
    use borrow_lite::Inline;
    use borrow_lite::state::LocalState;

    #[cfg(feature = "diagnostics")]
    assert!(core::mem::size_of::<Inline<u64>>() == 24);
    #[cfg(not(feature = "diagnostics"))]
    assert!(core::mem::size_of::<Inline<u64>>() == 16);
    assert!(core::mem::size_of::<std::cell::RefCell<u64>>() == 16);

    #[cfg(feature = "diagnostics")]
    assert!(core::mem::size_of::<borrow_lite::ReadGuard<'static, u64, LocalState>>() == 24);
    #[cfg(not(feature = "diagnostics"))]
    assert!(core::mem::size_of::<borrow_lite::ReadGuard<'static, u64, LocalState>>() == 16);
    assert!(core::mem::size_of::<borrow_lite::WriteGuard<'static, u64, LocalState>>() == 16);
};

/// The regression for the bug this file found: a panicking observer used to
/// drop the *rest* of the notification round, silently losing every observer
/// that had not run yet.
///
/// `a_panicking_observer_leaves_the_cell_usable` only ever registers one
/// observer, so it cannot see that. This one registers a panicker first and two
/// well-behaved observers after it, then removes the panicker and checks that
/// both survivors still fire - which is only possible if the round was restored
/// rather than dropped.
#[test]
fn a_panicking_observer_does_not_take_the_rest_of_the_round_with_it() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    let source = Observable::new(0u8);
    let after_first = Arc::new(AtomicUsize::new(0));
    let after_second = Arc::new(AtomicUsize::new(0));

    let panicker = source.observe(|_| panic!("observer boom"));

    let first = after_first.clone();
    source.observe(move |_| {
        first.fetch_add(1, Ordering::SeqCst);
    });

    let second = after_second.clone();
    source.observe(move |_| {
        second.fetch_add(1, Ordering::SeqCst);
    });

    let panic = catch_unwind(AssertUnwindSafe(|| source.notify()));
    assert!(panic.is_err(), "the observer panic should propagate");

    // All three are still registered: the interrupted round was put back, not
    // dropped.
    assert_eq!(
        source.observer_count(),
        3,
        "a panic during notification lost observers"
    );

    // Remove the panicker, then notify cleanly. Both survivors must fire: they
    // are the observers the old bug used to drop.
    assert!(source.unobserve(panicker), "the panicker was lost");
    source.notify();
    assert_eq!(
        after_first.load(Ordering::SeqCst),
        1,
        "the observer after the panicker was lost"
    );
    assert_eq!(
        after_second.load(Ordering::SeqCst),
        1,
        "a later observer was lost"
    );

    source.clear_observers();
}

/// The `try_*` forms convert a *conflict* into an `Err`, but they do not catch
/// a panic raised by an observer - that is the caller's own code failing, not a
/// conflict. This is documented on `Observable`, and it is the kind of claim
/// that quietly drifts, so it is pinned here: the panic comes through, and the
/// cell is still usable afterwards because the guard restores the list on the
/// way out.
mod try_forms_do_not_swallow_observer_panics {
    use std::panic::{AssertUnwindSafe, catch_unwind};

    use borrow_lite::Observable;

    #[test]
    fn a_panicking_observer_propagates_out_of_every_try_form() {
        // try_notify
        let notify = Observable::new(0i32);
        notify.observe(|_| panic!("observer boom"));
        assert!(
            catch_unwind(AssertUnwindSafe(|| {
                let _ = notify.try_notify();
            }))
            .is_err(),
            "try_notify must not swallow an observer panic"
        );

        // try_update
        let update = Observable::new(0i32);
        update.observe(|_| panic!("observer boom"));
        assert!(
            catch_unwind(AssertUnwindSafe(|| {
                let _ = update.try_update(|v| *v += 1);
            }))
            .is_err(),
            "try_update must not swallow an observer panic"
        );

        // try_replace
        let replace = Observable::new(0i32);
        replace.observe(|_| panic!("observer boom"));
        assert!(
            catch_unwind(AssertUnwindSafe(|| {
                let _ = replace.try_replace(5);
            }))
            .is_err(),
            "try_replace must not swallow an observer panic"
        );

        // try_take
        let take = Observable::new(0i32);
        take.observe(|_| panic!("observer boom"));
        assert!(
            catch_unwind(AssertUnwindSafe(|| {
                let _ = take.try_take();
            }))
            .is_err(),
            "try_take must not swallow an observer panic"
        );

        // All four left the cell usable: remove the panicker and use it.
        for cell in [&notify, &update, &replace, &take] {
            cell.clear_observers();
            assert!(
                cell.try_update(|v| *v += 1).is_ok(),
                "a try_* panic left the cell unusable"
            );
        }
    }

    /// The contrast that gives the rule its meaning: a *conflict* is turned
    /// into an `Err` by the same methods, with no panic.
    #[test]
    fn a_conflict_is_still_an_err_not_a_panic() {
        let value = Observable::new(0i32);
        value.observe(|_| {});
        let inner = value.clone();
        let mut refused = None;
        // Inside `update` the value is exclusively borrowed, so notifying is a
        // conflict - and `try_notify` returns it rather than panicking.
        value.update(|_| {
            refused = Some(inner.try_notify());
        });
        assert!(
            matches!(refused, Some(Err(_))),
            "a conflict must be an Err, not a panic"
        );
        // The list is untouched by the refusal.
        assert_eq!(value.observer_count(), 1);
        value.clear_observers();
    }
}

/// `ReadGuard::map` and `WriteGuard::map` run the narrowing closure *before*
/// consuming the guard, so a panic in that closure drops the guard and releases
/// the borrow. A leak here would permanently break the cell, so it is worth a
/// test rather than a comment.
mod map_is_panic_safe {
    use std::panic::{AssertUnwindSafe, catch_unwind};

    use borrow_lite::{Local, ReadGuard, Shared, WriteGuard, shared};

    #[test]
    fn a_panic_in_read_guard_map_releases_the_borrow() {
        let cell: Local<(u32, u32)> = Local::new((1, 2));
        let panic = catch_unwind(AssertUnwindSafe(|| {
            let _ = ReadGuard::map(cell.borrow(), |_t| -> &str { panic!("boom") });
        }));
        assert!(panic.is_err());
        assert!(
            cell.try_borrow_mut().is_ok(),
            "a panic in ReadGuard::map leaked the shared borrow"
        );
    }

    #[test]
    fn a_panic_in_write_guard_map_releases_the_borrow() {
        let cell: Local<(u32, u32)> = Local::new((1, 2));
        let panic = catch_unwind(AssertUnwindSafe(|| {
            let _ = WriteGuard::map(cell.borrow_mut(), |_t| -> &mut u32 { panic!("boom") });
        }));
        assert!(panic.is_err());
        assert!(
            cell.try_borrow_mut().is_ok(),
            "a panic in WriteGuard::map leaked the exclusive borrow"
        );
        assert_eq!(cell.with(|t| *t), (1, 2), "the value was disturbed");
    }

    #[test]
    fn a_panic_in_write_guard_map_on_a_shared_cell_releases_the_borrow() {
        let cell: Shared<(u32, u32)> = shared((1, 2));
        let panic = catch_unwind(AssertUnwindSafe(|| {
            let _: WriteGuard<'_, u32, _> =
                WriteGuard::map(cell.borrow_mut(), |_t| -> &mut u32 { panic!("boom") });
        }));
        assert!(panic.is_err());
        assert!(
            cell.try_borrow_mut().is_ok(),
            "a panic in WriteGuard::map leaked the exclusive borrow"
        );
    }
}
