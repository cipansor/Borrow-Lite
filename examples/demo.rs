//! Run with `cargo run --example demo`.
//!
//! A walk through the API, side by side with the `std` it replaces: the three
//! shapes (`Local`, `Shared`, `Inline`), the closure-free and closure forms,
//! conflicts as values, the `cell!` macro, and the threaded case. Each section
//! shows the same small operation the `std` type would need and what `borrow_lite`
//! prints when it goes wrong.

use std::cell::RefCell;
use std::rc::Rc;

use borrow_lite::{Conflict, Inline, Local, Observable, Shared, cell, inline, local, shared};

cell! {
    shared Stats, StatsData {
        count: usize => count_mut,
        name: String => name_mut,
    }
}

fn main() {
    refcell_style();
    borrow_style();
    closure_free();
    conflicts_as_values();
    named_fields();
    inline_shape();
    threaded();
    observers();
}

fn refcell_style() {
    println!("== Rc<RefCell<T>> ==");
    let total = Rc::new(RefCell::new(0i32));
    let other = Rc::clone(&total);

    *other.borrow_mut() += 5;
    println!("value: {}", total.borrow());

    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _held = total.borrow();
        *other.borrow_mut() += 1;
    }));
    let message = outcome
        .err()
        .and_then(|payload| payload.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| String::from("<not a String>"));
    println!("panic: {message}");
    println!();
}

fn borrow_style() {
    println!("== borrow_lite::Local ==");
    let total = local(0i32);
    let other = total.clone();

    other.with_mut(|v| *v += 5);
    println!("value: {}", total.get());

    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _held = total.borrow();
        *other.borrow_mut() += 1;
    }));
    let message = outcome
        .err()
        .and_then(|payload| payload.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| String::from("<not a String>"));
    println!("panic: {message}");
    println!();
}

fn closure_free() {
    println!("== closure-free access ==");
    let counter = local(0i32);
    counter.set(5);
    println!("get: {}", counter.get());

    // `try_set` hands the value back when the write is refused, so nothing is
    // lost to a conflict.
    let held = counter.borrow();
    match counter.try_set(6) {
        Ok(()) => println!("unexpected: wrote through a held borrow"),
        Err((conflict, value)) => println!("refused {value}: {conflict}"),
    }
    drop(held);
    counter.set(6);
    println!("after release: {}", counter.get());
    println!();
}

fn conflicts_as_values() {
    println!("== conflicts as values ==");

    fn deposit(account: &Local<i32>, amount: i32) -> Result<i32, Conflict> {
        let mut balance = account.try_borrow_mut()?;
        *balance += amount;
        Ok(*balance)
    }

    let account = local(100);
    println!("deposit: {:?}", deposit(&account, 50));

    let held = account.borrow();
    match deposit(&account, 50) {
        Ok(balance) => println!("unexpected: {balance}"),
        Err(conflict) => println!("refused: {conflict}"),
    }
    drop(held);
    println!("after release: {:?}", deposit(&account, 1));
    println!();
}

fn named_fields() {
    println!("== cell! named fields ==");
    let stats = Stats::new(0, String::from("requests"));
    *stats.count_mut() += 1;
    println!("{} = {}", stats.name().as_str(), *stats.count());

    // The whole struct lives in one cell, so a write held across a read is a
    // conflict. The accessors are closure-free already; the non-panicking form
    // goes through the underlying handle.
    let count = stats.count();
    let refused = stats.handle().try_with_mut(|d| d.name.push('!')).is_err();
    println!("write while a read is held: refused = {refused}");
    drop(count);
    stats.handle().with_mut(|d| d.name.push('!'));
    println!("after release: {}", stats.name().as_str());
    println!();
}

fn inline_shape() {
    println!("== Inline<T>: zero allocation ==");
    println!(
        "Inline<u64> = {} bytes (RefCell<u64> = {} bytes)",
        std::mem::size_of::<Inline<u64>>(),
        std::mem::size_of::<RefCell<u64>>()
    );

    let cell = inline(vec![1, 2, 3]);
    cell.with_mut(|v| v.push(4));
    println!("len = {}", cell.get().len());

    let held = cell.borrow();
    println!(
        "write refused while a read is held: {}",
        cell.try_borrow_mut().is_err()
    );
    drop(held);
    println!();
}

fn threaded() {
    println!("== Shared<T>: many threads, no deadlock ==");
    let counter = shared(0usize);
    let threads = 4;

    std::thread::scope(|scope| {
        for _ in 0..threads {
            let counter: Shared<usize> = counter.clone();
            scope.spawn(move || {
                // Borrows never wait, so a busy cell is retried rather than
                // blocked on - the opposite of `RwLock`, which deadlocks if
                // re-entered and poisons if a holder panics.
                while counter.try_with_mut(|c| *c += 1).is_err() {
                    std::thread::yield_now();
                }
            });
        }
    });

    println!("{threads} threads incremented to {}", counter.get());
    println!();
}

fn observers() {
    println!("== observers ==");

    let temperature = Observable::new(20i32);
    let readings = Observable::new(Vec::new());

    let readings_in_observer = readings.clone();
    temperature.observe(move |t| readings_in_observer.update(|r| r.push(*t)));

    temperature.update(|t| *t += 1);
    temperature.update(|t| *t += 1);
    println!("readings: {:?}", readings.get());

    // An observer that mutates the value it is being notified about is
    // refused, and told why.
    let temperature_in_observer = temperature.clone();
    let outcome = Observable::new(None);
    let outcome_in_observer = outcome.clone();
    temperature.observe(move |_| {
        let result = temperature_in_observer.try_update(|t| *t += 100);
        outcome_in_observer.update(|slot| *slot = Some(result.is_err()));
    });

    temperature.update(|t| *t += 1);
    println!("re-entrant update refused: {:?}", outcome.get());
}
