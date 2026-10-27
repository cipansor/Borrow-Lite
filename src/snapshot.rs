//! A read-mostly cell: reads take no counter, writes replace the whole value.
//!
//! [`Shared`](crate::Shared) counts every reader, which is what lets it report a
//! conflict - and also what stops it scaling when readers outnumber writers,
//! because every read borrow writes the same cache line. `Snapshot<T>` trades
//! the diagnostics for that scalability: a read is a single atomic load with no
//! increment, so readers never touch shared state and never conflict.
//!
//! The cost is that a write replaces the *whole* value instead of mutating it in
//! place, and that every superseded value is kept alive until the `Snapshot` is
//! dropped. That is the right shape for the data people read this way - a
//! config, a routing table, a feature-flag set - where writes are rare and small
//! compared with reads. It is the wrong shape for a counter or a queue.
//!
//! # Why the retired values are kept
//!
//! A reader that gets `&T` must be able to hold it while a writer publishes a
//! new value. Freeing the old value at the moment of the swap would leave that
//! reader with a dangling reference, and the usual ways to avoid it - hazard
//! pointers, epoch reclamation, a reader count - all put a shared counter back
//! on the read path, which is the thing this type exists to remove. So the
//! superseded values are retained in a list owned by the `Snapshot` and freed
//! together when it is dropped. The read stays a bare `Acquire` load.
//!
//! The practical consequence, stated plainly: a `Snapshot` written to an
//! unbounded number of times grows without bound. For a config reloaded on a
//! signal that is nothing; for a per-request cache it is a leak. When the write
//! rate is not clearly lower than the read rate, use [`Shared`](crate::Shared).

use core::cell::UnsafeCell;
use core::hint::spin_loop;
use core::ops::Deref;

use alloc::boxed::Box;
use alloc::vec::Vec;

use crate::sync::{AtomicBool, AtomicPtr, Ordering};

/// A minimal test-and-set lock, used only to serialise writers.
///
/// It is never taken on the read path, so it cannot slow readers down; its only
/// job is to make "record the retired value, then publish the new one" atomic
/// with respect to other writers.
struct WriterLock {
    locked: AtomicBool,
}

impl WriterLock {
    // Not `const`: loom's atomics cannot be built in a const context, and
    // nothing needs this to be const - it is only called from `Snapshot::new`.
    fn new() -> Self {
        Self {
            locked: AtomicBool::new(false),
        }
    }

    fn lock(&self) {
        while self
            .locked
            .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            while self.locked.load(Ordering::Relaxed) {
                spin_loop();
            }
        }
    }

    fn unlock(&self) {
        self.locked.store(false, Ordering::Release);
    }
}

/// A cell whose reads are a single atomic load.
///
/// A `Snapshot<T>` holds one value at a time. [`load`](Self::load) returns a
/// `&T` with no atomic read-modify-write and no conflict: readers never block a
/// writer, and a writer never refuses a reader. [`store`](Self::store) replaces
/// the value wholesale, and the previous value is kept alive (see the module
/// docs) rather than freed.
///
/// It is *not* a drop-in replacement for `std::sync::RwLock` or for
/// [`Shared`](crate::Shared): the value cannot be mutated in place, and a
/// read-then-write pair is not atomic. Reach for it when the value is replaced
/// occasionally and read constantly.
///
/// ```
/// use borrow_lite::Snapshot;
///
/// let config = Snapshot::new(vec![1, 2, 3]);
/// assert_eq!(config.load(), &[1, 2, 3]);
/// config.store(vec![4, 5]);
/// assert_eq!(config.load(), &[4, 5]);
/// ```
///
/// # Threads
///
/// `Snapshot<T>` is `Send` and `Sync` when `T` is `Send + Sync`, like `Arc<T>`.
/// Sharing it across threads is the point: the readers do not touch each other.
///
/// ```
/// use borrow_lite::Snapshot;
/// use std::sync::Arc;
///
/// let config = Arc::new(Snapshot::new(0i32));
/// let reader = {
///     let config = Arc::clone(&config);
///     std::thread::spawn(move || *config.load())
/// };
/// config.store(7);
/// let seen = reader.join().unwrap();
/// assert!(seen == 0 || seen == 7);
/// assert_eq!(*config.load(), 7);
/// ```
///
/// The bound is `T: Send + Sync`, exactly `Arc<T>`'s, and it is enforced the
/// same way - a payload that cannot cross threads cannot be shared:
///
/// ```compile_fail
/// fn is_sync<T: Sync>() {}
/// // `Cell<u8>` is `Send` but not `Sync`, so the cell cannot be shared.
/// is_sync::<borrow_lite::Snapshot<std::cell::Cell<u8>>>();
/// ```
pub struct Snapshot<T> {
    /// The current value: a pointer into the last box in `retired`.
    current: AtomicPtr<T>,
    /// Serialises writers, so a push and its publish cannot interleave.
    lock: WriterLock,
    /// Every value this cell has held, oldest first, the current one last, each
    /// as a `Box::into_raw` pointer. Keeping them all is what makes the bare
    /// `&T` from [`load`](Self::load) sound without a reader count; they are
    /// freed together on drop. Storing raw pointers rather than `Box<T>` keeps a
    /// single provenance per value - the `Box`'s `Unique` tag and the `&T` a
    /// reader takes must not be two owners of the same allocation.
    retired: UnsafeCell<Vec<*mut T>>,
}

// SAFETY: `Snapshot<T>` hands out `&T` to many threads, so `T` must be `Sync`,
// and `store` can move a `T` onto the cell's thread and drop the previous one
// there, so `T` must be `Send`. These are exactly `Arc<T>`'s bounds.
unsafe impl<T: Send + Sync> Send for Snapshot<T> {}
// SAFETY: as above.
unsafe impl<T: Send + Sync> Sync for Snapshot<T> {}

impl<T> Drop for Snapshot<T> {
    fn drop(&mut self) {
        // Every value was leaked by `Box::into_raw`, so reclaim and drop each.
        // The writer lock is not needed: `&mut self` proves no writer can be
        // running, and no reader can outlive the cell.
        for raw in self.retired.get_mut().drain(..) {
            // SAFETY: each pointer came from `Box::into_raw` on a `T` and is
            // dropped exactly once, here.
            drop(unsafe { Box::from_raw(raw) });
        }
    }
}

impl<T> Snapshot<T> {
    /// Create a `Snapshot` holding `value`.
    #[must_use]
    pub fn new(value: T) -> Self {
        // Leak the box to a raw pointer; `retired` and `current` share that one
        // provenance, so a reader's `&T` and the eventual `Box::from_raw` agree.
        let raw = Box::into_raw(Box::new(value));
        Self {
            current: AtomicPtr::new(raw),
            lock: WriterLock::new(),
            retired: UnsafeCell::new(alloc::vec![raw]),
        }
    }

    /// Read the current value.
    ///
    /// One `Acquire` load and nothing else: no counter, no lock, and no way to
    /// be refused. The returned reference borrows the `Snapshot`, so it cannot
    /// outlive it - which is what lets the cell free its retired values on drop
    /// without tracking readers.
    ///
    /// ```
    /// use borrow_lite::Snapshot;
    ///
    /// let cell = Snapshot::new(String::from("v1"));
    /// let seen = cell.load();
    /// cell.store(String::from("v2"));
    /// // `seen` still points at the value that was current when it was taken.
    /// assert_eq!(seen, "v1");
    /// assert_eq!(cell.load(), "v2");
    /// ```
    #[must_use]
    #[inline]
    pub fn load(&self) -> &T {
        let raw = self.current.load(Ordering::Acquire);
        // SAFETY: `raw` is one of the `Box::into_raw` pointers in `retired`,
        // which is only ever appended to and is drained only in `Drop`, which
        // has exclusive access. The returned reference borrows `self`, so the
        // value outlives it; the pointer is non-null because `new` stores a live
        // value before returning.
        unsafe { &*raw }
    }

    /// Run `f` on the current value, with the borrow scoped to the call.
    ///
    /// [`load`](Self::load) under a name that matches
    /// [`with`](crate::Inner::with), so a call site reads the same whichever
    /// cell kind it is holding.
    ///
    /// ```
    /// use borrow_lite::Snapshot;
    ///
    /// let cell = Snapshot::new(vec![1, 2, 3]);
    /// assert_eq!(cell.with(Vec::len), 3);
    /// ```
    #[inline]
    pub fn with<R>(&self, f: impl FnOnce(&T) -> R) -> R {
        f(self.load())
    }

    /// Replace the value, keeping the previous one alive.
    ///
    /// The swap is atomic: a reader sees the whole old value or the whole new
    /// one, never a mixture. Writers serialise against each other with a short
    /// spin; readers are never involved.
    ///
    /// ```
    /// use borrow_lite::Snapshot;
    ///
    /// let cell = Snapshot::new(1u8);
    /// cell.store(2);
    /// cell.store(3);
    /// assert_eq!(*cell.load(), 3);
    /// ```
    pub fn store(&self, value: T) {
        let raw = Box::into_raw(Box::new(value));
        self.lock.lock();
        // SAFETY: the writer lock is held, so no other writer is touching
        // `retired`. Recording the pointer before publishing it means a reader
        // that observes the new pointer is guaranteed to find the value in the
        // list, and the reference it forms is valid.
        unsafe { (*self.retired.get()).push(raw) };
        self.current.store(raw, Ordering::Release);
        self.lock.unlock();
    }

    /// Consume the cell and return the current value.
    ///
    /// The retired values are dropped. This is `O(1)` in the current value and
    /// `O(writes)` in cleanup, which is the cost the read path deferred.
    ///
    /// ```
    /// use borrow_lite::Snapshot;
    ///
    /// let cell = Snapshot::new(vec![1, 2]);
    /// cell.store(vec![3]);
    /// assert_eq!(cell.into_inner(), vec![3]);
    /// ```
    #[must_use]
    pub fn into_inner(self) -> T {
        // The current value is always the last pointer pushed, and `Drop` frees
        // the rest, so take the last one out and hand ownership back.
        let raw = self.current.load(Ordering::Acquire);
        // SAFETY: `raw` came from `Box::into_raw` and is dropped exactly once,
        // here; `Drop` skips it because it was removed from `retired`.
        let value = unsafe { *Box::from_raw(raw) };
        // SAFETY: `into_inner` has `self` by value, so nothing else can touch
        // `retired` now.
        unsafe { (*self.retired.get()).retain(|&p| p != raw) };
        value
    }
}

impl<T> Deref for Snapshot<T> {
    type Target = T;

    #[inline]
    fn deref(&self) -> &T {
        self.load()
    }
}

impl<T: core::fmt::Debug> core::fmt::Debug for Snapshot<T> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        core::fmt::Debug::fmt(self.load(), f)
    }
}
