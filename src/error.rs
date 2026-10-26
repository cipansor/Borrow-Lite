//! Borrow conflicts, and the diagnostics that come with them.

use alloc::string::{String, ToString};
use core::fmt::{self, Write};
use core::panic::Location;

/// What kind of borrow was already active when a new borrow was refused.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ConflictKind {
    /// One or more shared (read) borrows were active.
    Readers,
    /// An exclusive (write) borrow was active.
    Writer,
}

impl ConflictKind {
    /// Human readable description, used in [`Conflict`]'s `Display`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Readers => "immutably borrowed",
            Self::Writer => "mutably borrowed",
        }
    }
}

/// A refused borrow.
///
/// Unlike `std::cell::BorrowMutError`, a `Conflict` records *where* the borrow
/// that is in the way was taken, which turns "already borrowed" into an
/// actionable message.
///
/// ```
/// use lite::Local;
///
/// let cell = Local::new(vec![1, 2, 3]);
/// let _read = cell.borrow();
/// let err = cell.try_borrow_mut().unwrap_err();
///
/// assert_eq!(err.kind(), lite::ConflictKind::Readers);
/// assert!(err.holder().is_some()); // the `borrow()` call site
/// assert!(err.to_string().contains("already immutably borrowed"));
/// ```
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Conflict {
    kind: ConflictKind,
    /// Number of active borrows of that kind, when the mode can count them.
    count: Option<usize>,
    holder: Option<&'static Location<'static>>,
    wanted: Option<&'static Location<'static>>,
}

impl Conflict {
    pub(crate) const fn new(kind: ConflictKind) -> Self {
        Self {
            kind,
            count: None,
            holder: None,
            wanted: None,
        }
    }

    pub(crate) const fn with_count(mut self, count: usize) -> Self {
        self.count = Some(count);
        self
    }

    pub(crate) const fn with_holder(mut self, holder: Option<&'static Location<'static>>) -> Self {
        self.holder = holder;
        self
    }

    pub(crate) const fn with_wanted(mut self, wanted: &'static Location<'static>) -> Self {
        self.wanted = Some(wanted);
        self
    }

    /// Which kind of borrow is currently active.
    #[must_use]
    pub const fn kind(self) -> ConflictKind {
        self.kind
    }

    /// Where the borrow that is in the way was taken, if known.
    ///
    /// `None` is possible, and the cases are worth knowing:
    ///
    /// - On a [`Shared`](crate::Shared) cell a conflict blamed on
    ///   [`Readers`](ConflictKind::Readers) has no holder. Readers do not record
    ///   their location - that would be an atomic store on every read borrow,
    ///   and it would race - so the [`count`](Self::count) is reported instead.
    ///   A writer conflict always names the writer, because the writer claims the
    ///   cell and publishes its location in one compare-exchange.
    /// - On a [`Local`](crate::Local) cell the *first* reader records its
    ///   location, and it is named while that reader is alive - including when it
    ///   is the only reader. Once it is dropped while other readers remain, the
    ///   holder becomes `None` rather than naming a borrow that has already been
    ///   released; the [`count`](Self::count) still reports how many readers
    ///   block the write. A writer conflict always names the writer.
    ///
    /// One more case is worth stating: on a [`Shared`](crate::Shared) cell a
    /// [`Writer`](ConflictKind::Writer) conflict can name a writer that was only
    /// *briefly* claiming the cell. A writer publishes its location and then
    /// reads the reader count; if it finds readers it releases the claim again, so
    /// a reader that runs in that window is refused with a conflict naming a
    /// writer that never held the cell. The refusal is real - the cell was busy -
    /// but the named location is the momentarily-claiming writer, not a borrow
    /// that is still alive. The window is documented on the atomic cell's state;
    /// it is closed only at the cost of a wider atomic word.
    #[must_use]
    pub const fn holder(self) -> Option<&'static Location<'static>> {
        self.holder
    }

    /// Where the refused borrow was attempted.
    #[must_use]
    pub const fn wanted(self) -> Option<&'static Location<'static>> {
        self.wanted
    }

    /// Number of active borrows of [`kind`](Self::kind), if the mode counts them.
    ///
    /// A count is reported only when *readers* are what is in the way. A
    /// [`Writer`](ConflictKind::Writer) conflict - a borrow refused because an
    /// exclusive borrow is live, whether the refused borrow was a read or a
    /// write - is never counted: the reader count is necessarily zero at that
    /// moment, so reporting it would say "0 active borrows" about a cell that is
    /// very much borrowed. Those conflicts are told apart by
    /// [`holder`](Self::holder) instead, which names the writer.
    #[must_use]
    pub const fn count(self) -> Option<usize> {
        self.count
    }

    /// The panic payload used by [`borrow`](crate::Inner::borrow) and
    /// [`borrow_mut`](crate::Inner::borrow_mut).
    pub(crate) fn into_panic_message(self) -> String {
        let mut out = String::from("lite: cell is already ");
        out.push_str(self.kind.as_str());
        if let Some(wanted) = self.wanted {
            // Writing into the `String` cannot fail.
            let _ = write!(out, " (borrow attempted at {wanted})");
        }
        if let Some(holder) = self.holder {
            let _ = write!(out, "; held at {holder}");
        }
        if let Some(count) = self.count {
            let noun = if count == 1 { "borrow" } else { "borrows" };
            let _ = write!(out, " ({count} active {noun})");
        }
        out.push_str(" - a borrow is still alive. Drop the guard, or use `with`/`with_mut` so it cannot leak.");
        out
    }
}

impl fmt::Display for Conflict {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "already {}", self.kind.as_str())?;
        if let Some(holder) = self.holder {
            write!(f, " (held at {holder})")?;
        }
        if let Some(wanted) = self.wanted {
            write!(f, ", refused borrow at {wanted}")?;
        }
        Ok(())
    }
}

impl fmt::Debug for Conflict {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Conflict")
            .field("kind", &self.kind)
            .field("count", &self.count)
            .field("holder", &self.holder.map(ToString::to_string))
            .field("wanted", &self.wanted.map(ToString::to_string))
            .finish()
    }
}

impl core::error::Error for Conflict {}
