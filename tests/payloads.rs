//! What kinds of payload the cells accept, and what happens to them.
//!
//! Every other suite uses `u8`, `u64`, `Vec` or `String`, all of which are
//! ordinary sized, aligned, `Copy`-or-not types. The interesting payloads for a
//! cell built on `UnsafeCell` and raw pointers are the awkward ones: a type
//! with its own `Drop` (is it dropped exactly once, and not early?), a
//! zero-sized type (the pointer is dangling, which is fine only if nothing
//! dereferences it in a way that assumes otherwise), and a type with an
//! alignment stricter than the allocator's default.
//!
//! Miri is the point of most of these: a double drop or a leaked payload is
//! exactly what it reports, and a single-threaded test cannot see it.

use std::cell::Cell;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use borrow_lite::{Local, Observable, Shared, local, shared};

/// Counts its own drops, so a test can assert *how many times* the payload was
/// destroyed rather than only that it was.
struct CountsDrops(Rc<Cell<usize>>);

impl Drop for CountsDrops {
    fn drop(&mut self) {
        self.0.set(self.0.get() + 1);
    }
}

#[test]
fn a_payload_is_dropped_exactly_once() {
    let drops = Rc::new(Cell::new(0));
    {
        let cell = Local::new(CountsDrops(Rc::clone(&drops)));
        cell.with_mut(|v| v.0.set(v.0.get()));
        assert_eq!(drops.get(), 0, "the payload was dropped while still owned");
    }
    assert_eq!(drops.get(), 1, "the payload was not dropped exactly once");
}

#[test]
fn dropping_a_guard_does_not_drop_the_payload() {
    let drops = Rc::new(Cell::new(0));
    let cell = Local::new(CountsDrops(Rc::clone(&drops)));

    {
        let _read = cell.borrow();
        assert_eq!(drops.get(), 0);
    }
    {
        let _write = cell.borrow_mut();
        assert_eq!(drops.get(), 0);
    }

    assert_eq!(drops.get(), 0, "a guard drop destroyed the payload");
}

#[test]
fn live_clones_keep_the_payload_alive() {
    let drops = Rc::new(Cell::new(0));
    let cell = Local::new(CountsDrops(Rc::clone(&drops)));
    let alias = cell.clone();

    drop(cell);
    assert_eq!(drops.get(), 0, "the payload died while a clone was alive");

    drop(alias);
    assert_eq!(drops.get(), 1);
}

#[test]
fn a_payload_is_dropped_exactly_once_across_threads() {
    let drops = Arc::new(AtomicUsize::new(0));
    let cell: Shared<CountsDropsShared> = shared(CountsDropsShared(Arc::clone(&drops)));

    let handles: Vec<_> = (0..8).map(|_| cell.clone()).collect();
    for handle in handles {
        std::thread::spawn(move || {
            handle.with(|v| {
                assert_eq!(v.0.load(Ordering::SeqCst), 0, "dropped while shared");
            });
        })
        .join()
        .unwrap();
    }

    drop(cell);
    assert_eq!(
        drops.load(Ordering::SeqCst),
        1,
        "the payload was dropped other than exactly once"
    );
}

/// The threaded counterpart of [`CountsDrops`].
struct CountsDropsShared(Arc<AtomicUsize>);

impl Drop for CountsDropsShared {
    fn drop(&mut self) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

/// `replace` moves a value out and hands it back; it must not also drop it.
/// A cell that dropped the old value on the way out would be a double free once
/// the caller dropped the returned value, which Miri would report.
#[test]
fn replace_hands_back_the_old_value_without_dropping_it() {
    let old_drops = Rc::new(Cell::new(0));
    let new_drops = Rc::new(Cell::new(0));

    let cell = Local::new(CountsDrops(Rc::clone(&old_drops)));
    let returned = cell.replace(CountsDrops(Rc::clone(&new_drops)));

    assert_eq!(old_drops.get(), 0, "replace dropped the value it returned");
    assert_eq!(new_drops.get(), 0);

    drop(returned);
    assert_eq!(
        old_drops.get(),
        1,
        "the returned value was not dropped once"
    );

    drop(cell);
    assert_eq!(new_drops.get(), 1);
}

/// A zero-sized payload: the `NonNull` points at a dangling-but-aligned
/// address, so every use has to avoid forming a reference that assumes a real
/// object. Reading and writing it must still work.
#[test]
fn a_zero_sized_payload_works() {
    let unit: Local<()> = local(());
    // The payload is a ZST, so there is no value to compare - asserting
    // `*v == ()` would always succeed. Exercise the borrow instead.
    unit.with(|v| {
        let _: &() = v;
    });
    unit.with_mut(|v| {
        let _: &mut () = v;
    });
    assert!(unit.try_borrow().is_ok());

    // A ZST with a `Drop`, which must still run exactly once.
    let drops = Rc::new(Cell::new(0));
    struct ZstDrop(Rc<Cell<usize>>);
    impl Drop for ZstDrop {
        fn drop(&mut self) {
            self.0.set(self.0.get() + 1);
        }
    }
    {
        let cell = Local::new(ZstDrop(Rc::clone(&drops)));
        cell.with(|_| {});
        assert_eq!(drops.get(), 0);
    }
    assert_eq!(drops.get(), 1);

    // And a shared ZST.
    let shared_unit: Shared<()> = shared(());
    shared_unit.with_mut(|v| {
        let _: &mut () = v;
    });
    std::thread::spawn(move || {
        shared_unit.with(|v| {
            let _: &() = v;
        });
    })
    .join()
    .unwrap();
}

/// An over-aligned payload. The allocator only promises alignment for the
/// layout it is handed, so a cell that computed its pointer or its size wrong
/// would hand back a misaligned `&mut T`.
#[test]
fn a_highly_aligned_payload_keeps_its_alignment() {
    #[repr(align(64))]
    struct Aligned(u64);

    let cell = Local::new(Aligned(7));
    let observed = cell.with(|v| core::ptr::from_ref(v) as usize);
    assert_eq!(observed % 64, 0, "the payload was handed out misaligned");

    let after = cell.with_mut(|v| {
        let addr = core::ptr::from_mut(v) as usize;
        v.0 += 1;
        addr
    });
    assert_eq!(after % 64, 0);
    assert_eq!(cell.with(|v| v.0), 8);

    // The threaded cell too: `Arc` allocates its own layout.
    let shared_cell: Shared<Aligned> = shared(Aligned(1));
    let addr = shared_cell.with(|v| core::ptr::from_ref(v) as usize);
    assert_eq!(addr % 64, 0, "the shared payload was misaligned");
}

/// A payload much larger than a pointer, to catch any place that assumed the
/// value fit inline.
#[test]
fn a_large_payload_works() {
    const N: usize = 4096;

    let cell: Local<[u8; N]> = local([0u8; N]);
    cell.with_mut(|v| v[N - 1] = 42);
    assert_eq!(cell.with(|v| v[N - 1]), 42);
    assert_eq!(cell.with(|v| v.len()), N);

    let shared_cell: Shared<[u8; N]> = shared([7u8; N]);
    let sum = shared_cell.with(|v| v.iter().map(|b| u64::from(*b)).sum::<u64>());
    assert_eq!(sum, 7 * N as u64);
}

/// A payload whose `Drop` panics. The panic has to propagate, and the payload
/// must not be dropped a second time while unwinding - which would abort, and
/// which Miri reports as a double free.
#[test]
fn a_panicking_payload_drop_propagates_without_double_dropping() {
    struct PanicOnDrop;

    impl Drop for PanicOnDrop {
        fn drop(&mut self) {
            panic!("payload drop");
        }
    }

    let outcome = catch_unwind(AssertUnwindSafe(|| {
        let cell = Local::new(PanicOnDrop);
        drop(cell);
    }));
    assert!(outcome.is_err(), "the payload's panic did not propagate");
}

/// A panic *inside the value* while a guard is alive: the guard is dropped
/// during unwinding, the borrow is released, and the payload survives to be
/// dropped once afterwards.
#[test]
fn a_payload_survives_a_panic_while_borrowed() {
    let drops = Rc::new(Cell::new(0));
    let cell = Local::new(CountsDrops(Rc::clone(&drops)));

    let panic = catch_unwind(AssertUnwindSafe(|| {
        let mut guard = cell.borrow_mut();
        // A real `&mut` reborrow, so the guard's `DerefMut` is exercised
        // without dropping the payload (which would move the drop count).
        let payload: &mut CountsDrops = &mut guard;
        payload.0.set(payload.0.get());
        panic!("boom while borrowed");
    }));
    assert!(panic.is_err());

    assert_eq!(drops.get(), 0, "the payload died during unwinding");
    assert!(cell.try_borrow_mut().is_ok(), "the borrow leaked");
    drop(cell);
    assert_eq!(drops.get(), 1);
}

/// `Observable<T>` is a container of its own - it owns the payload *and* an
/// observer list - so its drop is a separate path from `Local`/`Shared`.
/// Dropping it with observers still registered must destroy the payload exactly
/// once (and the observers with it).
#[test]
fn an_observable_drops_its_payload_once() {
    let drops = Rc::new(Cell::new(0));
    let calls = Rc::new(Cell::new(0));

    {
        let source = Observable::new(CountsDrops(Rc::clone(&drops)));
        let calls_in_observer = Rc::clone(&calls);
        // Captures only `calls`, not `source`, so there is no reference cycle
        // to break before the drop.
        source.observe(move |_| calls_in_observer.set(calls_in_observer.get() + 1));
        source.update(|v| v.0.set(v.0.get()));

        assert_eq!(drops.get(), 0, "dropped while the observable was alive");
        assert_eq!(calls.get(), 1, "the observer did not run");
    }

    assert_eq!(
        drops.get(),
        1,
        "the observable did not drop its payload exactly once"
    );
}
