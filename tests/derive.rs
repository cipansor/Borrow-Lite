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

/// A `shared`/`local` handle's `Clone` must not require the *value* to be
/// `Clone`.
///
/// Cloning such a handle is an `Rc`/`Arc` bump - it never clones the value - so
/// the generated impl carries no `where` bound on the input struct. A bound
/// would make the handle un-clonable whenever the value is, which is not the
/// contract. This struct deliberately derives nothing but `Cell`, so a bound
/// would make the clone below fail to compile. (The `inline` kind is different:
/// its handle owns the value - see the inline tests further down.)
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

// Each accessor takes its field's visibility, so a caller outside the struct's
// module reaches the `pub` fields and nothing else. The private-field half is a
// `compile_fail` doctest on the `Cell` re-export; this is the half that must
// keep compiling.
mod shop {
    use borrow_lite::Cell;

    /// An item with a public name and a price only `shop` may set.
    #[derive(Cell)]
    #[cell(local)]
    pub struct Item {
        /// Anyone may rename the item.
        pub name: String,
        price: u32,
    }

    impl ItemCell {
        /// The module's own constructor, the only way to set a price.
        pub fn priced(name: &str, price: u32) -> Self {
            // `new` is private here (the fields' visibilities differ), but the
            // struct's own module can still call it.
            Self::new(String::from(name), price)
        }

        /// The module can still use the private field's accessor.
        pub fn price_cents(&self) -> u32 {
            *self.price() * 100
        }
    }
}

#[test]
fn the_derive_accessors_follow_field_visibility() {
    let item = shop::ItemCell::priced("tea", 3);
    item.name_mut().push('!');
    assert_eq!(item.name().as_str(), "tea!");
    assert_eq!(item.price_cents(), 300);
}

/// A `where` clause on the value struct is carried onto the handle struct, so
/// a value whose fields only make sense under the bound still derives.
pub trait Unit {
    /// How a value of this unit is stored.
    type Repr;
}

impl Unit for u8 {
    type Repr = u16;
}

/// A reading stored in the unit's representation.
#[derive(Cell)]
#[cell(local)]
pub struct Meter<U>
where
    U: Unit,
{
    /// The stored reading.
    pub raw: U::Repr,
}

#[test]
fn the_derive_keeps_the_where_clause() {
    let meter = MeterCell::<u8>::new(3);
    *meter.raw_mut() += 1;
    assert_eq!(*meter.raw(), 4);
}

/// An `inline` handle owns its value, so cloning it copies the value: the
/// clone is a separate cell, not a second handle to the same one.
#[test]
fn the_derive_inline_clone_copies_the_value() {
    let original = InlineFlagCell::new(false);
    let copy = original.clone();
    *copy.on_mut() = true;
    assert!(!*original.on());
    assert!(*copy.on());
}

/// ...which is why only the `inline` kind needs the value to be `Clone`. A
/// non-`Clone` struct still derives an inline handle; that handle is simply
/// not `Clone` (it used to fail to compile at all).
#[derive(Cell)]
#[cell(inline)]
struct InlineNotClone {
    value: u8,
}

#[test]
fn the_derive_inline_does_not_need_a_clone_value() {
    let cell = InlineNotCloneCell::new(1);
    *cell.value_mut() += 1;
    assert_eq!(*cell.value(), 2);
}

/// A generic inline value is cloneable exactly when the value is.
#[derive(Clone, Cell)]
#[cell(inline)]
struct InlinePair<T> {
    first: T,
    second: T,
}

#[test]
fn the_derive_inline_clone_follows_a_generic_value() {
    let pair = InlinePairCell::new(String::from("a"), String::from("b"));
    let copy = pair.clone();
    copy.first_mut().push('!');
    assert_eq!((pair.first().as_str(), copy.first().as_str()), ("a", "a!"));
    assert_eq!(*copy.second(), "b");
}
