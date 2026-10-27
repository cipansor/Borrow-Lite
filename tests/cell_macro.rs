//! The `cell!` macro: what it generates, and the guarantees it keeps.
//!
//! The macro is sugar over the public API, so these tests are less about new
//! behaviour and more about the generated surface: the accessors, the `Deref`
//! pass-through, `Clone` sharing one cell, and the two limits worth stating -
//! one cell guards the whole struct, and a conflict is still a runtime event.

use borrow_lite::{Conflict, ConflictKind, cell};

cell! {
    shared Stats, StatsData {
        count: usize => count_mut,
        name: String => name_mut,
    }
}

cell! {
    local Tally, TallyData {
        total: i64 => total_mut,
    }
}

#[test]
fn getter_borrows_shared_and_derefs_to_the_field() {
    let stats = Stats::new(3, String::from("hi"));
    assert_eq!(*stats.count(), 3);
    assert_eq!(stats.name().as_str(), "hi");
}

#[test]
fn setter_borrows_exclusively_and_releases_at_the_statement() {
    let stats = Stats::new(0, String::from("hi"));
    *stats.count_mut() += 1;
    stats.name_mut().push('!');
    assert_eq!((*stats.count(), stats.name().as_str()), (1, "hi!"));
}

#[test]
fn reads_compose_in_one_expression() {
    // The reason the getter is shared rather than exclusive: two reads in one
    // expression must not conflict.
    let stats = Stats::new(2, String::from("xy"));
    let sum = *stats.count() + stats.name().len();
    assert_eq!(sum, 4);
}

#[test]
fn deref_passes_through_to_the_whole_struct_api() {
    let stats = Stats::new(1, String::from("a"));
    assert_eq!(stats.with(|d| d.count), 1);
    stats.with_mut(|d| d.name.push('b'));
    assert_eq!(stats.handle().with(|d| d.name.clone()), "ab");
    assert_eq!(stats.try_with(|d| d.count).unwrap(), 1);
}

#[test]
fn clone_shares_one_cell() {
    let a = Stats::new(0, String::from("x"));
    let b = a.clone();
    *b.count_mut() = 9;
    assert_eq!(*a.count(), 9);
    assert!(a.handle().ptr_eq(b.handle()));
}

#[test]
fn new_takes_one_argument_per_field_in_order() {
    let stats = Stats::new(7, String::from("seven"));
    assert_eq!(
        stats.handle().with(|d| (d.count, d.name.clone())),
        (7, String::from("seven"))
    );
}

#[test]
fn local_variant_shares_the_same_surface() {
    let tally = Tally::new(41);
    *tally.total_mut() += 1;
    assert_eq!(*tally.total(), 42);
}

#[test]
fn a_read_held_across_a_write_is_a_conflict() {
    // The honest limit: one cell guards the whole struct.
    let stats = Stats::new(1, String::from("a"));
    let held = stats.count();
    let err = stats.try_borrow_mut().unwrap_err();
    assert_eq!(err.kind(), ConflictKind::Readers);
    drop(held);
    assert!(stats.try_borrow_mut().is_ok());
}

#[test]
fn accessors_return_the_same_conflict_type_as_a_plain_borrow() {
    // `cell!` adds no error surface of its own.
    fn write(cell: &Stats) -> Result<(), Conflict> {
        let _guard = cell.try_borrow_mut()?;
        Ok(())
    }
    let stats = Stats::new(0, String::from("x"));
    assert!(write(&stats).is_ok());
}

/// A refused field access must blame the *user's* call, not the generated
/// accessor body.
///
/// The accessors are ordinary functions calling `Inner::borrow`/
/// `Inner::borrow_mut`, which record their immediate caller as `wanted`. Without
/// `#[track_caller]` on the generated accessor that immediate caller is the
/// accessor body inside the macro expansion, so the panic names the `cell!`
/// invocation line instead of the `stats.name_mut()` the user wrote.
///
/// Because `cell!` expands inside *this* crate the file name alone cannot tell
/// the two apart - both are in `tests/cell_macro.rs` - so the assertion pins the
/// line: `line!()` and the accessor call are both tokens of the macro below, and
/// both resolve to the single line the macro is invoked on.
#[cfg(feature = "diagnostics")]
#[test]
fn a_refused_field_access_names_the_caller() {
    /// Panic line from a `borrow attempted at <file>:<line>:<col>` message.
    fn reported_line(message: &str) -> u32 {
        message
            .split("borrow attempted at ")
            .nth(1)
            .expect("no location in the message")
            .split(':')
            .nth(1)
            .expect("no line in the location")
            .parse()
            .expect("the line is not a number")
    }

    /// Call `$receiver.$method()` and require the refusal to name this line.
    macro_rules! refused_at_this_line {
        ($what:expr, $receiver:expr, $method:ident) => {{
            let expected = line!();
            let result = ::std::panic::catch_unwind(::std::panic::AssertUnwindSafe(|| {
                let _ = $receiver.$method();
            }));
            let panic = result.expect_err($what);
            let message = panic
                .downcast_ref::<String>()
                .cloned()
                .unwrap_or_else(|| String::from("<non-String panic payload>"));
            assert!(
                message.contains("borrow attempted at tests/cell_macro.rs:"),
                "{} blamed a location outside this file: {message}",
                $what
            );
            assert_eq!(
                reported_line(&message),
                expected,
                "{} blamed the generated accessor instead of the call: {message}",
                $what
            );
        }};
    }

    // A write accessor refused by a held read.
    let stats = Stats::new(1, String::from("a"));
    let held = stats.count();
    refused_at_this_line!("Stats::name_mut", stats, name_mut);
    drop(held);

    // A read accessor refused by a held write.
    let held = stats.count_mut();
    refused_at_this_line!("Stats::name", stats, name);
    drop(held);

    // The `local` shape expands the same accessors.
    let tally = Tally::new(0);
    let held = tally.total();
    refused_at_this_line!("Tally::total_mut", tally, total_mut);
    drop(held);

    // And the `inline` shape.
    cell! { inline Counter, CounterData { value: u32 => value_mut } }
    let counter = Counter::new(0);
    let held = counter.value();
    refused_at_this_line!("Counter::value_mut", counter, value_mut);
    drop(held);

    // `wanted` is the same location the non-panicking path reports.
    let stats = Stats::new(1, String::from("a"));
    let held = stats.count();
    let err = stats.handle().try_borrow_mut().unwrap_err();
    assert_eq!(err.wanted().map(|l| l.file()), Some(file!()));
    drop(held);
}

#[test]
fn the_macro_accepts_doc_comments_and_derives_them() {
    // A doc comment on the handle and a per-field one both survive.
    cell! {
        /// A point.
        shared Point, PointData {
            /// The x coordinate.
            x: i32 => x_mut,
            /// The y coordinate.
            y: i32 => y_mut,
        }
    }
    let p = Point::new(1, 2);
    *p.x_mut() += 1;
    assert_eq!((*p.x(), *p.y()), (2, 2));
}

#[test]
fn zero_field_struct_is_allowed() {
    cell! { shared Empty, EmptyData {} }
    let e = Empty::new();
    assert_eq!(e.handle().with(|d| d.clone()), EmptyData {});
}

/// The optional setter name: omitting `=> setter` derives `<field>_mut`.
///
/// This needs the `paste` feature (a declarative macro cannot build an
/// identifier from another), so the test is gated on it - but it is the whole
/// point of the feature, so it must actually run when the feature is on rather
/// than be left to a doctest that cannot.
#[cfg(feature = "paste")]
mod derived_setter_names {
    use borrow_lite::cell;

    cell! {
        shared Counter, CounterData {
            count: u64,
            label: String,
        }
    }

    #[test]
    fn the_setter_is_the_field_name_plus_mut() {
        let counter = Counter::new(0, String::from("x"));
        *counter.count_mut() += 5;
        counter.label_mut().push('!');
        assert_eq!((*counter.count(), counter.label().as_str()), (5, "x!"));
    }
}
