//! Run with `cargo run --example demo`.
//!
//! Side by side: the `Rc<RefCell<T>>` version of a small counter, and the
//! `lite` version, including what each one prints when it goes wrong.

use std::cell::RefCell;
use std::rc::Rc;

use lite::{Conflict, Local, Observable, local};

fn main() {
    refcell_style();
    lite_style();
    conflicts_as_values();
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

fn lite_style() {
    println!("== lite::Local ==");
    let total = local(0i32);
    let other = total.clone();

    other.with_mut(|v| *v += 5);
    println!("value: {}", total.with(|v| *v));

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

fn observers() {
    println!("== observers ==");

    let temperature = Observable::new(20i32);
    let readings = Observable::new(Vec::new());

    let readings_in_observer = readings.clone();
    temperature.observe(move |t| readings_in_observer.update(|r| r.push(*t)));

    temperature.update(|t| *t += 1);
    temperature.update(|t| *t += 1);
    println!("readings: {:?}", readings.with(|r| r.clone()));

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
    println!("re-entrant update refused: {:?}", outcome.with(|o| *o));
}
