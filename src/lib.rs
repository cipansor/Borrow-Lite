//! # `borrow_lite`
//!
//! Three everyday shapes of Rust ownership, made hard to get wrong:
//!
//! * [`Inline<T>`] - one owner, value stored inline (the `RefCell<T>` shape,
//!   and the cheapest of the three).
//! * [`Local<T>`] - shared, mutable data inside one thread (the
//!   `Rc<RefCell<T>>` shape).
//! * [`Shared<T>`] - shared, mutable data across threads (the
//!   `Arc<RwLock<T>>` shape).
//!
//! All three hand out the same borrows, report the same [`Conflict`]s, and
//! differ only in where the value lives and what that lets you do with it:
//!
//! | | value lives | handle is | cost per cell |
//! |---|---|---|---|
//! | [`Inline<T>`] | in the struct | `Send` when `T: Send`, `!Sync` | none |
//! | [`Local<T>`] | behind an `Rc` | `!Send`, `!Sync` | one allocation |
//! | [`Shared<T>`] | behind an `Arc` | `Send + Sync` when `T` is | one allocation + atomics |
//!
//! It is not a borrow checker replacement, and it does not make borrows
//! disappear. What it does is remove the three failure modes that make those
//! shapes annoying in practice:
//!
//! | Problem | `borrow_lite` |
//! |---|---|
//! | `RefCell::borrow_mut()` panics with `already borrowed: BorrowMutError`, which names neither the culprit nor the victim | the panic message carries the *source location* of both the borrow that is in the way and the borrow that was refused, plus how many borrows are active when readers are what is in the way |
//! | `RwLock` blocks forever, or poisons, when the same thread re-enters | every borrow has a `try_` twin, and [`Conflict`] is a plain `Error` you can `?` on, so a conflict is a recoverable error instead of a deadlock |
//! | guards leak into long scopes, so the next borrow panics far away from the cause | [`with`](Inner::with) / [`with_mut`](Inner::with_mut) run a closure and release the borrow before returning |
//!
//! ## Before and after
//!
//! The classic pain, with `Rc<RefCell<T>>`:
//!
//! ```should_panic
//! use std::cell::RefCell;
//! use std::rc::Rc;
//!
//! let stats = Rc::new(RefCell::new(vec![1, 2, 3]));
//! let reader = stats.borrow(); // read borrow, still alive
//! stats.borrow_mut().push(4); // panicked at 'already borrowed: BorrowMutError'
//! # let _ = reader;
//! ```
//!
//! The same code with [`Local`], where the panic tells you what to do:
//!
//! ```should_panic
//! use borrow_lite::Local;
//!
//! let stats = Local::new(vec![1, 2, 3]);
//! let guard = stats.borrow();
//! stats.borrow_mut().push(4);
//! // panicked at borrow_lite: cell is already immutably borrowed
//! //   (borrow attempted at src/main.rs:5:7); held at src/main.rs:4:15
//! //   (1 active borrow) - a borrow is still alive. Drop the guard, or use
//! //   `with`/`with_mut` so it cannot leak.
//! ```
//!
//! And the form that never gets into that state in the first place:
//!
//! ```
//! use borrow_lite::Local;
//!
//! let stats = Local::new(vec![1, 2, 3]);
//! let total: i32 = stats.with(|values| values.iter().sum());
//! stats.with_mut(|values| values.push(4));
//! assert_eq!((total, stats.with(|v| v.len())), (6, 4));
//! ```
//!
//! ## Conflicts as errors
//!
//! ```no_run
//! use borrow_lite::{Conflict, Local};
//!
//! fn bump(counter: &Local<i32>) -> Result<(), Conflict> {
//!     *counter.try_borrow_mut()? += 1;
//!     Ok(())
//! }
//!
//! let counter = Local::new(0);
//! bump(&counter).unwrap();
//! assert_eq!(counter.with(|c| *c), 1);
//! ```
//!
//! ## Across threads
//!
//! [`Shared<T>`] is `Send + Sync` exactly when `T` is, which is the same
//! contract as `Arc<RwLock<T>>` - but reads go through a shared borrow rather
//! than a blocking lock, and a failed borrow is an error rather than a
//! deadlock:
//!
//! ```
//! use borrow_lite::{Shared, shared};
//!
//! let counter = shared(0usize);
//! std::thread::scope(|scope| {
//!     for _ in 0..4 {
//!         scope.spawn(|| {
//!             // Borrows do not block, so retry when another thread holds one.
//!             while counter.try_with_mut(|c| *c += 1).is_err() {
//!                 std::thread::yield_now();
//!             }
//!         });
//!     }
//! });
//! assert_eq!(counter.with(|c| *c), 4);
//! ```
//!
//! ## Compile-time guarantees
//!
//! Two of the crate's safety properties are enforced by the type system rather
//! than by tests, so they are shown here as examples that must *not* compile.
//! A `Local` cell is single-threaded: the handle cannot leave the thread, which
//! is what makes the plain-`Cell` bookkeeping sound.
//!
//! ```compile_fail
//! use borrow_lite::Local;
//!
//! let cell = Local::new(0u8);
//! std::thread::spawn(move || {
//!     // error: `Local<u8>` cannot be sent between threads safely
//!     let _ = cell.with(|v| *v);
//! });
//! ```
//!
//! A guard borrows a cell that lives somewhere specific, so it cannot travel to
//! another thread either - even a `Shared` one, whose handle is `Send`.
//!
//! ```compile_fail
//! use borrow_lite::{shared, Shared};
//!
//! let cell: Shared<u8> = shared(0);
//! let guard = cell.borrow();
//! std::thread::spawn(move || {
//!     // error: the guard is `!Send`
//!     let _ = *guard;
//! });
//! ```
//!
//! ## What it does not do
//!
//! * It does not check borrows at compile time. For that, look at `qcell` or
//!   `ghost-cell`, which trade flexibility for static guarantees.
//! * It does not remove the need for `unsafe`; the crate has a small amount of
//!   it, confined to two modules, and the rest is safe code on top.
//! * It is not a lock. Borrows are not blocking, so a busy cell reports a
//!   conflict instead of waiting. If you want waiting, use a real lock. When
//!   you do retry, retry with `std::thread::yield_now()` rather than a spin
//!   loop: measured under contention, yielding beats both spinning and
//!   `RwLock`, while spinning is the slowest option because it burns the core
//!   it needs. See the benchmarks in the repository.

#![cfg_attr(not(any(loom, feature = "std")), no_std)]
// `docsrs` is set by docs.rs (via the manifest metadata below) and by the CI
// job that renders the docs the way docs.rs will. It turns on `doc_cfg` so the
// `std`-gated items below are labelled with the feature they need instead of
// silently vanishing from the rendered docs.
#![cfg_attr(docsrs, feature(doc_cfg))]
#![deny(missing_docs)]
#![warn(clippy::pedantic)]
#![allow(clippy::module_name_repetitions)]
#![deny(unsafe_op_in_unsafe_fn)]

extern crate alloc;

use core::ops::Deref;

use crate::sync::{Arc, Rc};

mod error;
mod guards;
#[doc(hidden)]
pub mod inner;
#[doc(hidden)]
pub mod state;
#[cfg(feature = "std")]
mod wait;

mod macros;
mod observ;
mod sync;

pub use crate::error::{Conflict, ConflictKind};
pub use crate::guards::{ReadGuard, WriteGuard};
pub use crate::inner::Inner;
pub use crate::observ::{MAX_NOTIFICATION_ROUNDS, Observable, ObserverId};
#[cfg(feature = "std")]
#[cfg_attr(docsrs, doc(cfg(feature = "std")))]
pub use crate::wait::{retry, wait};

/// Names the [`cell!`](crate::cell) expansion refers to, re-exported so that the
/// macro resolves them through `$crate` no matter where it is invoked.
///
/// This is an implementation detail of the macro and not part of the API.
#[doc(hidden)]
pub mod __macro_support {
    pub use crate::{Inline, Local, ReadGuard, Shared, WriteGuard, inline, local, shared};
    /// Re-exported so the `cell!` default-setter form can build `<field>_mut`
    /// from `<field>` through `$crate`, wherever the macro is invoked. Only
    /// present with the `paste` feature.
    #[cfg(feature = "paste")]
    pub use ::paste;
}

/// Shared, mutable data that lives in one thread.
///
/// A `Local<T>` is a handle to a cell. Cloning it clones the *handle*, not the
/// value, and every clone sees the same data:
///
/// ```
/// use borrow_lite::Local;
///
/// let a = Local::new(String::from("hi"));
/// let b = a.clone();
/// b.with_mut(|s| s.push('!'));
/// assert_eq!(a.with(|s| s.clone()), "hi!");
/// ```
///
/// The handle is `!Send` and `!Sync` (it wraps an `Rc`), so a single-threaded
/// cell cannot accidentally cross a thread boundary. That bound is what makes
/// the unsynchronised borrow bookkeeping sound, and it is checked here rather
/// than asserted in prose:
///
/// ```compile_fail
/// fn is_send<T: Send>() {}
/// is_send::<borrow_lite::Local<u8>>();
/// ```
///
/// ```compile_fail
/// fn is_sync<T: Sync>() {}
/// is_sync::<borrow_lite::Local<u8>>();
/// ```
///
/// Borrowing methods ([`borrow`](Inner::borrow), [`with`](Inner::with),
/// [`with_mut`](Inner::with_mut), and their `try_` twins) come from the
/// [`Deref`] target.
pub struct Local<T: ?Sized>(Rc<Inner<T, state::LocalState>>);

/// Shared, mutable data that can be used from several threads.
///
/// `Shared<T>` is `Send + Sync` when `T` is, and behaves like an `Arc` whose
/// contents can be borrowed mutably. Cloning clones the handle:
///
/// ```
/// use borrow_lite::{shared, Shared};
///
/// let a: Shared<u8> = shared(0);
/// let b = a.clone();
/// b.with_mut(|v| *v += 1);
/// assert_eq!(a.with(|v| *v), 1);
/// ```
///
/// A `T` that is not `Sync` can still live in a `Shared`, but such a cell is
/// usable from one thread only: the same contract as `Arc<RwLock<T>>`, where
/// the handle is `Send + Sync` exactly when `T: Send + Sync`. The negative half
/// of that contract is what the `unsafe impl Sync` for [`Inner`] rests on, so
/// it is checked:
///
/// ```compile_fail
/// // `Cell<u8>` is `Send` but not `Sync`, so the handle must not be `Sync`.
/// fn is_sync<T: Sync>() {}
/// is_sync::<borrow_lite::Shared<std::cell::Cell<u8>>>();
/// ```
///
/// ```compile_fail
/// // Nor may a `!Send` payload travel: `Rc<u8>` is not `Send`.
/// fn is_send<T: Send>() {}
/// is_send::<borrow_lite::Shared<std::rc::Rc<u8>>>();
/// ```
///
/// ```compile_fail
/// // A `Sync` but `!Send` payload must not be shareable either. `MutexGuard`
/// // is `Sync` (when its `T` is) but `!Send`; sharing the cell would let another
/// // thread take `&mut` and drop or move the guard on the wrong thread, which is
/// // exactly what `!Send` forbids. So `Shared` needs `T: Send + Sync`, not just
/// // `T: Sync`.
/// fn is_sync<T: Sync>() {}
/// is_sync::<borrow_lite::Shared<std::sync::MutexGuard<'static, u8>>>();
/// ```
///
/// The same hole through the [`Deref`] target rather than the handle: reaching
/// `&Inner` directly and sending *that* to a scoped thread is refused too.
///
/// ```compile_fail
/// use borrow_lite::{shared, Inner, state::AtomicState};
/// use std::sync::{Mutex, MutexGuard};
///
/// let m: &'static Mutex<u8> = Box::leak(Box::new(Mutex::new(1)));
/// let cell = shared(m.lock().unwrap());
/// let inner: &Inner<MutexGuard<'static, u8>, AtomicState> = &cell;
/// std::thread::scope(|s| {
///     s.spawn(move || {
///         // error: `MutexGuard` cannot be sent between threads safely
///         inner.with_mut(|slot| drop(std::mem::replace(slot, m.lock().unwrap())));
///     });
/// });
/// ```
pub struct Shared<T: ?Sized>(Arc<Inner<T, state::AtomicState>>);

/// Shared, mutable data stored inside the value itself.
///
/// `Inline<T>` is the [`RefCell`](core::cell::RefCell)-shaped cell: the borrow
/// flags and the value sit together in one struct, with no heap allocation and
/// no reference counting. It is the cheapest cell here, and the one to reach
/// for when a single owner holds the value and nothing is shared:
///
/// ```
/// use borrow_lite::Inline;
///
/// let cell = Inline::new(vec![1, 2, 3]);
/// cell.with_mut(|v| v.push(4));
/// assert_eq!(cell.with(Vec::len), 4);
/// ```
///
/// # What it costs
///
/// [`Local`] pays for an `Rc` and the pointer hop that comes with it; `Inline`
/// pays for neither. Measured on a parse-and-walk workload, `Inline` and
/// `RefCell` are indistinguishable (the workload is dominated by the work, not
/// the borrows) while [`Local`] is about 25% slower, because every cell is a
/// separate allocation.
///
/// What is left is the one thing `RefCell` does not do: recording *where* each
/// borrow was taken. That is a store per borrow, and in a tight loop it is
/// measurable - roughly 0.3 ns on top of `RefCell`'s ~1.2 ns - but it is
/// invisible in any real workload, and it is what buys the message below. If a
/// borrow conflict is what you are trying to debug, that is the trade.
///
/// The bookkeeping also takes more room: `Inline<u64>` is 24 bytes against
/// `RefCell<u64>`'s 16, because it carries a borrow count, an exclusive flag and
/// the holder's location instead of a single flag. Without the `diagnostics`
/// feature the holder is dropped and it is the same 16 bytes as `RefCell<u64>`.
/// For a struct with a handful of cells that is nothing; for an array of
/// millions of them it is the reason to use [`Cell`](core::cell::Cell) or
/// `RefCell` instead.
///
/// ```
/// use borrow_lite::Inline;
///
/// let cell = Inline::new(1u8);
/// let borrowed_at = line!() + 1; // the `borrow()` below
/// let held = cell.borrow();
/// let conflict = cell.try_borrow_mut().unwrap_err();
/// // The message names the file and line of the borrow that is in the way.
/// # #[cfg(feature = "diagnostics")]
/// assert!(conflict.to_string().contains("held at"));
/// # #[cfg(feature = "diagnostics")]
/// assert_eq!(conflict.holder().map(|l| l.line()), Some(borrowed_at));
/// # #[cfg(not(feature = "diagnostics"))]
/// assert!(conflict.to_string().contains("already immutably borrowed"));
/// # let _ = borrowed_at;
/// drop(held);
/// ```
///
/// # Threads
///
/// `Inline<T>` is `Send` when `T` is, exactly like `RefCell<T>`, so a cell that
/// owns its value can move to another thread:
///
/// ```
/// use borrow_lite::Inline;
///
/// let cell = Inline::new(String::from("hi"));
/// let seen = std::thread::spawn(move || cell.with(String::clone)).join().unwrap();
/// assert_eq!(seen, "hi");
/// ```
///
/// It is `!Sync`, also like `RefCell<T>`: the flags are plain `Cell`s, so two
/// threads may never borrow one cell at once. To share a cell between threads,
/// use [`Shared`].
///
/// ```compile_fail
/// fn is_sync<T: Sync>() {}
/// is_sync::<borrow_lite::Inline<u8>>();
/// ```
///
/// Borrowing methods ([`borrow`](Inner::borrow), [`with`](Inner::with),
/// [`with_mut`](Inner::with_mut), and their `try_` twins) come from the
/// [`Deref`] target, [`Inner`], exactly as they do for [`Local`] and [`Shared`].
pub struct Inline<T: ?Sized>(Inner<T, state::LocalState>);

impl<T> Inline<T> {
    /// Create a cell that stores `value` inline.
    #[must_use]
    pub fn new(value: T) -> Self {
        Self(Inner::new_in(value))
    }

    /// Consume the cell and hand back the value.
    ///
    /// The same contract as
    /// [`RefCell::into_inner`](core::cell::RefCell::into_inner): owning the cell
    /// proves nothing can alias it, so there is no borrow check.
    #[must_use]
    pub fn into_inner(self) -> T {
        self.0.into_inner()
    }
}

impl<T: ?Sized> Inline<T> {
    /// Direct access to the value, with no borrow flag and no guard.
    ///
    /// Taking `&mut self` proves that no guard is alive, so this is the cheap
    /// path for setup and teardown - the same contract as
    /// [`RefCell::get_mut`](core::cell::RefCell::get_mut).
    pub fn get_mut(&mut self) -> &mut T {
        self.0.get_mut()
    }
}

impl<T: ?Sized> Deref for Inline<T> {
    type Target = Inner<T, state::LocalState>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl<T> From<T> for Inline<T> {
    fn from(value: T) -> Self {
        Self::new(value)
    }
}

impl<T> core::ops::AddAssign<T> for Inline<T>
where
    T: Sized + core::ops::AddAssign<T>,
{
    /// `cell += delta`, one exclusive borrow. See [`Inner::modify`].
    #[track_caller]
    fn add_assign(&mut self, delta: T) {
        self.0.modify(delta);
    }
}

impl<T> core::ops::SubAssign<T> for Inline<T>
where
    T: Sized + core::ops::SubAssign<T>,
{
    /// `cell -= delta`, one exclusive borrow.
    #[track_caller]
    fn sub_assign(&mut self, delta: T) {
        self.0.update(|value| *value -= delta);
    }
}

impl<T: Default> Default for Inline<T> {
    fn default() -> Self {
        Self::new(T::default())
    }
}

impl<T: PartialEq> PartialEq for Inline<T> {
    /// Compares the values under a shared borrow, panicking if one is borrowed.
    ///
    /// An `Inline` is never shared, so unlike [`Local`] there is no
    /// self-comparison short-circuit to make: `cell == cell` borrows twice, and
    /// two shared borrows compose, so it still cannot panic on its own.
    #[track_caller]
    fn eq(&self, other: &Self) -> bool {
        // Both guards are taken here, not inside nested `with` closures: a
        // closure's call site is inside this method, so a refusal would name a
        // line here instead of the caller's `==`. `borrow` is `#[track_caller]`
        // and this method is too, so `Location::caller()` reaches the user.
        let a = self.borrow();
        let b = other.borrow();
        *a == *b
    }
}

impl<T: Eq> Eq for Inline<T> {}

impl<T: PartialOrd> PartialOrd for Inline<T> {
    /// Compares the values under a shared borrow, panicking if one is borrowed.
    ///
    /// As with [`Inline`]'s `PartialEq`, there is no self-comparison
    /// short-circuit to make (an `Inline` is never shared), and two shared
    /// borrows compose, so `cell.partial_cmp(&cell)` cannot panic on its own.
    #[track_caller]
    fn partial_cmp(&self, other: &Self) -> Option<core::cmp::Ordering> {
        let a = self.borrow();
        let b = other.borrow();
        a.partial_cmp(&b)
    }
}

impl<T: Ord> Ord for Inline<T> {
    #[track_caller]
    fn cmp(&self, other: &Self) -> core::cmp::Ordering {
        let a = self.borrow();
        let b = other.borrow();
        a.cmp(&b)
    }
}

impl<T: Clone> Clone for Inline<T> {
    /// Clone the cell, cloning the value inside it.
    ///
    /// The same contract as [`RefCell`](core::cell::RefCell)'s `Clone`: the
    /// value is read under a shared borrow, so this panics with a located
    /// message if the cell is exclusively borrowed at the time.
    ///
    /// ```
    /// use borrow_lite::inline;
    ///
    /// let cell = inline(vec![1, 2, 3]);
    /// let copy = cell.clone();
    /// copy.with_mut(|v| v.push(4));
    /// assert_eq!(cell.with(Vec::len), 3); // independent cells
    /// assert_eq!(copy.with(Vec::len), 4);
    /// ```
    #[track_caller]
    fn clone(&self) -> Self {
        // `T::clone`, not `.clone()`: a guard is now `Clone` too, and cloning
        // the guard would hand back the same cell rather than a copy of it.
        Self::new(T::clone(&self.borrow()))
    }
}

impl<T: ?Sized + core::fmt::Debug> core::fmt::Debug for Inline<T> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self.try_borrow() {
            Ok(value) => f.debug_tuple("Inline").field(&&*value).finish(),
            Err(conflict) => f.debug_tuple("Inline").field(&conflict).finish(),
        }
    }
}

impl<T> Local<T> {
    /// Create a single-threaded cell.
    #[must_use]
    pub fn new(value: T) -> Self {
        Self(Rc::new(Inner::new_in(value)))
    }
}

impl<T: ?Sized> Local<T> {
    /// Whether two handles point at the same cell.
    ///
    /// The same as [`Rc::ptr_eq`]: true for a clone, false for a separately
    /// built cell with an equal value. Comparing a cell with itself is
    /// therefore always `true`, and never borrows the value.
    ///
    /// ```
    /// use borrow_lite::local;
    ///
    /// let a = local(1u8);
    /// let b = a.clone();
    /// let c = local(1u8);
    /// assert!(a.ptr_eq(&b));
    /// assert!(!a.ptr_eq(&c));
    /// ```
    #[must_use]
    pub fn ptr_eq(&self, other: &Self) -> bool {
        Rc::ptr_eq(&self.0, &other.0)
    }

    /// Direct access to the value, bypassing the borrow flags.
    ///
    /// Returns `None` while another handle to the same cell exists, because
    /// that handle could have a borrow alive. When it returns `Some`, there is
    /// no borrow check, no guard, and no location recorded - the same contract
    /// as [`Rc::get_mut`]. Useful for building a cell up before sharing it, or
    /// for tearing one down after the handles are gone.
    ///
    /// ```
    /// use borrow_lite::local;
    ///
    /// let mut cell = local(vec![1, 2, 3]);
    /// cell.get_mut().unwrap().push(4);
    /// assert_eq!(cell.with(|v| v.len()), 4);
    ///
    /// let other = cell.clone();
    /// assert!(cell.get_mut().is_none()); // `other` may be borrowing
    /// # drop(other);
    /// ```
    #[must_use]
    pub fn get_mut(&mut self) -> Option<&mut T> {
        Rc::get_mut(&mut self.0).map(Inner::get_mut)
    }

    /// Consume the cell and hand back the value.
    ///
    /// `Err(self)` while another handle exists, because that handle could have
    /// a borrow alive - the same contract as [`Rc::try_unwrap`]. The pair to
    /// [`get_mut`](Local::get_mut): that borrows the value out of a unique
    /// handle, this takes it.
    ///
    /// ```
    /// use borrow_lite::local;
    ///
    /// let cell = local(String::from("bye"));
    /// let other = cell.clone();
    /// let cell = cell.into_inner().unwrap_err(); // `other` still exists
    /// drop(other);
    /// assert_eq!(cell.into_inner().unwrap(), "bye");
    /// ```
    ///
    /// # Errors
    ///
    /// Returns `Err(self)` if this is not the only handle to the cell.
    pub fn into_inner(self) -> Result<T, Self>
    where
        T: Sized,
    {
        Rc::try_unwrap(self.0).map(Inner::into_inner).map_err(Self)
    }
}

impl<T> Shared<T> {
    /// Create a cell that can be shared between threads.
    #[must_use]
    pub fn new(value: T) -> Self {
        Self(Arc::new(Inner::new_in(value)))
    }
}

impl<T: ?Sized> Shared<T> {
    /// Whether two handles point at the same cell.
    ///
    /// The same as [`Arc::ptr_eq`]. Comparing a cell with itself is always
    /// `true`, and never borrows the value.
    ///
    /// ```
    /// use borrow_lite::shared;
    ///
    /// let a = shared(1u8);
    /// let b = a.clone();
    /// let c = shared(1u8);
    /// assert!(a.ptr_eq(&b));
    /// assert!(!a.ptr_eq(&c));
    /// ```
    #[must_use]
    pub fn ptr_eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }

    /// Direct access to the value, bypassing the borrow flags.
    ///
    /// The same contract as [`Arc::get_mut`]: `Some` only while this is the
    /// last handle, which is what rules out a borrow held by another thread.
    ///
    /// ```
    /// use borrow_lite::shared;
    ///
    /// let mut cell = shared(String::from("hi"));
    /// cell.get_mut().unwrap().push('!');
    /// assert_eq!(cell.with(|v| v.clone()), "hi!");
    /// ```
    #[must_use]
    pub fn get_mut(&mut self) -> Option<&mut T> {
        Arc::get_mut(&mut self.0).map(Inner::get_mut)
    }

    /// Consume the cell and hand back the value.
    ///
    /// The same contract as [`Arc::try_unwrap`]: `Err(self)` while any other
    /// handle exists, which is what rules out a borrow held by another thread.
    ///
    /// ```
    /// use borrow_lite::shared;
    ///
    /// let cell = shared(7u8);
    /// let other = cell.clone();
    /// let cell = cell.into_inner().unwrap_err();
    /// drop(other);
    /// assert_eq!(cell.into_inner().unwrap(), 7);
    /// ```
    ///
    /// # Errors
    ///
    /// Returns `Err(self)` if this is not the only handle to the cell.
    pub fn into_inner(self) -> Result<T, Self>
    where
        T: Sized,
    {
        Arc::try_unwrap(self.0).map(Inner::into_inner).map_err(Self)
    }
}

impl<T: ?Sized> Deref for Local<T> {
    type Target = Inner<T, state::LocalState>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl<T: ?Sized> Deref for Shared<T> {
    type Target = Inner<T, state::AtomicState>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl<T: ?Sized> Clone for Local<T> {
    #[track_caller]
    fn clone(&self) -> Self {
        Self(Rc::clone(&self.0))
    }
}

impl<T: ?Sized> Clone for Shared<T> {
    #[track_caller]
    fn clone(&self) -> Self {
        Self(Arc::clone(&self.0))
    }
}

impl<T: PartialEq> PartialEq for Local<T> {
    /// Compares the values, panicking if one is borrowed.
    ///
    /// Two handles to the *same* cell are equal without borrowing, so
    /// `cell == cell` never panics. Distinct cells are compared under a shared
    /// borrow; if that borrow conflicts, this panics with a located message,
    /// exactly as `Rc<RefCell<T>>` does. Use [`Inner::try_eq`] for the
    /// non-panicking form.
    ///
    /// The self-comparison short-circuit runs for *every* `T: PartialEq`, which
    /// is one place this differs from `Rc`/`Arc`: they specialise on `T: Eq`, so
    /// `Rc<RefCell<f64>>` compared with itself still borrows. The difference is
    /// only observable for an irreflexive payload (a `NaN` compares equal to
    /// itself here and unequal through `Rc`), and it is pinned by a test.
    #[track_caller]
    fn eq(&self, other: &Self) -> bool {
        if self.ptr_eq(other) {
            return true;
        }
        let a = self.borrow();
        let b = other.borrow();
        *a == *b
    }
}

impl<T: Eq> Eq for Local<T> {}

impl<T: PartialOrd> PartialOrd for Local<T> {
    /// Compares the values, panicking if one is borrowed.
    ///
    /// The ordering counterpart of [`Local`]'s `PartialEq`, with the same
    /// contract: two handles to the *same* cell are `Some(Equal)` without
    /// borrowing, so `cell.partial_cmp(&cell)` never panics; distinct cells are
    /// compared under a shared borrow, and a conflict panics with a located
    /// message. Use [`Inner::try_cmp`] for the non-panicking form.
    ///
    /// The same one place differs from `Rc`: the self-comparison short-circuit
    /// runs for every `T: PartialOrd`, so an irreflexive payload like `NaN`
    /// compares `Some(Equal)` here and `None` through `Rc<RefCell<T>>`.
    #[track_caller]
    fn partial_cmp(&self, other: &Self) -> Option<core::cmp::Ordering> {
        if self.ptr_eq(other) {
            return Some(core::cmp::Ordering::Equal);
        }
        let a = self.borrow();
        let b = other.borrow();
        a.partial_cmp(&b)
    }
}

impl<T: Ord> Ord for Local<T> {
    #[track_caller]
    fn cmp(&self, other: &Self) -> core::cmp::Ordering {
        if self.ptr_eq(other) {
            return core::cmp::Ordering::Equal;
        }
        let a = self.borrow();
        let b = other.borrow();
        a.cmp(&b)
    }
}

impl<T: PartialEq + Sync> PartialEq for Shared<T> {
    /// Compares the values, panicking if one is borrowed.
    ///
    /// See [`Local`]'s `PartialEq` for the contract, including the one place the
    /// self-comparison short-circuit differs from `Arc`. [`Inner::try_eq`] is
    /// the non-panicking form.
    #[track_caller]
    fn eq(&self, other: &Self) -> bool {
        if self.ptr_eq(other) {
            return true;
        }
        let a = self.borrow();
        let b = other.borrow();
        *a == *b
    }
}

impl<T: Eq + Sync> Eq for Shared<T> {}

impl<T: PartialOrd + Sync> PartialOrd for Shared<T> {
    /// Compares the values, panicking if one is borrowed.
    ///
    /// See [`Local`]'s `PartialOrd` for the contract, including the one place the
    /// self-comparison short-circuit differs from `Arc`. [`Inner::try_cmp`] is
    /// the non-panicking form.
    #[track_caller]
    fn partial_cmp(&self, other: &Self) -> Option<core::cmp::Ordering> {
        if self.ptr_eq(other) {
            return Some(core::cmp::Ordering::Equal);
        }
        let a = self.borrow();
        let b = other.borrow();
        a.partial_cmp(&b)
    }
}

impl<T: Ord + Sync> Ord for Shared<T> {
    #[track_caller]
    fn cmp(&self, other: &Self) -> core::cmp::Ordering {
        if self.ptr_eq(other) {
            return core::cmp::Ordering::Equal;
        }
        let a = self.borrow();
        let b = other.borrow();
        a.cmp(&b)
    }
}

impl<T> From<T> for Local<T> {
    fn from(value: T) -> Self {
        Self::new(value)
    }
}

impl<T> From<T> for Shared<T> {
    fn from(value: T) -> Self {
        Self::new(value)
    }
}

/// `cell += delta` on a [`Local`], one exclusive borrow.
///
/// The operator form of [`Inner::modify`]: it panics with a located message if
/// the cell is borrowed at the time, exactly as `with_mut` does. Use
/// [`Inner::try_modify`] when a conflict is expected.
impl<T> core::ops::AddAssign<T> for Local<T>
where
    T: Sized + core::ops::AddAssign<T>,
{
    #[track_caller]
    fn add_assign(&mut self, delta: T) {
        self.0.modify(delta);
    }
}

/// `cell -= delta` on a [`Local`], one exclusive borrow.
impl<T> core::ops::SubAssign<T> for Local<T>
where
    T: Sized + core::ops::SubAssign<T>,
{
    #[track_caller]
    fn sub_assign(&mut self, delta: T) {
        self.0.update(|value| *value -= delta);
    }
}

/// `cell += delta` on a [`Shared`], one exclusive borrow.
///
/// The operator form of [`Inner::modify`]: it panics with a located message if
/// the cell is borrowed at the time. Use [`Inner::try_modify`] when a conflict
/// is expected - which, across threads, is the usual case.
impl<T> core::ops::AddAssign<T> for Shared<T>
where
    T: Sized + core::ops::AddAssign<T>,
{
    #[track_caller]
    fn add_assign(&mut self, delta: T) {
        self.0.modify(delta);
    }
}

/// `cell -= delta` on a [`Shared`], one exclusive borrow.
impl<T> core::ops::SubAssign<T> for Shared<T>
where
    T: Sized + core::ops::SubAssign<T>,
{
    #[track_caller]
    fn sub_assign(&mut self, delta: T) {
        self.0.update(|value| *value -= delta);
    }
}

impl<T: Default> Default for Local<T> {
    fn default() -> Self {
        Self::new(T::default())
    }
}

impl<T: Default> Default for Shared<T> {
    fn default() -> Self {
        Self::new(T::default())
    }
}

impl<T: ?Sized + core::fmt::Debug> core::fmt::Debug for Local<T> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self.try_borrow() {
            Ok(value) => f.debug_tuple("Local").field(&&*value).finish(),
            Err(conflict) => f.debug_tuple("Local").field(&conflict).finish(),
        }
    }
}

impl<T: ?Sized + core::fmt::Debug + Sync> core::fmt::Debug for Shared<T> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self.try_borrow() {
            Ok(value) => f.debug_tuple("Shared").field(&&*value).finish(),
            Err(conflict) => f.debug_tuple("Shared").field(&conflict).finish(),
        }
    }
}

/// Create a cell that stores its value inline.
///
/// Equivalent to [`Inline::new`].
#[must_use]
pub fn inline<T>(value: T) -> Inline<T> {
    Inline::new(value)
}

/// Create a single-threaded cell.
///
/// Equivalent to [`Local::new`].
#[must_use]
pub fn local<T>(value: T) -> Local<T> {
    Local::new(value)
}

/// Create a cell that can be shared between threads.
///
/// Equivalent to [`Shared::new`].
#[must_use]
pub fn shared<T>(value: T) -> Shared<T> {
    Shared::new(value)
}

/// Keeps the README's examples honest: they are compiled as doctests.
#[cfg(doctest)]
#[doc = include_str!("../README.md")]
pub struct ReadmeDoctests;
