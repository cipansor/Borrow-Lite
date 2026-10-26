//! The observer pattern, including the two ways it usually breaks.

use borrow_lite::{Conflict, Observable};

#[test]
fn observers_are_called_on_update() {
    let source = Observable::new(0i32);
    let log = Observable::new(Vec::new());
    let log_in_observer = log.clone();
    let id = source.observe(move |v| log_in_observer.update(|l| l.push(*v)));
    assert_eq!(source.observer_count(), 1);

    source.update(|v| *v += 1);
    source.update(|v| *v += 1);
    assert_eq!(log.with(|l| l.clone()), vec![1, 2]);

    assert!(source.unobserve(id));
    source.update(|v| *v += 1);
    assert_eq!(log.with(|l| l.clone()), vec![1, 2]);
    assert!(!source.unobserve(id));
}

#[test]
fn set_and_notify_are_explicit() {
    let source = Observable::new(0i32);
    let calls = Observable::new(0usize);
    let calls_in_observer = calls.clone();
    source.observe(move |_| calls_in_observer.update(|c| *c += 1));

    source.set(5);
    assert_eq!(source.with(|v| *v), 5);
    source.notify();
    source.notify();
    assert_eq!(calls.with(|c| *c), 3);
}

#[test]
fn an_observer_may_subscribe_another_and_the_new_one_still_sees_the_value() {
    let source = Observable::new(1u8);
    let log = Observable::new(Vec::new());

    let inner_log = log.clone();
    let source_for_inner = source.clone();
    source.observe(move |_| {
        let inner_log = inner_log.clone();
        source_for_inner.observe(move |v| inner_log.update(|l| l.push(*v)));
    });

    source.update(|v| *v = 2);
    assert_eq!(log.with(|l| l.clone()), vec![2]);

    // and the newly added observer keeps working afterwards
    source.update(|v| *v = 3);
    assert_eq!(log.with(|l| l.clone()), vec![2, 3, 3]);

    source.clear_observers();
}

#[test]
fn an_observer_may_unsubscribe_itself() {
    let source = Observable::new(0u8);
    let calls = Observable::new(0usize);
    let calls_in_observer = calls.clone();
    let source_in_observer = source.clone();

    // The observer needs its own id, so the id is smuggled in through a cell
    // that the closure reads on first call.
    let my_id = Observable::new(0u64);
    let id_slot = my_id.clone();
    let removed = Observable::new(false);
    let removed_in_observer = removed.clone();
    source.observe(move |_| {
        calls_in_observer.update(|c| *c += 1);
        let id = id_slot.with(|id| *id);
        let gone = source_in_observer.unobserve(id);
        removed_in_observer.update(|flag| *flag = gone);
    });
    let registered = 0;
    my_id.set(registered);

    source.update(|v| *v += 1);
    source.update(|v| *v += 1);
    // ran once, then removed itself; the second update has nobody to call
    assert_eq!(calls.with(|c| *c), 1);
    assert_eq!(source.observer_count(), 0);
    assert!(removed.with(|flag| *flag));
}

#[test]
fn re_entrant_update_is_a_conflict() {
    let source = Observable::new(0i32);
    let outcome = Observable::new(None);
    let source_in_observer = source.clone();
    let outcome_in_observer = outcome.clone();

    source.observe(move |_| {
        let result: Result<(), Conflict> = source_in_observer.try_update(|v| *v += 1);
        outcome_in_observer.update(|slot| *slot = Some(result));
    });

    source.try_update(|v| *v += 1).unwrap();
    let reported = outcome.with(|o| *o).expect("observer reported");
    assert_eq!(
        reported.unwrap_err().kind(),
        borrow_lite::ConflictKind::Readers
    );
    // the outer update still applied exactly once
    assert_eq!(source.with(|v| *v), 1);

    // The observer captures `source`, so the two hold each other alive.
    source.clear_observers();
}

#[test]
fn nested_notify_does_not_recurse() {
    let source = Observable::new(0u8);
    let calls = Observable::new(0usize);
    let calls_in_observer = calls.clone();
    let source_in_observer = source.clone();

    source.observe(move |_| {
        calls_in_observer.update(|c| *c += 1);
        source_in_observer.notify();
    });

    source.notify();
    assert_eq!(calls.with(|c| *c), 1);

    source.clear_observers();
}

#[test]
fn update_from_an_observer_panics_with_a_located_message() {
    let source = Observable::new(0i32);
    let source_in_observer = source.clone();
    source.observe(move |_| source_in_observer.update(|v| *v += 1));

    let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        source.update(|v| *v += 1);
    }))
    .expect_err("re-entrant update must panic");

    let message = panic.downcast_ref::<String>().unwrap();
    assert!(message.contains("already immutably borrowed"), "{message}");
    assert!(message.contains("held at"), "{message}");

    source.clear_observers();
}

#[test]
fn observers_see_a_stable_value() {
    let source = Observable::new(vec![1u8, 2]);
    let seen = Observable::new(Vec::new());
    let seen_in_observer = seen.clone();
    source.observe(move |v| seen_in_observer.update(|s| s.push(v.clone())));

    source.update(|v| v.push(3));
    source.update(|v| v.push(4));
    assert_eq!(
        seen.with(|s| s.clone()),
        vec![vec![1, 2, 3], vec![1, 2, 3, 4]]
    );
}

#[cfg(test)]
mod tests {
    use borrow_lite::{MAX_NOTIFICATION_ROUNDS, Observable};

    #[test]
    fn notification_handles_a_round_of_new_subscribers() {
        // Subscribers added during a notification are picked up by the same
        // notification, so an observer that subscribes another observer still
        // gets the current value delivered to it.
        let source = Observable::new(0u8);
        let log = Observable::new(Vec::new());

        let inner_log = log.clone();
        let source_for_inner = source.clone();
        source.observe(move |v| {
            inner_log.update(|l| l.push(*v));
            let inner_log = inner_log.clone();
            source_for_inner.observe(move |v| inner_log.update(|l| l.push(*v)));
        });

        source.notify();
        assert_eq!(log.with(|l| l.clone()), vec![0, 0]);

        // The observer captures `source`, so the two keep each other alive;
        // clearing the list breaks that cycle.
        source.clear_observers();
    }

    /// A chain of observers, each of which subscribes the next one.
    struct Replicator {
        source: Observable<u8>,
        calls: Observable<usize>,
    }

    impl Replicator {
        fn observer(&self) -> impl FnMut(&u8) + 'static {
            let source = self.source.clone();
            let calls = self.calls.clone();
            move |_| {
                calls.update(|n| *n += 1);
                source.observe(
                    Replicator {
                        source: source.clone(),
                        calls: calls.clone(),
                    }
                    .observer(),
                );
            }
        }
    }

    #[test]
    fn the_round_cap_stops_an_observer_that_always_resubscribes() {
        let source = Observable::new(0u8);
        let calls = Observable::new(0usize);
        source.observe(
            Replicator {
                source: source.clone(),
                calls: calls.clone(),
            }
            .observer(),
        );

        source.notify();

        // One observer runs per round, and the cap bounds the rounds: 32 runs,
        // each of which subscribed one more observer. None of them were
        // removed, so all 33 are still subscribed for the next notification.
        assert_eq!(calls.with(|n| *n), MAX_NOTIFICATION_ROUNDS);
        assert_eq!(source.observer_count(), MAX_NOTIFICATION_ROUNDS + 1);

        // Every replicator captures `source`; drop them to break the cycle.
        source.clear_observers();
    }
}

/// `set` is `update` with a whole value, and it notifies.
#[test]
fn set_replaces_the_value_and_notifies() {
    let source = Observable::new(0u8);
    let seen = Observable::new(Vec::new());
    let seen_in_observer = seen.clone();
    source.observe(move |v| seen_in_observer.update(|s| s.push(*v)));

    source.set(7);
    assert_eq!(source.with(|v| *v), 7);
    assert_eq!(seen.with(|s| s.clone()), vec![7]);
}

/// `try_set` had no test at all: it is the non-panicking form of `set`, and the
/// one an observer would have to use. Both outcomes matter.
#[test]
fn try_set_succeeds_normally_and_reports_a_conflict_from_an_observer() {
    let source = Observable::new(0u8);

    // The ordinary path: it writes, notifies, and reports success.
    assert!(source.try_set(3).is_ok());
    assert_eq!(source.with(|v| *v), 3);

    // The re-entrant path: an observer calling `try_set` must get an error, not
    // a panic and not a silent double write.
    let outcome = Observable::new(None);
    let inner = source.clone();
    let outcome_in_observer = outcome.clone();
    source.observe(move |_| {
        outcome_in_observer.update(|slot| *slot = Some(inner.try_set(99).is_err()));
    });

    assert!(source.try_set(4).is_ok());
    assert_eq!(
        outcome.with(|o| *o),
        Some(true),
        "try_set from an observer should report a conflict"
    );
    // The observer's refused write must not have landed.
    assert_eq!(source.with(|v| *v), 4, "a refused try_set still wrote");

    source.clear_observers();
}

/// The panicking counterpart: `set` from inside an observer panics, exactly as
/// `update` does, and the panic is the documented message rather than a borrow
/// error from somewhere deeper.
#[test]
fn set_from_an_observer_panics_like_update() {
    let source = Observable::new(0u8);
    let inner = source.clone();
    source.observe(move |_| {
        inner.set(99);
    });

    let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        source.set(1);
    }));
    assert!(panic.is_err(), "set from an observer must panic");

    // The cell survives, and the refused write did not land.
    source.clear_observers();
    assert_eq!(source.with(|v| *v), 1);
    assert!(source.try_set(2).is_ok());
}

/// An observer is handed the value *after* the write, so it can log what
/// changed rather than what used to be there.
#[test]
fn an_observer_sees_the_value_after_the_update() {
    let source = Observable::new(0u8);
    let seen = Observable::new(Vec::new());
    let seen_in_observer = seen.clone();
    source.observe(move |v| seen_in_observer.update(|s| s.push(*v)));

    source.update(|v| *v += 10);
    source.update(|v| *v += 10);
    assert_eq!(seen.with(|s| s.clone()), vec![10, 20]);
}

/// Observers run in subscription order, which is what makes an observer list a
/// usable pipeline.
#[test]
fn observers_run_in_subscription_order() {
    let source = Observable::new(0u8);
    let order = Observable::new(Vec::new());

    for index in 0..3u8 {
        let order_in_observer = order.clone();
        source.observe(move |_| order_in_observer.update(|o| o.push(index)));
    }

    source.notify();
    assert_eq!(order.with(|o| o.clone()), vec![0, 1, 2]);
}

/// A clone is the same cell, not a copy: writing through the clone is visible
/// through the original, and the observers are shared too.
#[test]
fn a_clone_is_the_same_cell() {
    let source = Observable::new(0u8);
    let calls = Observable::new(0usize);
    let calls_in_observer = calls.clone();
    source.observe(move |_| calls_in_observer.update(|c| *c += 1));

    let clone = source.clone();
    clone.update(|v| *v = 5);

    assert_eq!(source.with(|v| *v), 5, "the clone did not share the value");
    assert_eq!(source.observer_count(), 1);
    assert_eq!(
        calls.with(|c| *c),
        1,
        "an update through the clone did not notify the shared observers"
    );
}

/// Reading through `with` is a plain borrow and must not notify: only `update`,
/// `set`, `notify` and their `try_` twins do. Getting this backwards would make
/// every read fire the observers, which is the usual way an observer loop
/// starts.
///
/// `Observable` deliberately has no `with_mut`/`try_with`: mutating without
/// notifying is exactly the bug the type exists to prevent, so the only way to
/// change the value is through a method that notifies.
#[test]
fn a_read_does_not_notify() {
    let source = Observable::new(0u8);
    let calls = Observable::new(0usize);
    let calls_in_observer = calls.clone();
    source.observe(move |_| calls_in_observer.update(|c| *c += 1));

    for _ in 0..5 {
        source.with(|v| assert_eq!(*v, 0));
    }
    assert_eq!(calls.with(|c| *c), 0, "a read notified the observers");

    // The only mutating path notifies.
    source.update(|v| *v += 1);
    assert_eq!(calls.with(|c| *c), 1);
}

/// `get`/`try_get` are the closure-free spelling of `with`, so they must agree
/// with it on every path, including the re-entrant one.
#[test]
fn get_matches_with_and_does_not_notify() {
    let source = Observable::new(vec![1i32, 2, 3]);
    let calls = Observable::new(0usize);
    let calls_in_observer = calls.clone();
    source.observe(move |_| calls_in_observer.update(|c| *c += 1));

    assert_eq!(source.get(), source.with(|v| v.clone()));
    assert_eq!(calls.with(|c| *c), 0, "a read notified the observers");

    // A clone is the same cell, so a write through one handle is visible.
    let alias = source.clone();
    alias.set(vec![4]);
    assert_eq!(source.get(), vec![4]);
    assert_eq!(calls.with(|c| *c), 1);

    // Re-entrant read: `get` panics like `with`, `try_get` reports.
    let idle = Observable::new(0u8);
    assert_eq!(idle.try_get(), Ok(0));
    let inner = idle.clone();
    let outcome = Observable::new(None);
    let outcome_in_update = outcome.clone();
    idle.update(|_| outcome_in_update.set(Some(inner.try_get().is_err())));
    assert_eq!(outcome.get(), Some(true));
}

/// `unobserve` from inside an observer may target a *different* observer, not
/// just the running one, and the second call for the same id must report
/// `false`.
///
/// The documented timing is the subtle part, and it is easy to get wrong: the
/// removal is *queued* and applied when the notification finishes, so a peer
/// removed mid-round still runs in that round. The list is never mutated
/// underneath the iteration. Only self-removal was covered before, and it takes
/// a different branch (the removed observer is the one executing, so it is
/// already out of the round's list).
#[test]
fn an_observer_may_queue_a_peer_for_removal() {
    let source = Observable::new(0u8);
    let first_calls = Observable::new(0usize);
    let peer_calls = Observable::new(0usize);

    let first_calls_in = first_calls.clone();
    let peer_calls_in = peer_calls.clone();
    let source_in_first = source.clone();

    let peer_id = Observable::new(0u64);
    let peer_id_in_first = peer_id.clone();
    let queued = Observable::new((false, false));
    let queued_in_first = queued.clone();

    // Registered first, so it runs first and can cancel the peer.
    source.observe(move |_| {
        first_calls_in.update(|c| *c += 1);
        let id = peer_id_in_first.with(|id| *id);
        let accepted = source_in_first.unobserve(id);
        // A second call for the same id must report `false`: it is already
        // queued for removal.
        let again = source_in_first.unobserve(id);
        queued_in_first.update(|slot| *slot = (accepted, again));
    });

    // Registered second, so it is the peer the first observer cancels.
    let peer_id_value = source.observe(move |_| peer_calls_in.update(|c| *c += 1));
    peer_id.set(peer_id_value);

    source.notify();

    assert_eq!(
        queued.with(|slot| *slot),
        (true, false),
        "queuing a removal twice should accept once, then report false"
    );
    assert_eq!(
        first_calls.with(|c| *c),
        1,
        "the cancelling observer did not run"
    );
    // Queued, not immediate: the peer still runs in the round that removed it.
    assert_eq!(
        peer_calls.with(|c| *c),
        1,
        "a peer queued for removal should still run in the current round"
    );
    // The peer is gone, but the observer that cancelled it is still
    // subscribed.
    assert_eq!(
        source.observer_count(),
        1,
        "the peer should be gone once the round ends"
    );

    // So the next notification calls the survivor once more, and not the peer.
    source.notify();
    assert_eq!(peer_calls.with(|c| *c), 1, "the removed peer ran again");
    assert_eq!(
        first_calls.with(|c| *c),
        2,
        "the survivor did not run again"
    );

    // The survivor captures `source`, so the two hold each other alive. Miri
    // reports that as a leak, which is why every test here whose observer
    // captures its own source breaks the cycle before returning.
    source.clear_observers();
}

/// An observer may *read* the source it is being notified about, and sees the
/// stable, post-update value.
///
/// `notify` holds a shared borrow of the value for the duration of the round
/// (that is what makes the value stable for every observer), so a nested shared
/// borrow from inside an observer succeeds. Only mutation is refused. The suite
/// tested the refusal but never the read that is supposed to be allowed.
#[test]
fn an_observer_may_read_the_source_it_is_notified_about() {
    let source = Observable::new(0u64);
    let seen = Observable::new(Vec::new());
    let seen_in_observer = seen.clone();
    let source_in_observer = source.clone();

    source.observe(move |value| {
        let via_with = source_in_observer.with(|v| *v);
        seen_in_observer.update(|s| s.push((*value, via_with)));
    });

    source.update(|v| *v += 1);
    source.update(|v| *v += 1);

    // The value handed to the observer and the one it reads itself must agree,
    // and both must be the value *after* the update.
    assert_eq!(seen.with(|s| s.clone()), vec![(1, 1), (2, 2)]);

    source.clear_observers();
}

/// `Observable` had `with`/`update`/`set` but no way to reach the value without
/// a borrow flag, no way to take it back out, and no way to consume the cell -
/// the same lifetime gap `Local`/`Shared` had. The contract is the one the
/// underlying cell already promises, plus notification where the value changes.
#[test]
fn the_value_lifetime_paths_exist_and_match_the_cell() {
    // `get_mut` needs a unique handle, like `Local::get_mut`.
    let mut temp = Observable::new(vec![20i32]);
    temp.get_mut().unwrap().push(21);
    assert_eq!(temp.with(|v| v.clone()), vec![20, 21]);
    let other = temp.clone();
    assert!(temp.get_mut().is_none());
    drop(other);
    assert!(temp.get_mut().is_some());

    // `take` empties the value *and* notifies, unlike a raw `Local::take`.
    let temp = Observable::new(vec![1i32, 2]);
    let seen = Observable::new(Vec::new());
    let seen_in_observer = seen.clone();
    temp.observe(move |v| seen_in_observer.update(|s| s.push(v.len())));
    assert_eq!(temp.take(), vec![1, 2]);
    assert!(temp.with(Vec::is_empty));
    assert_eq!(seen.with(|s| s.clone()), vec![0]); // observers saw the empty value

    // `into_inner` consumes the cell; `Err` while a clone exists.
    let temp = Observable::new(String::from("done"));
    let other = temp.clone();
    let temp = temp.into_inner().unwrap_err();
    drop(other);
    assert_eq!(temp.into_inner().unwrap(), "done");
}

/// The error path of `into_inner` must not split the observer list. A clone
/// shares the subscribers; if a failed `into_inner` dropped and rebuilt the
/// list, the clone's observers would silently vanish.
#[test]
fn a_failed_into_inner_keeps_the_observers_intact() {
    let temp = Observable::new(0i32);
    let seen = Observable::new(Vec::new());
    let seen_in_observer = seen.clone();
    temp.observe(move |v| seen_in_observer.update(|s| s.push(*v)));

    let other = temp.clone();
    let temp = temp.into_inner().unwrap_err();
    assert_eq!(temp.observer_count(), 1);

    // The clone still notifies, and so does the returned handle.
    other.update(|v| *v += 1);
    assert_eq!(seen.with(|s| s.clone()), vec![1]);
    temp.update(|v| *v += 1);
    assert_eq!(seen.with(|s| s.clone()), vec![1, 2]);

    drop(other);
    assert_eq!(temp.into_inner().unwrap(), 2);
}

/// `take` must refuse a live borrow with a located panic, so the value is never
/// lost to a conflict - the `RefCell::take` contract, matching `Inner::take`.
#[test]
fn take_panics_while_the_value_is_borrowed_and_keeps_it() {
    let temp = Observable::new(vec![7i32]);
    let outcome = temp.with(|_| {
        // A read borrow is held here, so `take` must refuse it.
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| temp.take()))
    });
    assert!(
        outcome.is_err(),
        "take must panic while the value is borrowed"
    );
    assert_eq!(temp.with(|v| v.clone()), vec![7]); // and must not lose it
}

/// `Observable::try_take` is the non-panicking `take`. On a conflict the value
/// must be untouched and the observers must not be notified - a refused take
/// that still told everyone "the value changed" would be a silent lie.
#[test]
fn try_take_reports_a_conflict_without_notifying_or_emptying() {
    let temp = Observable::new(vec![1i32, 2]);
    let fired = Observable::new(0usize);
    let fired_in_observer = fired.clone();
    temp.observe(move |_| fired_in_observer.update(|n| *n += 1));

    let inner = temp.clone();
    let outcome = Observable::new(None);
    let outcome_in_observer = outcome.clone();
    temp.observe(move |_| {
        outcome_in_observer.update(|o| *o = Some(inner.try_take()));
    });

    let before = fired.with(|n| *n);
    temp.notify(); // the re-entrant try_take is refused
    assert_eq!(
        outcome.with(|o| o.as_ref().map(Result::is_err)),
        Some(true),
        "try_take from an observer must be refused"
    );
    assert_eq!(
        temp.with(|v| v.len()),
        2,
        "a refused try_take emptied the value"
    );
    // Both observers fired for the outer `notify`, so the count is stable; the
    // refused inner take must not have added a notification.
    assert_eq!(fired.with(|n| *n), before + 1);

    temp.clear_observers();

    // Idle: it succeeds and matches `take`.
    assert_eq!(temp.try_take().unwrap(), vec![1, 2]);
    assert!(temp.with(Vec::is_empty));
}

/// The conflict inside a notification is a no-op for `notify` (nested calls are
/// ignored), so it cannot tell "notify on conflict" from "do not". This holds a
/// plain read borrow *outside* a notification and calls `try_take` within it:
/// the refusal must not fire any observer.
#[test]
fn a_try_take_refused_outside_a_notification_does_not_notify() {
    let temp = Observable::new(vec![1i32, 2]);
    let fired = Observable::new(0usize);
    let fired_in_observer = fired.clone();
    temp.observe(move |_| fired_in_observer.update(|n| *n += 1));

    let refused = temp.with(|_| temp.try_take().is_err());
    assert!(refused, "try_take under a live read borrow must be refused");
    assert_eq!(fired.with(|n| *n), 0, "a refused try_take must not notify");
    assert_eq!(
        temp.with(|v| v.len()),
        2,
        "a refused try_take emptied the value"
    );

    temp.clear_observers();
}

/// `Observable::with` panics when the value is exclusively borrowed - which is
/// exactly the state inside an `update` closure - and reading from inside an
/// observer is fine, because the value is only shared-borrowed there. `try_with`
/// is the twin that reports the exclusive-borrow case instead.
#[test]
fn try_with_reports_the_exclusive_borrow_that_with_panics_on() {
    let value = Observable::new(0i32);
    let inner = value.clone();
    let outcome = Observable::new(None);
    let outcome_in_update = outcome.clone();

    // Inside an `update` closure the value is exclusively borrowed.
    value.update(|_| {
        let read = inner.try_with(|v| *v);
        outcome_in_update.update(|slot| *slot = Some(read.is_err()));
    });
    assert_eq!(
        outcome.with(|o| *o),
        Some(true),
        "a re-entrant read inside `update` must be refused"
    );

    // `with` really would panic there - the pair only matters if it does.
    let panicked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        value.update(|_| {
            let _ = inner.with(|v| *v);
        });
    }));
    assert!(
        panicked.is_err(),
        "`with` must panic inside an update closure"
    );

    // Inside an observer the value is shared, so a read is allowed.
    let seen = Observable::new(None);
    let seen_in_observer = seen.clone();
    let reader = value.clone();
    value.observe(move |_| {
        let read = reader.try_with(|v| *v);
        seen_in_observer.update(|slot| *slot = Some(read));
    });
    value.notify();
    assert_eq!(
        seen.with(|o| *o),
        Some(Ok(0)),
        "reading from inside an observer must succeed"
    );
    value.clear_observers();

    // Idle: it succeeds.
    assert_eq!(value.try_with(|v| *v), Ok(0));
}

/// `Observable::replace` is `set` that also hands the old value back, and its
/// `try_` twin refuses without notifying or losing the unwritten value - the
/// same contract `Inner::replace` has, plus the notification.
#[test]
fn replace_returns_the_old_value_and_notifies_like_set() {
    let temp = Observable::new(vec![20i32]);
    let seen = Observable::new(Vec::new());
    let seen_in_observer = seen.clone();
    temp.observe(move |v| seen_in_observer.update(|s| s.push(v[0])));

    let old = temp.replace(vec![21]);
    assert_eq!(old, vec![20]);
    assert_eq!(
        seen.with(|s| s.clone()),
        vec![21],
        "observers must be notified"
    );

    // A conflict hands the value back and does not notify.
    let fired = Observable::new(0usize);
    let fired_in_observer = fired.clone();
    temp.observe(move |_| fired_in_observer.update(|n| *n += 1));
    let before = fired.with(|n| *n);

    let inner = temp.clone();
    let refused = Observable::new(None);
    let refused_in_update = refused.clone();
    temp.update(|_| {
        // Inside `update` the value is exclusively borrowed.
        let outcome = inner.try_replace(vec![99]);
        refused_in_update.update(|slot| {
            *slot = Some(outcome.map_err(|(conflict, value)| (conflict.kind(), value)));
        });
    });
    assert_eq!(
        refused.with(|o| o.clone()),
        Some(Err((borrow_lite::ConflictKind::Writer, vec![99]))),
        "a refused replace must report the conflict and hand the value back"
    );
    assert_eq!(
        temp.with(|v| v.clone()),
        vec![21],
        "the value must be untouched"
    );
    // Only the outer `update` notified; the refused replace added nothing.
    assert_eq!(fired.with(|n| *n), before + 1);
    temp.clear_observers();
}

/// `clear_observers` from inside an observer must actually clear. The observers
/// of the round in progress are held by the notification guard, not by the
/// subscription list, so clearing the list alone left them alive and the guard
/// put them back - the count stayed at 2 and they fired again on the next
/// notification. The clear now records an id boundary the guard honours.
#[test]
fn clear_observers_from_inside_an_observer_clears_the_list() {
    let value = Observable::new(0i32);
    let fired = Observable::new(0usize);
    let counts = Observable::new(Vec::new());

    let handle = value.clone();
    value.observe(move |_| handle.clear_observers());
    // The second observer runs after the clear (the round finishes), so it can
    // check that the count already reflects the clear *during* the round.
    let fired_in_second = fired.clone();
    let counts_in_second = counts.clone();
    let count_handle = value.clone();
    value.observe(move |_| {
        counts_in_second.update(|c| c.push(count_handle.observer_count()));
        fired_in_second.update(|n| *n += 1);
    });

    value.notify();
    assert_eq!(
        counts.with(|c| c.clone()),
        vec![0],
        "observer_count must reflect a clear requested earlier in the same round"
    );
    assert_eq!(
        value.observer_count(),
        0,
        "clear_observers from inside an observer did not clear"
    );

    // Neither observer is put back, so a second notification fires nothing.
    let before = fired.with(|n| *n);
    value.notify();
    assert_eq!(
        fired.with(|n| *n),
        before,
        "a cleared observer fired again on the next notification"
    );
}

/// `clear_observers` clears the *existing* subscriptions and keeps observers
/// subscribed afterwards - so the "an observer may subscribe another observer,
/// and the new one still sees the value" rule is not broken by a clear in the
/// same notification.
#[test]
fn clear_observers_keeps_observers_added_afterwards() {
    let value = Observable::new(0i32);
    let ran = Observable::new(Vec::new());

    let ran_first = ran.clone();
    let ran_in_observer = ran.clone();
    let handle = value.clone();
    let source_for_new = value.clone();
    value.observe(move |v| {
        ran_first.update(|r| r.push(*v));
        handle.clear_observers();
        // Subscribe a fresh observer after the clear; it must survive and, per
        // the documented rule, still receive this notification's value. The
        // handle is cloned here because this closure may run more than once.
        let ran_new = ran_in_observer.clone();
        source_for_new.observe(move |v| ran_new.update(|r| r.push(*v + 100)));
    });

    value.update(|v| *v = 7);
    assert_eq!(
        ran.with(|r| r.clone()),
        vec![7, 107],
        "the observer added after the clear must run with the current value"
    );
    assert_eq!(value.observer_count(), 1, "only the new observer remains");

    value.clear_observers();
}

/// `observer_count` is honest from inside an observer: the round in progress is
/// held out of the subscription list, so a naive `observers.len()` reported zero
/// there even though observers were registered.
#[test]
fn observer_count_is_correct_from_inside_an_observer() {
    let value = Observable::new(0i32);
    let seen = Observable::new(Vec::new());

    // Two observers, each recording what `observer_count` says while it runs.
    for _ in 0..2 {
        let seen_here = seen.clone();
        let handle = value.clone();
        value.observe(move |_| seen_here.update(|s| s.push(handle.observer_count())));
    }

    value.notify();
    assert_eq!(
        seen.with(|s| s.clone()),
        vec![2, 2],
        "observer_count must report both observers from inside an observer"
    );

    // And it is back to normal afterwards.
    assert_eq!(value.observer_count(), 2);
    value.clear_observers();
}

/// `notify` borrows the value to hand `&T` to the observers, so a manual
/// notification from inside an `update` closure panics - and the panic must
/// name the caller, not a line inside the crate. `notify` lacked
/// `#[track_caller]`, so the message pointed at `src/observ.rs`, which is
/// exactly the unhelpful diagnostic this crate exists to remove.
#[test]
fn notify_from_inside_an_update_closure_names_the_caller() {
    let value = Observable::new(0i32);
    value.observe(|_| {}); // something to run, so the value must be borrowed

    let inner = value.clone();
    let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        value.update(|_| {
            let here = line!() + 1;
            inner.notify();
            let _ = here;
        });
    }))
    .unwrap_err();

    let message = panic
        .downcast_ref::<String>()
        .map(String::as_str)
        .or_else(|| panic.downcast_ref::<&str>().copied())
        .unwrap_or("");
    assert!(
        message.contains("already mutably borrowed"),
        "unexpected message: {message}"
    );
    // The reported holder must be this test file, not the crate internals.
    assert!(
        message.contains("observers.rs"),
        "the panic must name the caller, not the crate: {message}"
    );
    assert!(
        !message.contains("src/observ.rs"),
        "the panic must not name the crate's own source: {message}"
    );
    value.clear_observers();
}

/// `try_notify` is the twin of `notify`: a refused notification is an error,
/// not a panic, and it leaves the observer list untouched so the next
/// notification still reaches everyone.
#[test]
fn try_notify_reports_the_conflict_and_keeps_the_observers() {
    let value = Observable::new(0i32);
    let fired = Observable::new(0usize);
    let fired_in_observer = fired.clone();
    value.observe(move |_| fired_in_observer.update(|n| *n += 1));

    let outcome = Observable::new(None);
    let outcome_in_update = outcome.clone();
    let inner = value.clone();
    value.update(|_| {
        outcome_in_update.update(|o| *o = Some(inner.try_notify()));
    });
    let conflict = outcome.with(|o| *o).unwrap().unwrap_err();
    assert_eq!(conflict.kind(), borrow_lite::ConflictKind::Writer);

    // The refused inner notification ran nobody; the outer `update` then
    // notified once on its own, so the total is 1 and not 2.
    assert_eq!(fired.with(|n| *n), 1);

    // The refused notification did not drop the subscription, so the next one
    // still reaches the observer.
    assert_eq!(value.observer_count(), 1);
    value.notify();
    assert_eq!(fired.with(|n| *n), 2);
    value.clear_observers();
}

/// With no observers there is nothing to run, so `notify` needs no borrow of
/// the value and cannot panic even inside an `update` closure. This is the
/// asymmetry that made the first doctest for `try_notify` wrong.
#[test]
fn notify_with_no_observers_cannot_panic_inside_an_update() {
    let value = Observable::new(0i32);
    let inner = value.clone();
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        value.update(|_| inner.notify());
    }));
    assert!(result.is_ok(), "an empty notification must not panic");
    assert_eq!(value.try_notify(), Ok(()));
}

/// `Debug` must not panic and must not lie. It runs inside panic messages and
/// test-failure output, so a panic here (during unwinding) would abort the
/// process; and the observer count it prints has to be the live count, which
/// `observer_count()` reports and the raw list length does not.
mod debug_output {
    use borrow_lite::Observable;

    /// Reading `Debug` from inside an observer used to print `0 observer(s)`,
    /// because the round in progress has been moved out of the list. It must
    /// agree with `observer_count()`.
    #[test]
    fn debug_from_inside_an_observer_reports_the_live_count() {
        let value = Observable::new(0i32);
        let captured = Observable::new(String::new());
        let captured_in_observer = captured.clone();
        let handle = value.clone();

        value.observe(move |_| {
            captured_in_observer.update(|s| *s = format!("{handle:?}"));
        });
        value.notify();

        let text = captured.with(|s| s.clone());
        assert!(
            text.contains("1 observer(s)"),
            "Debug must report the live count, not the emptied list: {text}"
        );
        assert_eq!(value.observer_count(), 1);
        assert!(format!("{value:?}").contains("1 observer(s)"));
        value.clear_observers();
        assert!(format!("{value:?}").contains("0 observer(s)"));
    }

    /// `Debug` must not panic when the value is exclusively borrowed. The
    /// panicking `with` did panic here, and a `Debug` that panics during
    /// unwinding aborts the process.
    #[test]
    fn debug_does_not_panic_inside_an_update_closure() {
        let value = Observable::new(1i32);
        let inner = value.clone();
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            value.update(|_| {
                // The value is exclusively borrowed here; formatting must still
                // succeed and show the conflict rather than the value.
                let text = format!("{inner:?}");
                assert!(
                    text.contains("already mutably borrowed"),
                    "expected a conflict in the Debug output, got: {text}"
                );
            });
        }));
        assert!(result.is_ok(), "Debug must never panic");
        // Outside the borrow it shows the value again.
        assert!(format!("{value:?}").contains("value: 1"));
    }

    /// The dangerous case: formatting the value while a panic is unwinding.
    /// A `Debug` that panicked here would be a panic during unwinding, which
    /// aborts the process. This drives the borrow through a panic and formats
    /// inside the panic message.
    #[test]
    fn debug_is_safe_while_a_panic_unwinds() {
        let value = Observable::new(7i32);
        let inner = value.clone();
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            value.update(|_| {
                // Panic while the value is exclusively borrowed, formatting it
                // on the way out.
                panic!("boom: {inner:?}");
            });
        }));
        let payload = result.unwrap_err();
        let message = payload
            .downcast_ref::<String>()
            .map(String::as_str)
            .unwrap_or("");
        assert!(
            message.contains("boom:"),
            "the panic message must have been built: {message}"
        );
        // The cell is usable again, so the borrow was released on unwind.
        assert_eq!(value.with(|v| *v), 7);
    }
}

/// `unobserve` must answer the same way whether or not a notification is in
/// flight. During one, the observers of the running round are held by the
/// notification guard rather than the list, so a check against the list alone
/// reported `true` for an id that was never subscribed - breaking the "returns
/// `false` for an unknown id" contract at exactly the moment the return value
/// is hardest to reason about.
mod unobserve_contract {
    use borrow_lite::Observable;

    #[test]
    fn an_unknown_id_is_false_outside_and_inside_a_notification() {
        let value = Observable::new(0i32);
        let seen = Observable::new(Vec::new());
        let seen_in_observer = seen.clone();
        let handle = value.clone();

        let id = value.observe(move |_| {
            // 9999 was never subscribed; 0 is the running observer itself.
            let unknown = handle.unobserve(9999);
            let live = handle.unobserve(0);
            let repeat = handle.unobserve(0);
            seen_in_observer.update(|v| *v = vec![unknown, live, repeat]);
        });
        assert_eq!(id, 0);

        value.notify();
        assert_eq!(
            seen.with(|v| v.clone()),
            vec![false, true, false],
            "unknown id must be false, a live one true, a repeat false"
        );
        // The observer really was removed by its own request.
        assert_eq!(value.observer_count(), 0);

        // The same answers outside a notification.
        assert!(!value.unobserve(9999));
        assert!(!value.unobserve(id));
    }

    /// An observer from an *earlier* round is held by the guard, not the list,
    /// when a later round runs. Removing it must still report success - which
    /// is why `in_flight` keeps ids and not only a count. This is the companion
    /// to the unknown-id test: it guards against over-restricting the check.
    #[test]
    fn an_observer_of_an_earlier_round_is_still_removable() {
        let value = Observable::new(0i32);
        let removed = Observable::new(None);
        let counted = Observable::new(None);

        // id 0: round 1, does nothing.
        value.observe(|_| {});
        // id 1: round 1, subscribes id 2 so id 2 runs in round 2.
        let adder = value.clone();
        value.observe(move |_| {
            let _ = adder.observe(|_| {});
        });
        // id 2: round 2, when id 0 has run and sits in the guard, not the list.
        let remover = value.clone();
        let outcome = removed.clone();
        let seen_count = counted.clone();
        value.observe(move |_| {
            let ok = remover.unobserve(0);
            outcome.update(|o| *o = Some(ok));
            seen_count.update(|c| *c = Some(remover.observer_count()));
        });

        value.notify();
        value.clear_observers(); // break the `Rc` cycle the observers hold
        assert_eq!(
            removed.with(|o| *o),
            Some(true),
            "an earlier round's observer is still registered"
        );
        // Four live: id 1, id 2, the id the adder added, and id 0 until it is
        // removed - the count sees the guard-held observers too.
        assert_eq!(counted.with(|c| *c), Some(4));
    }

    /// An observer subscribed *during* a notification sits in the list, not in
    /// the guard, and removing it in the same notification must succeed. This
    /// is the other half of the `registered` check: it fails if the check looks
    /// only at the in-flight ids.
    #[test]
    fn an_observer_added_during_a_notification_is_removable() {
        let value = Observable::new(0i32);
        let removed = Observable::new(None);

        let adder = value.clone();
        let outcome = removed.clone();
        value.observe(move |_| {
            // Subscribe a new observer; it lands in the list immediately.
            let new_id = adder.observe(|_| {});
            // Remove it again before this notification finishes.
            outcome.update(|o| *o = Some(adder.unobserve(new_id)));
        });

        value.notify();
        assert_eq!(
            removed.with(|o| *o),
            Some(true),
            "an observer added during the notification is in the list, not in flight"
        );
        assert_eq!(value.observer_count(), 1);
        value.clear_observers(); // break the `Rc` cycle the observer holds
    }
}
