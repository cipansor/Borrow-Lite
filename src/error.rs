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
    /// `None` is possible, and the two cases are worth knowing:
    ///
    /// - On a [`Shared`](crate::Shared) cell a conflict blamed on
    ///   [`Readers`](ConflictKind::Readers) has no holder. Readers do not record
    ///   their location - that would be an atomic store on every read borrow,
    ///   and it would race - so the [`count`](Self::count) is reported instead.
    ///   A writer conflict always names the writer.
    /// - On a [`Local`](crate::Local) cell the holder is always recorded when a
    ///   borrow is in the way: the first reader for a readers conflict, the
    ///   writer for a writer conflict.
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
