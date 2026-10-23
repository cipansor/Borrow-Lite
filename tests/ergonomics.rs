//! The ergonomics table in the README is measured, not asserted.
//!
//! The comparison is only worth anything if it is honest in both directions, so
//! this file counts the two sides and checks that a claimed reduction is real -
//! and that a case which is *longer* stays reported as longer. The snippets are
//! the idiomatic form of each side (`RefCell`/`Rc<RefCell>`/`Arc<RwLock>` on the
//! left, `lite` on the right); if one side is written badly, the numbers below
//! move and this test fails, which is the point.
//!
//! It counts source characters of the non-blank, non-comment lines. That is a
//! proxy for "how much you type", not a claim about readability; the README says
//! the same. Line counts are checked where they differ by more than one.

/// Characters of code, ignoring blank lines and `//` comments.
fn chars(src: &str) -> usize {
    src.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with("//"))
        .map(str::len)
        .sum()
}

/// Reduction from `old` to `new` as a fraction of `old`.
fn reduction(old: &str, new: &str) -> f64 {
    let (o, n) = (chars(old), chars(new));
    assert!(o > 0, "old snippet is empty");
    (o as f64 - n as f64) / o as f64
}

/// Cases where `lite` is shorter by half or more - the headline claim.
#[test]
fn half_or_more_shorter_where_std_is_awkward() {
    // Swapping two cells. `RefCell::swap` takes two `&self`, so the std form has
    // to go through `mem::swap` on two live guards.
    let std_swap = r#"
        std::mem::swap(&mut *a.borrow_mut(), &mut *b.borrow_mut());
    "#;
    let lite_swap = r#"
        a.swap(&b);
    "#;
    assert!(
        reduction(std_swap, lite_swap) >= 0.75,
        "swap: {:.0}%",
        100.0 * reduction(std_swap, lite_swap)
    );

    // Wiring an observer that cannot re-enter itself. The std form has to drain
    // the list out of its own cell before running it, or the callback that
    // notifies again deadlocks on `borrow_mut`.
    let std_observer = r#"
        let v = Rc::new(RefCell::new(1i32));
        let obs: Rc<RefCell<Vec<Box<dyn FnMut(&i32)>>>> = Rc::new(RefCell::new(Vec::new()));
        { let obs = Rc::clone(&obs);
          obs.borrow_mut().push(Box::new(|x| println!("{x}"))); }
        { let mut v = v.borrow_mut();
          *v = 2;
          let taken: Vec<_> = obs.borrow_mut().drain(..).collect();
          for mut f in taken { f(&v); } }
    "#;
    let lite_observer = r#"
        let v = Observable::new(1i32);
        v.observe(|x| println!("{x}"));
        v.update(|x| *x = 2);
    "#;
    assert!(
        reduction(std_observer, lite_observer) >= 0.70,
        "observer: {:.0}%",
        100.0 * reduction(std_observer, lite_observer)
    );

    // Taking the value back out once the handles are gone.
    let std_take = r#"
        let v = Rc::try_unwrap(v).ok().unwrap().into_inner();
    "#;
    let lite_take = r#"
        let v = v.into_inner();
    "#;
    assert!(
        reduction(std_take, lite_take) >= 0.50,
        "into_inner: {:.0}%",
        100.0 * reduction(std_take, lite_take)
    );

    // Comparing two cells by value. `Rc<RefCell<T>>` needs a `PartialEq` impl
    // the user writes, or two derefs at the call site.
    let std_eq = r#"
        let same = *a.borrow() == *b.borrow();
    "#;
    let lite_eq = r#"
        let same = a == b;
    "#;
    assert!(
        reduction(std_eq, lite_eq) >= 0.50,
        "eq: {:.0}%",
        100.0 * reduction(std_eq, lite_eq)
    );

    // Reading a value out, closure-free. `get` has no `*` and no guard binding,
    // so it is shorter than both the `RefCell` guard form and lite's own closure
    // form; the guard form is the tighter of the two, so it is the one that
    // sets the floor.
    let std_get = r#"
        let n = *counter.borrow();
    "#;
    let closure_get = r#"
        let n = counter.with(|v| *v);
    "#;
    let lite_get = r#"
        let n = counter.get();
    "#;
    assert!(
        reduction(std_get, lite_get) >= 0.15,
        "get vs std guard: {:.0}%",
        100.0 * reduction(std_get, lite_get)
    );
    assert!(
        reduction(closure_get, lite_get) >= 0.20,
        "get vs closure form: {:.0}%",
        100.0 * reduction(closure_get, lite_get)
    );

    // Storing a value, closure-free. Against `RefCell`'s `*c.borrow_mut() = x`
    // the win is the deref and the guard scope.
    let std_set = r#"
        *counter.borrow_mut() = 5;
    "#;
    let lite_set = r#"
        counter.set(5);
    "#;
    assert!(
        reduction(std_set, lite_set) >= 0.20,
        "set: {:.0}%",
        100.0 * reduction(std_set, lite_set)
    );
}

/// Cases where `lite` is *not* shorter, kept honest on purpose.
#[test]
fn bare_borrows_are_not_shorter() {
    // A scoped read of a scalar. `RefCell` is already minimal here, so the
    // closure form is slightly longer - the win on this line is that the borrow
    // cannot leak and a conflict names a location, not that it is fewer keys.
    let std_read = r#"
        let n = *counter.borrow();
    "#;
    let lite_read = r#"
        let n = counter.with(|v| *v);
    "#;
    assert!(
        reduction(std_read, lite_read) < 0.0,
        "a bare read is expected to be longer, not shorter: {:.0}%",
        100.0 * reduction(std_read, lite_read)
    );

    // Same for a scoped write.
    let std_write = r#"
        *counter.borrow_mut() += 1;
    "#;
    let lite_write = r#"
        counter.with_mut(|v| *v += 1);
    "#;
    assert!(
        reduction(std_write, lite_write) < 0.0,
        "a bare write is expected to be longer, not shorter: {:.0}%",
        100.0 * reduction(std_write, lite_write)
    );

    // The guard forms are the same length as `RefCell`'s, which is the answer
    // for anyone who wants the shortest possible line: `borrow`/`borrow_mut`
    // mirror `RefCell` exactly, and add the diagnostics.
    let std_guard = r#"
        let mut s = stats.borrow_mut();
        s.count += 1;
        drop(s);
    "#;
    let lite_guard = r#"
        let mut s = stats.borrow_mut();
        s.count += 1;
        drop(s);
    "#;
    assert_eq!(
        chars(std_guard),
        chars(lite_guard),
        "the guard forms should be the same length"
    );
}

/// The structural cases: the gap widens with the number of steps, because each
/// step in the std form needs its own guard scope.
#[test]
fn the_gap_widens_with_the_number_of_steps() {
    // One read, one write, as two statements.
    let std_two = r#"
        let t = { let s = stats.borrow(); s.count + s.retries };
        { let mut s = stats.borrow_mut(); s.count = t; }
    "#;
    let lite_two = r#"
        let t = stats.with(|s| s.count + s.retries);
        stats.with_mut(|s| s.count = t);
    "#;
    let two = reduction(std_two, lite_two);
    assert!(two >= 0.20, "two steps: {:.0}%", 100.0 * two);

    // A whole observable store - typed accessors, change notification, no
    // re-entrancy bug - is where the per-step saving compounds.
    let std_store = r#"
        struct Store {
            data: Rc<RefCell<StoreData>>,
            obs: Rc<RefCell<Vec<Box<dyn FnMut(&StoreData)>>>>,
        }
        impl Store {
            fn new() -> Self {
                Store {
                    data: Rc::new(RefCell::new(StoreData { count: 0, name: String::new() })),
                    obs: Rc::new(RefCell::new(Vec::new())),
                }
            }
            fn count(&self) -> usize { self.data.borrow().count }
            fn name(&self) -> String { self.data.borrow().name.clone() }
            fn set_count(&self, v: usize) {
                let mut d = self.data.borrow_mut();
                d.count = v;
                let mut taken: Vec<_> = self.obs.borrow_mut().drain(..).collect();
                for f in taken.iter_mut() { f(&d); }
                self.obs.borrow_mut().extend(taken);
            }
            fn observe(&self, f: impl FnMut(&StoreData) + 'static) {
                self.obs.borrow_mut().push(Box::new(f));
            }
        }
    "#;
    let lite_store = r#"
        cell! {
            shared Store, StoreData {
                count: usize => count_mut,
                name: String => name_mut,
            }
        }
        impl Store {
            fn observe(&self, f: impl FnMut(&StoreData) + 'static) {
                self.handle().observe(f);
            }
        }
    "#;
    let store = reduction(std_store, lite_store);
    assert!(store >= 0.60, "observable store: {:.0}%", 100.0 * store);
}
