# borrow_lite_derive

[![crates.io](https://img.shields.io/crates/v/borrow_lite_derive.svg)](https://crates.io/crates/borrow_lite_derive)
[![docs.rs](https://docs.rs/borrow_lite_derive/badge.svg)](https://docs.rs/borrow_lite_derive)
[![license](https://img.shields.io/crates/l/borrow_lite_derive.svg)](https://github.com/cipansor/Borrow-Lite/blob/main/LICENSE)

The `#[derive(Cell)]` implementation for
[`borrow_lite`](https://crates.io/crates/borrow_lite).

## Do not depend on this crate directly

This is an implementation detail of `borrow_lite`'s `derive` feature. A
proc-macro crate cannot export anything but proc-macros, so the derive lives in
its own package. Depend on `borrow_lite` with the feature on:

```toml
[dependencies]
borrow_lite = { version = "0.1", features = ["derive"] }
```

Then use it through the re-export:

```rust
use borrow_lite::Cell;

#[derive(Cell)]
#[cell(shared)]
struct Stats {
    hits: u64,
    name: String,
}
```

That generates a `StatsCell` handle beside `Stats`, with one accessor per field
(`stats.hits()`, `stats.hits_mut()`) and the whole-struct API through `Deref`.

The `cell!` macro in `borrow_lite` does the same thing without a dependency; the
derive is the one-name alternative for when spelling the value type twice is
unwelcome. See the [`borrow_lite` README](https://github.com/cipansor/Borrow-Lite#derivecell-the-field-accessors-without-the-macro)
for the full comparison.

## License

Licensed under the Apache License, Version 2.0.
