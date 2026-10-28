# borrow_lite

Shared, mutable data in Rust without the parts that usually bite you.

`borrow_lite` is a small experiment in ergonomics, not a new borrow checker. It
gives three types that cover the three shapes people actually write, plus a
read-mostly cell for the case those three are worst at, and it changes what
happens when a borrow is refused: instead of a bare
`already borrowed: BorrowMutError`, you get an error that names the file and
line of the borrow that is in the way.

The value semantics are unchanged. What changes is the failure mode, and how
easy it is to write code that never hits it.

## Contents

- **[Getting started](#getting-started)**
  - [The shapes](#the-shapes)
  - [Old way vs `borrow_lite`](#old-way-vs-borrow_lite)
- **[The API, piece by piece](#the-api-piece-by-piece)**
  - [What changes, in detail](#what-changes-in-detail)
  - [The cheapest cell: `Inline<T>`](#the-cheapest-cell-inlinet)
  - [Threads](#threads)
  - [Observers without the usual re-entrancy bugs](#observers-without-the-usual-re-entrancy-bugs)
  - [Macros](#macros)
  - [Named fields without the closure](#named-fields-without-the-closure)
  - [`#[derive(Cell)]`: the field accessors without the macro](#derivecell-the-field-accessors-without-the-macro)
  - [`unsafe-unchecked`: no check, no flag](#unsafe-unchecked-no-check-no-flag)
- **[Trade-offs and limits](#trade-offs-and-limits)**
  - [What it does not do](#what-it-does-not-do)
  - [What it is up against](#what-it-is-up-against)
- **[Performance](#performance)**
  - [What a borrow costs](#what-a-borrow-costs)
  - [The fast path without `diagnostics`](#the-fast-path-without-diagnostics)
  - [Waiting for a busy cell](#waiting-for-a-busy-cell)
  - [Under contention](#under-contention)
  - [Many readers, no writer](#many-readers-no-writer)
  - [What observing costs](#what-observing-costs)
- **[Testing and evidence](#testing-and-evidence)**
  - [Testing](#testing)
- **[License](#license)**
  - [License and authors](#license-and-authors)

---

## Getting started

### The shapes

| Shape | std | `borrow_lite` |
|---|---|---|
| one owner, value stored inline | `RefCell<T>` | `Inline<T>` |
| shared + mutable, one thread | `Rc<RefCell<T>>` | `Local<T>` |
| shared + mutable, many threads | `Arc<RwLock<T>>` | `Shared<T>` |
| read-mostly, replaced wholesale | `ArcSwap<T>` | `Snapshot<T>` |

`Inline<T>` is the cheapest: no allocation, no reference count, and `Send` when
`T` is - the same auto traits as `RefCell<T>`. `Local<T>` and `Shared<T>` add
sharing on top, at the cost of one allocation each. All three expose the same
method set, so moving a value between shapes is a type change, not a rewrite.

`Snapshot<T>` is a different bargain, not a fourth spelling of the same one: its
reads take no counter at all, and its writes replace the whole value instead of
mutating it in place. It is described under [many readers, no
writer](#many-readers-no-writer), where that trade pays off.

```rust
use borrow_lite::{Local, Shared, local, shared};

let a: Local<i32> = local(5);
let b: Local<i32> = local(5);
assert!(a == b); // compares the values, like Rc<RefCell>
assert!(a.ptr_eq(&a.clone())); // same cell, like Rc::ptr_eq
assert!(!a.ptr_eq(&b)); // a separate cell with an equal value
assert_eq!(Local::from(7).with(|v| *v), 7);
assert_eq!(Local::<i32>::default().with(|v| *v), 0);
assert!(local(1) < local(2)); // ordering by value, so `sort()` works

let c: Shared<i32> = shared(1);
assert!(c == c.clone());
```

### Old way vs `borrow_lite`

The honest way to judge ergonomics is to put the code people already write next
to the code `borrow_lite` asks for, on the same task.

#### Side by side: one owner

```rust
use std::cell::RefCell;

let stats = RefCell::new(vec![1, 2, 3]);
stats.borrow_mut().push(4);
assert_eq!(stats.borrow().len(), 4);
```

```rust
use borrow_lite::inline;

let stats = inline(vec![1, 2, 3]);
stats.with_mut(|v| v.push(4));
assert_eq!(stats.with(Vec::len), 4);
```

Same shape, same line count. The only change is `borrow_mut()`/`borrow()` become
`with_mut(..)`/`with(..)`, which release the borrow before returning.

#### Side by side: shared

```rust
use std::cell::RefCell;
use std::rc::Rc;

let stats = Rc::new(RefCell::new(vec![1, 2, 3]));
let total: i32 = stats.borrow().iter().sum();
stats.borrow_mut().push(4);
assert_eq!(total, 6);
```

```rust
use borrow_lite::local;

let stats = local(vec![1, 2, 3]);
let total: i32 = stats.with(|v| v.iter().sum());
stats.with_mut(|v| v.push(4));
assert_eq!(total, 6);
```

`Rc::new(RefCell::new(v))` collapses to `local(v)`, and the read collapses from
a deref-and-method-call into one closure. The difference is small here; it
compounds in a struct full of fields, where the old way is
`self.field.borrow_mut()` on every access and the new way is one `cell!` accessor
(see [Named fields without the closure](#named-fields-without-the-closure)).

#### Side by side: when it goes wrong

This is where the two diverge. `std` tells you a borrow exists; `borrow_lite` tells you
which one.

```rust
use std::cell::RefCell;

let cell = RefCell::new(0);
let held = cell.borrow();
let err = cell.try_borrow_mut().unwrap_err();
// `BorrowMutError` is a unit struct: no location, no kind, no count.
let _: std::cell::BorrowMutError = err;
# drop(held);
```

```rust
use borrow_lite::inline;

let cell = inline(0);
let held = cell.borrow();
let err = cell.try_borrow_mut().unwrap_err();
// `Conflict` carries the location of the borrow in the way, its kind and count.
# #[cfg(feature = "diagnostics")]
assert!(err.to_string().contains("held at"));
assert_eq!(err.kind(), borrow_lite::ConflictKind::Readers);
assert_eq!(err.count(), Some(1));
# drop(held);
```

The `std` wording is also a trap: `try_borrow_mut()` on a shared borrow returns
`BorrowMutError`, while `try_borrow()` on a write returns `BorrowError`, and the
two messages (`"already borrowed"` / `"already mutably borrowed"`, or
`"RefCell already borrowed"` on newer compilers) neither name the offending line
nor distinguish the case clearly. So you hunt for it by eye.

#### How much shorter, measured

"More ergonomic" is worth nothing as a slogan, so the two sides are counted.
The numbers below are code characters (blank lines and comments dropped) of the
idiomatic form of each side; `tests/ergonomics.rs` computes them and fails if a
claim stops holding. Character count is a proxy for what you type, not for
readability.

| Task | `std` | `borrow_lite` | shorter by |
|---|---|---|---|
| swap two cells | `std::mem::swap(&mut *a.borrow_mut(), &mut *b.borrow_mut())` | `a.swap(&b)` | **81%** |
| wire an observer (re-entrancy safe) | build both `Rc<RefCell<..>>`, drain before notifying | `Observable::new`, `observe`, `update` | **75%** |
| take the value back out | `Rc::try_unwrap(v).ok().unwrap().into_inner()` | `v.into_inner()` | **57%** |
| compare two cells by value | `*a.borrow() == *b.borrow()` | `a == b` | **53%** |
| read an `Observable`, closure-free | `source.with(\|v\| v.clone())` | `source.get()` | **34%** |
| read then write, one statement each | two guard scopes | `with` + `with_mut` | **27%** |
| build alone, then share | `Rc::new(RefCell::new(v))`, `Rc::get_mut` | `local(v)`, `get_mut` | **24%** |
| read a field of a struct | `stats.borrow().count` | `*stats.count()` | **21%** |
| write a field of a struct | `stats.borrow_mut().count += 1` | `*stats.count_mut() += 1` | **20%** |
| read a scalar, closure-free | `*counter.borrow()` | `counter.get()` | **15%** |
| write a scalar, closure-free | `*counter.borrow_mut() = 5` | `counter.set(5)` | **15%** |
| clone a field out | `stats.borrow().name.clone()` | `stats.name().clone()` | **19%** |
| a scoped read of a scalar | `*counter.borrow()` | `counter.with(\|v\| *v)` | **-12%** (longer) |
| a scoped write of a scalar | `*counter.borrow_mut() += 1` | `counter.with_mut(\|v\| *v += 1)` | **-11%** (longer) |
| a threaded counter, 4 threads | `Arc<RwLock<_>>`, `write().unwrap()` | retry loop with `yield_now` | **-17%** (longer) |

The honest reading is that there are two regimes:

* **A single borrow is already minimal in `std`.** `*c.borrow()` is three tokens
  the compiler cannot be talked out of, so the closure form is *longer* on that
  one line. What you get there is not fewer keys: the borrow cannot leak, and a
  refused borrow names a location instead of saying `already borrowed`. If you
  want the shortest line, the guard form (`borrow`/`borrow_mut`) is exactly as
  long as `RefCell`'s.
* **A pattern is where the gap opens.** The moment a task is more than one
  borrow - swap, compare, tear down, read-then-write, observers, a struct of
  fields - each `std` step needs its own guard scope, its own `Rc::clone`, or its
  own hand-rolled `PartialEq`, and the saving compounds: 20-27% for a short
  sequence, 53-81% for the shapes people actually complain about.

So the claim is deliberately not "always 50% shorter". It is "the same length
for one borrow, and 20-81% shorter the moment there is a pattern" - which is
where `Rc<RefCell<T>>` and `Arc<RwLock<T>>` stop being fun.

#### The same tasks in Python

For a reader coming from a language without a borrow checker, the same three
tasks are shorter still - and it is worth saying why, because it is not a win:

| Task | Python | chars | `borrow_lite` | chars |
|---|---|---|---|---|
| push onto a shared list | `stats.append(4)` | 15 | `stats.with_mut(\|v\| v.push(4));` | 30 |
| increment a shared counter | `counter[0] += 1` | 15 | `counter.with_mut(\|v\| *v += 1);` | 30 |
| read a shared length | `len(stats)` | 10 | `stats.with(\|v\| v.len());` | 24 |

The Python column is shorter because there is nothing to check. Every Python
object is a shared reference with no borrow flag, so there is no borrow to
refuse and no conflict to report - the second line is shorter for exactly the
reason the first line is unsafe in Rust. `borrow_lite` is not competing with
Python here; it is competing with `RefCell` while keeping a *located* refusal,
which Python cannot offer at all. `tests/ergonomics.rs` pins these six numbers so
the comment cannot drift.

#### Construct by construct

The measured table above covers the *repetitive* lines (reads, writes, field
access). This one is the mapping for the rest - which `std` construct becomes
which `borrow_lite` one:

| Task | old (`std`) | `borrow_lite` |
|---|---|---|
| one-owner cell | `RefCell<T>` | `Inline<T>` |
| shared cell, one thread | `Rc<RefCell<T>>` | `Local<T>` |
| shared cell, many threads | `Arc<RwLock<T>>` | `Shared<T>` |
| build one | `Rc::new(RefCell::new(v))` | `local(v)` |
| recover from a conflict | `.try_borrow_mut()` -> `Err(BorrowMutError)`, no location | `.try_borrow_mut()` -> `Err(Conflict)`, located |
| a guard that cannot leak | manual `drop` or a `let _ = ..` block | `with`/`with_mut`, by construction |
| read/write a scalar, no closure | `*c.borrow()`, `*c.borrow_mut() = x` | `c.get()`, `c.set(x)` |
| wait for a busy cell | `lock()` - blocks, and poisons on panic | retry loop, or the bounded `retry(n, op)` |

Two rows are the whole pitch. A conflict becomes a value you can match on
instead of a panic or a deadlock, and a borrow you cannot forget to release is
the default rather than a discipline.

#### Where `borrow_lite` is *longer* (the honest column)

Ergonomics is a trade, not a free win, and a few places cost more keystrokes:

- **The `with` closure cannot return a reference into the cell.** Copy or clone
  what you need out, or drop back to a guard (`borrow()`), exactly as with
  `RefCell`. A `Deref`-based read is shorter but cannot be made safe here.
- **Writes nest one level deeper.** `*c.borrow_mut() = x` becomes
  `c.with_mut(|v| *v = x)`. The closure is the price of the borrow being
  released for you - unless you take the closure-free form below.
- **`get`/`set` remove the closure, not the copy.** For a scalar they are the
  shortest option (`c.get()`, `c.set(x)`), and they keep the located panic on
  conflict. `get` still needs `T: Clone`, because the value leaves the cell by
  value, exactly as `with(|v| v.clone())` does.
- **`get_mut` returns `Option`.** Like `RefCell::get_mut`, because a second
  handle means a borrow could exist; you `.unwrap()` it.
- **`cell!` needs two names**, the handle and the value struct, because a
  declarative macro cannot hide one behind the other without a proc-macro
  dependency.

If those costs matter more than the diagnostics, `RefCell` is a fine choice -
see [When not to use `borrow_lite`](#when-not-to-use-borrow_lite).

---

## The API, piece by piece

### What changes, in detail

#### 1. Panic messages that name the culprit and the victim

`RefCell` reports `already borrowed: BorrowMutError`. That is all. You know a
borrow exists somewhere; you do not know which line, in which function, or
whether it is a reader or a writer.

```rust,should_panic
use std::cell::RefCell;
use std::rc::Rc;

let stats = Rc::new(RefCell::new(vec![1, 2, 3]));
let reader = stats.borrow(); // a read borrow that stays alive
stats.borrow_mut().push(4); // panicked at 'already borrowed: BorrowMutError'
# let _ = reader;
```

`borrow_lite` tracks the source location of every borrow that is holding the
cell, so the panic reads like a stack trace you can act on:

```rust,should_panic
use borrow_lite::Local;

let stats = Local::new(vec![1, 2, 3]);
let guard = stats.borrow();
stats.borrow_mut().push(4);
// panicked at borrow_lite: cell is already immutably borrowed
//   (borrow attempted at src/main.rs:5:7)
//   ; held at src/main.rs:4:15 (1 active borrow)
//   - a borrow is still alive. Drop the guard, or use `with`/`with_mut`
//     so it cannot leak.
```

The count is printed only when readers are what is in the way. A writer
conflict - a borrow refused because an exclusive borrow is live, whether the
refused borrow was a read or a write - names the writer's location instead,
because the reader count is necessarily zero at that moment and printing it
would claim `0 active borrows` about a cell that is very much borrowed:

```rust,should_panic
use borrow_lite::Local;

let stats = Local::new(vec![1, 2, 3]);
let guard = stats.borrow_mut();
stats.borrow_mut().push(4);
// panicked at borrow_lite: cell is already mutably borrowed
//   (borrow attempted at src/main.rs:5:7)
//   ; held at src/main.rs:4:15 - a borrow is still alive...
# let _ = guard;
```

#### 2. Conflicts are errors, not panics or deadlocks

Every borrowing method has a `try_` twin - including `replace` and `take`,
whose `try_` forms (`try_replace`, `try_take`) hand back the unwritten value
rather than losing it, `replace_with`, whose `try_replace_with` hands back the
closure, `swap`, whose `try_swap` reports which cell was in the way, `==`, whose
twin is `try_eq`, `<` and friends, whose twin is `try_cmp`, and `with`, whose
twin is `try_with` - and a conflict is a normal `std::error::Error`:

```rust
use borrow_lite::{Conflict, Local};

fn bump(counter: &Local<i32>) -> Result<(), Conflict> {
    *counter.try_borrow_mut()? += 1;
    Ok(())
}

let counter = Local::new(0);
bump(&counter).unwrap();
assert_eq!(counter.with(|c| *c), 1);
```

On the threaded side this matters more. `RwLock` on a single thread re-entering
the same lock deadlocks, and a panic while holding it poisons the lock for
everyone afterwards. `Shared` never blocks and never poisons: a conflicting
borrow comes back as `Err(Conflict)`.

#### 3. Closures instead of leaked guards

The most common cause of a "borrow conflict" panic is a guard that outlived the
statement you needed it for:

```rust
use borrow_lite::Local;

let stats = Local::new(vec![1, 2, 3]);
let total: i32 = stats.with(|values| values.iter().sum());
stats.with_mut(|values| values.push(4));
assert_eq!((total, stats.with(|v| v.len())), (6, 4));
```

`with` and `with_mut` release the borrow before they return, so the pattern
above cannot produce a conflict. When you do want a guard, `borrow`/`borrow_mut`
and the `borrow!`/`borrow_mut!` macros are still there.

When the operation is a plain read or a plain store, the closure is noise, so
`get` and `set` are the closure-free spelling of the same borrow:

```rust
use borrow_lite::shared;

let counter = shared(0i32);
counter.set(5);       // == counter.with_mut(|v| *v = 5)
assert_eq!(counter.get(), 5); // == counter.with(|v| *v)
```

They keep the same contract as the closure forms: the borrow is released before
the call returns, a conflict panics with the holder's location, and the `try_`
twins (`try_get`, `try_set`) report it as an `Err` instead - with `try_set`
handing the unwritten value back. `get` requires `T: Clone` because the value
leaves the cell by value. The `cell!` field accessors are already closure-free
in the same way, so `*stats.count()` reads and `*stats.count_mut() += 1` writes,
and [`Observable`](#observers-without-the-usual-re-entrancy-bugs) carries the
same pair (`get`/`try_get` alongside its existing `set`/`try_set`).

One limit worth knowing before you reach for `with`: **the closure cannot return
a reference into the cell.** The guard is gone by the time it returns, so the
borrow checker refuses:

```compile_fail
use borrow_lite::Local;

let cell = Local::new(vec![1, 2, 3]);
let item: &i32 = cell.with(|v| &v[0]); // error: borrowed value does not live long enough
```

Copy or clone what you need out, or use a guard (`borrow`) when you need to keep
a reference alive.

#### 4. A free path when you already have `&mut`

The borrow flags exist to guard access through `&self`. When you hold `&mut`,
the compiler has already proven the handle is unique and no guard is alive, so
the flags are pure overhead. `get_mut` skips them, the same way `RefCell::get_mut`
and `Rc::get_mut` do:

```rust
use borrow_lite::local;

let mut stats = local(Vec::new());
stats.get_mut().unwrap().push(1); // no flag, no guard, no location
let shared = stats.clone();
assert!(stats.get_mut().is_none()); // a second handle could be borrowing
```

It returns `Option`, because a second handle means a borrow could exist on
another thread or later in this one. Building a value before sharing it, or
taking it apart once the handles are gone, needs no borrow bookkeeping at all.

The same story covers the rest of the value's lifetime. `into_inner` consumes
the cell and hands the value back, and `take` empties it in place - the pair to
`get_mut`, and the same pair `RefCell`/`Rc`/`Arc` offer.

`get_mut` is free, `into_inner` *is* `Rc::try_unwrap` plus a move, and `take`
pays only the borrow-flag check that every other borrow pays - it is `replace`
under a shorter name. So the "no bookkeeping" paths are genuinely free, and the
one that still checks a flag is honest about it. The three are priced against
their `std` counterparts in [What a borrow costs](#what-a-borrow-costs).

Two more value-moving calls round out the surface, both mirroring `RefCell`:

```rust
use borrow_lite::local;

// `replace_with` computes the new value from the old, so a replacement that
// must consume the old one needs no clone.
let cell = local(vec![1, 2, 3]);
let old = cell.replace_with(core::mem::take);
assert!(old.is_empty()); // `f` ran first and took the vector
assert_eq!(cell.with(Vec::clone), vec![1, 2, 3]);

// `swap` exchanges two cells. `RefCell::swap` on the same cell double-borrows
// and reports a generic second-borrow; `borrow_lite` detects it and points at you.
let a = local(1u8);
let b = local(2u8);
a.swap(&b);
assert_eq!(a.with(|v| *v), 2);
let self_swap = a.try_swap(&a).unwrap_err();
assert_eq!(self_swap.kind(), borrow_lite::ConflictKind::Writer); // named, not generic
```

#### 5. The trait surface you already reach for

`Local<T>` and `Shared<T>` carry the everyday traits `Rc<RefCell<T>>` and
`Arc<RwLock<T>>` do - `==`, `Ord`, `Default`, `From`, `ptr_eq` - so you do not
have to unwrap the cell to compare two of them or build one from a value.
`Inline<T>` carries the same set bar `ptr_eq` (there is no handle to compare).
The example is in [The shapes](#the-shapes).

On top of that set, all three carry the traits that make a cell usable *as* the
value in the places a value is expected: `Display`, `Hash`, the arithmetic
assignment operators (`+=`, `-=`, `*=`, `/=`), and comparison against a bare `T`
(`cell == 5`, `cell < 6`). Each is one borrow, released before the call returns,
and each carries `#[track_caller]` so a conflict names the caller's line:

```rust
use borrow_lite::local;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

let mut counter = local(10i32);
counter += 5;                 // one exclusive borrow
counter *= 2;
assert_eq!(counter, 30);      // PartialEq<T>
assert_eq!(format!("{counter}"), "30"); // Display

let mut hasher = DefaultHasher::new();
counter.hash(&mut hasher);    // Hash, by value
```

`Hash` is the one place `borrow_lite` is deliberately *looser* than the
`Rc<RefCell<T>>` it stands in for: `RefCell` has no `Hash`, so a
`HashMap<RefCell<T>, _>` cannot be built. Here it can, because hashing borrows
the value the same way `==` does. The caveat is the same as for any
interior-mutable key: do not mutate a cell while it is a key in a map, or the map
will not find it - the cell does not stop you, and neither does `RefCell`.

`==` on two *different* cells borrows both values, so it panics if one is
borrowed - exactly as `Rc<RefCell<T>>` does. Comparing a cell with *itself*
short-circuits on the pointer and never borrows, so `cell == cell` never panics.
One difference from `Rc`/`Arc` is worth naming: they run that short-circuit only
when `T: Eq` (they specialise on `Eq`, because "a `PartialEq` might be
deliberately irreflexive"), so `Rc<RefCell<f64>>` compared with itself still
borrows. `borrow_lite` short-circuits for every `T: PartialEq`, so a `Local<f64>`
holding `NaN` is equal to itself where the `Rc` would not be. That is the only
observable divergence, and a test pins it.
When you cannot afford the panic, `try_eq` is the twin that returns the conflict:

```rust
use borrow_lite::local;

let a = local(1);
let b = local(1);
let held = a.borrow_mut();
assert!(a.try_eq(&b).is_err()); // would panic, reported instead
assert_eq!(a.try_eq(&a.clone()), Ok(true)); // self-comparison never borrows
# drop(held);
```

Ordering (`PartialOrd`/`Ord`) follows the same shape as `==`, because it had to:
the alternative was to leave `sort()` and `<` unavailable while `==` worked,
which would surprise anyone porting from `Rc<RefCell<T>>`. The same cell
short-circuits to `Equal`; distinct cells compare by value under a shared borrow
and panic if one is borrowed. `try_cmp` is the twin:

```rust
use borrow_lite::local;
use core::cmp::Ordering;

let a = local(1);
let b = local(2);
assert_eq!(a.try_cmp(&b), Ok(Some(Ordering::Less)));
let held = a.borrow_mut();
assert!(a.try_cmp(&b).is_err()); // would panic, reported instead
# drop(held);
```

One honest limit on `Display`: the *refused* location names the crate's `Display`
impl, not your caller, because `format!` reaches `Display::fmt` through
`core::fmt`'s own non-`track_caller` dispatch. The *holder* is still named at the
caller's line, which is the half that tells you what to fix. The other traits
(`Hash`, `PartialEq<T>`, the operators) are called directly and do blame the
caller.

`map_get` is the closure form of a field read, named: `cell.map_get(|v|
v.field.clone())` is what `map` is almost always used for, and it says so in one
method. It is the same shared borrow as `map`, released before it returns.
`get`/`set` remain the closure-free forms for a `Copy` value or a whole-value
read and store.

### The cheapest cell: `Inline<T>`

If nothing is shared, you do not need an `Rc` or an `Arc` - and paying for one
costs you an allocation per cell. `Inline<T>` is the `RefCell<T>` shape: the
flags and the value sit in the same struct.

```rust
use borrow_lite::{inline, Inline};

let cell: Inline<Vec<i32>> = inline(vec![1, 2, 3]);
cell.with_mut(|v| v.push(4));
assert_eq!(cell.with(Vec::len), 4);
```

It has the same auto traits as `RefCell<T>`: `Send` when `T: Send`, and `!Sync`.
That `Send` is the part `Local<T>` cannot do, because `Local` is built on an `Rc`:

```rust
use borrow_lite::Inline;

let cell = Inline::new(String::from("hi"));
let seen = std::thread::spawn(move || cell.with(String::clone)).join().unwrap();
assert_eq!(seen, "hi");
```

The trade is one recorded source location per borrow, which is what makes the
message in [annoyance 1](#1-panic-messages-that-name-the-culprit-and-the-victim)
possible. On a real workload `Inline` and `RefCell` are indistinguishable; in a
tight loop the extra store costs a fraction of a nanosecond. The allocation
count is the sharper difference, and it is asserted in the test suite:
`Inline::new` allocates **zero** times, `Local::new` once.

The bookkeeping is larger than `RefCell`'s, though: `Inline<u64>` is 24 bytes
against `RefCell<u64>`'s 16, because it tracks a count, a flag and a location
rather than a single flag. Turn the `diagnostics` feature off and it is the
same 16 bytes as `RefCell<u64>`, at the cost of the located message. Fine for a
struct; for a million-element array, use `Cell` or `RefCell`.

### Threads

```rust
use borrow_lite::shared;

let counter = shared(0usize);
std::thread::scope(|scope| {
    for _ in 0..4 {
        scope.spawn(|| {
            // Borrows do not block, so retry when another thread holds one.
            while counter.try_with_mut(|c| *c += 1).is_err() {
                std::thread::yield_now();
            }
        });
    }
});
assert_eq!(counter.with(|c| *c), 4);
```

`Shared<T>` is `Send + Sync` exactly when `T` is, the same contract as
`Arc<RwLock<T>>`. Guards are deliberately `!Send`; use `with`/`with_mut` inside
the worker thread instead of moving a guard across.

`Inline<T>` is `Send` when `T` is, like `RefCell<T>`, but `!Sync`: to share a
cell between threads you need `Shared<T>`, because the single-threaded flags are
plain `Cell`s.

Retrying with `yield_now` rather than `spin_loop` is not a style choice - it is
the measured winner. [Under contention](#under-contention) and
[Waiting for a busy cell](#waiting-for-a-busy-cell) have the numbers.

### Observers without the usual re-entrancy bugs

The observer pattern is where `Rc<RefCell<...>>` usually falls apart, so it is
included as a worked example. `Observable<T>` keeps its observers in a
`Local<Vec<Box<dyn FnMut(&T)>>>`, and notifies by moving the list out before
running it:

```rust
use borrow_lite::Observable;

let source = Observable::new(1u8);
let log = Observable::new(Vec::new());

let inner_log = log.clone();
let source_for_inner = source.clone();
// an observer that subscribes another observer, which still sees the value
source.observe(move |_| {
    let inner_log = inner_log.clone();
    source_for_inner.observe(move |v| inner_log.update(|l| l.push(*v)));
});

source.update(|v| *v = 2);
assert_eq!(log.with(|l| l.clone()), vec![2]);

// The observer captures `source`, so the pair keeps itself alive; clear the
// list to break the cycle (the usual `Rc` cycle, and the usual fix).
source.clear_observers();
```

Mutating the value from inside an observer is the other classic bug: it would
alias the `&T` the observers are being handed. `try_update` reports it as a
`Conflict` instead. Calling `notify` by hand from inside an `update` closure hits
the same borrow, and `try_notify` reports it the same way.

Clearing from inside an observer is the third, and it is the one this crate got
wrong until a fuzzer caught it: the round in progress is held by the notification
guard, not by the subscription list, so `clear_observers()` found an empty list
and the guard put the observers straight back - the count stayed at 2 and they
fired again. A clear during a notification now records an id boundary the guard
honours, so the round finishes and none of it is re-registered. Observers
subscribed *after* the clear survive, which keeps the "a new observer still sees
the current value" rule intact.

### Macros

```rust
use borrow_lite::{borrow, borrow_mut, try_borrow_mut, with, with_mut, Local};

let cell = Local::new(vec![1, 2, 3]);

borrow!(cell, values);
assert_eq!(values.len(), 3);
drop(values);

with_mut!(cell, values, { values.push(4) });

let len = with!(cell, values, { values.len() });
assert_eq!(len, 4);

borrow_mut!(cell, values);
values.push(5);
drop(values);

fn push(cell: &Local<Vec<i32>>, value: i32) -> Result<(), borrow_lite::Conflict> {
    try_borrow_mut!(cell, values);
    values.push(value);
    Ok(())
}
push(&cell, 6).unwrap();
assert_eq!(cell.with(|v| v.clone()), vec![1, 2, 3, 4, 5, 6]);
```

### Named fields without the closure

The macros above still make you name the whole struct. When a cell holds a
struct you read and write field by field, `cell!` generates one accessor pair per
field, so the closure goes away:

```rust
use borrow_lite::cell;

cell! {
    shared Stats, StatsData {
        count: usize => count_mut,
        name: String => name_mut,
    }
}

let stats = Stats::new(0, String::from("hi"));
*stats.count_mut() += 1;             // exclusive borrow, released at `;`
stats.name_mut().push('!');
assert_eq!((*stats.count(), stats.name().as_str()), (1, "hi!"));
```

The getter is named after the field and takes a *shared* borrow, so reads compose
in one expression; the setter takes an exclusive borrow and its name is spelled
out, because a declarative macro cannot build `count_mut` from `count`. The
handle `Deref`s to the cell, so `stats.with(..)`, `stats.borrow()` and
`stats.try_with(..)` keep working.

Two names are needed - the handle and the value struct - because a struct and a
module cannot share a name, so the value type cannot be hidden behind the handle
without a proc-macro dependency. The crate takes the extra name over the
dependency.

Each accessor is `ReadGuard::map`/`WriteGuard::map`, so it costs exactly what a
plain borrow costs (measured: 3.92 ns, the same as `borrow`, against 4.23 ns for
the `with` closure it replaces) and reports the same conflicts. The limit is the
one that comes with a single cell: two reads compose, but a read and a write
cannot overlap, exactly as `RefCell<Struct>` would behave.

### `#[derive(Cell)]`: the field accessors without the macro

`cell!` needs two names and an explicit field list. The optional `derive`
feature generates the same handle from the value struct, so the list is the
struct's own fields:

```rust
# #[cfg(feature = "derive")]
# {
use borrow_lite::Cell;

#[derive(Cell)]
struct Stats {
    count: usize,
    name: String,
}

let stats = StatsCell::new(0, String::from("hi"));
*stats.count_mut() += 1;
stats.name_mut().push('!');
assert_eq!((*stats.count(), stats.name().as_str()), (1, "hi!"));
# }
```

It generates `StatsCell` beside `Stats` (the value struct stays the value), with
the same `field()`/`field_mut()` surface and the same cost. Off by default: it
pulls in a proc-macro crate, and the default build is dependency-free.

The handle is exactly what `cell!` produces, so the same three rules apply:

- its `Clone` clones the *cell*, not the value, so the value does not need to be
  `Clone` - and the generated impl carries no bound that would demand it;
- it `Deref`s to the cell, so the whole-cell API (`with`, `borrow`, `try_with`,
  the operators) is reachable;
- it inherits the visibility of the value struct, so a private struct gets a
  private handle rather than one that leaks the value through `Deref::Target`.

### `unsafe-unchecked`: no check, no flag

The checked cells are the point of the crate, but there is one case where the
check is provably unnecessary: a cell built with `&mut` access, used before it is
shared. The `unsafe-unchecked` feature adds `Unchecked<T>`, a cell the size of
`UnsafeCell<T>` with no flag, no count and no holder:

```rust
# #[cfg(feature = "unsafe-unchecked")]
# {
use borrow_lite::Unchecked;

let cell = Unchecked::new(5i32);
// SAFETY: `cell` is not borrowed, and nothing else can reach it.
unsafe { *cell.borrow_mut_unchecked() += 1; }
// SAFETY: the write above has ended, so no other borrow is alive.
assert_eq!(unsafe { *cell.borrow_unchecked() }, 6);
# }
```

`borrow_unchecked`/`borrow_mut_unchecked` are `unsafe` because nothing enforces
the aliasing rule for you - that is exactly what was traded away. `get_mut` is
the safe path when you have `&mut`: it needs no `unsafe` and no flag, the same
contract as `UnsafeCell::get_mut`. `Unchecked<T>` is `Send` when `T` is and,
like `UnsafeCell`, never `Sync`.

Because a caller can hold a live `&mut T` from `borrow_mut_unchecked`, nothing
safe may read the value through `&self`. `Debug` therefore prints a placeholder
(`Unchecked { .. }`), exactly as `UnsafeCell`'s own `Debug` prints
`UnsafeCell { .. }` rather than the value. A value-reading `Debug` would retag a
shared reference over the live `&mut T` and be undefined behaviour the caller
cannot avoid - `-Zmiri-strict-provenance` rejects it, and a regression test in
`tests/unchecked.rs` pins the placeholder.

---

## Trade-offs and limits

### What it does not do

- It does not check borrows at compile time. If you want static guarantees, the
  crates to look at are `qcell` and `ghost-cell`, which get them by restricting
  what you can express.
- It does not eliminate `unsafe`. The crate contains a little of it, confined to
  the cell and guard modules; everything else is safe code on top.
- It is not a lock. Borrows do not wait. A busy cell returns a conflict, and it
  is up to you whether that is an error or a retry. There is deliberately no
  `wait_until_free` helper: waiting can deadlock, and that is the failure this
  crate exists to avoid. (The `std` feature does add a *bounded* `retry` and a
  single-step `wait` - see [Waiting for a busy cell](#waiting-for-a-busy-cell) -
  which is the one shape of waiting that cannot deadlock.)
- The single-threaded cells store the holder location in a `Cell` with no
  synchronisation, which is sound only because neither cell can be borrowed from
  two threads at once: `Inline` and `Local` are both `!Sync`. For `Local` that
  comes from the `Rc`, and for `Inline` from the `Cell` flags themselves; both
  are asserted in the tests.

### What it is up against

`borrow_lite` is not the only answer to "shared mutable state in Rust", and it is
not always the right one. This section is the honest placement: which crate
wins which workload, measured or read from source, and where `borrow_lite` should
not be used at all.

#### The alternatives, by what they trade

| Crate | Guarantee | Cost | When it wins |
|---|---|---|---|
| `std::cell::RefCell` | runtime, panics | none beyond a flag | when you want the smallest cell (16 bytes vs `Inline`'s 24 with diagnostics on, 16 with them off) and the last few percent of speed, and can live with `already borrowed` |
| `std::sync::RwLock` | runtime, blocks, poisons | park/unpark, futex | long critical sections, where parking beats retrying |
| `parking_lot::RwLock` | runtime, blocks, no poison | one word, one CAS | heavy contention with long holds |
| `arc_swap` / `Arc<T>` | none (immutable reads) | none on read | read-mostly: config, feature flags, routing tables |
| `AtomicU64` and friends | none | one atomic op | counters and flags |
| `borrow_lite::Snapshot<T>` | none (immutable reads) | one atomic load per read, unbounded retained values | read-mostly where the value can be replaced wholesale and need not outlive the cell |
| `tokio::sync::Mutex` | runtime, async, blocks | async state machine | state held across `.await` |
| `ghost-cell`, `qcell` | **compile time** | brand/token plumbing | when you can afford lifetimes everywhere to get zero runtime checks |
| `borrow_lite` | runtime, reports, never blocks | ~0.3 ns per borrow | low contention, short borrows, re-entrancy, and diagnostics you can act on |

#### Where `borrow_lite` wins, and by how much

- **In a release build, with `--no-default-features`, it matches `RefCell` and
  `Rc<RefCell>` and beats both `RwLock`s.** The borrow path with `diagnostics`
  off is a load and a store with no `Location` to write, so the single-threaded
  cell costs what `RefCell` costs; the threaded cell is faster than `RwLock` and
  `parking_lot::RwLock` on the fast path because it never has to be able to
  park a waiter. The six numbers are in [The fast path without
  `diagnostics`](#the-fast-path-without-diagnostics).

  The cells it does **not** beat are stated with their gap, and the two regimes
  are kept apart. On the single-threaded fast path a bare `AtomicU64` read is
  0.60 ns against `Shared`'s 4.25 ns (about 7x), because a load does not write a
  shared cache line and a borrow must; a plain `Arc<T>` deref is the same 0.60 ns
  and is free for the same reason. `arc_swap::ArcSwap::load()` is *not* free on
  that path either - it is 4.98 ns, slightly *slower* than `Shared`, because it
  takes a guard - and `Snapshot<T>` (0.66 ns) is the cell to reach for when a
  read-only fast path is what you want. What `arc_swap`/`Arc<T>` win is the
  *contended read-mostly* regime, not the fast path: at eight threads on
  read-mostly work `Shared` loses by hundreds of times (120 ms against 251 µs),
  because every reader writes the borrow count's cache line and an `Arc` read
  does not. Both numbers are in [Many readers, no writer](#many-readers-no-writer);
  neither is a claim to have been closed.

- **Against `RefCell`** (with `diagnostics` on, the default): the borrow path
  costs about +0.3 ns (1.51 vs 1.19 ns for a read). In exchange, a refused
  borrow names the location of both the attempt and the holder instead of saying
  `already borrowed`. It also adds `try_*`, so a conflict can be a value instead
  of a panic. `RefCell` has one thing `borrow_lite` lacked - `get_mut` - and that
  gap is now closed.
- **`Inline` as a drop-in for `RefCell`**: swapping
  `use std::cell::{Cell, RefCell}` for `use borrow_lite::Inline as RefCell` in
  `markup5ever_rcdom` - a real HTML parser, not a benchmark harness - compiles
  with no changes and passes that crate's tests. On its parse-and-walk workload
  the cost is about +4% (709-722 µs against 684-689 µs). `Local` costs +30% on
  the same workload, because every cell is a separate allocation.
- **Against `std::sync::RwLock`**: the borrow itself is *faster* (4.20 vs
  5.15 ns read), because `RwLock` must be able to park a waiter and so does more
  work on its fast path. `borrow_lite` never parks, so it also never poisons and
  never deadlocks on re-entrancy.
- **Against `parking_lot::RwLock`**: this is the real competition, and the
  verdict depends on the retry loop. With `spin_loop` - the loop most people
  write - `borrow_lite` loses badly (14.4 ms vs 1.17 ms at eight threads). With
  `thread::yield_now`, it wins at 4 and 8 threads (523 µs vs 729 µs, 932 µs vs
  1.17 ms) and is level at 2. The cell is not the variable; the waiting is.
  `parking_lot` also keeps its entire state in one `AtomicUsize` (a bitfield of
  `PARKED_BIT`, `WRITER_BIT` and a reader count) and decides read-versus-write
  with a single `compare_exchange`, whereas `borrow_lite` runs a two-flag
  `SeqCst` handshake. That is the one place `parking_lot` is structurally ahead,
  and it is a real implementation difference, not a measurement artefact - but
  copying it does not pay, for the measured reason given below.
- **Against `arc_swap` / `Arc<T>`**: `Shared` loses outright on read-mostly
  work - 120 ms against 251 µs at eight threads, hundreds of times - because
  every reader writes a shared cache line to record its borrow, and `arc_swap`
  readers do not. There is no version of a *borrow-counting* cell that closes
  this; the counter is the cost. `Snapshot<T>` removes the counter and lands
  within 1.4x of that floor, so for read-mostly work reach for it (or `arc_swap`
  when the value must be owned past the cell); keep `Shared` for the cases that
  need in-place mutation and conflict reporting.

#### The gap that cannot be closed, and what to do instead

The read-mostly gap is structural for any cell that counts borrows, because the
count is shared state mutated per borrow. Two things follow.

First, `borrow_lite` should say so, which it now does, rather than pretending to
be a general `RwLock` replacement.

Second, the one gap that looked closable was the `parking_lot` fast path: pack
the reader count and the writer flag into one `AtomicUsize` so a
reader-versus-writer decision is a single `compare_exchange`. That was built and
measured rather than argued about, and **it lost**, so readers and the writer
still use two words. Single-threaded read went from 4.20 ns to 4.51 ns, write
from 3.56 ns to 4.35 ns, and the contended write benchmark was 7% to 21% slower
at every thread count, on the same cell with the same retry.

The reason is that `parking_lot`'s design is not transplantable, because it does
not carry the thing this crate exists to carry. `parking_lot` has no per-borrow
location, so its fast path is one CAS and nothing else. `borrow_lite` records a
holder `Location` to name the culprit, and a holder pointer is a second location
- so packing the *reader count* in too buys nothing while forcing a `cmpxchg`
where the shipped version's `fetch_add`/`fetch_sub` compile to a cheaper
`lock xadd`.

What did ship is the same trick applied where it pays: the writer's *presence*
and its *location* are one word, a single `AtomicPtr` that is null when no writer
holds the cell and the writer's `Location` when one does. Claiming the cell and
publishing the location are then the *same* compare-exchange, so a reader can
never observe a claimed cell without a location - the window that a separate
writer flag and holder pointer left open. Readers stay a plain `AtomicUsize`
count, and a readers conflict reports its count rather than a location, because
recording a reader's location would be an atomic store on every read.

The single-word design did buy one real thing: it made reader-versus-writer
exclusion *model-checkable*. The shipped protocol cannot be checked for that
property, because it needs `SeqCst`'s total order across two locations and loom
does not model it. Both facts are recorded as tests in `tests/loom.rs` - one
`#[ignore]`d test that fails on the two-location protocol (the documented false
alarm) and passes on the single-word one. The trade was a measurable slowdown
for a checkable proof, and the numbers said no.

The same review that rejected the single word found a real bug in the protocol
it kept. `end_shared` used to clear the writer's location once the
reader count reached zero - but reaching zero is exactly when a writer may win
the handshake, so a writer could store its location between the decrement and the
clear, and the clear would wipe it. The cell was held, yet a later conflict would
name no holder: the diagnostic the crate exists for, silently lost. The clear was
also pointless in the first place, since the holder is only ever written *after*
a writer wins, which requires the count to already be zero - so while any reader
is alive there is nothing to clear. Removing it fixed the race and made the
shared release path about 3% faster (`tests/loom.rs` pins the race; a stress loop
does not, because the window is a few instructions wide).

A later review of the same area found two more holes, both now closed:

- **A `Sync` but `!Send` payload could cross threads.** `Inner`'s `Sync` impl
  asked only for `T: Sync`, but sharing the cell also hands out `&mut T` through
  `borrow_mut`/`with_mut`, and `&mut T` is enough to move or drop the value. A
  `MutexGuard` - `Sync`, `!Send` - could therefore be swapped out and dropped on
  another thread. The bound is now `T: Send + Sync`, checked by two
  `compile_fail` doctests on `Shared` (one through the handle, one through the
  `Deref` target).
- **A writer conflict could lose the writer's location.** The writer flag and the
  holder pointer were two locations: the flag was set first, then the location
  published, so a reader in between saw a claimed cell with no location (and the
  release path had the mirror gap). Folding presence and location into the one
  `AtomicPtr` compare-exchange removes the window; `tests/loom.rs` now models it.

#### When not to use `borrow_lite`

Say it plainly, because a crate that hides its own limits is not trustworthy:

- **Many readers, no writer** -> `Arc<T>`, `arc_swap`, `Snapshot<T>`, or a
  plain copy. `borrow_lite`'s counting cells cost hundreds of times more here
  for a guarantee nobody needs; `Snapshot` drops the count and gets within 1.4x
  of the atomic floor, but gives up in-place mutation and conflict reporting.
- **High contention with long critical sections** -> `parking_lot::RwLock`.
  Parking a thread is the right shape when the wait is long; retrying is not.
- **State held across `.await`** -> `tokio::sync::Mutex`. `borrow_lite`'s guards
  are `!Send` on purpose, so a borrow cannot outlive an await point, but that
  makes `borrow_lite` the wrong tool for async shared state, not a safe one.
- **Compile-time proof that no conflict exists** -> `ghost-cell` or `qcell`.
  `borrow_lite` checks at runtime; those check at compile time, at the cost of
  what you can express.

If your case is not one of those four, `borrow_lite` is likely a strict
improvement over the `RefCell`/`RwLock` you would have written.

---

## Performance

Every number here is a criterion median from `cargo bench` on one 4-core AMD
EPYC 9B14 container, rustc 1.99.1, and each table names the bench group it came
from. Treat them as orders of magnitude, not absolutes: the single-thread
numbers are stable, the multi-threaded ones move between runs, and the *ranking*
is the finding rather than any last digit. The multi-threaded benches
(`contention`, `retry`, `readonly`) are self-checking - the counter is read
before and after and a lost update fails the run - so a wrong cell is caught,
not merely timed. The single-thread benches have no such guard; there the
numbers are only as good as the harness, and the caveats below record where it
misled.

### What a borrow costs

The diagnostics are not free, so here is what they cost, with `diagnostics` on
(the default). Numbers are from `cargo bench --bench compare`. The value-moving
paths (`get_mut`, `into_inner`, `take`) are from its `unique` group, next to the
`std` calls they mirror.

| Operation | `std` | `borrow_lite` | Difference |
|---|---|---|---|
| read borrow, one thread | 1.19 ns | 1.51 ns | +0.32 ns |
| write borrow, one thread | 1.49 ns | 2.09 ns | +0.60 ns |
| read a scalar (`get`) | `Cell::get` 0.59 ns | 1.49 ns | +0.90 ns |
| write a scalar (`set`) | `Cell::set` 0.59 ns | 1.19 ns | +0.60 ns |
| read borrow, shared | 5.15 ns | 4.20 ns | **faster** |
| write borrow, shared | 4.65 ns | 3.88 ns | **faster** |
| borrow out of a unique handle (`get_mut`) | `RefCell::get_mut` 0.59 ns | 0.63 ns | +0.04 ns |
| consume the cell (`into_inner`) | `Rc::try_unwrap` 16.9 ns | 16.9 ns | same |
| empty it in place (`take`) | `RefCell::take` 0.89 ns | 1.47 ns | +0.58 ns |
| refused write borrow | 0.60 ns | 0.69 ns | +0.09 ns |
| handle clone | 0.98 ns | 0.97 ns | same |
| allocate a cell | 9.62 ns | 9.58 ns | same |
| 32 cells created and dropped | 402 ns | 422 ns | +20 ns |
| 16 borrows in a loop | 1.48 ns | 1.48 ns | same |
| format a conflict (`to_string`) | 24 ns | 236 ns | +212 ns |
| read a conflict's fields | - | 5.6 ns | - |
| a refused `borrow_mut` (panic + unwind) | 2.14 µs | 2.29 µs | +0.15 µs |

Five things are worth reading out of that table.

**The threaded cell is faster than `RwLock`, not slower.** That is not a
trick: `RwLock` has to be able to park a waiting thread, so its fast path still
does more work than two `SeqCst` atomics. `borrow_lite` never parks, so its
borrow is cheaper - and that is exactly why it reports a conflict instead of
waiting. You get the latency and you pay with the retry.

**The single-threaded cell costs a few hundred picoseconds** on a borrow,
because it writes a `Location` into a `Cell` that `RefCell` does not have. On
the recommended `with`/`with_mut` path it is about 2.7 ns against 0.89 ns for a
bare `Cell`, so call it roughly 1.8 ns per call - that is the price of the
message you get when something goes wrong.

**`get` is faster than the `with` form it replaces** (1.49 ns against 2.38 ns),
because the closure does not inline away in the benchmark harness. `set` is
level with `with_mut` (1.19 ns against 1.18 ns) once both sides receive the
value the same way - comparing them with a `black_box` *inside* the closure
reports a 20% penalty that is an artefact of the benchmark, not of the cell.

**Cloning and allocation are unchanged.** The handle is still an `Rc`/`Arc`,
and the cell is still one allocation - verified by counting allocator calls, not
by timing (`tests/allocations.rs`). Nothing was added to the pointer-chasing
paths.

**The borrow path is cheap; the message is what costs.** Building a `Conflict`
is free - it is a `Copy` struct of a few words, and the `try_*` numbers above
already include it. What is not free is *rendering* it: `to_string` is 236 ns
against 24 ns for `BorrowMutError`, roughly 10x, because it writes two source
locations and a count. Reading the fields directly (`kind`, `holder`, `count`)
is 5.6 ns, so a caller that wants to log or branch on a conflict should read
those rather than format it.

One asymmetry to know when you branch on the fields: `holder()` can be `None`
on a `Shared` cell. Only the writer path records a location, because a reader
recording its own would be an atomic store on every read borrow - and it would
race, last writer wins. So a *readers* conflict on a `Shared` cell reports
`count` but no holder, while the single-threaded `Local` cell names the first
reader for free. A writer conflict always names the writer, on both. This is a
deliberate trade, pinned by a test rather than left to prose.

A `try_*` method turns a *borrow conflict* into an `Err`; it does not catch a
panic raised by one of your own observers. That panic is your code failing, not
a conflict, so it propagates out of `try_update`, `try_replace`, `try_take` and
`try_notify` unchanged - the cell is restored on the way out, so it stays
usable, but the panic is not converted into a value. Catch it inside the
observer if you need to contain it. The same holds for the narrowing guards:
`ReadGuard::map` and `WriteGuard::map` run your closure before consuming the
guard, so a panic there releases the borrow rather than leaking it.

The panicking form is the expensive one, at 2.29 µs - about 3000x a refused
`try_borrow_mut`. Almost all of that is the unwind, not the message: `std`'s own
refused `RefCell::borrow_mut` panic is 2.14 µs, so the richer message adds only
about 0.15 µs on top. The point stands either way, and it is the argument for
`try_` on any path that can be refused - but it is the *panic*, not the
diagnostics, that you are paying for.

Two measurement errors are worth recording here, because both made the numbers
look worse than they are. An earlier draft *guessed* `std` at ~700 ns; measuring
it moved that by 3x. And the first measured version of these two rows was 2.4x
too slow - 5.2 µs and 5.6 µs - because it did not silence the panic hook.
`catch_unwind` stops the unwind but the default hook still *prints* every caught
panic to stderr, and criterion runs the closure millions of times: that run
wrote 532 MB to stderr and was timing the write, not the unwind. The benchmark
now takes the hook out for these two rows and puts it back. If a figure here
ever looks implausibly bad, suspect the harness before the crate.

One caveat on method: an earlier version of the `with`/`Cell` benchmarks looked
about twice as fast as the real numbers, because the `std` side mutated nothing
and LLVM deleted the borrow entirely. Both sides now write through the cell, and
the `Cell` baseline is behind a `black_box`. If a benchmark here ever looks
implausibly good, suspect the optimiser before the crate.

### The fast path without `diagnostics`

Turning `diagnostics` off drops the holder from the single-threaded cell, so
`Inline<u64>` is 16 bytes and the borrow path is a load and a store with no
`Location` to write. In that build the single-threaded cell matches `RefCell`,
and the threaded cell beats both `RwLock`s on the fast path because it never has
to be able to park a waiter. `cargo bench --no-default-features`, release,
rustc 1.99.1:

| Operation | `std` | `borrow_lite` (no `diagnostics`) | Ratio |
|---|---|---|---|
| read, one thread | `Rc<RefCell>` 1.19 ns | `Local` 1.19 ns | **1.00x** |
| write, one thread | `Rc<RefCell>` 1.49 ns | `Local` 1.47 ns | **0.99x** |
| read, shared | `RwLock` 5.15 ns | `Shared` 4.16 ns | **0.81x** |
| write, shared | `RwLock` 4.66 ns | `Shared` 2.35 ns | **0.50x** |
| read, shared | `parking_lot` 5.93 ns | `Shared` 4.24 ns | **0.72x** |
| write, shared | `parking_lot` 5.18 ns | `Shared` 2.37 ns | **0.46x** |

### Waiting for a busy cell

The benchmarks changed a design decision. The obvious retry loop is
`spin_loop()`, and it is the *worst* option measured - 14.4 ms against 0.93 ms
for yielding, on the same cell, at eight threads. Yielding wins because the
critical section is nanoseconds and a spin burns a core that the holder needs.
So the `std` feature ships the loop that measured fastest rather than leaving
each caller to discover it:

```rust
use borrow_lite::shared;

let counter = shared(0usize);
std::thread::scope(|scope| {
    for _ in 0..4 {
        scope.spawn(|| {
            for _ in 0..1_000 {
                // The `std` feature's `retry(n, op)` wraps exactly this loop.
                while counter.try_with_mut(|c| *c += 1).is_err() {
                    std::thread::yield_now();
                }
            }
        });
    }
});
assert_eq!(counter.with(|c| *c), 4_000);
```

`wait()` is one `thread::yield_now()`. `retry(n, op)` calls `op` up to `n`
times, yielding between attempts, and returns the last `Conflict` if they all
fail. It is **bounded** on purpose: the crate still has no unbounded
`wait_until_free`, because waiting forever can deadlock and that is the failure
this crate exists to avoid. `retry` is opt-in via the `std` feature; the default
build stays `no_std` and has neither helper. Both are exercised in
`tests/wait.rs`, and the example above stays on the core API so it compiles
without the feature.

### Under contention

The tables above are one thread at a time. Under contention the *retry
strategy* matters more than the nanoseconds - more than the choice of cell, as
it turns out. Every thread increments one counter. The numbers come from `cargo
bench --bench contention`, which waits with `spin_loop`, and `cargo bench --bench
retry`, which adds the yielding and backing-off loops; both use the same
workload (20,000 increments per thread), so they are comparable, and the
`backoff` column is the one the `retry` run contributes.

| Threads | `AtomicU64` | `Arc<RwLock>` | `parking_lot` | `borrow_lite` `spin` | `borrow_lite` `yield` | `borrow_lite` `backoff` |
|---|---|---|---|---|---|---|
| 1 | 91 µs | 153 µs | 153 µs | 111 µs | 114 µs | - |
| 2 | 216 µs | 1.14 ms | 311 µs | 622 µs | **218 µs** | 498 µs |
| 4 | 936 µs | 1.54 ms | 729 µs | 5.20 ms | **523 µs** | 688 µs |
| 8 | 1.52 ms | 2.91 ms | 1.17 ms | 14.36 ms | **932 µs** | 1.78 ms |

The finding is in the three `borrow_lite` columns: the cell is identical in all
three, and only the waiting differs. With a spinning retry it is the slowest
option on the board; with a yielding one it wins at four and eight threads
(1.4x and 1.26x over `parking_lot`). The loss was never the cell, it was the
loop - and the loop is the part you control.

`parking_lot::RwLock` is the real competitor: it packs its whole state into one
word and takes a single CAS on the fast path, so it beats `std::sync::RwLock`
everywhere here (311 µs against 1.14 ms at two threads). Comparing only against
`std` flattered `borrow_lite`; comparing against `parking_lot` is the honest
test, and `borrow_lite` with a yielding retry still wins at 4 and 8 threads. At
two threads it is level (218 µs against 311 µs), and at one thread `parking_lot`
ties `std`.

Thread creation is 44-223 µs of each number, so subtract roughly that to read
the contended part.

The naive `spin_loop()` loop is the worst option on the list, and it is the one
most people write. Spinning only pays off when the wait is shorter than a
context switch, and here the whole critical section is a single increment.

The `backoff` column is "spin 16 times, then yield": it is *worse* than plain
`yield_now`, and unstable across runs, because its spin phase burns cores that
plain yielding gives up immediately. A useful negative result - for
nanosecond-long sections, do not spin first.

The obvious objection is that backoff should win when the critical section is
long enough to be worth spinning for. It was measured (`retry/section` runs
`backoff` alongside the others) and it does not: across work levels 0, 64, 256
and 1024 it measured 368, 325, 326 and 321 µs, against 223, 253, 256 and 252 µs
for plain `yield` - consistently about 45% slower, and flat rather than
improving as the section grows. Spinning only pays when the wait is shorter than
a context switch *and* the holder is guaranteed to be running; a retry loop
cannot promise the second, so the spin phase is a bet that loses either way.

The same group also sweeps the length of the exclusive borrow with `spin` and
`yield` side by side, four threads, two runs. `yield` is flat - 223 to 284 µs
regardless of how long the borrow is held, and roughly half of `RwLock` at every
level - while `spin` is both slower and erratic, swinging 2.76 ms to 1.38 ms at
the same work level in different runs, so its individual numbers are not worth
reading. The earlier claim that `borrow_lite` degrades with section length was
an artefact of spinning, and does not survive the yielding loop.

Two honest caveats.

**Multi-threaded numbers move between runs.** `RwLock/2` measured 991 µs, 997 µs
and 1.14 ms across three runs; `backoff/4` measured 1.09 ms and 688 µs. Treat
the table as orders of magnitude, and the *ranking* - `yield` best, `spin`
worst - as the finding, since that held in every run.

That said, the advice is unchanged, because the reason is different. Yielding
still works when the borrow is long, but the moment a borrow is held across a
long section you probably want a lock for other reasons - you cannot nest
borrows, and a retry loop makes the section's length everyone's problem. Use
`yield_now` for short sections and genuine re-entrancy; use a real lock when the
section is long or the access pattern is a queue.

### Many readers, no writer

Every other contended benchmark here involves a writer. The pure-read case is
the one a read-write lock is meant to excel at, so it is worth measuring on its
own, and it is where [`Snapshot<T>`](#the-shapes) exists.
`cargo bench --bench readonly`, 500,000 reads per thread, `cargo bench` medians:

| Threads | `Arc` (no checking) | `AtomicU64` | `Snapshot` | `ArcSwap` | `Shared` | `Arc<RwLock>` |
|---|---|---|---|---|---|---|
| 1 | 51 µs | 332 µs | 433 µs | 2.50 ms | 1.99 ms | 2.69 ms |
| 2 | 83 µs | 391 µs | 505 µs | 2.62 ms | 16.8 ms | 22.1 ms |
| 4 | 136 µs | 700 µs | 891 µs | 4.84 ms | 67.3 ms | 80.6 ms |
| 8 | 251 µs | 1.40 ms | 1.87 ms | 9.98 ms | 120 ms | 164 ms |

Three things fall out of this table.

**The counter is the cost, not the cell.** With no bookkeeping at all, 4 million
reads across eight threads take 251 µs. `Shared` and `RwLock` take 120 ms and
164 ms for the same work - **hundreds of times slower** - because every read
borrow does a `fetch_add` on one shared counter, so readers serialise on the same
cache line. `Shared` is the faster of the two cells (1.4x at eight threads), but
both are in the same regime.

**Removing the counter closes the gap.** `Snapshot` reads are a single `Acquire`
load with no increment, so readers never touch shared state. At eight threads
that is 1.87 ms against `Shared`'s 120 ms - **64x** - and within 1.4x of the
`AtomicU64` floor, which is the same single load without a value behind it. The
first row also shows why `Snapshot` is not simply "a faster `Shared`": it has
given up the reader count, so it cannot report a conflict, and a write replaces
the whole value rather than mutating in place.

**`Snapshot` beats `arc_swap` on this workload, and the reason is instructive.**
`ArcSwap::load` hands back a refcounted handle, so every read is an `Arc` clone
- an atomic increment and later decrement on a shared count. `Snapshot::load`
borrows the cell instead, so a read is one load. Measured here that is 5x at one
thread (433 µs vs 2.50 ms) and 5.3x at eight (1.87 ms vs 9.98 ms). The price is
that the `&T` cannot outlive the `Snapshot` (no refcount means no way to own it
independently), and that superseded values are kept until the cell is dropped.
`arc_swap` wins when you need to hold the value past the cell, or write from many
threads with lock-free swaps; `Snapshot` wins when reads dominate and the value
is replaced occasionally.

`borrow_lite`'s own margin over `RwLock` is thinner than it looks: 1.2x at four
threads and 1.4x at eight. The eight-thread row is the worst in the file for
reproducibility - `Shared` measured 120 ms and 132 ms on two runs, `RwLock`
164 ms and 177 ms - because eight threads on four cores are oversubscribed and
the median depends on how the scheduler interleaved them. Trust the shape, not
the last digit.

Shared borrows genuinely coexist - they do not queue as exclusive borrows. The
group `readonly/hold` puts every reader behind a barrier while holding a borrow,
so all N are provably inside at once, and it costs the same as `RwLock` (both
dominated by the barrier itself). The coexistence is asserted in the test suite
too, in `shared_borrows_coexist_across_threads`.

The practical reading, by what you have:

- many readers, value replaced occasionally -> `Snapshot<T>`
- many readers, value is one scalar -> `AtomicU64`/`AtomicUsize`
- many readers, value must be owned past the cell -> `ArcSwap`
- many readers, never written -> a plain `Arc<T>` copy, or copy it out

`Shared<T>` is still the right cell for low contention, short borrows, and
re-entrancy, where a `RwLock` would deadlock instead of reporting a `Conflict`.
If you do contend, retry with `yield_now` rather than `spin_loop`. And if the
borrow is held for a long time, use a real lock - a busy-wait is the wrong shape
for that regardless of which cell is underneath.

### What observing costs

`Observable<T>` had no performance data at all before this: the other suites
measure borrows, and observing is a different path. `cargo bench --bench
observable`, 200,000 operations, two runs:

| Operation | Time | Per op |
|---|---|---|
| `with`, no observers | 60 µs | 0.3 ns |
| `with`, 16 observers | 60 µs | 0.3 ns |
| `Local::with` (reference) | 61 µs | 0.3 ns |
| `update`, 0 observers | 5.0 ms | 25 ns |
| `update`, 1 observer | 11.9 ms | 60 ns |
| `update`, 4 observers | 13.4 ms | 67 ns |
| `update`, 16 observers | 17.3 ms | 86 ns |
| `Local::with_mut` (reference) | 196 µs | 1 ns |
| `observe` + `unobserve` | 1.95 ms | 10 ns |
| `observer_count`, 0 observers | 0.83 ms | 4 ns |
| `observer_count`, 64 observers | 0.79 ms | 4 ns |
| observer's `try_update`, refused | 17.1 ms | 85 ns |
| observer's `with`, allowed | 17.2 ms | 86 ns |

Reading is free. `with` costs the same as a bare `Local::with` whether zero or
sixteen observers are registered - registering observers does not slow down the
read path, because `with` only takes a borrow.

`observer_count` is the two lengths when no removal is queued, so it is flat
across the observer count (4 ns at both 0 and 64 observers, the borrow around it
rather than the count). It scans only while a removal is queued mid-notification,
which is what lets it leave out an observer `unobserve` has already removed.

The time table hides a second, sharper fact that a counting allocator makes
visible (`tests/allocations.rs`, which installs a `GlobalAlloc` and counts):

- `Observable::new` is **two** allocations, not one - the value and the observer
  list are separate `Local`s. That is why the "one allocation" claim elsewhere in
  this file is scoped to `Local`/`Shared` and does not stretch to `Observable`.
- An `update` with **no** observers allocates **nothing**. The entry cost the
  table shows (25 ns) is real but it is not the allocator; the round breaks out
  before either scratch list grows.
- An `update` with **any** observers allocates **twice per call, plus once on
  the first call**. `notify` moves the list into two scratch `Vec`s (`running`
  and `finished`) for panic-safety, and each allocates once; the guard also
  records the in-flight observer *ids* in a third list, which grows on the first
  notification and then keeps its capacity. So the steady-state count is a
  constant two - four observers cost the same two allocations as one, so it is a
  fixed per-update cost, not a per-observer one - and the third allocation is a
  one-time growth, not a per-call cost. The table's 25 ns entry cost and its
  flat +2 ns per observer are the same facts seen as time.

A plain borrow never allocates, including the refusal path: a `Conflict` is a
`Copy` struct of a few words, and only `to_string` builds the `String`.

`Snapshot<T>` is on the same list, and its numbers are the ones that make its
read path credible: `Snapshot::new` is **two** allocations (the value is boxed
and the retired list is a `Vec` seeded with it), `load` allocates **nothing** -
that is the whole point of the type - and a steady-state `store` allocates
exactly **one**, the box for the new value. All three are asserted in
`tests/allocations.rs`.

Writing is where the cost is, and there is a surprise. `update` with **no
observers at all** is still 25x a plain `with_mut` (25 ns against 1 ns), because
every update enters the notification machinery - taking the observer list out of
its cell and putting it back - before discovering nobody is listening. Past
that, each observer adds about 2 ns: the jump from zero to one observer is the
fixed entry cost, and the rest scales gently.

Refusing a re-entrant mutation is free: an observer whose `try_update` is
rejected costs the same 85 ns as one whose `with` is allowed (86 ns), because
the borrow that the notification holds is detected in the same flag check that
any other borrow does. The safety property costs nothing extra to enforce.

That is not a defect, but it is worth knowing: if you have a hot update loop and
nobody is observing, use `Local`/`Shared` directly and keep `Observable` for the
wiring. At 25 ns an update it only matters in a loop that runs millions of
times, which is exactly when the observer pattern is the wrong shape anyway.

`Observable` now has the same value-lifetime paths `Local` does - `get_mut`,
`into_inner`, `replace`/`try_replace`, `take`/`try_take`, plus the read twins
`with`/`try_with` and the closure-free `get`/`try_get`. `cargo bench --bench
observable -- lifecycle` prices them against `Local` so the notification is
visible in the difference. `get_mut` is identical (0.65 ns against `Local`'s
0.66 ns) because it reaches straight into the value cell. `into_inner` costs
16 ns more (34.1 ns against 18.3 ns) because an `Observable` owns *two* `Rc`s
(the value and the observer list), so consuming it is two deallocations, not
one. `take` is the interesting one: it must notify, because observers are
supposed to see the value change, so it pays `Local::take` plus a full
notification round - 26 ns with nobody listening against `Local`'s 1.6 ns,
rising to 108 ns with 16 observers, exactly as `update` does. If you want to
empty the cell without telling anyone, `update` it to a default yourself;
`take` is the version that keeps observers honest.

One benchmark bug worth recording, because it nearly produced a false headline.
The `Local::with_mut` reference first measured **1.9 µs** for 200,000 iterations
- 0.01 ns each, below one CPU cycle, which is not a measurement of anything. The
optimiser had collapsed the plain-cell loop to a single assignment, while
`update` could not be collapsed because `notify` touches a heap allocation. The
comparison would have shown a fake 2500x gap. A `black_box` on the stored value
in both loops fixed it; the honest gap is 25x.

---

## Testing and evidence

### Testing

```text
cargo test --all-targets
cargo test --doc
cargo test --features std --all-targets   # the `retry`/`wait` helper
cargo test --features std --doc
# `diagnostics` is on by default, so this is the only pass that compiles the
# crate without it - the mode that drops `Inline` to 16 bytes.
cargo test --no-default-features --all-targets
cargo test --no-default-features --doc
cargo clippy --all-targets -- -D warnings
cargo clippy --features std --all-targets -- -D warnings
cargo clippy --no-default-features --all-targets -- -D warnings
cargo clippy --workspace --all-features --all-targets -- -D warnings
cargo test --workspace --all-features --all-targets  # every feature at once
cargo fmt --check
cargo bench                       # all six suites
cargo bench --bench compare       # the two cells, next to the `std` types
cargo bench --bench retry         # wait strategies, and the section sweep
cargo bench --bench contention    # the retry strategies under contention
cargo bench --bench readonly      # many readers, no writer
cargo bench --bench observable    # the observer path, including refusals
cargo bench --bench ergonomic_paths  # the ergonomic call paths, next to `std`
cargo bench -- --test             # run each bench once, assertions included

# One loom model is `#[ignore]`d on purpose: it fails on the shipped two-location
# protocol and passes on a single-word one, which is the documented loom false
# alarm. Run it deliberately when touching `AtomicState`.
LOOM_MAX_PREEMPTIONS=3 RUSTFLAGS="--cfg loom" cargo test --test loom --release -- --ignored

# The `no_std` claim and the declared MSRV are checked, not assumed: the library
# is built for a bare-metal target, and the whole crate is built on the oldest
# supported compiler.
cargo build --target thumbv7em-none-eabi
cargo +1.85 build --all-targets
```

CI runs each of these in its own job, plus Miri, loom and all three fuzz targets.
The `std` feature is additive: the default build is still `no_std` and is
checked on a bare-metal target, so the helper cannot leak `std` into a build
that did not ask for it.

`tests/safety.rs` covers the properties the other suites do not: that a panic
while a borrow is alive releases it, that a panicking observer leaves the cell
usable with its observers intact, and that the handles are one pointer wide
(a `const` assertion, so it cannot silently stop checking). The same file pins
`Inline<u64>` at 24 bytes with `diagnostics` and 16 without it (the same 16 as
`RefCell<u64>`), both guards, and `Snapshot<u64>` at 40 - the read-mostly cell's
pointer, writer lock and retired list - all as `const` assertions, so a layout
change fails the build rather than a printed example.

`tests/behaviour.rs` also runs *differential* tests: the same script of
operations against `Rc<RefCell<T>>`/`Arc<RwLock<T>>` and against `Local`/`Shared`,
comparing the results. That is the honest way to check the "the value is the
same" claim above, since asserting each side in isolation could agree with a
shared mistake.

`tests/allocations.rs` installs a counting global allocator and checks the
*counts* the timing benchmarks cannot see: a `Local`/`Shared` is one allocation,
cloning a handle allocates nothing, a borrow (granted or refused) allocates
nothing, and dropping the last handle frees exactly one block. It is the only
way to check "one allocation" - a benchmark would agree with a cell that
allocated twice.

`tests/payloads.rs` covers the payload types the other suites never use: one
with its own `Drop` (dropped exactly once, and not while a clone or a guard is
alive), a zero-sized type, an over-aligned one (`#[repr(align(64))]`, whose
pointer is checked to stay aligned), and a 4 KiB one. It also covers the two
awkward drops: a payload whose own `Drop` panics, and a panic *inside the value*
while a guard is alive, after which the borrow must be released and the payload
still dropped once. It also checks that `Observable<T>`, which owns the payload
*and* an observer list, drops its payload exactly once with observers still
registered. These are Miri's territory - a double drop or a leak is invisible to
a single-threaded test - and they are the payloads an `UnsafeCell`-plus-raw-pointer
cell is most likely to get wrong.

The `Send`/`Sync` bounds are checked rather than described. `Local` is `!Send`
and `!Sync`; `Shared<T>` is `Send + Sync` exactly when `T` is; a guard is
`!Send`; `Observable<T>` is `!Send` *and* `!Sync` (its "cannot be shared between
threads" claim is the `!Sync` half, a separate bound, so it has its own
`compile_fail`). Each is a `compile_fail` doctest, so loosening an `unsafe impl`
makes the suite fail instead of silently widening the contract.

Nested borrows are covered both ways. Only writes-in-writes were tested before;
two *shared* borrows are compatible, so `with` inside `with` on the same cell
must succeed, and now a test says so (and fails if a reader is ever refused).
The mirror case - an observer reading the source it is being notified about,
and seeing the same post-update value the observer was handed - is covered too.

Both examples are run in CI (`demo` walks the whole API, `sizes` prints the
handle and guard sizes), because `cargo test --all-targets` only *compiles*
them.

#### The borrow protocol under a model checker

`tests/concurrency.rs` runs real threads under Miri, which catches real bugs but
only on the interleavings it happens to hit. Two more tools go further:

```text
# Miri, with a fresh schedule per seed instead of one
MIRIFLAGS="-Zmiri-disable-isolation -Zmiri-many-seeds=0..16" cargo +nightly miri test

# Loom, which is exhaustive over the interleavings it can construct
LOOM_MAX_PREEMPTIONS=3 RUSTFLAGS="--cfg loom" cargo test --test loom --release

# Fuzzing: operation scripts replayed against a shadow model
cargo +nightly fuzz run borrow_ops fuzz/corpus/borrow_ops fuzz/seeds/borrow_ops
cargo +nightly fuzz run observers fuzz/corpus/observers fuzz/seeds/observers
cargo +nightly fuzz run snapshot fuzz/corpus/snapshot fuzz/seeds/snapshot
```

The two corpus directories are deliberate. `fuzz/seeds/<target>` is tracked and
read as the starting corpus, and doubles as the crash-regression set: a file that
once made a target fail is dropped in there and replayed on every run. The first
directory is where libfuzzer *writes* what it finds; `fuzz/corpus` is ignored, so
those thousands of files never reach the tree. Pass only one directory and
libfuzzer writes its finds into that one - straight into the tracked seeds if
that is the one you passed.

Loom is worth a note, because the obvious test does not work. The handshake in
`AtomicState` is Dekker-style and rests on `SeqCst` being a single total order
over all locations. Loom does not model that - its README says `SeqCst` accesses
"are regarded as `AcqRel` ... causing Loom to generate false alarms". A loom test
that asserts "a reader and a writer are never both inside the cell" therefore
fails on this protocol, and it fails for a standalone copy of the same handshake
with no `borrow_lite` code in it at all, which is how the cause was pinned down.

So `tests/loom.rs` checks the parts that do not need a global order - release
frees the cell, readers do not conflict, two writers never lose an update,
bookkeeping balances - and includes one test that asserts loom *does* accept the
`SeqCst`-forbidden store-buffering outcome, so a passing run is never mistaken
for proof that `SeqCst` is unnecessary. It also models `Snapshot`'s value
pointer: a plain `u64` payload would make that test pass no matter how the
pointer was published, so the payload carries its own loom atomic and the test
fails if `store`'s `Release` or `load`'s `Acquire` is weakened (both verified by
mutation). `Snapshot`'s `WriterLock` is a spin lock, and loom explores a spin
lock's retry loop as an unbounded branch, so a *contended* `store` diverges the
model - that property is left to `tests/concurrency.rs` under real threads. The
global-order property of the borrow cell is likewise left to
`tests/concurrency.rs` and Miri, neither of which has that limit.

The fuzz targets replay each input as a script of operations against a small
shadow model, and fail if the crate's answers disagree with it. `borrow_ops`
covers the borrow protocol; `observers` covers subscription bookkeeping, the
re-entrancy rules, and self-removal; `snapshot` covers whole-value replacement
and the retirement contract - a reference taken before a `store` still reads the
old value, and every published value is freed exactly once. Their value was
checked by mutation rather than assumed:

* `borrow_ops`: making `try_exclusive` ignore live readers is caught within
  seconds, while deleting the second `writer` load in `try_shared` is *not* -
  that load only matters across threads, which is exactly the gap Miri and loom
  cover.
* `observers`: making `unobserve` always report `false` is caught immediately.
* `snapshot`: reverting `into_inner` to remove the current value by pointer
  equality - the zero-sized-`T` bug the crate actually had - is caught in under
  a second, because the drop counter then reports fewer drops than creations.
  The target runs the same script against a sized and a zero-sized payload for
  exactly this reason; the sized one cannot see the bug, since its pointers
  differ. The failing input is kept as `fuzz/seeds/snapshot/zst_into_inner_loses_retired_drops`.

One thing is deliberately *not* fuzzed. `libfuzzer-sys` installs a panic hook
that calls `std::process::abort()` before unwinding (see its `initialize`), so a
panic in a cargo-fuzz target aborts the process and `catch_unwind` can never see
it - a target that injects panics cannot pass, whatever the crate does. Panic
recovery therefore lives in `tests/safety.rs`, under plain `cargo test` and
Miri, where it includes the bug the observer path once had.

The crate is `no_std` plus `alloc`, and has no dependencies. `criterion` is a
dev-dependency, and `loom` a `cfg(loom)`-gated one, so neither is part of the
library's normal build. Edition 2024, MSRV 1.85 (the first release with that
edition). The `no_std` build and the MSRV are both CI jobs, so neither claim can
rot quietly.

---

## License

### License and authors

Licensed under the Apache License, Version 2.0 - see [`LICENSE`](LICENSE) for
the full text.

Written and maintained by Yayasan Pesantren Cipansor and contributors.
