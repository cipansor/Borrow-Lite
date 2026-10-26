//! The shared cell behind [`Local`](crate::Local) and
//! [`Shared`](crate::Shared).
#![allow(unsafe_code)]

use core::cell::UnsafeCell;
use core::panic::Location;

use crate::error::Conflict;
use crate::guards::{ReadGuard, WriteGuard};
use crate::state::{AtomicState, LocalState, State};

/// The cell: borrow flags plus the value they guard.
///
/// You almost never name this type; it is what [`Local`](crate::Local) and
/// [`Shared`](crate::Shared) deref to, so `cell.borrow()` and
/// `cell.with_mut(..)` are the methods below.
///
/// The methods are split across impls on purpose. Exclusive borrowing needs
/// nothing beyond the flags, so it is available for any `T`. Shared borrowing
/// hands out `&T`, which for the atomic cell can be observed by several
/// threads at once, so *that* half is gated on `T: Sync`.
///
/// # Safety
///
/// The value is only reachable through a guard, and a guard only exists while
/// the corresponding flag in `state` is held. `UnsafeCell` is `!Sync`, so the
/// manual [`Sync`] impl below is what lets `Shared` be shared; it is
/// conditional on `T: Sync`, which is exactly the condition the shared-borrow
/// methods repeat.
pub struct Inner<T: ?Sized, S: State> {
    state: S,
    value: UnsafeCell<T>,
}

// SAFETY: moving the cell to another thread moves the value, which requires
// `T: Send`; `S: Send` covers the flags.
unsafe impl<T: ?Sized + Send, S: State + Send> Send for Inner<T, S> {}

// SAFETY: sharing `&Inner` lets another thread reach the value two ways, and
// each needs its own bound. A *shared* borrow hands out `&T`, which is sound
// only when `T: Sync`. An *exclusive* borrow hands out `&mut T`, and `&mut T`
// is enough to move or drop the value - so it requires `T: Send` as well. The
// `T: Send` half is easy to miss because the exclusive methods themselves are
// available for any `T` (they need only the flags); it is the *sharing* of the
// cell across threads that must be forbidden for a `!Send` payload. Without it
// a `Sync + !Send` value such as a `MutexGuard` could be swapped out and
// dropped on another thread, which is exactly what `!Send` forbids.
unsafe impl<T: ?Sized + Send + Sync, S: State + Sync> Sync for Inner<T, S> {}

impl<T, S: State> Inner<T, S> {
    pub(crate) fn new_in(value: T) -> Self {
        Self {
            state: S::new(),
            value: UnsafeCell::new(value),
        }
    }

    /// Consume the cell and hand back the value, with no borrow check.
    ///
    /// Only sound once the handle is uniquely owned, which is the caller's
    /// job - [`Local::into_inner`](crate::Local::into_inner) and
    /// [`Shared::into_inner`](crate::Shared::into_inner) get that from
    /// `Rc::try_unwrap`/`Arc::try_unwrap` first. `UnsafeCell::into_inner`
    /// needs no `unsafe`: consuming `self` proves nothing can alias it.
    pub(crate) fn into_inner(self) -> T {
        self.value.into_inner()
    }
}

/// The call site recorded for a borrow.
///
/// With `diagnostics` on this is `Location::caller()`, threaded through the
/// `#[track_caller]` layers above so a conflict can name the line that
/// attempted the borrow. With the feature off no location is ever stored, so
/// this deliberately is *not* `#[track_caller]`: `Location::caller()` then
/// returns the location of this very call - a compile-time constant - and the
/// caller chain above it has no observable effect.
#[cfg_attr(feature = "diagnostics", track_caller)]
fn borrow_site() -> &'static Location<'static> {
    Location::caller()
}

impl<T: ?Sized, S: State> Inner<T, S> {
    /// Direct access to the value, with no borrow flag and no guard.
    ///
    /// Taking `&mut self` proves that no guard is alive (a guard borrows the
    /// cell) and that the cell is uniquely owned, so no other borrow can be
    /// created. `UnsafeCell::get_mut` needs no `unsafe` for exactly that
    /// reason. This is the cheap path for setup and teardown, where the
    /// single-threaded borrow flag and the atomic handshake are pure overhead.
    pub fn get_mut(&mut self) -> &mut T {
        self.value.get_mut()
    }
}

impl<T: ?Sized, S: State> Inner<T, S> {
    /// Borrow the value mutably, panicking with a located message if that is
    /// not possible right now.
    #[cfg_attr(feature = "diagnostics", track_caller)]
    pub fn borrow_mut(&self) -> WriteGuard<'_, T, S> {
        match self.state.try_exclusive(borrow_site()) {
            Ok(()) => WriteGuard::new(&self.state, self.value.get()),
            Err(conflict) => panic!("{}", conflict.into_panic_message()),
        }
    }

    /// Borrow the value mutably, reporting the conflict instead of panicking.
    #[track_caller]
    pub fn try_borrow_mut(&self) -> Result<WriteGuard<'_, T, S>, Conflict> {
        self.state
            .try_exclusive(borrow_site())
            .map(|()| WriteGuard::new(&self.state, self.value.get()))
    }

    /// Run `f` with an exclusive borrow, released before this call returns.
    ///
    /// This is the form that cannot leak a guard, so it can never turn into a
    /// borrow conflict later on. Note that `f` must not itself call `with_mut`
    /// on this cell; the panic message will name the call site that is still
    /// holding the borrow.
    #[track_caller]
    pub fn with_mut<R>(&self, f: impl FnOnce(&mut T) -> R) -> R {
        f(&mut self.borrow_mut())
    }

    /// Run `f` with an exclusive borrow, or return the conflict to the caller.
    #[track_caller]
    pub fn try_with_mut<R>(&self, f: impl FnOnce(&mut T) -> R) -> Result<R, Conflict> {
        self.try_borrow_mut().map(|mut guard| f(&mut guard))
    }

    /// Run `f` with an exclusive borrow, released before this call returns.
    ///
    /// The same borrow as [`with_mut`](Self::with_mut) under a shorter name, and
    /// the verb `Observable` already uses for "mutate the value". It is the
    /// write-shaped counterpart of [`map`](Self::map), so a cell reads as
    /// `cell.map(..)` to look and `cell.update(..)` to change.
    ///
    /// ```
    /// use borrow_lite::local;
    ///
    /// let stats = local(vec![1, 2, 3]);
    /// stats.update(|v| v.push(4));
    /// assert_eq!(stats.map(Vec::len), 4);
    /// ```
    ///
    /// # Panics
    ///
    /// Panics with a located message if the cell is already borrowed. Use
    /// [`try_update`](Self::try_update) for that case as a `Result`.
    #[track_caller]
    pub fn update<R>(&self, f: impl FnOnce(&mut T) -> R) -> R {
        self.with_mut(f)
    }

    /// Run `f` with an exclusive borrow, or return the conflict to the caller.
    ///
    /// The non-panicking form of [`update`](Self::update).
    ///
    /// # Errors
    ///
    /// Returns the [`Conflict`] if the cell cannot be borrowed exclusively.
    ///
    /// ```
    /// use borrow_lite::local;
    ///
    /// let cell = local(1i32);
    /// let held = cell.borrow();
    /// assert!(cell.try_update(|v| *v += 1).is_err());
    /// drop(held);
    /// assert_eq!(cell.try_update(|v| *v += 1), Ok(()));
    /// ```
    #[track_caller]
    pub fn try_update<R>(&self, f: impl FnOnce(&mut T) -> R) -> Result<R, Conflict> {
        self.try_with_mut(f)
    }

    /// Add `delta` to the value, with one method.
    ///
    /// The operator-shaped form of [`update`](Self::update): `cell.modify(5)`
    /// is `cell.update(|v| *v += 5)`, and the `+=` operator on the handle is the
    /// same call. It takes the same exclusive borrow, released before it
    /// returns, so it cannot leak. For subtraction use the `-=` operator (or
    /// [`update`](Self::update) with `-=`); a method cannot tell an add from a
    /// subtract without knowing the sign of `T`.
    ///
    /// ```
    /// use borrow_lite::local;
    ///
    /// let counter = local(10i32);
    /// counter.modify(5);
    /// assert_eq!(counter.map(|v| *v), 15);
    ///
    /// let mut cell = local(10i32);
    /// cell += 5; // the operator, same borrow
    /// cell -= 3;
    /// assert_eq!(cell.map(|v| *v), 12);
    /// ```
    ///
    /// # Panics
    ///
    /// Panics with a located message if the cell is already borrowed. Use
    /// [`try_modify`](Self::try_modify) for that case as a `Result`.
    #[track_caller]
    pub fn modify(&self, delta: T)
    where
        T: Sized + core::ops::AddAssign<T>,
    {
        self.with_mut(|value| *value += delta);
    }

    /// Add `delta` to the value, or return the conflict to the caller.
    ///
    /// The non-panicking form of [`modify`](Self::modify).
    ///
    /// # Errors
    ///
    /// Returns the [`Conflict`] if the cell cannot be borrowed exclusively.
    ///
    /// ```
    /// use borrow_lite::local;
    ///
    /// let counter = local(10i32);
    /// let held = counter.borrow();
    /// assert!(counter.try_modify(5).is_err());
    /// drop(held);
    /// assert_eq!(counter.try_modify(5), Ok(()));
    /// assert_eq!(counter.map(|v| *v), 15);
    /// ```
    #[track_caller]
    pub fn try_modify(&self, delta: T) -> Result<(), Conflict>
    where
        T: Sized + core::ops::AddAssign<T>,
    {
        self.try_with_mut(|value| *value += delta)
    }

    /// Store a new value, without a closure.
    ///
    /// The assignment-shaped form: `cell.set(v)` where the closure form is
    /// `cell.with_mut(|slot| *slot = v)`. It is the same borrow, released when
    /// this call returns, so it cannot leak - the only difference is that there
    /// is nothing to type but the value.
    ///
    /// ```
    /// use borrow_lite::{local, shared};
    ///
    /// let counter = local(0i32);
    /// counter.set(5);
    /// assert_eq!(counter.get(), 5);
    ///
    /// let flag: borrow_lite::Shared<bool> = shared(false);
    /// flag.set(true);
    /// assert!(flag.get());
    /// ```
    ///
    /// # Panics
    ///
    /// Panics with a located message if the cell is already borrowed. Use
    /// [`try_set`](Self::try_set) for that case as a `Result`.
    #[track_caller]
    pub fn set(&self, value: T)
    where
        T: Sized,
    {
        // The same path as `with_mut(|slot| *slot = value)`: borrow (panicking
        // with a location on conflict) and assign in place. Going through
        // `try_set` instead would wrap the borrow in a `Result` and cost an
        // extra move of the guard on the hot path.
        *self.borrow_mut() = value;
    }

    /// Store a new value, without a closure, or report the conflict.
    ///
    /// The non-panicking form of [`set`](Self::set). Like
    /// [`try_replace`](Self::try_replace), the value is handed back on failure,
    /// so a refused assignment never loses it.
    ///
    /// # Errors
    ///
    /// Returns the [`Conflict`] and the unwritten `value` if the cell is
    /// already borrowed.
    ///
    /// ```
    /// use borrow_lite::local;
    ///
    /// let cell = local(1i32);
    /// let held = cell.borrow();
    /// let (conflict, value) = cell.try_set(2).unwrap_err();
    /// assert_eq!(value, 2); // handed back, not lost
    /// assert_eq!(conflict.kind(), borrow_lite::ConflictKind::Readers);
    /// drop(held);
    /// cell.try_set(2).unwrap();
    /// assert_eq!(cell.get(), 2);
    /// ```
    #[track_caller]
    pub fn try_set(&self, value: T) -> Result<(), (Conflict, T)>
    where
        T: Sized,
    {
        // Assign through the guard rather than going via `replace`: `replace`
        // would move the old value out of the cell and drop it in a temporary,
        // which is a wasted move for a large `T`. Assigning in place drops the
        // old value where it sits, and keeping the value owned here is what lets
        // it be handed back on a conflict.
        match self.try_borrow_mut() {
            Ok(mut guard) => {
                *guard = value;
                Ok(())
            }
            Err(conflict) => Err((conflict, value)),
        }
    }

    /// Replace the value, returning the old one.
    ///
    /// # Panics
    ///
    /// Panics with a located message if the cell is already borrowed. Use
    /// [`try_replace`](Self::try_replace) for that case as a `Result`.
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

    /// Replace the value, returning the old one, or the conflict and the value
    /// that was not stored.
    ///
    /// The non-panicking form of [`replace`](Self::replace). The value is given
    /// back on failure, so a refused replace never loses it.
    ///
    /// # Errors
    ///
    /// Returns the [`Conflict`] and the unwritten `value` if the cell is
    /// already borrowed.
    ///
    /// ```
    /// use borrow_lite::local;
    ///
    /// let cell = local(1u8);
    /// let held = cell.borrow();
    /// let (conflict, value) = cell.try_replace(2).unwrap_err();
    /// assert_eq!(value, 2); // handed back, not lost
    /// assert_eq!(conflict.kind(), borrow_lite::ConflictKind::Readers);
    /// drop(held);
    /// assert_eq!(cell.try_replace(3).unwrap(), 1);
    /// ```
    #[track_caller]
    pub fn try_replace(&self, value: T) -> Result<T, (Conflict, T)>
    where
        T: Sized,
    {
        // Borrow explicitly rather than through `try_with_mut`, so the value
        // stays owned here and can be handed back when the borrow is refused.
        match self.try_borrow_mut() {
            Ok(mut guard) => Ok(core::mem::replace(&mut *guard, value)),
            Err(conflict) => Err((conflict, value)),
        }
    }

    /// Replace the value with one computed from the old value.
    ///
    /// The same contract as
    /// [`RefCell::replace_with`](core::cell::RefCell::replace_with): `f` runs
    /// under an exclusive borrow and receives the slot by `&mut`, so it can move
    /// out of the old value without cloning, and its return becomes the new
    /// value. The value handed back is the one that was in the slot *when the
    /// replacement was written*, which is whatever `f` left behind - for a
    /// read-only `f` that is the original value.
    ///
    /// ```
    /// use borrow_lite::local;
    ///
    /// let cell = local(5u32);
    /// let old = cell.replace_with(|v| *v + 1);
    /// assert_eq!(old, 5);
    /// assert_eq!(cell.with(|v| *v), 6);
    /// ```
    ///
    /// A `f` that empties the slot hands back the empty value, because it ran
    /// first:
    ///
    /// ```
    /// use borrow_lite::local;
    ///
    /// let cell = local(vec![1, 2, 3]);
    /// let old = cell.replace_with(|v| core::mem::take(v));
    /// assert!(old.is_empty()); // `f` already took the vector
    /// assert_eq!(cell.with(|v| v.clone()), vec![1, 2, 3]); // the take moved it here
    /// ```
    ///
    /// # Panics
    ///
    /// Panics with a located message if the cell is already borrowed. Use
    /// [`try_replace_with`](Self::try_replace_with) for that case as a
    /// `Result`.
    #[track_caller]
    pub fn replace_with<F>(&self, f: F) -> T
    where
        T: Sized,
        F: FnOnce(&mut T) -> T,
    {
        let mut guard = self.borrow_mut();
        let replacement = f(&mut guard);
        core::mem::replace(&mut *guard, replacement)
    }

    /// Replace the value with one computed from the old value, or report the
    /// conflict and hand the closure back.
    ///
    /// The non-panicking form of [`replace_with`](Self::replace_with). The
    /// closure is returned on failure, so a refused replace never loses it -
    /// the same contract [`try_replace`](Self::try_replace) keeps for the
    /// value. Because the closure is handed back rather than dropped, the error
    /// type is `(Conflict, F)`; match on it rather than calling `unwrap_err`,
    /// which would need `F: Debug`.
    ///
    /// # Errors
    ///
    /// Returns the [`Conflict`] and the unwritten closure if the cell is
    /// already borrowed.
    ///
    /// ```
    /// use borrow_lite::local;
    ///
    /// let cell = local(1u8);
    /// let held = cell.borrow();
    /// let (conflict, f) = match cell.try_replace_with(|v| *v + 1) {
    ///     Ok(old) => panic!("expected a conflict, got {old}"),
    ///     Err(pair) => pair,
    /// };
    /// assert_eq!(conflict.kind(), borrow_lite::ConflictKind::Readers);
    /// drop(held);
    /// let old = match cell.try_replace_with(f) {
    ///     Ok(old) => old,
    ///     Err(_) => panic!("the retry must succeed"),
    /// };
    /// assert_eq!(old, 1);
    /// assert_eq!(cell.with(|v| *v), 2);
    /// ```
    #[track_caller]
    pub fn try_replace_with<F>(&self, f: F) -> Result<T, (Conflict, F)>
    where
        T: Sized,
        F: FnOnce(&mut T) -> T,
    {
        match self.try_borrow_mut() {
            Ok(mut guard) => {
                let replacement = f(&mut guard);
                Ok(core::mem::replace(&mut *guard, replacement))
            }
            Err(conflict) => Err((conflict, f)),
        }
    }

    /// Take the value out, leaving `T::default()` in its place.
    ///
    /// The same contract as [`RefCell::take`](core::cell::RefCell::take): a
    /// located panic if the cell is already borrowed, so the value is never
    /// lost to a conflict.
    ///
    /// ```
    /// use borrow_lite::local;
    ///
    /// let cell = local(vec![1, 2, 3]);
    /// assert_eq!(cell.take(), vec![1, 2, 3]);
    /// assert!(cell.with(Vec::is_empty));
    /// ```
    #[track_caller]
    pub fn take(&self) -> T
    where
        T: Sized + Default,
    {
        self.replace(T::default())
    }

    /// Take the value out, leaving `T::default()`, or report the conflict.
    ///
    /// The non-panicking form of [`take`](Self::take).
    ///
    /// # Errors
    ///
    /// Returns the [`Conflict`] if the cell is already borrowed.
    ///
    /// ```
    /// use borrow_lite::local;
    ///
    /// let cell = local(vec![1, 2, 3]);
    /// let held = cell.borrow();
    /// assert!(cell.try_take().is_err());
    /// drop(held);
    /// assert_eq!(cell.try_take().unwrap(), vec![1, 2, 3]);
    /// assert!(cell.with(Vec::is_empty));
    /// ```
    #[track_caller]
    pub fn try_take(&self) -> Result<T, Conflict>
    where
        T: Sized + Default,
    {
        self.try_replace(T::default())
            .map_err(|(conflict, _)| conflict)
    }

    /// Swap the values of two cells.
    ///
    /// The same contract as [`RefCell::swap`](core::cell::RefCell::swap): both
    /// cells must be unborrowed. Where this differs is the diagnostics - a
    /// refused swap names the cell that was in the way and the call site, and
    /// swapping a cell with *itself* is refused with a message that says so
    /// rather than a second-borrow report that blames `borrow_lite`'s own internals.
    ///
    /// ```
    /// use borrow_lite::{local, shared};
    ///
    /// let a = local(1u8);
    /// let b = local(2u8);
    /// a.swap(&b);
    /// assert_eq!(a.with(|v| *v), 2);
    /// assert_eq!(b.with(|v| *v), 1);
    ///
    /// let x = shared(String::from("x"));
    /// let y = shared(String::from("y"));
    /// x.swap(&y);
    /// assert_eq!(x.with(|v| v.clone()), "y");
    /// ```
    ///
    /// # Panics
    ///
    /// Panics with a located message if either cell is borrowed, or if `self`
    /// and `other` are the same cell. Use [`try_swap`](Self::try_swap) for the
    /// non-panicking form.
    #[track_caller]
    pub fn swap(&self, other: &Self)
    where
        T: Sized,
    {
        match self.try_swap(other) {
            Ok(()) => {}
            Err(conflict) => panic!("{}", conflict.into_panic_message()),
        }
    }

    /// Swap the values of two cells, or report the conflict.
    ///
    /// The non-panicking form of [`swap`](Self::swap).
    ///
    /// Swapping a cell with itself reports a
    /// [`Writer`](crate::ConflictKind::Writer) conflict: the first borrow is
    /// exclusive, so the second could never be granted. It is detected before
    /// either borrow is attempted, so the reported
    /// [`wanted`](Conflict::wanted) location is this call - not a line inside
    /// `borrow_lite` - and there is no [`holder`](Conflict::holder), because no borrow
    /// was actually in the way.
    ///
    /// # Errors
    ///
    /// Returns the [`Conflict`] if either cell is borrowed, or if `self` and
    /// `other` are the same cell.
    ///
    /// ```
    /// use borrow_lite::local;
    ///
    /// let a = local(1u8);
    /// let b = local(2u8);
    /// let held = a.borrow();
    /// let conflict = a.try_swap(&b).unwrap_err();
    /// assert_eq!(conflict.kind(), borrow_lite::ConflictKind::Readers);
    /// drop(held);
    /// assert!(a.try_swap(&b).is_ok());
    /// assert_eq!(a.with(|v| *v), 2);
    /// ```
    #[track_caller]
    pub fn try_swap(&self, other: &Self) -> Result<(), Conflict>
    where
        T: Sized,
    {
        // A cell cannot be swapped with itself. Detect it here so the report is
        // about the caller's mistake and points at the caller, instead of the
        // second borrow failing inside this function.
        if core::ptr::eq(self, other) {
            return Err(
                Conflict::new(crate::error::ConflictKind::Writer).with_wanted(borrow_site())
            );
        }
        let mut a = self.try_borrow_mut()?;
        let mut b = other.try_borrow_mut()?;
        core::mem::swap(&mut *a, &mut *b);
        Ok(())
    }
}

// The single-threaded cell: no `T: Sync` needed, because the handle is `!Send`
// and `!Sync` so nothing else can ever observe the value.
impl<T: ?Sized> Inner<T, LocalState> {
    /// Borrow the value, panicking with a located message if that is not
    /// possible right now.
    #[cfg_attr(feature = "diagnostics", track_caller)]
    pub fn borrow(&self) -> ReadGuard<'_, T, LocalState> {
        match self.state.try_shared(borrow_site()) {
            Ok(recorder) => ReadGuard::new(&self.state, self.value.get(), recorder),
            Err(conflict) => panic!("{}", conflict.into_panic_message()),
        }
    }

    /// Borrow the value, reporting the conflict instead of panicking.
    #[track_caller]
    pub fn try_borrow(&self) -> Result<ReadGuard<'_, T, LocalState>, Conflict> {
        self.state
            .try_shared(borrow_site())
            .map(|recorder| ReadGuard::new(&self.state, self.value.get(), recorder))
    }

    /// Run `f` with a shared borrow, released before this call returns.
    #[track_caller]
    pub fn with<R>(&self, f: impl FnOnce(&T) -> R) -> R {
        f(&self.borrow())
    }

    /// Run `f` with a shared borrow, or return the conflict to the caller.
    #[track_caller]
    pub fn try_with<R>(&self, f: impl FnOnce(&T) -> R) -> Result<R, Conflict> {
        self.try_borrow().map(|guard| f(&guard))
    }

    /// Project the value through `f`, with the borrow released before returning.
    ///
    /// The read-shaped counterpart of [`update`](Self::update): `cell.map(f)`
    /// is `cell.with(f)`, named for the transformation rather than the borrow.
    /// It is the form to reach for when the closure's job is to look at the
    /// value and hand something back - a sum, a length, a field - and the
    /// result must not borrow the cell:
    ///
    /// ```
    /// use borrow_lite::local;
    ///
    /// let stats = local(vec![1, 2, 3]);
    /// assert_eq!(stats.map(|v| v.iter().sum::<i32>()), 6);
    /// assert_eq!(stats.map(Vec::len), 3);
    /// ```
    ///
    /// The result `R` may not borrow the cell - `f` returns an owned value, or
    /// a reference to something with a longer life - so this cannot leak a
    /// borrow the way a guard can. To hand out a reference into the cell, take
    /// the guard with [`borrow`](Self::borrow) instead.
    ///
    /// # Panics
    ///
    /// Panics with a located message if the cell is already borrowed. Use
    /// [`try_map`](Self::try_map) for that case as a `Result`.
    #[track_caller]
    pub fn map<R>(&self, f: impl FnOnce(&T) -> R) -> R {
        self.with(f)
    }

    /// Project the value through `f`, or return the conflict to the caller.
    ///
    /// The non-panicking form of [`map`](Self::map).
    ///
    /// # Errors
    ///
    /// Returns the [`Conflict`] if the value cannot be borrowed.
    ///
    /// ```
    /// use borrow_lite::local;
    ///
    /// let stats = local(vec![1, 2, 3]);
    /// let held = stats.borrow_mut();
    /// assert!(stats.try_map(Vec::len).is_err());
    /// drop(held);
    /// assert_eq!(stats.try_map(Vec::len), Ok(3));
    /// ```
    #[track_caller]
    pub fn try_map<R>(&self, f: impl FnOnce(&T) -> R) -> Result<R, Conflict> {
        self.try_with(f)
    }

    /// Copy the value out, without a closure.
    ///
    /// The read-shaped counterpart of [`set`](Self::set): `cell.get()` where
    /// the closure form is `cell.with(|v| v.clone())`. It requires `T: Clone`
    /// because the value is handed back by value, and it is the same shared
    /// borrow the closure form takes - so a `Copy` type reads exactly like
    /// [`Cell::get`](core::cell::Cell::get), but a conflict panics with a
    /// location instead of being silently ignored.
    ///
    /// ```
    /// use borrow_lite::{inline, local, shared};
    ///
    /// let counter = local(vec![1, 2, 3]);
    /// assert_eq!(counter.get(), vec![1, 2, 3]);
    ///
    /// let cell = inline(7u8);
    /// assert_eq!(cell.get(), 7);
    ///
    /// let flag: borrow_lite::Shared<bool> = shared(true);
    /// assert!(flag.get());
    /// ```
    ///
    /// # Panics
    ///
    /// Panics with a located message if the value cannot be borrowed. Use
    /// [`try_get`](Self::try_get) for that case as a `Result`.
    #[track_caller]
    pub fn get(&self) -> T
    where
        T: Clone,
    {
        T::clone(&self.borrow())
    }

    /// Copy the value out, without a closure, or report the conflict.
    ///
    /// The non-panicking form of [`get`](Self::get).
    ///
    /// # Errors
    ///
    /// Returns the [`Conflict`] if the value cannot be borrowed.
    ///
    /// ```
    /// use borrow_lite::local;
    ///
    /// let cell = local(1i32);
    /// let held = cell.borrow_mut();
    /// assert!(cell.try_get().is_err());
    /// drop(held);
    /// assert_eq!(cell.try_get().unwrap(), 1);
    /// ```
    #[track_caller]
    pub fn try_get(&self) -> Result<T, Conflict>
    where
        T: Clone,
    {
        self.try_with(T::clone)
    }

    /// Compare two cells for equality, reporting a conflict instead of
    /// panicking.
    ///
    /// Two handles to the *same* cell compare equal without looking at the
    /// value, so a cell compared with itself never conflicts. Otherwise the
    /// values are compared under a shared borrow, and a conflict comes back as
    /// an error rather than a panic.
    ///
    /// The short-circuit runs for every `T: PartialEq`. `Rc`/`Arc` run theirs
    /// only when `T: Eq`, because they specialise on `Eq` ("a `PartialEq` might
    /// be deliberately irreflexive"); so for a payload like `f64` a self
    /// comparison here is `Ok(true)` where `Rc` would borrow the value (and
    /// panic under a live borrow). See [`Local`](crate::Local)'s `PartialEq`.
    ///
    /// # Errors
    ///
    /// Returns the [`Conflict`] if either value cannot be borrowed.
    #[track_caller]
    pub fn try_eq(&self, other: &Self) -> Result<bool, Conflict>
    where
        T: PartialEq,
    {
        if core::ptr::eq(self, other) {
            return Ok(true);
        }
        // Take both guards here rather than nesting `try_with` closures: the
        // borrows then happen in this `#[track_caller]` frame, so a refusal
        // names the caller of `try_eq`, not a line inside this method.
        let a = self.try_borrow()?;
        let b = other.try_borrow()?;
        Ok(*a == *b)
    }

    /// Compare two cells, reporting a conflict instead of panicking.
    ///
    /// The ordering counterpart of [`try_eq`](Inner::try_eq), with the same
    /// short-circuit: two handles to the *same* cell are `Ok(Some(Equal))`
    /// without looking at the value. Otherwise the values are compared under a
    /// shared borrow.
    ///
    /// # Errors
    ///
    /// Returns the [`Conflict`] if either value cannot be borrowed.
    #[track_caller]
    pub fn try_cmp(&self, other: &Self) -> Result<Option<core::cmp::Ordering>, Conflict>
    where
        T: PartialOrd,
    {
        if core::ptr::eq(self, other) {
            return Ok(Some(core::cmp::Ordering::Equal));
        }
        let a = self.try_borrow()?;
        let b = other.try_borrow()?;
        Ok(a.partial_cmp(&b))
    }
}

// The atomic cell: a shared borrow hands out `&T` that another thread can
// observe at the same time, so it needs `T: Sync`.
impl<T: ?Sized + Sync> Inner<T, AtomicState> {
    /// Borrow the value, panicking with a located message if that is not
    /// possible right now.
    #[track_caller]
    pub fn borrow(&self) -> ReadGuard<'_, T, AtomicState> {
        match self.state.try_shared(borrow_site()) {
            Ok(recorder) => ReadGuard::new(&self.state, self.value.get(), recorder),
            Err(conflict) => panic!("{}", conflict.into_panic_message()),
        }
    }

    /// Borrow the value, reporting the conflict instead of panicking.
    #[track_caller]
    pub fn try_borrow(&self) -> Result<ReadGuard<'_, T, AtomicState>, Conflict> {
        self.state
            .try_shared(borrow_site())
            .map(|recorder| ReadGuard::new(&self.state, self.value.get(), recorder))
    }

    /// Run `f` with a shared borrow, released before this call returns.
    #[track_caller]
    pub fn with<R>(&self, f: impl FnOnce(&T) -> R) -> R {
        f(&self.borrow())
    }

    /// Run `f` with a shared borrow, or return the conflict to the caller.
    #[track_caller]
    pub fn try_with<R>(&self, f: impl FnOnce(&T) -> R) -> Result<R, Conflict> {
        self.try_borrow().map(|guard| f(&guard))
    }

    /// Project the value through `f`, with the borrow released before returning.
    ///
    /// The read-shaped counterpart of [`update`](Self::update): `cell.map(f)`
    /// is `cell.with(f)`, named for the transformation rather than the borrow.
    /// It is the form to reach for when the closure's job is to look at the
    /// value and hand something back - a sum, a length, a field - and the
    /// result must not borrow the cell:
    ///
    /// ```
    /// use borrow_lite::local;
    ///
    /// let stats = local(vec![1, 2, 3]);
    /// assert_eq!(stats.map(|v| v.iter().sum::<i32>()), 6);
    /// assert_eq!(stats.map(Vec::len), 3);
    /// ```
    ///
    /// The result `R` may not borrow the cell - `f` returns an owned value, or
    /// a reference to something with a longer life - so this cannot leak a
    /// borrow the way a guard can. To hand out a reference into the cell, take
    /// the guard with [`borrow`](Self::borrow) instead.
    ///
    /// # Panics
    ///
    /// Panics with a located message if the cell is already borrowed. Use
    /// [`try_map`](Self::try_map) for that case as a `Result`.
    #[track_caller]
    pub fn map<R>(&self, f: impl FnOnce(&T) -> R) -> R {
        self.with(f)
    }

    /// Project the value through `f`, or return the conflict to the caller.
    ///
    /// The non-panicking form of [`map`](Self::map).
    ///
    /// # Errors
    ///
    /// Returns the [`Conflict`] if the value cannot be borrowed.
    ///
    /// ```
    /// use borrow_lite::local;
    ///
    /// let stats = local(vec![1, 2, 3]);
    /// let held = stats.borrow_mut();
    /// assert!(stats.try_map(Vec::len).is_err());
    /// drop(held);
    /// assert_eq!(stats.try_map(Vec::len), Ok(3));
    /// ```
    #[track_caller]
    pub fn try_map<R>(&self, f: impl FnOnce(&T) -> R) -> Result<R, Conflict> {
        self.try_with(f)
    }

    /// Copy the value out, without a closure.
    ///
    /// The read-shaped counterpart of [`set`](Self::set): `cell.get()` where
    /// the closure form is `cell.with(|v| v.clone())`. It requires `T: Clone`
    /// because the value is handed back by value, and it is the same shared
    /// borrow the closure form takes - so a `Copy` type reads exactly like
    /// [`Cell::get`](core::cell::Cell::get), but a conflict panics with a
    /// location instead of being silently ignored.
    ///
    /// ```
    /// use borrow_lite::{inline, local, shared};
    ///
    /// let counter = local(vec![1, 2, 3]);
    /// assert_eq!(counter.get(), vec![1, 2, 3]);
    ///
    /// let cell = inline(7u8);
    /// assert_eq!(cell.get(), 7);
    ///
    /// let flag: borrow_lite::Shared<bool> = shared(true);
    /// assert!(flag.get());
    /// ```
    ///
    /// # Panics
    ///
    /// Panics with a located message if the value cannot be borrowed. Use
    /// [`try_get`](Self::try_get) for that case as a `Result`.
    #[track_caller]
    pub fn get(&self) -> T
    where
        T: Clone,
    {
        T::clone(&self.borrow())
    }

    /// Copy the value out, without a closure, or report the conflict.
    ///
    /// The non-panicking form of [`get`](Self::get).
    ///
    /// # Errors
    ///
    /// Returns the [`Conflict`] if the value cannot be borrowed.
    ///
    /// ```
    /// use borrow_lite::local;
    ///
    /// let cell = local(1i32);
    /// let held = cell.borrow_mut();
    /// assert!(cell.try_get().is_err());
    /// drop(held);
    /// assert_eq!(cell.try_get().unwrap(), 1);
    /// ```
    #[track_caller]
    pub fn try_get(&self) -> Result<T, Conflict>
    where
        T: Clone,
    {
        self.try_with(T::clone)
    }

    /// Compare two cells for equality, reporting a conflict instead of
    /// panicking.
    ///
    /// Two handles to the *same* cell compare equal without looking at the
    /// value, so a cell compared with itself never conflicts. Otherwise the
    /// values are compared under a shared borrow, and a conflict comes back as
    /// an error rather than a panic.
    ///
    /// The short-circuit runs for every `T: PartialEq`. `Rc`/`Arc` run theirs
    /// only when `T: Eq`, because they specialise on `Eq` ("a `PartialEq` might
    /// be deliberately irreflexive"); so for a payload like `f64` a self
    /// comparison here is `Ok(true)` where `Rc` would borrow the value (and
    /// panic under a live borrow). See [`Local`](crate::Local)'s `PartialEq`.
    ///
    /// # Errors
    ///
    /// Returns the [`Conflict`] if either value cannot be borrowed.
    #[track_caller]
    pub fn try_eq(&self, other: &Self) -> Result<bool, Conflict>
    where
        T: PartialEq,
    {
        if core::ptr::eq(self, other) {
            return Ok(true);
        }
        // Explicit guards, not nested `try_with` closures: see the
        // single-threaded cell's `try_eq` for why (caller location).
        let a = self.try_borrow()?;
        let b = other.try_borrow()?;
        Ok(*a == *b)
    }

    /// Compare two cells, reporting a conflict instead of panicking.
    ///
    /// See [`Inner::try_cmp`] on the single-threaded cell for the contract.
    ///
    /// # Errors
    ///
    /// Returns the [`Conflict`] if either value cannot be borrowed.
    #[track_caller]
    pub fn try_cmp(&self, other: &Self) -> Result<Option<core::cmp::Ordering>, Conflict>
    where
        T: PartialOrd,
    {
        if core::ptr::eq(self, other) {
            return Ok(Some(core::cmp::Ordering::Equal));
        }
        let a = self.try_borrow()?;
        let b = other.try_borrow()?;
        Ok(a.partial_cmp(&b))
    }
}
