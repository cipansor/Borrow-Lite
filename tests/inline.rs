//! `Inline<T>`: the `RefCell`-shaped cell.
//!
//! The point of this type is that it is the same borrow checker `RefCell` has,
//! with the same auto traits, but with a conflict message that names the file
//! and line of the borrow in the way. These tests pin all three of those
//! claims: behaviour, diagnostics, and auto traits.

use borrow_lite::{ConflictKind, Inline, inline};

#[test]
fn borrow_and_borrow_mut_work() {
    let cell = Inline::new(vec![1, 2, 3]);
    assert_eq!(cell.borrow().len(), 3);
    cell.borrow_mut().push(4);
    assert_eq!(cell.with(Vec::len), 4);
}

#[test]
fn two_reads_compose() {
    let cell = Inline::new((1u8, String::from("x")));
    let a = cell.borrow();
    let b = cell.borrow();
    assert_eq!((a.0, b.1.as_str()), (1, "x"));
}

#[test]
fn a_read_and_a_write_conflict() {
    let cell = Inline::new(1u8);
    let held = cell.borrow();
    let conflict = cell.try_borrow_mut().unwrap_err();
    assert_eq!(conflict.kind(), ConflictKind::Readers);
    assert_eq!(conflict.count(), Some(1));
    drop(held);
    cell.with_mut(|v| *v += 1);
}

#[cfg(feature = "diagnostics")]
#[test]
fn the_panic_names_where_the_borrow_is_held() {
    let cell = Inline::new(1u8);
    let _held = cell.borrow();
    let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _ = cell.borrow_mut();
    }))
    .unwrap_err();
    let message = panic
        .downcast_ref::<String>()
        .expect("panic payload is a String");
    assert!(
        message.contains("held at tests/inline.rs:"),
        "the holder location should be in the message: {message}"
    );
    assert!(
        message.contains("borrow attempted at tests/inline.rs:"),
        "the refused location should be in the message: {message}"
    );
}

#[test]
fn a_conflict_leaves_the_cell_usable() {
    let cell = Inline::new(0u32);
    {
        let _held = cell.borrow();
        assert!(cell.try_borrow_mut().is_err());
    }
    cell.with_mut(|v| *v += 1);
    assert_eq!(cell.with(|v| *v), 1);
}

#[test]
fn get_mut_and_into_inner_bypass_the_flags() {
    let mut cell = Inline::new(1u8);
    *cell.get_mut() += 1;
    assert_eq!(cell.into_inner(), 2);
}

#[test]
fn from_default_and_eq() {
    assert_eq!(Inline::from(7u8).into_inner(), 7);
    assert_eq!(Inline::<u8>::default().into_inner(), 0);
    assert_eq!(inline(1u8), inline(1u8));
    assert_ne!(inline(1u8), inline(2u8));
}

#[test]
fn debug_reads_the_value_and_reports_a_conflict() {
    let cell = Inline::new(5u8);
    assert_eq!(format!("{cell:?}"), "Inline(5)");
    // An *exclusive* borrow held is what makes `try_borrow` fail; two shared
    // borrows would compose, so `borrow()` here would not do it.
    let _held = cell.borrow_mut();
    let rendered = format!("{cell:?}");
    assert!(
        rendered.contains("Conflict") && rendered.contains("holder"),
        "a cell with a live write borrow should debug as its conflict: {rendered}"
    );
}

#[test]
fn is_send_when_the_payload_is_like_refcell() {
    fn is_send<T: Send>() {}
    is_send::<Inline<u8>>();
    is_send::<Inline<String>>();
}

#[test]
fn a_cell_moves_between_threads_like_refcell() {
    // The case `Local<T>` cannot serve: `RefCell<T: Send>` is `Send`, and so is
    // `Inline<T>`. This is the whole reason the type exists.
    let cell = Inline::new(String::from("start"));
    let seen = std::thread::spawn(move || {
        cell.with_mut(|s| s.push_str("-done"));
        cell.with(String::clone)
    })
    .join()
    .unwrap();
    assert_eq!(seen, "start-done");
}

#[test]
fn a_cell_with_a_non_send_payload_does_not_travel() {
    // `Rc<u8>` is `!Send`, so an `Inline<Rc<u8>>` must be too. The negative is
    // checked with a `compile_fail` doctest on the type; this is the positive
    // half: such a cell still works perfectly well in one thread.
    let cell = Inline::new(std::rc::Rc::new(1u8));
    assert_eq!(**cell.borrow(), 1);
}

#[test]
fn is_not_sync() {
    // `Inline<T>` must stay `!Sync`, exactly like `RefCell<T>`. This is a
    // runtime-visible consequence of that: a `&Inline` cannot be sent to
    // another thread. The `compile_fail` doctest on the type covers the static
    // claim; here we only assert the cell is usable while borrowed twice.
    let cell = Inline::new(1u8);
    let a = cell.borrow();
    let b = cell.borrow();
    assert_eq!(*a + *b, 2);
}

/// The README's claim is that the *value* is the same as `RefCell<T>`: only
/// the behaviour on a refused borrow differs. This walks the same operations
/// through both and compares, so a drift in the semantics shows up here.
#[test]
fn behaviour_matches_refcell() {
    let borrow_lite = Inline::new(vec![1i32, 2, 3]);
    let std_cell = std::cell::RefCell::new(vec![1i32, 2, 3]);

    borrow_lite.borrow_mut().push(4);
    std_cell.borrow_mut().push(4);
    assert_eq!(*borrow_lite.borrow(), *std_cell.borrow());

    assert_eq!(borrow_lite.borrow().len(), std_cell.borrow().len());
    assert_eq!(
        borrow_lite.try_borrow().is_ok(),
        std_cell.try_borrow().is_ok()
    );
    assert_eq!(
        borrow_lite.try_borrow_mut().is_err(),
        std_cell.try_borrow_mut().is_err()
    );

    // Both refuse an exclusive borrow under a live shared borrow, and neither
    // is left poisoned by the refusal.
    {
        let _a = borrow_lite.borrow();
        let _b = std_cell.borrow();
        assert!(borrow_lite.try_borrow_mut().is_err());
        assert!(std_cell.try_borrow_mut().is_err());
    }
    assert_eq!(*borrow_lite.borrow_mut(), *std_cell.borrow_mut());
}

/// `Inline` is the one cell that pays no allocation at all. The count is the
/// honest measure of that; a size assertion cannot tell indirection from
/// padding.
#[test]
fn no_heap_indirection() {
    use core::mem::size_of;
    // The value lives inside the cell: `Inline<u64>` is strictly larger than
    // `u64`, and a `Local<u64>` is one pointer wide because the value is behind
    // that pointer. A size comparison alone would be fooled by padding, so the
    // allocation count in `tests/allocations.rs` is the real proof; this only
    // pins the shape.
    assert!(size_of::<Inline<u64>>() >= size_of::<u64>());
    assert_eq!(size_of::<borrow_lite::Local<u64>>(), size_of::<usize>());
}

/// `Inline::clone` copies the value, like `RefCell::clone`, rather than sharing
/// a handle the way `Local::clone` does. The two are opposite, so both are
/// pinned.
#[test]
fn clone_copies_the_value_not_the_handle() {
    let cell = inline(vec![1u8, 2, 3]);
    let copy = cell.clone();
    copy.with_mut(|v| v.push(4));
    assert_eq!(cell.with(Vec::len), 3);
    assert_eq!(copy.with(Vec::len), 4);
}

/// Cloning under an exclusive borrow must panic with a located message, the
/// same way `RefCell::clone` does, rather than reading through the borrow.
#[cfg(feature = "diagnostics")]
#[test]
fn clone_under_a_write_borrow_reports_the_conflict() {
    let cell = inline(1u8);
    let _held = cell.borrow_mut();
    let err = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| cell.clone()))
        .expect_err("clone under an exclusive borrow must panic");
    let message = err
        .downcast_ref::<String>()
        .expect("borrow_lite panics with a formatted String");
    assert!(message.contains("already mutably borrowed"), "{message}");
    // `Clone` carries `#[track_caller]`, so the refused attempt is reported at
    // this test's `cell.clone()` line, not inside the crate. The holder is the
    // live `borrow_mut()` above, which is the line worth naming.
    assert!(message.contains("held at tests/inline.rs:"), "{message}");
    assert!(
        message.contains("borrow attempted at tests/inline.rs:"),
        "the refused location should be the caller, not the crate: {message}"
    );
}

/// `cell! { inline .. }` generates the same accessor surface as the other two
/// shapes; this pins that the keyword is wired up.
#[test]
fn the_macro_accepts_the_inline_keyword() {
    use borrow_lite::cell;

    cell! {
        inline Counter, CounterData {
            hits: u32 => hits_mut,
            name: String => name_mut,
        }
    }

    let c = Counter::new(0, String::from("a"));
    *c.hits_mut() += 3;
    c.name_mut().push('b');
    assert_eq!(*c.hits(), 3);
    assert_eq!(c.name().as_str(), "ab");
    // `Clone` on the handle copies the value, matching `Inline`'s own `Clone`.
    let d = c.clone();
    *d.hits_mut() += 1;
    assert_eq!(*c.hits(), 3);
    assert_eq!(*d.hits(), 4);
}

/// Ordering for `Inline`, mirroring its `PartialEq`: by value under a shared
/// borrow, with the same self-comparison short-circuit `Local`, `Shared` and
/// `try_cmp` use.
#[test]
fn ordering_matches_refcell() {
    use core::cmp::Ordering;
    use std::cell::RefCell;

    let a = inline(1i32);
    let b = inline(2i32);
    let std_a = RefCell::new(1i32);
    let std_b = RefCell::new(2i32);

    assert_eq!(a.partial_cmp(&b), std_a.partial_cmp(&std_b));
    assert_eq!(a.partial_cmp(&b), Some(Ordering::Less));
    assert!(a < b);
    assert!(a <= inline(1));
    assert_eq!(inline(1).cmp(&inline(2)), Ordering::Less);

    // Self-comparison short-circuits without borrowing, matching `try_cmp` and
    // the shared/local cells.
    assert_eq!(
        a.partial_cmp(&inline(1)),
        std_a.partial_cmp(&RefCell::new(1))
    );
    assert_eq!(a.partial_cmp(&a), Some(Ordering::Equal));
    assert_eq!(a.cmp(&a), Ordering::Equal);
    assert_eq!(a.try_cmp(&a), Ok(Some(Ordering::Equal)));

    // The short-circuit holds under a live exclusive borrow, so `==`, `cmp` and
    // their `try_` twins cannot disagree about a cell compared with itself.
    let held = a.borrow_mut();
    assert!(a == a);
    assert_eq!(a.partial_cmp(&a), Some(Ordering::Equal));
    assert_eq!(a.cmp(&a), Ordering::Equal);
    assert_eq!(a.try_eq(&a), Ok(true));
    assert_eq!(a.try_cmp(&a), Ok(Some(Ordering::Equal)));

    // A *distinct* comparison still panics under the live borrow, like std.
    let ours = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| a < b)).is_err();
    let theirs = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _h = std_a.borrow_mut();
        std_a < std_b
    }))
    .is_err();
    assert_eq!(ours, theirs, "panic behaviour must match RefCell");
    drop(held);
}

/// `Inline`'s `==`/`partial_cmp` agree with their `try_` twins for an
/// irreflexive payload, the one case where the short-circuit is observable.
///
/// Before the short-circuit was added, `inline(NAN) == inline(NAN)` was `false`
/// while `try_eq` was `Ok(true)` - the two entry points disagreed about the
/// same cell, and `Local`/`Shared` agreed with neither's opposite. This pins
/// them together, and against `RefCell`, which has no short-circuit at all.
#[test]
fn eq_and_try_eq_agree_on_an_irreflexive_payload() {
    let a = inline(f64::NAN);
    assert!(a == a, "`==` must short-circuit like `try_eq`");
    assert_eq!(a.try_eq(&a), Ok(true));
    assert_eq!(a.partial_cmp(&a), Some(core::cmp::Ordering::Equal));
    assert_eq!(a.try_cmp(&a), Ok(Some(core::cmp::Ordering::Equal)));

    // The short-circuit runs even under a live exclusive borrow, so it cannot
    // panic where `try_eq` returns `Ok`.
    let held = a.borrow_mut();
    assert!(a == a);
    assert_eq!(a.try_eq(&a), Ok(true));
    drop(held);

    // The consequence, stated plainly and shared with `Local`: a self-comparison
    // of an irreflexive payload reports `true`, where `RefCell` reports `false`.
    assert!(!(std::cell::RefCell::new(f64::NAN) == std::cell::RefCell::new(f64::NAN)));
    let distinct = inline(f64::NAN);
    assert!(a != distinct, "distinct cells still compare by value");
}

#[test]
fn swap_and_replace_with_match_refcell() {
    use std::cell::RefCell;

    // `Inline` derefs to `Inner`, so `swap`/`replace_with` are reachable and
    // must behave exactly like `RefCell` for the happy path.
    let a = inline(1u8);
    let b = inline(2u8);
    let std_a = RefCell::new(1u8);
    let std_b = RefCell::new(2u8);
    a.swap(&b);
    std_a.swap(&std_b);
    assert_eq!(a.with(|v| *v), *std_a.borrow());
    assert_eq!(b.with(|v| *v), *std_b.borrow());

    let c = inline(vec![1, 2, 3]);
    let old = c.replace_with(core::mem::take);
    assert_eq!(old, Vec::<i32>::new());
    assert_eq!(c.with(Vec::clone), vec![1, 2, 3]);

    // A refused swap reports the conflict; a self-swap is refused with a
    // caller-pointed message rather than a second-borrow panic from inside.
    let held = a.borrow();
    assert_eq!(a.try_swap(&b).unwrap_err().kind(), ConflictKind::Readers);
    drop(held);
    let self_conflict = a.try_swap(&a).unwrap_err();
    assert_eq!(self_conflict.kind(), ConflictKind::Writer);
    #[cfg(feature = "diagnostics")]
    assert_eq!(self_conflict.wanted().map(|l| l.file()), Some(file!()));
    #[cfg(not(feature = "diagnostics"))]
    assert_eq!(
        self_conflict.wanted(),
        None,
        "no locations without diagnostics"
    );
    assert_eq!(self_conflict.holder(), None, "a self-swap named a holder");
}

// `Inline::new` and `inline()` are `const`, so an inline cell can be built at
// compile time. The case that needs it is a large value in a `const`-initialised
// `thread_local!`: a lazy initialiser runs at first use and builds the whole
// value as a stack temporary before copying it into the thread-local slot, which
// overflows a small thread stack for a multi-megabyte arena. A `const`
// initialiser stores the value in place instead.
//
// The primary guarantee is at compile time: `const { Inline::new(..) }` only
// compiles if `Inline::new` is a `const fn`, so a regression to a runtime
// constructor is a build error, not a silent behaviour change. The small-stack
// thread below is the runtime backstop - it turns "the arena was built on the
// stack at first use" into a visible overflow, which a plain 2 MB test thread
// would not, since a 1 MiB temporary still fits there.
//
// `thread_local!` is used rather than a bare `static` because `Inline` is
// `!Sync`; that is the shape a per-thread arena actually takes.
mod const_init {
    use borrow_lite::{Inline, inline};

    const WORDS: usize = 1 << 17; // 1 MiB

    struct Arena {
        words: [u64; WORDS],
    }

    impl Arena {
        const fn new() -> Self {
            Self { words: [0; WORDS] }
        }
    }

    thread_local! {
        static ARENA: Inline<Arena> = const { Inline::new(Arena::new()) };
        static COUNTER: Inline<u64> = const { inline(0) };
    }

    #[test]
    fn a_large_value_can_be_const_initialised() {
        // 256 KiB of stack against a 1 MiB arena: a lazy initialiser would
        // overflow this thread before the first borrow, while the `const` path
        // never puts the arena on the stack and so fits with room to spare.
        std::thread::Builder::new()
            .stack_size(256 * 1024)
            .spawn(|| {
                ARENA.with(|cell| {
                    cell.with_mut(|arena| arena.words[0] = 42);
                    assert_eq!(cell.with(|arena| arena.words[0]), 42);
                });
            })
            .unwrap()
            .join()
            .unwrap();
    }

    #[test]
    fn the_const_helper_works_too() {
        COUNTER.with(|cell| {
            cell.with_mut(|n| *n += 1);
            assert_eq!(cell.with(|n| *n), 1);
        });
    }
}
