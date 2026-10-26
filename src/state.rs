//! Borrow bookkeeping, in two flavours: single-threaded and atomic.
//!
//! All of the `unsafe` in this crate is confined to this file and
//! the `guards` module; everything else is safe code on top.
#![allow(unsafe_code)]

use crate::sync::{AtomicPtr, AtomicUsize, Ordering};
use core::cell::Cell;
use core::panic::Location;

use crate::error::{Conflict, ConflictKind};

/// The single-word encoding of a [`LocalState`]'s borrow flags.
///
/// `0..WRITER` is the number of live shared borrows; `WRITER` (`usize::MAX`) is
/// "an exclusive borrow is live". Folding the two into one word is what makes
/// `LocalState` fit in two `Cell`s - the flags and, with `diagnostics`, the
/// holder - so `Inline<u64>` is 24 bytes rather than 32, and 16 with
/// `diagnostics` off (the same as `RefCell<u64>`).
///
/// A reader count can never reach `WRITER`: the count is only allowed to grow to
/// `WRITER - 1`, and the increment past that panics (see
/// [`add_local_reader`]), so the sentinel is never mistaken for a count.
const WRITER: usize = usize::MAX;

mod sealed {
    pub trait Sealed {}
}

/// The panic used when the reader count would overflow.
///
/// Reaching this needs a `ReadGuard` leaked with [`mem::forget`] repeated
/// `usize::MAX` times - a borrow that is never released. It is checked rather
/// than left to wrap because a count that wrapped to zero would let
/// [`State::try_exclusive`] hand out `&mut T` while a `&T` is still alive,
/// which is exactly the aliasing the cell exists to prevent.
///
/// [`mem::forget`]: core::mem::forget
#[cold]
#[inline(never)]
fn reader_count_overflow() -> ! {
    // Formatted (`{}`) rather than a bare literal so the panic payload is an
    // `alloc::string::String`, matching the messages `Conflict` produces.
    panic!(
        "{}",
        "borrow_lite: too many active reader borrows; the reader counter would overflow. \
         A `ReadGuard` was leaked (e.g. with `mem::forget`) instead of being dropped"
    )
}

/// Increment a non-atomic reader count that must not wrap.
///
/// The word holds either a reader count or the [`WRITER`] sentinel, so a count
/// that reached `WRITER` would be indistinguishable from an exclusive borrow.
/// The guard therefore trips one step early - at `WRITER - 1` - rather than
/// letting the count collide with the sentinel.
fn add_local_reader(flags: &Cell<usize>) -> usize {
    let Some(next) = flags.get().checked_add(1).filter(|&next| next != WRITER) else {
        reader_count_overflow()
    };
    flags.set(next);
    next
}

/// Increment an atomic reader count that must not wrap.
///
/// The check mirrors `Arc::clone`'s: the count is only allowed to grow to
/// `isize::MAX`, and a `fetch_add` past that is undone before the panic so the
/// cell is left exactly as it was found.
fn add_atomic_reader(shared: &AtomicUsize) {
    if shared.fetch_add(1, Ordering::SeqCst) > isize::MAX as usize {
        shared.fetch_sub(1, Ordering::SeqCst);
        reader_count_overflow();
    }
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
///
/// The flags are one word: `0..WRITER` readers, or the `WRITER` sentinel for an
/// exclusive borrow. The holder location is a second `Cell`, and it only exists
/// with the `diagnostics` feature - without it the struct is a single `Cell`
/// and costs what `RefCell`'s flag costs.
pub struct LocalState {
    flags: Cell<usize>,
    #[cfg(feature = "diagnostics")]
    holder: Cell<Option<&'static Location<'static>>>,
}

impl LocalState {
    const fn unborrowed() -> Self {
        Self {
            flags: Cell::new(0),
            #[cfg(feature = "diagnostics")]
            holder: Cell::new(None),
        }
    }

    /// The location of the borrow holding the cell, when diagnostics are on.
    #[cfg(feature = "diagnostics")]
    fn holder(&self) -> Option<&'static Location<'static>> {
        self.holder.get()
    }

    /// With diagnostics off there is nothing to record, so this is always `None`
    /// and the caller's `Cell` store disappears entirely.
    #[cfg(not(feature = "diagnostics"))]
    #[allow(clippy::unused_self)]
    const fn holder(&self) -> Option<&'static Location<'static>> {
        None
    }
}

impl sealed::Sealed for LocalState {}

impl State for LocalState {
    fn new() -> Self {
        Self::unborrowed()
    }

    fn try_shared(&self, wanted: &'static Location<'static>) -> Result<bool, Conflict> {
        // One load of the flags, reused for the writer test and the increment,
        // so the fast path is a load and a store like `RefCell`'s rather than
        // two loads. The overflow test is fused into the same `checked_add`.
        let flags = self.flags.get();
        if flags == WRITER {
            return Err(Conflict::new(ConflictKind::Writer)
                .with_holder(self.holder())
                .with_wanted(wanted));
        }
        let Some(next) = flags.checked_add(1).filter(|&next| next != WRITER) else {
            reader_count_overflow()
        };
        self.flags.set(next);
        let recorder = next == 1;
        #[cfg(feature = "diagnostics")]
        if recorder {
            self.holder.set(Some(wanted));
        }
        Ok(recorder)
    }

    fn try_exclusive(&self, wanted: &'static Location<'static>) -> Result<(), Conflict> {
        let flags = self.flags.get();
        if flags == WRITER {
            // A writer conflict has nothing to count: the count is necessarily
            // zero while the cell is exclusively borrowed, so attaching it would
            // report "0 active borrows" about a cell that is very much borrowed.
            // Leave the count unset, matching the atomic cell and the documented
            // contract.
            return Err(Conflict::new(ConflictKind::Writer)
                .with_holder(self.holder())
                .with_wanted(wanted));
        }
        if flags > 0 {
            return Err(Conflict::new(ConflictKind::Readers)
                .with_count(flags)
                .with_holder(self.holder())
                .with_wanted(wanted));
        }
        self.flags.set(WRITER);
        #[cfg(feature = "diagnostics")]
        self.holder.set(Some(wanted));
        Ok(())
    }

    fn end_shared(&self, recorder: bool) {
        let next = self.flags.get() - 1;
        self.flags.set(next);
        // Only the recording reader clears the holder. If a *different* reader
        // left first, the recorder may still be alive, so the holder must stay;
        // it is cleared when the recorder itself goes, or when the last reader
        // goes (a safety net for the clone path, which does not set the flag).
        #[cfg(feature = "diagnostics")]
        if recorder || next == 0 {
            self.holder.set(None);
        }
        #[cfg(not(feature = "diagnostics"))]
        let _ = (recorder, next);
    }

    fn add_shared(&self) {
        add_local_reader(&self.flags);
    }

    fn end_exclusive(&self) {
        self.flags.set(0);
        #[cfg(feature = "diagnostics")]
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
///
/// # A brief phantom holder
///
/// [`try_exclusive`](State::try_exclusive) has one window in which the writer
/// word names a writer that is not, in the end, holding the cell. The writer
/// claims the word with its compare-exchange *before* it reads the reader count,
/// so if it finds readers waiting it must release the claim. Between those two
/// steps a reader that loads the word sees the writer's token, and is refused
/// with a [`Writer`](ConflictKind::Writer) conflict naming it - even though that
/// writer is about to back off and the cell was, in fact, only ever
/// read-borrowed. The conflict is not wrong (the reader *was* refused, and the
/// cell was busy), only the attribution is momentary. Closing it would mean
/// publishing the claim and the reader count in one atomic step, which needs a
/// wider word than the two flags this cell is built from; the phantom is
/// therefore documented rather than removed. See [`Conflict::holder`] for what a
/// caller should make of a holder.
///
/// [`Conflict::holder`]: crate::Conflict::holder
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

    // ── Memory ordering, and why it is not weakened ────────────────────────
    //
    // The flag juggling below is the classic Dekker-style handshake between a
    // reader (increments `shared`, then loads `writer`) and a writer (claims
    // `writer`, then loads `shared`). Each side does a store to one location
    // followed by a load of the other, and mutual exclusion requires that at
    // least one side observe the other's store.
    //
    // `Acquire`/`Release` cannot give that. Each side's store is a `Release`
    // and its load an `Acquire`, but those only order the *same* location;
    // across two locations there is no total order, so the store-buffering
    // outcome - both sides read zero and both conclude they own the cell - is
    // permitted. Only `SeqCst` supplies the single total order that rules it
    // out. So the four operations that make up the handshake
    // (`shared.fetch_add`, the first `writer` load, the `writer`
    // compare-exchange, and the `shared` load) stay `SeqCst`.
    //
    // The release operations - `end_shared`'s `fetch_sub` and
    // `end_exclusive`'s `store` - are *already* only `Release`, and that is the
    // weakest that is sound, not an oversight:
    //
    // * they do not participate in the handshake, so they need no global order;
    // * they must, however, publish the borrow's effects to whoever next
    //   acquires the cell, which is exactly `Release`. A `Relaxed` release
    //   would let a subsequent `Acquire` on the *next* borrow read a stale
    //   value, so `Relaxed` is not an option.
    //
    // This was checked against the model, not assumed: `tests/loom.rs` asserts
    // that loom's `AcqRel` approximation accepts the store-buffering outcome,
    // which is the evidence that the handshake genuinely needs `SeqCst` and
    // cannot be relaxed. Weakening any of the four handshake operations would
    // therefore be a soundness change with no measurement to justify it, so
    // none of them is weakened. See the `AtomicState` docs for the one window
    // (`try_exclusive`'s back-off) that a wider word could close.
    fn try_shared(&self, wanted: &'static Location<'static>) -> Result<bool, Conflict> {
        // One load, decoded from the value read, so the "is there a writer"
        // answer and the location cannot disagree.
        let seen = self.writer.load(Ordering::SeqCst);
        if !seen.is_null() {
            return Err(Conflict::new(ConflictKind::Writer)
                .with_holder(Self::location_of(seen))
                .with_wanted(wanted));
        }
        add_atomic_reader(&self.shared);
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
            // Back off, undoing the claim. Between the successful CAS above and
            // this store a reader can see our token and be refused with a writer
            // conflict naming us, though we never hold the cell. That phantom
            // holder is documented on `AtomicState`; it cannot be closed without
            // a wider word, and the reader was genuinely refused either way.
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
        add_atomic_reader(&self.shared);
    }
}

#[cfg(test)]
mod overflow_tests {
    //! Regression tests for the reader-count overflow guards.
    //!
    //! A `ReadGuard` leaked with `mem::forget` never releases its borrow, so
    //! enough of them can drive the count to `usize::MAX`. Without a check the
    //! next increment wraps to zero, and `try_exclusive` then hands out `&mut T`
    //! while a `&T` is still alive - the aliasing the cell exists to prevent.
    //! The counters are driven straight to the limit here rather than by leaking
    //! `usize::MAX` guards, which is the same state with no memory cost.

    use super::*;
    use alloc::string::String;
    extern crate std;
    use std::panic::{self, AssertUnwindSafe};

    fn here() -> &'static Location<'static> {
        Location::caller()
    }

    #[test]
    fn local_reader_count_overflow_panics_instead_of_wrapping() {
        let state = LocalState::unborrowed();
        state.flags.set(usize::MAX - 1);

        let panic = panic::catch_unwind(AssertUnwindSafe(|| {
            let _ = state.try_shared(here());
        }))
        .expect_err("a full reader count must panic, not wrap to zero");
        let message = panic
            .downcast_ref::<String>()
            .expect("borrow_lite panics with a formatted String");
        assert!(message.contains("overflow"), "{message}");

        // The refused increment left the count alone, so the cell still refuses
        // a writer: the reader that filled the counter is still accounted for.
        assert_eq!(state.flags.get(), usize::MAX - 1);
        assert!(state.try_exclusive(here()).is_err());
    }

    #[test]
    fn local_add_shared_overflow_panics_instead_of_wrapping() {
        let state = LocalState::unborrowed();
        state.flags.set(usize::MAX - 1);

        let panic = panic::catch_unwind(AssertUnwindSafe(|| state.add_shared()))
            .expect_err("cloning a reader past the counter's limit must panic");
        let message = panic
            .downcast_ref::<String>()
            .expect("borrow_lite panics with a formatted String");
        assert!(message.contains("overflow"), "{message}");
        assert_eq!(state.flags.get(), usize::MAX - 1);
    }

    #[test]
    fn atomic_reader_count_overflow_panics_and_undoes_the_increment() {
        let state = AtomicState::unborrowed();
        state.shared.store(usize::MAX, Ordering::SeqCst);

        let panic = panic::catch_unwind(AssertUnwindSafe(|| state.add_shared()))
            .expect_err("an atomic reader count past isize::MAX must panic");
        let message = panic
            .downcast_ref::<String>()
            .expect("borrow_lite panics with a formatted String");
        assert!(message.contains("overflow"), "{message}");
        // The increment is undone before the panic, so the cell is left exactly
        // as it was found - still full, still refusing a writer.
        assert_eq!(state.shared.load(Ordering::SeqCst), usize::MAX);
        assert!(state.try_exclusive(here()).is_err());
    }

    #[test]
    fn a_reader_count_below_the_limit_still_grows() {
        // The guard must not fire on the ordinary path.
        let local = LocalState::unborrowed();
        assert_eq!(add_local_reader(&local.flags), 1);
        assert_eq!(local.flags.get(), 1);

        let atomic = AtomicState::unborrowed();
        add_atomic_reader(&atomic.shared);
        assert_eq!(atomic.shared.load(Ordering::SeqCst), 1);
    }
}
