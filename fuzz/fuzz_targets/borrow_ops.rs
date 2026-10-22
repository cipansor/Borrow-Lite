//! Fuzz the borrow bookkeeping against a shadow model.
//!
//! The point is not "does it crash". Each byte of input is a script of
//! operations against a `Local<u64>` or `Shared<u64>`, and the same script is
//! replayed against a trivial oracle - the expected value plus counters for how
//! many readers and writers the script believes are live. After every operation
//! the crate's answers must match the oracle's:
//!
//! * a read succeeds exactly when the oracle has no writer;
//! * a write succeeds exactly when the oracle has no reader and no writer;
//! * a refused borrow blames the right thing (`Writer` vs `Readers`);
//! * the value read back is the value the oracle holds;
//! * the closure forms release their borrow, and a held guard's drop releases
//!   its share, so the cell is never left permanently borrowed.
//!
//! Miri and loom check the memory model. This checks the *logic* of the state
//! machine over far more operation sequences than either can enumerate.
//!
//! Run with:
//!
//! ```text
//! cargo +nightly fuzz run borrow_ops
//! ```

#![no_main]

use lite::{local, shared, ConflictKind, ReadGuard, WriteGuard};
use libfuzzer_sys::fuzz_target;

/// One scripted operation, decoded from a byte.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Op {
    TryRead,
    TryWrite,
    HoldRead,
    HoldWrite,
    DropHeld,
    Replace,
    ReplaceThenReplaceBack,
    Compare,
    Order,
    Swap,
    ReplaceWith,
}

fn decode(byte: u8) -> Op {
    match byte % 11 {
        0 => Op::TryRead,
        1 => Op::TryWrite,
        2 => Op::HoldRead,
        3 => Op::HoldWrite,
        4 => Op::DropHeld,
        5 => Op::Replace,
        6 => Op::ReplaceThenReplaceBack,
        7 => Op::Compare,
        8 => Op::Order,
        9 => Op::Swap,
        _ => Op::ReplaceWith,
    }
}

/// What the script believes is true about the cell.
#[derive(Default)]
struct Model {
    value: u64,
    /// The sibling cell's value. The script never borrows the sibling, so it is
    /// always available for a swap.
    sibling: u64,
    readers: usize,
    writer: bool,
}

impl Model {
    fn can_read(&self) -> bool {
        !self.writer
    }

    fn can_write(&self) -> bool {
        !self.writer && self.readers == 0
    }

    /// The kind a refusal must blame, given the current state.
    fn blame(&self) -> ConflictKind {
        if self.writer {
            ConflictKind::Writer
        } else {
            ConflictKind::Readers
        }
    }
}

/// Shared body for both cell types. `$cell` is a `Local<u64>` or `Shared<u64>`;
/// `$label` names it in failure messages.
macro_rules! run_script {
    ($cell:expr, $sibling:expr, $data:expr, $label:expr) => {{
        let cell = $cell;
        let sibling = $sibling;
        let data: &[u8] = $data;
        let label: &str = $label;

        let mut model = Model::default();
        let mut held_reads: Vec<ReadGuard<'_, u64, _>> = Vec::new();
        let mut held_writes: Vec<WriteGuard<'_, u64, _>> = Vec::new();

        for (index, &byte) in data.iter().enumerate() {
            let op = decode(byte);
            match op {
                Op::TryRead => match cell.try_with(|v| *v) {
                    Ok(value) => {
                        assert!(model.can_read(), "{label}: read granted, model says no");
                        assert_eq!(value, model.value, "{label}: read the wrong value");
                    }
                    Err(conflict) => {
                        assert!(
                            !model.can_read(),
                            "{label}: read refused, model says yes ({conflict})"
                        );
                        assert_eq!(
                            conflict.kind(),
                            ConflictKind::Writer,
                            "{label}: a refused read must blame a writer"
                        );
                    }
                },

                Op::TryWrite => match cell.try_with_mut(|v| *v += 1) {
                    Ok(()) => {
                        assert!(model.can_write(), "{label}: write granted, model says no");
                        model.value = model.value.wrapping_add(1);
                    }
                    Err(conflict) => {
                        assert!(
                            !model.can_write(),
                            "{label}: write refused, model says yes ({conflict})"
                        );
                        assert_eq!(
                            conflict.kind(),
                            model.blame(),
                            "{label}: wrong blame at op {index}"
                        );
                    }
                },

                Op::HoldRead => match cell.try_borrow() {
                    Ok(guard) => {
                        assert!(model.can_read(), "{label}: held read granted, model says no");
                        assert_eq!(*guard, model.value, "{label}: held read saw the wrong value");
                        model.readers += 1;
                        held_reads.push(guard);
                    }
                    Err(conflict) => {
                        assert!(!model.can_read(), "{label}: held read refused, model says yes");
                        assert_eq!(conflict.kind(), ConflictKind::Writer);
                    }
                },

                Op::HoldWrite => match cell.try_borrow_mut() {
                    Ok(mut guard) => {
                        assert!(model.can_write(), "{label}: held write granted, model says no");
                        *guard += 1;
                        model.writer = true;
                        model.value = model.value.wrapping_add(1);
                        held_writes.push(guard);
                    }
                    Err(conflict) => {
                        assert!(!model.can_write(), "{label}: held write refused, model says yes");
                        assert_eq!(conflict.kind(), model.blame());
                    }
                },

                Op::DropHeld => {
                    let reads = held_reads.len();
                    let writes = held_writes.len();
                    held_reads.clear();
                    held_writes.clear();
                    model.readers -= reads;
                    if writes > 0 {
                        model.writer = false;
                    }
                }

                // `replace` uses the panicking form, so it may only be called
                // when the oracle says the write will be allowed. When it is
                // not, the refusable form must still refuse - which is the
                // check worth making.
                Op::Replace => {
                    if model.can_write() {
                        let taken = cell.replace(0);
                        assert_eq!(taken, model.value, "{label}: replace returned the wrong value");
                        model.value = 0;
                    } else {
                        assert!(cell.try_with_mut(|_| ()).is_err(), "{label}: replace would panic");
                    }
                }

                // Equality: a cell compared with *itself* short-circuits on the
                // pointer, so it is `Ok(true)` even while a writer is live -
                // exactly the invariant `PartialEq` and `try_eq` share with
                // `Rc`/`Arc`. A *different* cell holding value 0 needs a read
                // borrow, so it agrees with the oracle's readability.
                Op::Compare => {
                    let alias = cell.clone();
                    assert!(cell.ptr_eq(&alias), "{label}: a clone must be ptr-equal");
                    assert!(
                        !cell.ptr_eq(&sibling),
                        "{label}: distinct cells must not be ptr-equal"
                    );
                    assert_eq!(
                        cell.try_eq(&alias),
                        Ok(true),
                        "{label}: self-comparison must not borrow"
                    );
                    // The panicking form carries the same short-circuit: it
                    // must not borrow either, so it cannot panic here even with
                    // a writer active.
                    assert!(
                        cell == alias,
                        "{label}: `==` on a clone must short-circuit, not borrow"
                    );
                    match cell.try_eq(&sibling) {
                        Ok(equal) => {
                            assert!(model.can_read(), "{label}: compared while model says no");
                            assert_eq!(
                                equal,
                                model.value == model.sibling,
                                "{label}: try_eq disagreed with the model"
                            );
                        }
                        Err(conflict) => {
                            assert!(!model.can_read(), "{label}: compare refused, model says yes");
                            assert_eq!(conflict.kind(), ConflictKind::Writer);
                        }
                    }
                }

                Op::ReplaceThenReplaceBack => {
                    if model.can_write() {
                        let taken = cell.replace(0);
                        assert_eq!(taken, model.value);
                        let back = cell.replace(taken);
                        assert_eq!(back, 0, "{label}: the second replace was wrong");
                        model.value = taken;
                    } else {
                        assert!(cell.try_with_mut(|_| ()).is_err(), "{label}: replace would panic");
                    }
                }

                // Ordering mirrors equality: the self-comparison short-circuits
                // on the pointer for every `T: Ord`, so `cmp`/`partial_cmp` on a
                // clone never borrow and cannot panic even with a writer live.
                // Distinct cells order by value under a shared borrow, so they
                // agree with the oracle's readability - and the ordering must be
                // consistent with the values the model holds.
                Op::Order => {
                    let alias = cell.clone();
                    assert_eq!(
                        cell.cmp(&alias),
                        core::cmp::Ordering::Equal,
                        "{label}: self-cmp must short-circuit to Equal"
                    );
                    assert_eq!(
                        cell.partial_cmp(&alias),
                        Some(core::cmp::Ordering::Equal),
                        "{label}: self-partial_cmp must short-circuit to Equal"
                    );
                    assert_eq!(
                        cell.try_cmp(&alias),
                        Ok(Some(core::cmp::Ordering::Equal)),
                        "{label}: try_cmp on self must not borrow"
                    );
                    match cell.try_cmp(&sibling) {
                        Ok(ordering) => {
                            assert!(model.can_read(), "{label}: ordered while model says no");
                            let expected = model.value.cmp(&model.sibling);
                            assert_eq!(
                                ordering,
                                Some(expected),
                                "{label}: try_cmp disagreed with the model"
                            );
                            assert_eq!(
                                cell.partial_cmp(&sibling),
                                Some(expected),
                                "{label}: partial_cmp disagreed with try_cmp"
                            );
                            assert_eq!(
                                cell.cmp(&sibling),
                                expected,
                                "{label}: cmp disagreed with try_cmp"
                            );
                        }
                        Err(conflict) => {
                            assert!(!model.can_read(), "{label}: order refused, model says yes");
                            assert_eq!(conflict.kind(), ConflictKind::Writer);
                        }
                    }
                }

                // Swap with the sibling. The script never borrows the sibling,
                // so the swap succeeds exactly when this cell can be written -
                // and it must exchange the two values, with the sibling's value
                // tracked in the model.
                Op::Swap => match cell.try_swap(&sibling) {
                    Ok(()) => {
                        assert!(model.can_write(), "{label}: swap granted, model says no");
                        core::mem::swap(&mut model.value, &mut model.sibling);
                    }
                    Err(conflict) => {
                        assert!(!model.can_write(), "{label}: swap refused, model says yes");
                        assert_eq!(conflict.kind(), model.blame());
                    }
                },

                // `replace_with` under the panicking form is only called when
                // the model says the write is allowed. When it is refused, the
                // closure comes back and the retry must then work.
                Op::ReplaceWith => {
                    if model.can_write() {
                        let old = cell.replace_with(|v| v.wrapping_add(7));
                        assert_eq!(old, model.value, "{label}: replace_with returned the wrong value");
                        model.value = model.value.wrapping_add(7);
                    } else {
                        match cell.try_replace_with(|v| v.wrapping_add(7)) {
                            Ok(_) => panic!("{label}: replace_with was allowed, model says no"),
                            Err((conflict, f)) => {
                                assert_eq!(conflict.kind(), model.blame());
                                // The refused closure is usable; do not run it
                                // (the model forbids the write), just drop it.
                                let _ = f;
                            }
                        }
                    }
                }
            }
        }

        // However the script ended, dropping every held guard must leave the
        // cell usable. A leaked counter would make this fail.
        held_reads.clear();
        held_writes.clear();
        assert!(
            cell.try_borrow_mut().is_ok(),
            "{label}: the cell was left permanently borrowed"
        );
    }};
}

fuzz_target!(|data: &[u8]| {
    run_script!(local(0u64), local(0u64), data, "Local");
    run_script!(shared(0u64), shared(0u64), data, "Shared");
});
