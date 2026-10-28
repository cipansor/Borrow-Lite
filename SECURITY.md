# Security policy

## What `borrow_lite` is

`borrow_lite` is a `no_std` library of interior-mutability cells. It does no I/O,
opens no network connection, reads no environment variable, and has no
dependencies in its default build. Its whole attack surface is *memory safety*:
the crate contains `unsafe` code (confined to `src/state.rs`, `src/guards.rs`,
`src/inner.rs`, `src/snapshot.rs`, and - under the `unsafe-unchecked` feature -
`src/unchecked.rs`), and a mistake there is the only way this library can hurt a
program that uses it correctly.

That means the vulnerabilities that matter here are soundness bugs:

- an `unsafe impl Send`/`Sync` that is too loose, so a value crosses a thread
  boundary it should not;
- a borrow protocol that hands out `&mut T` while a `&T` is still alive;
- a counter that can wrap, so a "shared" cell is treated as free;
- a lifetime escape in a guard, a mapped guard, or `Snapshot::load`;
- a use-after-free or double-free in `Snapshot`'s value retirement.

A panic is not a vulnerability by itself - the crate refuses a conflicting
borrow by panicking, by design - but a panic that is reachable *without* a
programming error (an input that a correct caller cannot avoid) is in scope.

## Supported versions

Only the latest `0.1.x` release is supported. The crate has not reached 1.0, so
there is no back-porting: a fix ships in a new patch release.

| Version | Supported |
|---|---|
| latest `0.1.x` | yes |
| anything older | no |

## Reporting a vulnerability

Please report privately, not in a public issue, so a fix can be prepared before
the details are public.

Use GitHub's private vulnerability reporting on the repository:

**<https://github.com/cipansor/Borrow-Lite/security/advisories/new>**

(That is the *Security* tab -> *Report a vulnerability* form. If the form is not
available, open an issue that says only that you have a security report and asks
for a private channel - do not include the details in the issue itself.)

A useful report includes:

- the crate version, and the toolchain (`rustc -V`) and target;
- the smallest program that reproduces it, ideally a `tests/`-style file;
- what you expected and what happened instead;
- for a soundness bug, whether Miri reports it (`cargo +nightly miri test`) and
  the exact `MIRIFLAGS`.

## What to expect

This is a small, volunteer-run project, so these are targets rather than
guarantees:

- acknowledgement of the report within about a week;
- an assessment - reproduced, not reproducible, or by design - with reasoning;
- for a confirmed soundness bug, a fix and a patch release, and credit in the
  release notes unless you prefer otherwise.

## How the crate is checked

The properties above are the ones the test suite exists to defend, so a report is
usually faster to triage against them:

- **Miri**, under `-Zmiri-strict-provenance` and many seeds, checks the `unsafe`
  code for undefined behaviour;
- **loom** model-checks the atomic handshake in the shared cell;
- **fuzz targets** replay operation scripts against a shadow model, including
  `Snapshot`'s retirement contract against a drop counter;
- **`compile_fail` doctests** pin every `Send`/`Sync` bound, so loosening one
  fails the suite instead of silently widening the contract;
- a **mutation sweep** over the borrow bookkeeping confirms each assertion fails
  when the branch it guards is broken.

See the README's [Testing and evidence](README.md#testing-and-evidence)
section for the details.
