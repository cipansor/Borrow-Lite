//! The zero-overhead mode: a cell with no borrow flags at all.
#![allow(unsafe_code)]

use core::cell::UnsafeCell;

/// A cell with no borrow bookkeeping, for a caller that proves exclusivity.
///
/// This is the `unsafe-unchecked` mode. It is *exactly* an [`UnsafeCell<T>`] -
/// same size, same auto traits (`Send` when `T: Send`, `!Sync` always) - with
/// the two borrow operations spelled out as `unsafe` methods. It exists so that
/// a hot path that has already proven no other borrow can exist pays nothing
/// for the check, without the rest of the crate losing its diagnostics.
///
/// ```
/// use borrow_lite::Unchecked;
///
/// let cell = Unchecked::new(1u8);
/// // SAFETY: this cell is not borrowed anywhere else.
/// assert_eq!(unsafe { *cell.borrow_unchecked() }, 1);
/// ```
///
/// # Why it is `unsafe`
///
/// [`borrow_unchecked`](Unchecked::borrow_unchecked) hands out `&T` and
/// [`borrow_mut_unchecked`](Unchecked::borrow_mut_unchecked) hands out `&mut T`
/// with no flag set and no guard. If any other reference to the value is alive
/// when `borrow_mut_unchecked` is called, the two aliases are undefined
/// behaviour - the same contract [`UnsafeCell::get`] carries, and the reason
/// `UnsafeCell` is `!Sync`.
///
/// The checked cells ([`Inline`](crate::Inline), [`Local`](crate::Local),
/// [`Shared`](crate::Shared)) are the default; reach for this only when a
/// profile says the flag is the cost, and only inside a scope where the
/// invariant is locally obvious.
///
/// It is `!Sync`, exactly like `UnsafeCell`, so it cannot be shared across
/// threads and the unchecked borrows cannot race:
///
/// ```compile_fail
/// fn is_sync<T: Sync>() {}
/// is_sync::<borrow_lite::Unchecked<u8>>();
/// ```
pub struct Unchecked<T: ?Sized>(UnsafeCell<T>);

impl<T> Unchecked<T> {
    /// Create a cell holding `value`, with no borrow flags.
    #[must_use]
    pub fn new(value: T) -> Self {
        Self(UnsafeCell::new(value))
    }

    /// Consume the cell and hand back the value.
    #[must_use]
    pub fn into_inner(self) -> T {
        self.0.into_inner()
    }
}

impl<T: ?Sized> Unchecked<T> {
    /// Direct access to the value through a unique borrow.
    ///
    /// The safe path: `&mut self` proves nothing else can alias the value, so
    /// this needs no `unsafe` and no flag - the same contract as
    /// [`UnsafeCell::get_mut`].
    pub fn get_mut(&mut self) -> &mut T {
        self.0.get_mut()
    }

    /// Borrow the value shared, with no check and no guard.
    ///
    /// # Safety
    ///
    /// No exclusive borrow of this cell may be alive, and no
    /// [`borrow_mut_unchecked`](Unchecked::borrow_mut_unchecked) may be called
    /// while the returned reference is in use. The cell does not track borrows,
    /// so nothing else enforces this.
    #[must_use]
    pub unsafe fn borrow_unchecked(&self) -> &T {
        // SAFETY: the caller promised no conflicting borrow exists, which is
        // exactly the contract `UnsafeCell::get` requires.
        unsafe { &*self.0.get() }
    }

    /// Borrow the value exclusively, with no check and no guard.
    ///
    /// # Safety
    ///
    /// No other reference to the value may be alive - no shared borrow, no
    /// other exclusive borrow, and not another handle to this cell. The
    /// returned `&mut T` is unique, and nothing else enforces that.
    #[must_use]
    // Returning `&mut T` from `&self` is the whole point of the mode: it is how
    // `UnsafeCell::get` is used, and the caller's `unsafe` obligation is what
    // makes it sound. The checked cells do the same behind a guard.
    #[allow(clippy::mut_from_ref)]
    pub unsafe fn borrow_mut_unchecked(&self) -> &mut T {
        // SAFETY: the caller promised the value is unaliased, which is the
        // contract a unique `&mut T` requires.
        unsafe { &mut *self.0.get() }
    }
}

impl<T: ?Sized> core::fmt::Debug for Unchecked<T> {
    /// Format the cell *without* reading the value.
    ///
    /// Reading the value here would be unsound: a caller may hold a live
    /// `&mut T` from [`borrow_mut_unchecked`](Unchecked::borrow_mut_unchecked),
    /// and formatting through `&self` would retag a shared reference over it,
    /// invalidating that `&mut T` - undefined behaviour the caller cannot
    /// avoid, because `Debug` is safe. `&UnsafeCell<T>` cannot be safely read
    /// either, so [`UnsafeCell`]'s own `Debug` prints a placeholder too
    /// (`UnsafeCell { .. }`); this matches it, and `-Zmiri-strict-provenance`
    /// catches the reading version.
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Unchecked").finish_non_exhaustive()
    }
}

impl<T> From<T> for Unchecked<T> {
    fn from(value: T) -> Self {
        Self::new(value)
    }
}

/// Create a cell with no borrow flags.
///
/// Equivalent to [`Unchecked::new`].
#[must_use]
pub fn unchecked<T>(value: T) -> Unchecked<T> {
    Unchecked::new(value)
}
