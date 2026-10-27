//! Handle and guard sizes, as a quick sanity check on the "no extra overhead"
//! claim. Run with `cargo run --release --example sizes`.
use borrow_lite::{Inline, Local, Shared, Snapshot, inline, local, shared};
use std::rc::Rc;
use std::sync::Arc;

fn main() {
    println!(
        "Inline<u64> = {} bytes (RefCell<u64> = {} bytes)",
        std::mem::size_of::<Inline<u64>>(),
        std::mem::size_of::<std::cell::RefCell<u64>>()
    );
    println!(
        "Local<u64>  = {} bytes (Rc<u64>  = {} bytes)",
        std::mem::size_of::<Local<u64>>(),
        std::mem::size_of::<Rc<u64>>()
    );
    println!(
        "Shared<u64> = {} bytes (Arc<u64> = {} bytes)",
        std::mem::size_of::<Shared<u64>>(),
        std::mem::size_of::<Arc<u64>>()
    );
    println!(
        "Snapshot<u64> = {} bytes (the read-mostly cell: value pointer + writer lock + retired list)",
        std::mem::size_of::<Snapshot<u64>>()
    );

    let i = inline(0u64);
    let read = i.borrow();
    println!("inline ReadGuard  = {} bytes", std::mem::size_of_val(&read));
    drop(read);
    let write = i.borrow_mut();
    println!(
        "inline WriteGuard = {} bytes",
        std::mem::size_of_val(&write)
    );
    drop(write);

    let l = local(0u64);
    let read = l.borrow();
    println!("local  ReadGuard  = {} bytes", std::mem::size_of_val(&read));
    drop(read);
    let write = l.borrow_mut();
    println!(
        "local  WriteGuard = {} bytes",
        std::mem::size_of_val(&write)
    );
    drop(write);

    let s = shared(0u64);
    let read = s.borrow();
    println!("shared ReadGuard  = {} bytes", std::mem::size_of_val(&read));
    drop(read);
    let write = s.borrow_mut();
    println!(
        "shared WriteGuard = {} bytes",
        std::mem::size_of_val(&write)
    );
}
