//! How many times does a cell hit the allocator?
//!
//! The README claims "the cell is still one allocation". That is a *count*, and
//! the existing suite only ever measured the *time* of an allocation
//! (`benches/compare.rs`), which cannot tell one allocation from two. A counting
//! global allocator can, and it is the only way to check the claim - a timing
//! benchmark would happily agree with a cell that allocated twice.
//!
//! Everything lives in one `#[test]`: the assertions read a counter, and
//! splitting them across tests would make each one's delta depend on the order
//! the harness happened to run them in.
//!
//! The counters are *thread-local*, not process-wide. `-Zmiri-many-seeds` runs
//! several seeds as parallel workers inside one process, and each worker's own
//! allocations would otherwise land in a shared counter and show up as a
//! phantom extra allocation. A thread-local counter counts only what the thread
//! taking the measurement did, which is the quantity this test is about. The
//! `const` initialiser matters: a lazily-initialised `thread_local!` can itself
//! allocate on first use, which from inside a global allocator is a re-entrancy
//! bug.

use core::cell::Cell;
use std::alloc::{GlobalAlloc, Layout, System};

use lite::{Inline, Local, Observable, shared};

struct Counting;

thread_local! {
    static ALLOCS: Cell<usize> = const { Cell::new(0) };
    static DEALLOCS: Cell<usize> = const { Cell::new(0) };
}

fn bump(counter: &'static std::thread::LocalKey<Cell<usize>>) {
    // `with` on a `const`-initialised key cannot allocate, so this is safe to
    // call from inside `alloc`/`dealloc`.
    counter.with(|c| c.set(c.get().wrapping_add(1)));
}

fn count(counter: &'static std::thread::LocalKey<Cell<usize>>) -> usize {
    counter.with(Cell::get)
}

// SAFETY: every method forwards to `System`, which is a correct allocator, and
// the counters are thread-local `Cell`s used only for bookkeeping.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        bump(&ALLOCS);
        // SAFETY: `System` is a correct allocator, and `GlobalAlloc::alloc`
        // requires the caller to uphold `layout`'s invariants.
        unsafe { System.alloc(layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        bump(&ALLOCS);
        // SAFETY: same contract as `alloc`, forwarded to `System`.
        unsafe { System.alloc_zeroed(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        bump(&DEALLOCS);
        // SAFETY: `ptr` came from this allocator, which forwards to `System`
        // with the same `layout`.
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        bump(&ALLOCS);
        // SAFETY: same contract as `dealloc`, forwarded to `System`.
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static ALLOCATOR: Counting = Counting;

/// Run `f`, returning its result and how many allocations it made.
fn allocs<T>(f: impl FnOnce() -> T) -> (T, usize) {
    let before = count(&ALLOCS);
    let out = f();
    (out, count(&ALLOCS) - before)
}

#[test]
fn a_cell_allocates_exactly_once() {
    // First prove the counter works. Without this, every `assert_eq!(.., 0)`
    // below would pass on a hook that never fires.
    let (v, n) = allocs(|| Vec::<u8>::with_capacity(8));
    assert_eq!(n, 1, "the counter missed a real allocation");
    assert_eq!(v.capacity(), 8);

    // `Local<u64>`: one allocation, and a clone must not add one. The `Rc`
    // clone bumps a count; it must not touch the allocator.
    let (cell, created) = allocs(|| Local::new(0u64));
    assert_eq!(created, 1, "Local::new did not allocate exactly once");

    // `Inline<u64>`: the value lives in the cell, so creation, borrowing and
    // even a refused borrow are all allocation-free. This is the whole reason
    // the type exists, and it is a count no timing benchmark can check.
    let (inline_cell, created) = allocs(|| Inline::new(0u64));
    assert_eq!(created, 0, "Inline::new allocated");

    let (_, borrowed) = allocs(|| {
        let read = inline_cell.borrow();
        let value = *read;
        drop(read);
        value
    });
    assert_eq!(borrowed, 0, "an Inline read borrow allocated");

    let (_, borrowed) = allocs(|| {
        let mut write = inline_cell.borrow_mut();
        *write += 1;
    });
    assert_eq!(borrowed, 0, "an Inline write borrow allocated");

    let inline_held = inline_cell.borrow();
    let (_, refused) = allocs(|| inline_cell.try_borrow_mut().unwrap_err());
    assert_eq!(refused, 0, "a refused Inline borrow allocated");
    drop(inline_held);

    let (_clone, cloned) = allocs(|| cell.clone());
    assert_eq!(cloned, 0, "cloning a handle allocated");

    // Borrowing is pointer arithmetic, not allocation.
    let (_, borrowed) = allocs(|| {
        let read = cell.borrow();
        let value = *read;
        drop(read);
        value
    });
    assert_eq!(borrowed, 0, "a read borrow allocated");

    let (_, borrowed) = allocs(|| {
        let mut write = cell.borrow_mut();
        *write += 1;
    });
    assert_eq!(borrowed, 0, "a write borrow allocated");

    // A refused borrow builds a `Conflict`, which is `Copy` and owns no string,
    // so even the refusal path must not allocate.
    let held = cell.borrow();
    let (conflict, refused) = allocs(|| cell.try_borrow_mut().unwrap_err());
    assert_eq!(refused, 0, "a refused borrow allocated");
    drop(held);

    // The message is built only when asked for, and that does allocate. The
    // exact count is `String` growth (5 here), so only "it costs something" is
    // asserted - the claim under test is that a *refused* borrow is free and
    // that building the *message* is what costs.
    let (_, formatted) = allocs(|| conflict.to_string());
    assert!(formatted >= 1, "Conflict::to_string allocated nothing");

    // `Shared` is an `Arc`: same story.
    let (shared_cell, created) = allocs(|| shared(0u64));
    assert_eq!(created, 1, "shared() did not allocate exactly once");
    let (clone, cloned) = allocs(|| shared_cell.clone());
    assert_eq!(cloned, 0, "cloning a Shared allocated");

    // Dropping *both* handles frees the one block. Dropping only one must not:
    // the `Arc` still has a live owner.
    let deallocs_before = count(&DEALLOCS);
    drop(clone);
    assert_eq!(
        count(&DEALLOCS) - deallocs_before,
        0,
        "dropping one of two handles freed the block early"
    );
    drop(shared_cell);
    assert_eq!(
        count(&DEALLOCS) - deallocs_before,
        1,
        "dropping the last handle did not free exactly one block"
    );

    // `Observable<T>` is two `Local`s - the value and the observer list - so it
    // is two allocations, not one. Documenting it here means the "one
    // allocation" claim above cannot be quietly stretched to cover it.
    let (observable, created) = allocs(|| Observable::new(0u64));
    assert_eq!(
        created, 2,
        "Observable::new allocated {created} times, expected 2"
    );

    // `observe` pushes into the list's `Vec`, so it may allocate - but only the
    // first few times, as the `Vec` grows. What must not happen is an
    // allocation per call forever.
    let (_id, first) = allocs(|| observable.observe(|_| {}));
    assert!(first <= 1, "the first observe allocated {first} times");
    let (_id, second) = allocs(|| observable.observe(|_| {}));
    assert_eq!(
        second, 0,
        "the second observe allocated, so it is not amortised"
    );
    let (_id, third) = allocs(|| observable.observe(|_| {}));
    assert_eq!(third, 0, "the third observe allocated");
    let (_id, fourth) = allocs(|| observable.observe(|_| {}));
    assert_eq!(fourth, 0, "the fourth observe allocated");

    // An update with **no** observers must not allocate: the round breaks out
    // before either scratch list grows. That is the "taking the list out and
    // putting it back" cost the README describes, and it is pointer work only.
    //
    // The cell is built *before* the measured region on purpose. Nesting the
    // measurement inside the creation lets the optimiser sink the creation's
    // two allocations past the inner snapshot, which reports 2 for an update
    // that allocates nothing. Measured separately, the answer is 0.
    let empty = Observable::new(0u64);
    let (_, no_observer_update) = allocs(|| empty.update(|v| *v += 1));
    assert_eq!(
        no_observer_update, 0,
        "an update with no observers allocated"
    );

    // With observers it allocates, and the count is a constant 2 per update -
    // `notify` moves the list into two scratch `Vec`s (`running`, `finished`),
    // each of which allocates once. It does not scale with the observer count
    // (`observable` has four), but it is not free either, and the README's time
    // table is what hides it.
    // Three on the very first update, not two: the notification guard also
    // records the in-flight observer *ids* (so `unobserve` can tell a live
    // observer from an unknown one mid-notification), and that list grows from
    // empty here. It keeps its capacity, so every update after the first is
    // back to two - the cost is one-time, not per call. The amortised number is
    // what matters, and it is pinned by measuring the second update too.
    //
    // `<= 3`, not `== 3`: the exact count on the first update depends on the
    // `Vec` growth strategy (a first push may or may not reallocate), which is
    // not part of what this test pins. The steady state below is the assertion
    // that has to be exact.
    let (_, first_update) = allocs(|| observable.update(|v| *v += 1));
    assert!(
        first_update <= 3,
        "the first update grew the in-flight id list by more than expected"
    );
    let (_, with_observers) = allocs(|| observable.update(|v| *v += 1));
    assert_eq!(
        with_observers, 2,
        "an update with observers allocated in the steady state"
    );
    let (_, again) = allocs(|| observable.update(|v| *v += 1));
    assert_eq!(
        with_observers, again,
        "the next update allocated a different amount"
    );
}
