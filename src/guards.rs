//! RAII guards: the only way to reach the value inside a cell.
//!
//! Guards are deliberately `!Send`: a guard is a borrow of a cell that lives
//! somewhere specific, and letting it travel to another thread makes the
//! "who is holding this" question much harder to answer for no real gain.
//! Use [`with`](crate::Inner::with) inside the worker instead.
#![allow(unsafe_code)]

use core::fmt;
use core::marker::PhantomData;
use core::ops::{Deref, DerefMut};
use core::ptr::NonNull;

use crate::state::State;

/// A shared borrow of a cell.
///
/// Created by [`borrow`](crate::Inner::borrow) and friends; derefs to `&T`
/// and releases the borrow when dropped.
#[must_use = "a guard that is dropped immediately borrows nothing"]
pub struct ReadGuard<'a, T: ?Sized, S: State> {
    state: &'a S,
    ptr: NonNull<T>,
    _not_send: PhantomData<*const ()>,
}

impl<'a, T: ?Sized, S: State> ReadGuard<'a, T, S> {
    pub(crate) fn new(state: &'a S, ptr: *const T) -> Self {
        // SAFETY: `Inner` hands out `UnsafeCell::get()` pointers, which are
        // never null.
        let ptr = unsafe { NonNull::new_unchecked(ptr.cast_mut()) };
        Self {
            state,
            ptr,
            _not_send: PhantomData,
        }
    }

    /// Narrow the borrow to a part of the value, keeping it alive.
    ///
    /// If `f` panics the borrow is released, not leaked: `f` runs before the
    /// guard is consumed, so unwinding drops it normally. The cell is usable
    /// again once the panic is caught.
    ///
    /// ```
    /// use lite::{Local, ReadGuard};
    ///
    /// let cell = Local::new((1u32, String::from("x")));
    /// let second: ReadGuard<'_, str, _> = ReadGuard::map(cell.borrow(), |t| t.1.as_str());
    /// assert_eq!(&*second, "x");
    /// ```
    pub fn map<U: ?Sized, F>(guard: Self, f: F) -> ReadGuard<'a, U, S>
    where
        F: FnOnce(&T) -> &U,
    {
        // SAFETY: the guard keeps the cell shared-borrowed for `'a`, and the
        // reference produced by `f` is derived from the value it points at.
        let mapped: *const U = core::ptr::from_ref(f(unsafe { &*guard.ptr.as_ptr() }));
        let state = guard.state;
        core::mem::forget(guard);
        ReadGuard {
            state,
            // SAFETY: as above; `mapped` cannot be null because it is a reference.
            ptr: unsafe { NonNull::new_unchecked(mapped.cast_mut()) },
            _not_send: PhantomData,
        }
    }
}

impl<T: ?Sized, S: State> Deref for ReadGuard<'_, T, S> {
    type Target = T;

    fn deref(&self) -> &T {
        // SAFETY: the cell is shared-borrowed for as long as this guard lives,
        // so nothing can be mutating the value, and the pointer is valid.
        unsafe { &*self.ptr.as_ptr() }
    }
}

impl<T: ?Sized + fmt::Debug, S: State> fmt::Debug for ReadGuard<'_, T, S> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&**self, f)
    }
}

impl<T: ?Sized + fmt::Display, S: State> fmt::Display for ReadGuard<'_, T, S> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&**self, f)
    }
}

impl<T: ?Sized, S: State> Drop for ReadGuard<'_, T, S> {
    fn drop(&mut self) {
        self.state.end_shared();
    }
}

impl<T: ?Sized, S: State> Clone for ReadGuard<'_, T, S> {
    /// Clone the guard, taking a second shared borrow.
    ///
    /// This cannot fail: the guard being cloned already holds a shared borrow,
    /// so a second one is always compatible. The clone releases its own borrow
    /// when it is dropped, and the cell is free only once every clone is gone -
    /// the same contract as [`Ref::clone`](core::cell::Ref::clone).
    ///
    /// ```
    /// use lite::local;
    ///
    /// let cell = local(vec![1, 2, 3]);
    /// let first = cell.borrow();
    /// let second = first.clone();
    /// // Both are live, so the cell is still read-borrowed...
    /// assert!(cell.try_borrow_mut().is_err());
    /// drop(second);
    /// assert!(cell.try_borrow_mut().is_err()); // ...until the last one goes.
    /// drop(first);
    /// assert!(cell.try_borrow_mut().is_ok());
    /// ```
    fn clone(&self) -> Self {
        self.state.add_shared();
        Self {
            state: self.state,
            ptr: self.ptr,
            _not_send: PhantomData,
        }
    }
}

/// An exclusive borrow of a cell.
///
/// Created by [`borrow_mut`](crate::Inner::borrow_mut) and friends; derefs to
/// `&mut T` and releases the borrow when dropped.
#[must_use = "a guard that is dropped immediately borrows nothing"]
pub struct WriteGuard<'a, T: ?Sized, S: State> {
    state: &'a S,
    ptr: NonNull<T>,
    _not_send: PhantomData<*const ()>,
}

impl<'a, T: ?Sized, S: State> WriteGuard<'a, T, S> {
    pub(crate) fn new(state: &'a S, ptr: *mut T) -> Self {
        // SAFETY: `Inner` hands out `UnsafeCell::get()` pointers, which are
        // never null.
        let ptr = unsafe { NonNull::new_unchecked(ptr) };
        Self {
            state,
            ptr,
            _not_send: PhantomData,
        }
    }

    /// Narrow the borrow to a part of the value, keeping it alive.
    ///
    /// If `f` panics the borrow is released, not leaked: `f` runs before the
    /// guard is consumed, so unwinding drops it normally. The cell is usable
    /// again once the panic is caught.
    ///
    /// ```
    /// use lite::{Local, WriteGuard};
    ///
    /// let cell = Local::new((1u32, 2u32));
    /// {
    ///     let mut first: WriteGuard<'_, u32, _> = WriteGuard::map(cell.borrow_mut(), |t| &mut t.0);
    ///     *first = 10;
    /// }
    /// assert_eq!(cell.with(|t| *t), (10, 2));
    /// ```
    pub fn map<U: ?Sized, F>(guard: Self, f: F) -> WriteGuard<'a, U, S>
    where
        F: FnOnce(&mut T) -> &mut U,
    {
        // SAFETY: the guard holds the exclusive borrow for `'a`, so no other
        // reference to the value exists while `f` runs.
        let mapped: *mut U = core::ptr::from_mut(f(unsafe { &mut *guard.ptr.as_ptr() }));
        let state = guard.state;
        core::mem::forget(guard);
        WriteGuard {
            state,
            // SAFETY: as above; `mapped` cannot be null because it is a reference.
            ptr: unsafe { NonNull::new_unchecked(mapped) },
            _not_send: PhantomData,
        }
    }
}

impl<T: ?Sized, S: State> Deref for WriteGuard<'_, T, S> {
    type Target = T;

    fn deref(&self) -> &T {
        // SAFETY: the cell is exclusively borrowed for as long as this guard
        // lives, so nothing else can be reading or writing the value.
        unsafe { &*self.ptr.as_ptr() }
    }
}

impl<T: ?Sized, S: State> DerefMut for WriteGuard<'_, T, S> {
    fn deref_mut(&mut self) -> &mut T {
        // SAFETY: exclusive borrow, as above, and this guard is the only thing
        // holding it.
        unsafe { &mut *self.ptr.as_ptr() }
    }
}

impl<T: ?Sized + fmt::Debug, S: State> fmt::Debug for WriteGuard<'_, T, S> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&**self, f)
    }
}

impl<T: ?Sized + fmt::Display, S: State> fmt::Display for WriteGuard<'_, T, S> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&**self, f)
    }
}

impl<T: ?Sized, S: State> Drop for WriteGuard<'_, T, S> {
    fn drop(&mut self) {
        self.state.end_exclusive();
    }
}
