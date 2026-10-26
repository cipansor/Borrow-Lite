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
