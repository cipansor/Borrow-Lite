//! The optional `#[derive(Cell)]`: one struct name instead of `cell!`'s two.
//!
//! Gated on the `derive` feature, which is off by default. The derive is the
//! proc-macro counterpart of the declarative `cell!` macro, so it must produce
//! the same surface: a `<Name>Cell` handle, per-field `field()`/`field_mut()`
//! accessors, `new`, `handle`, `Deref`, and `Clone`.
#![cfg(feature = "derive")]

use borrow_lite::Cell;

/// The default kind is `shared`, so a bare `#[derive(Cell)]` gives a
/// thread-safe cell. The input struct is the value; the derive adds the handle.
#[derive(Clone, Debug, PartialEq, Cell)]
#[allow(dead_code)]
pub struct Stats {
    hits: u64,
    name: String,
}

/// `#[cell(local)]` selects the single-threaded cell.
#[derive(Clone, Debug, PartialEq, Cell)]
#[cell(local)]
#[allow(dead_code)]
pub struct LocalPoint {
    x: i32,
    y: i32,
}

/// `#[cell(inline)]` selects the inline cell.
#[derive(Clone, Debug, PartialEq, Cell)]
#[cell(inline)]
#[allow(dead_code)]
pub struct InlineFlag {
    on: bool,
}

#[test]
fn the_derive_builds_a_shared_handle() {
    let stats = StatsCell::new(3, String::from("x"));
    assert_eq!(*stats.hits(), 3);
    *stats.hits_mut() += 1;
    assert_eq!(*stats.hits(), 4);
    assert_eq!(stats.name().clone(), "x");
}

#[test]
fn the_derive_handle_has_the_whole_struct_api() {
    let stats = StatsCell::new(0, String::from("a"));
    // `Deref` reaches the whole-cell methods.
    stats.with_mut(|s| s.hits += 5);
    assert_eq!(stats.with(|s| s.hits), 5);
    // `handle()` hands back the underlying `Shared`.
    let handle: &borrow_lite::Shared<Stats> = stats.handle();
    assert_eq!(handle.with(|s| s.hits), 5);
    // `Clone` clones the handle, not the value.
    let copy = stats.clone();
    copy.with_mut(|s| s.hits = 99);
    assert_eq!(stats.with(|s| s.hits), 99);
}

#[test]
fn the_derive_selects_the_cell_kind() {
    let point = LocalPointCell::new(1, 2);
    assert_eq!((*point.x(), *point.y()), (1, 2));
    *point.x_mut() += 10;
    assert_eq!(*point.x(), 11);

    let flag = InlineFlagCell::new(false);
    *flag.on_mut() = true;
    assert!(*flag.on());
}

#[test]
fn the_derive_send_sync_matches_the_kind() {
    fn assert_send<T: Send>() {}
    fn assert_sync<T: Sync>() {}
    // The `shared` kind is `Send + Sync` when its fields are.
    assert_send::<StatsCell>();
    assert_sync::<StatsCell>();
    // The `inline` kind is `Send` but not `Sync`, like `RefCell`.
    assert_send::<InlineFlagCell>();
}

/// The handle's `Clone` must not require the *value* to be `Clone`.
///
/// Cloning a handle is an `Rc`/`Arc` bump - it never clones the value - so the
/// generated impl carries no `where` bound on the input struct. A bound would
/// make the handle un-clonable whenever the value is, which is not the
/// contract, and for a concrete struct an unsatisfied `where` is a hard error
/// rather than a latent one. This struct deliberately derives nothing but
/// `Cell`, so the old bound would not compile.
#[derive(Cell)]
struct NotClone {
    value: u8,
}

#[test]
fn the_derive_clone_does_not_need_a_clone_value() {
    let a = NotCloneCell::new(1);
    let b = a.clone();
    *b.value_mut() = 2;
    assert_eq!((*a.value(), *b.value()), (2, 2));
}

/// The derive generates no `Debug` for the handle, so a value that is not
/// `Debug` still works - matching the `cell!` macro, whose handle is only
/// `Clone` and `Deref`.
#[derive(Cell)]
struct NotDebug {
    value: u8,
}

#[test]
fn the_derive_needs_no_debug_value() {
    let cell = NotDebugCell::new(7);
    assert_eq!(*cell.value(), 7);
}
