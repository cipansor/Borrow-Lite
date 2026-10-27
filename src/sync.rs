//! The synchronisation primitives the crate is built on, in one place.
//!
//! Under `cfg(loom)` these are loom's instrumented versions, so the model
//! checker can see every load, store and thread switch. Otherwise they are the
//! real ones.
//!
//! `alloc`'s types are used rather than `std`'s, so the crate stays `no_std` on
//! the normal path. Loom is only ever enabled by its own test job, where `std`
//! is available.

// `Local` is single-threaded and `Rc`-based, so loom has nothing to model
// there and there is no `loom::rc` to swap in. Only the shared cell changes.
pub(crate) use alloc::rc::Rc;

#[cfg(not(loom))]
pub(crate) use alloc::sync::Arc;
#[cfg(not(loom))]
pub(crate) use core::sync::atomic::{AtomicBool, AtomicPtr, AtomicUsize, Ordering};

#[cfg(loom)]
pub(crate) use loom::sync::Arc;
#[cfg(loom)]
pub(crate) use loom::sync::atomic::{AtomicBool, AtomicPtr, AtomicUsize, Ordering};
