//! Borrow bookkeeping, in two flavours: single-threaded and atomic.
//!
//! All of the `unsafe` in this crate is confined to this file and
//! the `guards` module; everything else is safe code on top.
#![allow(unsafe_code)]

use crate::sync::{AtomicBool, AtomicPtr, AtomicUsize, Ordering};
use core::cell::Cell;
use core::panic::Location;

use crate::error::{Conflict, ConflictKind};

mod sealed {
    pub trait Sealed {}
}

/// The borrow flags of a cell.
///
/// This trait is an implementation detail: it exists so that [`Inner`] can be
/// generic over the single-threaded and the atomic bookkeeping, and it is
/// sealed so that the invariants it carries cannot be broken from outside.
///
/// [`Inner`]: crate::inner::Inner
pub trait State: sealed::Sealed + 'static {
    /// Create the flags in their "nobody is borrowing" state.
    fn new() -> Self;

    /// Record a shared borrow, or explain why it is impossible.
    fn try_shared(&self, wanted: &'static Location<'static>) -> Result<(), Conflict>;

    /// Record an exclusive borrow, or explain why it is impossible.
    fn try_exclusive(&self, wanted: &'static Location<'static>) -> Result<(), Conflict>;

    /// Release a shared borrow previously granted by [`State::try_shared`].
    fn end_shared(&self);

    /// Record an extra shared borrow that is already known to be possible.
    ///
    /// Used by [`ReadGuard`]'s `Clone`: the guard being cloned already holds a
    /// shared borrow, so a second one cannot conflict, and there is no failure
    /// to report. The holder is left alone - it names the *first* borrow, and
    /// that borrow is still alive.
    ///
    /// [`ReadGuard`]: crate::ReadGuard
    fn add_shared(&self);

    /// Release an exclusive borrow previously granted by [`State::try_exclusive`].
    fn end_exclusive(&self);
}

/// Non-atomic flags for a cell that is only ever borrowed from one thread at a
/// time.
///
/// These flags are `Cell`s, so the type is `!Sync` and no two threads can ever
/// borrow the same cell concurrently. It is nevertheless `Send`, which is what
/// lets a cell that owns its value move to another thread - the same contract
/// as [`RefCell`](core::cell::RefCell).
///
/// The two users of this state are pinned down by the type that wraps it:
/// [`Inline`](crate::Inline) owns the value and is `Send` when `T: Send`, while
/// [`Local`](crate::Local) holds it behind an `Rc` and is therefore `!Send`.
pub struct LocalState {
    shared: Cell<usize>,
    exclusive: Cell<bool>,
    holder: Cell<Option<&'static Location<'static>>>,
}

impl LocalState {
    const fn unborrowed() -> Self {
        Self {
            shared: Cell::new(0),
            exclusive: Cell::new(false),
            holder: Cell::new(None),
        }
    }
}

impl sealed::Sealed for LocalState {}

impl State for LocalState {
    fn new() -> Self {
        Self::unborrowed()
    }

    fn try_shared(&self, wanted: &'static Location<'static>) -> Result<(), Conflict> {
        if self.exclusive.get() {
            return Err(Conflict::new(ConflictKind::Writer)
                .with_holder(self.holder.get())
                .with_wanted(wanted));
        }
        let next = self.shared.get() + 1;
        self.shared.set(next);
        if next == 1 {
            self.holder.set(Some(wanted));
        }
        Ok(())
    }

    fn try_exclusive(&self, wanted: &'static Location<'static>) -> Result<(), Conflict> {
        let readers = self.shared.get();
        if self.exclusive.get() || readers > 0 {
            let kind = if self.exclusive.get() {
                ConflictKind::Writer
            } else {
                ConflictKind::Readers
            };
            return Err(Conflict::new(kind)
                .with_count(readers)
                .with_holder(self.holder.get())
                .with_wanted(wanted));
        }
        self.exclusive.set(true);
        self.holder.set(Some(wanted));
        Ok(())
    }

    fn end_shared(&self) {
        let next = self.shared.get() - 1;
        self.shared.set(next);
        if next == 0 {
            self.holder.set(None);
        }
    }

    fn add_shared(&self) {
        self.shared.set(self.shared.get() + 1);
    }

    fn end_exclusive(&self) {
        self.exclusive.set(false);
        self.holder.set(None);
    }
}

/// Atomic flags for a cell shared between threads.
pub struct AtomicState {
    shared: AtomicUsize,
    exclusive: AtomicBool,
    holder: AtomicPtr<()>,
}

impl AtomicState {
    // Not `const`: loom's atomics cannot be built in a const context, and
    // nothing needs this to be const - it is only called from `State::new`.
    fn unborrowed() -> Self {
        Self {
            shared: AtomicUsize::new(0),
            exclusive: AtomicBool::new(false),
            holder: AtomicPtr::new(core::ptr::null_mut()),
        }
    }

    /// The recorded holder, if any. Diagnostics only.
    ///
    /// It is written only by the writer path - [`try_exclusive`](State::try_exclusive)
    /// sets it, [`end_exclusive`](State::end_exclusive) clears it - so it names
    /// the writer, never a reader. A reader does not store its location: that
    /// would be an atomic store on every read borrow, and it would race (last
    /// writer wins), which is exactly the cost the two-flag protocol avoids.
    /// The consequence is that a *readers* conflict on a `Shared` cell reports
    /// its `count` but no holder, while the single-threaded cell can name the
    /// first reader for free.
    fn holder(&self) -> Option<&'static Location<'static>> {
        let raw = self.holder.load(Ordering::Relaxed);
        if raw.is_null() {
            return None;
        }
        // SAFETY: only `&'static Location<'static>` values are ever stored here.
        Some(unsafe { &*(raw as *const Location<'static>) })
    }

    fn set_holder(&self, holder: Option<&'static Location<'static>>) {
        let raw = holder.map_or(core::ptr::null_mut(), |loc| {
            core::ptr::from_ref::<Location<'static>>(loc)
                .cast_mut()
                .cast::<()>()
        });
        self.holder.store(raw, Ordering::Relaxed);
    }
}

impl sealed::Sealed for AtomicState {}

impl State for AtomicState {
    fn new() -> Self {
        Self::unborrowed()
    }

    // The flag juggling below is the classic Dekker-style handshake between a
    // reader that increments a counter and a writer that claims a flag. Using
    // `SeqCst` on every operation that participates in that handshake is what
    // makes it correct: it puts them in one total order, so reader and writer
    // cannot both conclude that they own the cell.
    fn try_shared(&self, wanted: &'static Location<'static>) -> Result<(), Conflict> {
        if self.exclusive.load(Ordering::SeqCst) {
            return Err(Conflict::new(ConflictKind::Writer)
                .with_holder(self.holder())
                .with_wanted(wanted));
        }
        self.shared.fetch_add(1, Ordering::SeqCst);
        if self.exclusive.load(Ordering::SeqCst) {
            self.shared.fetch_sub(1, Ordering::SeqCst);
            return Err(Conflict::new(ConflictKind::Writer)
                .with_holder(self.holder())
                .with_wanted(wanted));
        }
        Ok(())
    }

    fn try_exclusive(&self, wanted: &'static Location<'static>) -> Result<(), Conflict> {
        if self.exclusive.load(Ordering::SeqCst)
            || self
                .exclusive
                .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
                .is_err()
        {
            return Err(Conflict::new(ConflictKind::Writer)
                .with_holder(self.holder())
                .with_wanted(wanted));
        }
        let readers = self.shared.load(Ordering::SeqCst);
        if readers > 0 {
            self.exclusive.store(false, Ordering::SeqCst);
            return Err(Conflict::new(ConflictKind::Readers)
                .with_count(readers)
                .with_holder(self.holder())
                .with_wanted(wanted));
        }
        self.set_holder(Some(wanted));
        Ok(())
    }

    fn end_shared(&self) {
        // No holder to clear. `holder` is written only after a writer wins the
        // handshake, and a writer can only win while no reader is active - so
        // while any reader is alive the holder is already absent, and clearing it
        // here would be a `None`-to-`None` store on every shared release. It is
        // also the store that used to run *only* when the last reader left, which
        // is the same case: nothing to clear.
        self.shared.fetch_sub(1, Ordering::Release);
    }

    fn end_exclusive(&self) {
        self.set_holder(None);
        self.exclusive.store(false, Ordering::Release);
    }

    fn add_shared(&self) {
        // The caller already holds a shared borrow, which means the writer flag
        // is clear and cannot become set while this count is non-zero. `SeqCst`
        // matches the handshake in `try_shared` so the increment cannot be
        // reordered against a writer's flag check.
        self.shared.fetch_add(1, Ordering::SeqCst);
    }
}
