//! The declarative macros.
//!
//! They exist because the guard-taking forms are the ones people reach for
//! (`let value = cell.borrow_mut();`) while the closure forms are the ones
//! that stay correct, and a macro can make the second group as short as the
//! first.

/// Take a shared borrow into a named binding.
///
/// Equivalent to `let name = cell.borrow();`, and the borrow lives until the
/// end of the enclosing block.
///
/// ```
/// use lite::{borrow, Local};
///
/// let cell = Local::new(vec![1, 2, 3]);
/// borrow!(cell, values);
/// assert_eq!(values.len(), 3);
/// ```
#[macro_export]
macro_rules! borrow {
    ($cell:expr, $name:ident) => {
        let $name = ($cell).borrow();
    };
}

/// Take an exclusive borrow into a named binding.
///
/// ```
/// use lite::{borrow_mut, Local};
///
/// let cell = Local::new(vec![1, 2, 3]);
/// borrow_mut!(cell, values);
/// values.push(4);
/// drop(values);
/// assert_eq!(cell.with(|v| v.len()), 4);
/// ```
#[macro_export]
macro_rules! borrow_mut {
    ($cell:expr, $name:ident) => {
        let mut $name = ($cell).borrow_mut();
    };
}

/// Take a shared borrow, propagating a [`Conflict`](crate::Conflict) with `?`.
///
/// The enclosing function must return a `Result` whose error type can be built
/// from [`Conflict`](crate::Conflict).
///
/// ```
/// use lite::{try_borrow, Conflict, Local};
///
/// fn first(cell: &Local<Vec<u8>>) -> Result<u8, Conflict> {
///     try_borrow!(cell, values);
///     Ok(values[0])
/// }
///
/// assert_eq!(first(&Local::new(vec![7])).unwrap(), 7);
/// ```
#[macro_export]
macro_rules! try_borrow {
    ($cell:expr, $name:ident) => {
        let $name = ($cell).try_borrow()?;
    };
}

/// Take an exclusive borrow, propagating a [`Conflict`](crate::Conflict) with `?`.
///
/// ```
/// use lite::{try_borrow_mut, Conflict, Local};
///
/// fn push(cell: &Local<Vec<u8>>, value: u8) -> Result<(), Conflict> {
///     try_borrow_mut!(cell, values);
///     values.push(value);
///     Ok(())
/// }
///
/// let cell = Local::new(Vec::new());
/// push(&cell, 1).unwrap();
/// assert_eq!(cell.with(|v| v.clone()), vec![1]);
/// ```
#[macro_export]
macro_rules! try_borrow_mut {
    ($cell:expr, $name:ident) => {
        let mut $name = ($cell).try_borrow_mut()?;
    };
}

/// Run a block with a shared borrow in scope, releasing it afterwards.
///
/// Unlike [`borrow!`](crate::borrow), the borrow cannot outlive the block, so
/// it cannot be the cause of a later conflict.
///
/// ```
/// use lite::{with, Local};
///
/// let cell = Local::new(vec![1, 2, 3]);
/// let len = with!(cell, values, { values.len() });
/// assert_eq!(len, 3);
/// ```
#[macro_export]
macro_rules! with {
    ($cell:expr, $name:ident, $body:block) => {
        ($cell).with(|$name| $body)
    };
}

/// Run a block with an exclusive borrow in scope, releasing it afterwards.
///
/// ```
/// use lite::{with_mut, Local};
///
/// let cell = Local::new(vec![1, 2, 3]);
/// with_mut!(cell, values, { values.push(4) });
/// assert_eq!(cell.with(|v| v.len()), 4);
/// ```
#[macro_export]
macro_rules! with_mut {
    ($cell:expr, $name:ident, $body:block) => {
        ($cell).with_mut(|$name| $body)
    };
}

/// Declare a cell whose fields get their own accessor methods.
///
/// The point is to delete the closure. A struct inside a cell is normally read
/// through `with` and written through `with_mut`, which is correct but verbose
/// and easy to get subtly wrong (the wrong field name, a borrow held too long).
/// `cell!` generates one accessor pair per field instead:
///
/// ```
/// use lite::cell;
///
/// cell! {
///     shared Stats, StatsData {
///         count: usize => count_mut,
///         name: String => name_mut,
///     }
/// }
///
/// let stats = Stats::new(0, String::from("hi"));
/// *stats.count_mut() += 1;          // exclusive borrow, released at `;`
/// stats.name_mut().push('!');
/// assert_eq!((*stats.count(), stats.name().len()), (1, 3));
/// ```
///
/// The getter is named after the field; the setter name is spelled out, because
/// a declarative macro cannot build an identifier like `count_mut` from `count`.
///
/// The keyword chooses the cell, so all three shapes work: `inline` (no
/// allocation, `Send`), `local` (single-threaded, shared by `Rc`), `shared`
/// (thread-safe, shared by `Arc`).
///
/// ```
/// use lite::cell;
///
/// cell! {
///     inline Point, PointData {
///         x: i32 => x_mut,
///         y: i32 => y_mut,
///     }
/// }
///
/// let p = Point::new(1, 2);
/// *p.x_mut() += 10;
/// assert_eq!((*p.x(), *p.y()), (11, 2));
/// ```
///
/// # What is generated
///
/// For `shared Stats, StatsData { .. }`:
///
/// * `StatsData` - the value struct, `Clone + Debug + PartialEq`, one public
///   field per declaration, in order.
/// * `Stats::new(fields..)` - one argument per field, in declaration order.
/// * `Stats::handle()` - the underlying [`Shared`](crate::Shared) (or
///   [`Local`](crate::Local)), for the whole-struct API.
/// * `Stats::count() -> impl Deref<Target = usize>` - a shared borrow.
/// * `Stats::count_mut() -> impl DerefMut<Target = usize>` - an exclusive borrow.
/// * `Clone` for `Stats`, and `Deref` to the cell, so `stats.with(..)`,
///   `stats.borrow()` and `stats.try_with(..)` work directly on the handle.
///
/// Every line is safe code on the public API: the accessors are
/// [`ReadGuard::map`](crate::ReadGuard::map) and
/// [`WriteGuard::map`](crate::WriteGuard::map), so the conflict diagnostics and
/// the cost are exactly those of a plain borrow.
///
/// # One cell, not one per field
///
/// The whole struct lives in a single cell, so a shared and an exclusive borrow
/// cannot overlap - the same rule as `RefCell<Struct>`. Two reads compose, but a
/// read and a write held at once is a conflict:
///
/// ```should_panic
/// use lite::cell;
///
/// cell! { local Pair, PairData { a: u8 => a_mut, b: u8 => b_mut } }
///
/// let pair = Pair::new(1, 2);
/// let a = pair.a();        // shared borrow, still alive
/// let b = pair.b_mut();    // panics: a is still borrowed
/// # let _ = (a, b);
/// ```
#[macro_export]
macro_rules! cell {
    ($(#[$meta:meta])* local $name:ident, $data:ident {
        $($(#[$fmeta:meta])* $field:ident : $ty:ty => $set:ident),* $(,)?
    }) => {
        $crate::cell!(@make $(#[$meta])* Local, local, $name, $data, $($(#[$fmeta])* $field : $ty => $set),*);
    };
    ($(#[$meta:meta])* shared $name:ident, $data:ident {
        $($(#[$fmeta:meta])* $field:ident : $ty:ty => $set:ident),* $(,)?
    }) => {
        $crate::cell!(@make $(#[$meta])* Shared, shared, $name, $data, $($(#[$fmeta])* $field : $ty => $set),*);
    };
    ($(#[$meta:meta])* inline $name:ident, $data:ident {
        $($(#[$fmeta:meta])* $field:ident : $ty:ty => $set:ident),* $(,)?
    }) => {
        $crate::cell!(@make $(#[$meta])* Inline, inline, $name, $data, $($(#[$fmeta])* $field : $ty => $set),*);
    };
    (@make $(#[$meta:meta])* $kind:ident, $ctor:ident, $name:ident, $data:ident,
     $($(#[$fmeta:meta])* $field:ident : $ty:ty => $set:ident),* $(,)?) => {
        #[doc = concat!("The value guarded by a [`", stringify!($name), "`].")]
        #[derive(Clone, Debug, PartialEq)]
        pub struct $data {
            $(
                $(#[$fmeta])*
                #[doc = concat!("The `", stringify!($field), "` field.")]
                pub $field: $ty,
            )*
        }

        $(#[$meta])*
        #[doc = concat!("A handle to a [`", stringify!($data), "`] cell.")]
        pub struct $name($crate::__macro_support::$kind<$data>);

        impl $name {
            #[doc = concat!("Build a `", stringify!($name), "` from one value per field, in order.")]
            #[must_use]
            pub fn new($($field: $ty),*) -> Self {
                Self($crate::__macro_support::$ctor($data { $($field),* }))
            }

            #[doc = concat!("The underlying cell, for the whole-struct API.")]
            #[must_use]
            pub fn handle(&self) -> &$crate::__macro_support::$kind<$data> {
                &self.0
            }

            $(
                #[doc = concat!("Borrow `", stringify!($field), "` shared.")]
                #[must_use]
                pub fn $field(&self) -> impl ::core::ops::Deref<Target = $ty> + '_ {
                    $crate::__macro_support::ReadGuard::map(self.0.borrow(), |d| &d.$field)
                }

                #[doc = concat!("Borrow `", stringify!($field), "` exclusively.")]
                #[must_use]
                pub fn $set(&self) -> impl ::core::ops::DerefMut<Target = $ty> + '_ {
                    $crate::__macro_support::WriteGuard::map(self.0.borrow_mut(), |d| &mut d.$field)
                }
            )*
        }
        impl Clone for $name {
            fn clone(&self) -> Self {
                Self(self.0.clone())
            }
        }

        impl ::core::ops::Deref for $name {
            type Target = $crate::__macro_support::$kind<$data>;

            fn deref(&self) -> &Self::Target {
                &self.0
            }
        }
    };
}
