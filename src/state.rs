//! Borrow bookkeeping, in two flavours: single-threaded and atomic.
//!
//! All of the `unsafe` in this crate is confined to this file and
//! the `guards` module; everything else is safe code on top.
#![allow(unsafe_code)]

use crate::sync::{AtomicPtr, AtomicUsize, Ordering};
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
    ///
    /// Returns `true` if *this* borrow is the one that recorded the holder, so
    /// the guard can clear it on release. See [`State::end_shared`].
    fn try_shared(&self, wanted: &'static Location<'static>) -> Result<bool, Conflict>;

    /// Record an exclusive borrow, or explain why it is impossible.
    fn try_exclusive(&self, wanted: &'static Location<'static>) -> Result<(), Conflict>;

    /// Release a shared borrow previously granted by [`State::try_shared`].
    ///
    /// `recorder` is the flag [`State::try_shared`] returned: only the borrow
    /// that recorded the holder may clear it, and only it. A different reader
    /// leaving must leave the holder alone - it names the recorder, which may
    /// still be alive.
    fn end_shared(&self, recorder: bool);

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

    fn try_shared(&self, wanted: &'static Location<'static>) -> Result<bool, Conflict> {
        if self.exclusive.get() {
            return Err(Conflict::new(ConflictKind::Writer)
                .with_holder(self.holder.get())
                .with_wanted(wanted));
        }
        let next = self.shared.get() + 1;
        self.shared.set(next);
        let recorder = next == 1;
        if recorder {
            self.holder.set(Some(wanted));
        }
        Ok(recorder)
    }

    fn try_exclusive(&self, wanted: &'static Location<'static>) -> Result<(), Conflict> {
        if self.exclusive.get() {
            // A writer conflict has nothing to count: `shared` is necessarily
            // zero while the cell is exclusively borrowed, so attaching it would
            // report "0 active borrows" about a cell that is very much borrowed.
            // Leave the count unset, matching the atomic cell and the documented
            // contract.
            return Err(Conflict::new(ConflictKind::Writer)
                .with_holder(self.holder.get())
                .with_wanted(wanted));
        }
        let readers = self.shared.get();
        if readers > 0 {
            return Err(Conflict::new(ConflictKind::Readers)
                .with_count(readers)
                .with_holder(self.holder.get())
                .with_wanted(wanted));
        }
        self.exclusive.set(true);
        self.holder.set(Some(wanted));
        Ok(())
    }

    fn end_shared(&self, recorder: bool) {
        let next = self.shared.get() - 1;
        self.shared.set(next);
        // Only the recording reader clears the holder. If a *different* reader
        // left first, the recorder may still be alive, so the holder must stay;
        // it is cleared when the recorder itself goes, or when the last reader
        // goes (a safety net for the clone path, which does not set the flag).
        if recorder || next == 0 {
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
///
/// The writer side is a single `AtomicPtr`: it is null when no writer holds the
/// cell, and the writer's location when one does. Folding the "a writer is here"
/// bit and the "where" pointer into one word is what makes a writer conflict
/// always carry a location - there is no window in which the flag is set but the
/// location has not been published yet, because claiming the cell and publishing
/// the location are the *same* compare-exchange.
///
/// Readers are a separate `AtomicUsize` count. A reader does not record its
/// location: that would be an atomic store on every read borrow, and it would
/// race (last writer wins). So a readers conflict reports its count but no
/// holder, which is the one documented gap.
pub struct AtomicState {
    shared: AtomicUsize,
    writer: AtomicPtr<()>,
}

impl AtomicState {
    // Not `const`: loom's atomics cannot be built in a const context, and
    // nothing needs this to be const - it is only called from `State::new`.
    fn unborrowed() -> Self {
        Self {
            shared: AtomicUsize::new(0),
            writer: AtomicPtr::new(core::ptr::null_mut()),
        }
    }

    /// The address that stands for "a writer is holding the cell at `loc`".
    fn writer_token(loc: &'static Location<'static>) -> *mut () {
        core::ptr::from_ref::<Location<'static>>(loc)
            .cast_mut()
            .cast::<()>()
    }

    /// Decode a writer word that was read once, atomically, into a location.
    ///
    /// The word is loaded a single time and decoded from that value, so the
    /// "is there a writer" test and the "where" answer come from the same read.
    /// Two separate loads would open a window: a reader could observe a writer
    /// and then, by the time it asked where, the writer had released.
    fn location_of(raw: *mut ()) -> Option<&'static Location<'static>> {
        if raw.is_null() {
            return None;
        }
        // SAFETY: only `&'static Location<'static>` values are ever stored here.
        Some(unsafe { &*(raw as *const Location<'static>) })
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
    fn try_shared(&self, wanted: &'static Location<'static>) -> Result<bool, Conflict> {
        // One load, decoded from the value read, so the "is there a writer"
        // answer and the location cannot disagree.
        let seen = self.writer.load(Ordering::SeqCst);
        if !seen.is_null() {
            return Err(Conflict::new(ConflictKind::Writer)
                .with_holder(Self::location_of(seen))
                .with_wanted(wanted));
        }
        self.shared.fetch_add(1, Ordering::SeqCst);
        let seen = self.writer.load(Ordering::SeqCst);
        if !seen.is_null() {
            self.shared.fetch_sub(1, Ordering::SeqCst);
            return Err(Conflict::new(ConflictKind::Writer)
                .with_holder(Self::location_of(seen))
                .with_wanted(wanted));
        }
        // A reader never records a holder on the atomic cell - the writer word
        // names only writers - so there is nothing for it to clear on release.
        Ok(false)
    }

    fn try_exclusive(&self, wanted: &'static Location<'static>) -> Result<(), Conflict> {
        // Claim the cell *and* publish the location in one step, so a reader that
        // sees the claim always sees the location with it. On failure the CAS
        // hands back the value it saw, which is the competing writer's token -
        // use that rather than a second load, so the conflict cannot observe a
        // writer that has already released.
        if let Err(seen) = self.writer.compare_exchange(
            core::ptr::null_mut(),
            Self::writer_token(wanted),
            Ordering::SeqCst,
            Ordering::SeqCst,
        ) {
            return Err(Conflict::new(ConflictKind::Writer)
                .with_holder(Self::location_of(seen))
                .with_wanted(wanted));
        }
        let readers = self.shared.load(Ordering::SeqCst);
        if readers > 0 {
            self.writer.store(core::ptr::null_mut(), Ordering::SeqCst);
            return Err(Conflict::new(ConflictKind::Readers)
                .with_count(readers)
                .with_wanted(wanted));
        }
        Ok(())
    }

    fn end_shared(&self, _recorder: bool) {
        // No holder to clear: the writer side is a separate word, and a reader
        // never touches it. Clearing it here would be the store that used to race
        // a writer's own claim - see the loom test about a reader release erasing
        // a writer's location.
        self.shared.fetch_sub(1, Ordering::Release);
    }

    fn end_exclusive(&self) {
        self.writer.store(core::ptr::null_mut(), Ordering::Release);
    }

    fn add_shared(&self) {
        // The caller already holds a shared borrow, which means the writer word is
        // null and cannot become non-null while this count is non-zero. `SeqCst`
        // matches the handshake in `try_shared` so the increment cannot be
        // reordered against a writer's claim.
        self.shared.fetch_add(1, Ordering::SeqCst);
    }
}
