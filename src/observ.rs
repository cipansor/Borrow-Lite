//! A value that can be watched, without the borrow problems that usually come
//! with observer lists.

use alloc::boxed::Box;
use alloc::vec::Vec;

use crate::error::Conflict;
use crate::{Local, local};

/// Identifies a subscription, so it can be removed later.
pub type ObserverId = u64;

type Observer<T> = Box<dyn FnMut(&T)>;

/// Upper bound on notification rounds in one [`notify`](Observable::notify).
///
/// An observer is allowed to subscribe more observers; each round runs the
/// ones added by the previous round. The cap exists so that an observer that
/// re-subscribes itself on every call cannot spin forever. Observers left over
/// when the cap is reached stay subscribed, and run on the next notification.
pub const MAX_NOTIFICATION_ROUNDS: usize = 32;

struct Subs<T> {
    observers: Vec<(ObserverId, Observer<T>)>,
    /// Ids removed while a notification was in flight, applied when it ends.
    pending_removal: Vec<ObserverId>,
    /// Set when [`clear_observers`](Observable::clear_observers) is called during
    /// a notification. Holds the `next_id` at that moment: every observer with a
    /// smaller id was subscribed when the clear was requested, so the guard drops
    /// those when it puts the list back. `None` means no clear is pending.
    clear_before: Option<ObserverId>,
    /// Ids the notification guard has taken out of `observers` but that are
    /// still registered - the round in progress and the rounds already run.
    /// Kept as ids rather than a count so [`unobserve`](Observable::unobserve)
    /// can tell a live observer from an unknown id while a notification is in
    /// flight; the length is what keeps
    /// [`observer_count`](Observable::observer_count) honest. A
    /// [`clear_observers`](Observable::clear_observers) during the notification
    /// clears it, because those observers are no longer registered.
    in_flight: Vec<ObserverId>,
    notifying: bool,
    next_id: ObserverId,
}

/// A value plus a list of observers that are called whenever it changes.
///
/// # Reference cycles
///
/// An observer that captures a clone of this `Observable` keeps it alive, and
/// the value keeps the observer alive in turn, so the two never drop. This is
/// the usual `Rc` cycle, and [`clear_observers`](Self::clear_observers) is the
/// usual way out. Observers that capture a different cell are fine.
///
/// The usual implementation of this pattern is an `Rc<RefCell<Vec<Box<dyn
/// FnMut(&T)>>>>` next to an `Rc<RefCell<T>>`, and it breaks in two ways as
/// soon as an observer does anything interesting: notifying while the observer
/// list is borrowed, or mutating the value while it is being handed to an
/// observer. `Observable` handles both:
///
/// * the observer list is *moved out* of its cell before any observer runs, so
///   an observer may subscribe or unsubscribe;
/// * observers added during a notification still see the current value;
/// * mutating the value from inside an observer is reported as a
///   [`Conflict`] by [`try_update`](Observable::try_update) instead of being
///   undefined behaviour, and the panicking
///   [`update`](Observable::update) tells you which call is holding the
///   borrow.
///
/// ```
/// use lite::Observable;
///
/// let temperature = Observable::new(20i32);
/// let seen = Observable::new(Vec::new());
///
/// let seen_in_observer = seen.clone();
/// temperature.observe(move |t| seen_in_observer.update(|v| v.push(*t)));
///
/// temperature.update(|t| *t += 1);
/// assert_eq!(seen.with(|v| v.clone()), vec![21]);
/// ```
///
/// # What "non-panicking" means for the `try_*` forms
///
/// Every `try_*` method turns a *borrow conflict* into an `Err` instead of a
/// panic. It does **not** catch a panic raised by an observer: that panic is
/// not a conflict, it is the caller's own code failing, so it propagates out of
/// `try_update`, `try_replace`, `try_take` and `try_notify` unchanged. The
/// observer list and the `notifying` flag are still restored on the way out
/// (see [`notify`](Self::notify)), so the cell stays usable - but the panic
/// itself is not converted into a value. If you need to contain an observer's
/// panic, do it inside the observer, where you can also decide what to do about
/// it.
///
/// An observer may subscribe another observer, and the new one still gets the
/// value that triggered the notification:
///
/// ```
/// use lite::Observable;
///
/// let source = Observable::new(1u8);
/// let log = Observable::new(Vec::new());
///
/// let inner_log = log.clone();
/// let source_for_inner = source.clone();
/// source.observe(move |_| {
///     let inner_log = inner_log.clone();
///     source_for_inner.observe(move |v| inner_log.update(|l| l.push(*v)));
/// });
///
/// source.update(|v| *v = 2);
/// assert_eq!(log.with(|l| l.clone()), vec![2]);
///
/// // The observer captures `source`, so clear the list to break the cycle.
/// source.clear_observers();
/// ```
///
/// # Single-threaded
///
/// `Observable` is built on [`Local`](crate::Local), so it is **not** `Send` and
/// cannot be shared between threads:
///
/// ```compile_fail
/// use lite::Observable;
///
/// let temperature = Observable::new(20i32);
/// std::thread::spawn(move || {
///     // error: `Observable<i32>` cannot be sent between threads safely
///     temperature.update(|t| *t += 1);
/// });
/// ```
///
/// The other half of that claim - that a *shared* reference cannot cross a
/// thread either, which is what "cannot be shared between threads" means - is a
/// separate bound, so it is checked separately:
///
/// ```compile_fail
/// fn is_sync<T: Sync>() {}
/// is_sync::<lite::Observable<i32>>();
/// ```
///
/// This is deliberate: the observer list is `Rc<Cell<...>>`-shaped bookkeeping,
/// and making it thread-safe would put an atomic in the hot path of every
/// `observe`, `unobserve` and notification for a pattern that is usually
/// single-threaded. If you need an observer across threads, keep the
/// `Observable` on one thread and send it messages, or use
/// [`Shared`](crate::Shared) directly.
pub struct Observable<T> {
    value: Local<T>,
    subs: Local<Subs<T>>,
}

impl<T> Clone for Observable<T> {
    fn clone(&self) -> Self {
        Self {
            value: self.value.clone(),
            subs: self.subs.clone(),
        }
    }
}

impl<T> Observable<T> {
    /// Wrap a value, with no observers yet.
    pub fn new(value: T) -> Self {
        Self {
            value: local(value),
            subs: local(Subs {
                observers: Vec::new(),
                pending_removal: Vec::new(),
                clear_before: None,
                in_flight: Vec::new(),
                notifying: false,
                next_id: 0,
            }),
        }
    }

    /// Register an observer, returning its id.
    pub fn observe(&self, observer: impl FnMut(&T) + 'static) -> ObserverId {
        self.subs.with_mut(|subs| {
            let id = subs.next_id;
            subs.next_id += 1;
            subs.observers.push((id, Box::new(observer)));
            id
        })
    }

    /// Remove an observer.
    ///
    /// Returns `false` if the id is unknown, or if the same id is already
    /// queued for removal. This holds inside a notification too, where an
    /// observer that is subscribed but not yet run - or one that has already
    /// run and is held by the notification guard - still counts as known.
    ///
    /// This may be called from inside an observer, including to remove the
    /// running observer itself; the removal is queued and applied when the
    /// notification finishes, so the list is never mutated underneath it.
    #[must_use]
    pub fn unobserve(&self, id: ObserverId) -> bool {
        self.subs.with_mut(|subs| {
            if subs.notifying {
                if subs.pending_removal.contains(&id) {
                    return false;
                }
                // The observer may still be in the list, or in the guard's
                // hands (the round in progress and the rounds already run). An
                // id in neither is unknown, and the contract says `false`.
                let registered = subs.observers.iter().any(|(oid, _)| *oid == id)
                    || subs.in_flight.contains(&id);
                if !registered {
                    return false;
                }
                subs.pending_removal.push(id);
                return true;
            }
            let before = subs.observers.len();
            subs.observers.retain(|(observer_id, _)| *observer_id != id);
            subs.observers.len() != before
        })
    }

    /// How many observers are subscribed.
    ///
    /// Correct from inside an observer too: the observers of the round in
    /// progress have been taken out of the list, so they are counted separately
    /// rather than reported as zero.
    #[must_use]
    pub fn observer_count(&self) -> usize {
        self.subs
            .with(|subs| subs.observers.len() + subs.in_flight.len())
    }

    /// Drop every subscription.
    ///
    /// This is also how you break a reference cycle: an observer that captures
    /// a clone of this `Observable` keeps it alive, so the observer list and
    /// the value keep each other alive forever, exactly as any `Rc` cycle
    /// would. Calling this drops those closures and frees the cycle. Miri
    /// flags the leak otherwise, which is how this method came to exist.
    ///
    /// It works from inside an observer as well, with the same "applied when
    /// the notification finishes" rule [`unobserve`](Self::unobserve) has: the
    /// round in progress runs to completion (those observers are held by the
    /// notification, not by the list), and none of them is put back. Observers
    /// subscribed *after* this call - including by the observer that called it -
    /// are kept, so the "observers added during a notification still see the
    /// value" rule is not broken.
    ///
    /// ```
    /// use lite::Observable;
    ///
    /// let value = Observable::new(0i32);
    /// let ran = Observable::new(Vec::new());
    /// let ran_in_observer = ran.clone();
    ///
    /// let handle = value.clone();
    /// value.observe(move |_| {
    ///     ran_in_observer.update(|r| r.push(1));
    ///     handle.clear_observers();
    /// });
    /// let ran_in_second = ran.clone();
    /// value.observe(move |_| ran_in_second.update(|r| r.push(2)));
    ///
    /// value.notify();
    /// // The round finished, then the clear took effect.
    /// assert_eq!(ran.with(|r| r.clone()), vec![1, 2]);
    /// assert_eq!(value.observer_count(), 0);
    ///
    /// // Nothing fires again.
    /// value.notify();
    /// assert_eq!(ran.with(|r| r.clone()), vec![1, 2]);
    /// ```
    pub fn clear_observers(&self) {
        self.subs.with_mut(|subs| {
            subs.observers.clear();
            subs.pending_removal.clear();
            if subs.notifying {
                // The observers of the round in progress are not in the list
                // right now - the notification guard holds them - so clearing
                // the list alone would leave them alive and they would be put
                // back when the notification ends. Remember the id boundary so
                // the guard drops them too. Observers subscribed *after* this
                // call have larger ids and are kept, which is what makes the
                // "observers added during a notification still see the value"
                // rule hold.
                subs.clear_before = Some(subs.next_id);
                // Those observers are still physically held by the guard, but
                // they are no longer registered, so they must not be counted.
                subs.in_flight.clear();
            }
        });
    }

    /// Read the value.
    ///
    /// # Panics
    ///
    /// Panics if the value is exclusively borrowed, which happens when this is
    /// called from inside an [`update`](Self::update) closure. Use
    /// [`try_with`](Self::try_with) for that case as an error. Reading from
    /// inside an *observer* is fine: the value is shared-borrowed there.
    #[track_caller]
    pub fn with<R>(&self, f: impl FnOnce(&T) -> R) -> R {
        self.value.with(f)
    }

    /// Read the value, reporting a conflict instead of panicking.
    ///
    /// The non-panicking form of [`with`](Self::with). The conflict only arises
    /// when the value is exclusively borrowed, i.e. from inside an
    /// [`update`](Self::update) closure.
    ///
    /// # Errors
    ///
    /// Returns the [`Conflict`] if the value cannot be borrowed.
    ///
    /// ```
    /// use lite::Observable;
    ///
    /// let value = Observable::new(0i32);
    /// let inner = value.clone();
    /// let outcome = Observable::new(None);
    /// let outcome_in_update = outcome.clone();
    ///
    /// value.update(|_| {
    ///     // The value is exclusively borrowed here, so a re-entrant read is
    ///     // reported rather than panicking.
    ///     outcome_in_update.update(|slot| *slot = Some(inner.try_with(|v| *v).is_err()));
    /// });
    /// assert_eq!(outcome.with(|o| *o), Some(true));
    ///
    /// // Idle, it succeeds.
    /// assert_eq!(value.try_with(|v| *v), Ok(0));
    /// ```
    #[track_caller]
    pub fn try_with<R>(&self, f: impl FnOnce(&T) -> R) -> Result<R, Conflict> {
        self.value.try_with(f)
    }

    /// Mutate the value and notify the observers.
    ///
    /// # Panics
    ///
    /// Panics if this is called from inside an observer, because the value is
    /// already borrowed to pass to the running observers. Use
    /// [`try_update`](Self::try_update) if you want that case as an error.
    #[track_caller]
    pub fn update(&self, f: impl FnOnce(&mut T)) {
        if let Err(conflict) = self.try_update(f) {
            panic!("{}", conflict.into_panic_message());
        }
    }

    /// Mutate the value and notify the observers, reporting a re-entrant
    /// update instead of panicking.
    ///
    /// # Errors
    ///
    /// Returns a [`Conflict`] when this is called from inside an observer, so
    /// the value is already borrowed for the notification.
    ///
    /// ```
    /// use lite::Observable;
    ///
    /// let value = Observable::new(0i32);
    /// let inner = value.clone();
    /// let outcome = Observable::new(None);
    ///
    /// let outcome_in_observer = outcome.clone();
    /// value.observe(move |_| {
    ///     // Re-entering `update` from an observer is refused, not UB.
    ///     let result = inner.try_update(|v| *v += 1);
    ///     outcome_in_observer.update(|slot| *slot = Some(result.is_err()));
    /// });
    ///
    /// value.try_update(|v| *v += 1).unwrap();
    /// assert_eq!(outcome.with(|o| *o), Some(true));
    ///
    /// // The observer captures `value`; clear the list to break the cycle.
    /// value.clear_observers();
    /// ```
    #[track_caller]
    pub fn try_update(&self, f: impl FnOnce(&mut T)) -> Result<(), Conflict> {
        self.value.try_with_mut(f)?;
        self.notify();
        Ok(())
    }

    /// Replace the value and notify the observers.
    ///
    /// # Panics
    ///
    /// See [`update`](Self::update).
    #[track_caller]
    pub fn set(&self, value: T) {
        self.update(|slot| *slot = value);
    }

    /// Replace the value and notify the observers, reporting a re-entrant
    /// update instead of panicking.
    ///
    /// # Errors
    ///
    /// See [`try_update`](Self::try_update).
    #[track_caller]
    pub fn try_set(&self, value: T) -> Result<(), Conflict> {
        self.try_update(|slot| *slot = value)
    }

    /// Direct access to the value, bypassing the borrow flags, while no other
    /// handle exists.
    ///
    /// The same contract as [`Local::get_mut`](crate::Local::get_mut): `None`
    /// while a clone of this `Observable` exists, because that clone could hold
    /// a borrow. This is the setup path - build the value up before subscribing
    /// anyone, with no notification and no borrow check.
    ///
    /// ```
    /// use lite::Observable;
    ///
    /// let mut temp = Observable::new(vec![20i32]);
    /// temp.get_mut().unwrap().push(21);
    /// assert_eq!(temp.with(|v| v.clone()), vec![20, 21]);
    ///
    /// let other = temp.clone();
    /// assert!(temp.get_mut().is_none());
    /// # drop(other);
    /// ```
    #[must_use]
    pub fn get_mut(&mut self) -> Option<&mut T> {
        self.value.get_mut()
    }

    /// Consume the `Observable` and hand back the value, dropping the observers.
    ///
    /// `Err(self)` while another handle exists, the same contract as
    /// [`Local::into_inner`](crate::Local::into_inner). Because a clone clones
    /// both the value and the observer list, a unique value handle implies a
    /// unique list, so the error path returns `self` untouched and a clone's
    /// observers are never split off.
    ///
    /// ```
    /// use lite::Observable;
    ///
    /// let temp = Observable::new(20i32);
    /// let other = temp.clone();
    /// let temp = temp.into_inner().unwrap_err();
    /// drop(other);
    /// assert_eq!(temp.into_inner().unwrap(), 20);
    /// ```
    ///
    /// # Errors
    ///
    /// Returns `Err(self)` if this is not the only handle to the `Observable`.
    pub fn into_inner(self) -> Result<T, Self> {
        let Self { value, subs } = self;
        match value.into_inner() {
            Ok(value) => Ok(value),
            Err(value) => Err(Self { value, subs }),
        }
    }

    /// Replace the value, returning the old one, and notify the observers.
    ///
    /// The same contract as [`Inner::replace`](crate::Inner::replace), plus the
    /// notification [`set`](Self::set) does - so observers see the new value and
    /// you keep the old one.
    ///
    /// # Panics
    ///
    /// See [`update`](Self::update).
    #[track_caller]
    pub fn replace(&self, value: T) -> T
    where
        T: Sized,
    {
        match self.try_replace(value) {
            Ok(previous) => previous,
            Err((conflict, _value)) => panic!("{}", conflict.into_panic_message()),
        }
    }

    /// Replace the value and notify, or report the conflict and hand the
    /// unwritten value back.
    ///
    /// The non-panicking form of [`replace`](Self::replace). On a conflict the
    /// value is given back and the observers are not notified.
    ///
    /// # Errors
    ///
    /// Returns the [`Conflict`] and the unwritten `value` if the value is
    /// already borrowed.
    ///
    /// ```
    /// use lite::Observable;
    ///
    /// let temp = Observable::new(vec![20i32]);
    /// let seen = Observable::new(Vec::new());
    /// let seen_in_observer = seen.clone();
    /// temp.observe(move |v| seen_in_observer.update(|s| s.push(v[0])));
    ///
    /// let old = temp.replace(vec![21]);
    /// assert_eq!(old, vec![20]);
    /// assert_eq!(seen.with(|s| s.clone()), vec![21]);
    /// ```
    #[track_caller]
    pub fn try_replace(&self, value: T) -> Result<T, (Conflict, T)>
    where
        T: Sized,
    {
        let previous = self.value.try_replace(value)?;
        self.notify();
        Ok(previous)
    }

    /// Take the value out, leaving `T::default()` and notifying the observers.
    ///
    /// The same contract as [`RefCell::take`](core::cell::RefCell::take) and
    /// [`Inner::take`](crate::Inner::take): a located panic if the value is
    /// borrowed, so a conflict never loses it. Notifying is what makes this
    /// different from `set`: observers see the emptied value.
    ///
    /// # Panics
    ///
    /// See [`update`](Self::update).
    ///
    /// ```
    /// use lite::Observable;
    ///
    /// let temp = Observable::new(vec![20i32]);
    /// assert_eq!(temp.take(), vec![20]);
    /// assert!(temp.with(Vec::is_empty));
    /// ```
    #[must_use]
    #[track_caller]
    pub fn take(&self) -> T
    where
        T: Sized + Default,
    {
        match self.try_take() {
            Ok(taken) => taken,
            Err(conflict) => panic!("{}", conflict.into_panic_message()),
        }
    }

    /// Take the value out, leaving `T::default()` and notifying, or report the
    /// conflict.
    ///
    /// The non-panicking form of [`take`](Self::take). On a conflict the value
    /// is untouched and the observers are not notified.
    ///
    /// # Errors
    ///
    /// Returns the [`Conflict`] if the value is already borrowed, which is the
    /// case inside an observer.
    ///
    /// ```
    /// use lite::Observable;
    ///
    /// let temp = Observable::new(vec![1i32, 2]);
    /// let inner = temp.clone();
    /// let outcome = Observable::new(None);
    /// let outcome_in_observer = outcome.clone();
    /// temp.observe(move |_| {
    ///     // Re-entering `take` from an observer is refused, not a panic.
    ///     outcome_in_observer.update(|o| *o = Some(inner.try_take().is_err()));
    /// });
    /// temp.notify();
    /// assert_eq!(outcome.with(|o| *o), Some(true));
    /// assert_eq!(temp.with(|v| v.len()), 2); // nothing was taken
    /// temp.clear_observers();
    /// ```
    #[track_caller]
    pub fn try_take(&self) -> Result<T, Conflict>
    where
        T: Sized + Default,
    {
        self.try_replace(T::default())
            .map_err(|(conflict, _)| conflict)
    }

    /// Notify the observers without changing the value.
    ///
    /// Nested calls are ignored: the outer call keeps notifying, so an
    /// observer that triggers another notification cannot recurse.
    ///
    /// If an observer panics, the panic propagates, and the observer list and
    /// the "notifying" flag are restored on the way out, so the cell stays
    /// usable afterwards.
    ///
    /// # Panics
    ///
    /// Panics if there are observers to run and the value is exclusively
    /// borrowed, which happens when this is called from inside an
    /// [`update`](Self::update) or [`with_mut`](crate::Inner::with_mut) closure
    /// on the same cell: the observers need `&T`, and the borrow is in the way.
    /// Use [`try_notify`](Self::try_notify) for that case as an error. With no
    /// observers subscribed there is nothing to run, so it cannot panic.
    #[track_caller]
    pub fn notify(&self) {
        if let Err(conflict) = self.try_notify() {
            panic!("{}", conflict.into_panic_message());
        }
    }

    /// Notify the observers, reporting a conflict instead of panicking.
    ///
    /// The non-panicking form of [`notify`](Self::notify). The conflict only
    /// arises when the value is exclusively borrowed, i.e. from inside an
    /// [`update`](Self::update) closure; from inside an observer the value is
    /// shared-borrowed and a nested notification is simply ignored.
    ///
    /// # Errors
    ///
    /// Returns the [`Conflict`] if there are observers to run and the value
    /// cannot be borrowed, leaving the observer list untouched. With no
    /// observers subscribed there is nothing to run, so it succeeds without
    /// touching the value.
    ///
    /// ```
    /// use lite::Observable;
    ///
    /// let value = Observable::new(0i32);
    /// value.observe(|_| {}); // a notification with something to run
    ///
    /// let outcome = Observable::new(None);
    /// let outcome_in_update = outcome.clone();
    /// let inner = value.clone();
    ///
    /// value.update(|_| {
    ///     // The value is exclusively borrowed, so notifying is refused.
    ///     outcome_in_update.update(|o| *o = Some(inner.try_notify().is_err()));
    /// });
    /// assert_eq!(outcome.with(|o| *o), Some(true));
    ///
    /// // Idle, it succeeds.
    /// assert!(value.try_notify().is_ok());
    /// # value.clear_observers();
    /// ```
    #[track_caller]
    pub fn try_notify(&self) -> Result<(), Conflict> {
        let already_notifying = self.subs.with_mut(|subs| {
            let was = subs.notifying;
            subs.notifying = true;
            was
        });
        if already_notifying {
            return Ok(());
        }

        let mut restore = Restore {
            subs: &self.subs,
            finished: Vec::new(),
            running: Vec::new(),
        };
        for _ in 0..MAX_NOTIFICATION_ROUNDS {
            // The round lives inside `restore`, not in a local, so that an
            // observer that panics cannot take the not-yet-called observers
            // down with it: the guard puts them back on the way out.
            let subs = restore.subs;
            subs.with_mut(|subs| {
                restore.running.append(&mut subs.observers);
                subs.in_flight
                    .extend(restore.running.iter().map(|(id, _)| *id));
            });
            if restore.running.is_empty() {
                break;
            }
            // The value is shared-borrowed for the duration of the round: an
            // observer sees a stable value, and trying to mutate it from an
            // observer reports a conflict instead of aliasing. A failure here
            // can only be the value already being exclusively borrowed, and
            // the guard puts the round's observers back on the way out.
            let running = &mut restore.running;
            let outcome = self.value.try_with(|value| {
                for (_, observer) in running.iter_mut() {
                    observer(value);
                }
            });
            outcome?;
            restore.finished.append(&mut restore.running);
        }
        drop(restore);
        Ok(())
    }
}

/// Puts the observer list back, applies queued removals, and clears the
/// `notifying` flag, even if an observer panics.
///
/// `running` holds the observers of the round in progress. It is deliberately a
/// field rather than a local in [`Observable::notify`]: if an observer panics,
/// unwinding drops this guard, and the round's remaining observers are restored
/// from here instead of being dropped.
struct Restore<'a, T> {
    subs: &'a Local<Subs<T>>,
    finished: Vec<(ObserverId, Observer<T>)>,
    running: Vec<(ObserverId, Observer<T>)>,
}

impl<T> Drop for Restore<'_, T> {
    fn drop(&mut self) {
        let mut finished = core::mem::take(&mut self.finished);
        // The interrupted round comes before observers added during the
        // notification, matching the documented ordering.
        finished.append(&mut self.running);
        self.subs.with_mut(|subs| {
            let mut all = finished;
            all.append(&mut subs.observers);
            // Observers added during the notification come after the ones that
            // were already there, and removals requested during it are applied
            // now that nothing is iterating over the list.
            let removed = core::mem::take(&mut subs.pending_removal);
            if !removed.is_empty() {
                all.retain(|(id, _)| !removed.contains(id));
            }
            // A `clear_observers` during the notification removed the list but
            // not the round in flight; drop everything that was subscribed when
            // it was requested.
            if let Some(boundary) = subs.clear_before.take() {
                all.retain(|(id, _)| *id >= boundary);
            }
            subs.observers = all;
            subs.in_flight.clear();
            subs.notifying = false;
        });
    }
}

impl<T: core::fmt::Debug> core::fmt::Debug for Observable<T> {
    /// Formats without ever panicking, and without lying about the observer
    /// count.
    ///
    /// `Debug` runs inside panic messages and test failures, so it must not
    /// panic: a panic here during unwinding would abort the process. `Local`,
    /// `Shared` and the guards therefore borrow fallibly and print the
    /// [`Conflict`] when the value is busy; `Observable` now does the same
    /// instead of calling the panicking `with`.
    ///
    /// The count comes from [`observer_count`](Observable::observer_count)
    /// rather than the raw list length, because during a notification the round
    /// in progress has been moved out of the list - reading the list there
    /// reported zero observers that were very much alive.
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        // `subs` is summarised rather than printed: its observers are boxed
        // closures, which have nothing useful to show.
        let count = self.observer_count();
        let value = match self.try_with(|v| alloc::format!("{v:?}")) {
            Ok(text) => text,
            Err(conflict) => alloc::format!("<{conflict}>"),
        };
        f.debug_struct("Observable")
            .field("value", &format_args!("{value}"))
            .field("subs", &format_args!("{count} observer(s)"))
            .finish()
    }
}
