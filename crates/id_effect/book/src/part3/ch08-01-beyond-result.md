# Beyond Result — Why Cause Exists

`Result<T, E>` handles the errors you expect. But what about the errors that aren't your `E`?

## Expected vs. Unexpected

```rust
# use id_effect::{Effect, caps};
# struct User;
# struct UserNotFound;
# struct Db;
# type Expected =
// Expected: you planned for this
Effect<User, UserNotFound, caps!(Db)>
# ;

// But what if the database driver panics?
// What if the fiber is interrupted?
// Neither of these is a UserNotFound.
```

Traditional Rust handles unexpected failures through panics, which unwind (or abort) and bypass your error handling. In async code a panic in a task can silently swallow errors or leave resources unreleased.

## The Cause Type

`Cause<E>` is id_effect's complete taxonomy of failure (`id_effect::Cause`):

<!-- ignore: excerpt of the id_effect::Cause definition, not a standalone program -->
```rust,ignore
pub enum Cause<E> {
    Fail(E),                            // your typed, expected error
    Die(Defect),                        // a panic or `die(..)`: a bug
    Interrupt(FiberId),                 // the fiber was interrupted
    Both(Box<Cause<E>>, Box<Cause<E>>), // two causes in parallel (e.g. zip_par)
    Then(Box<Cause<E>>, Box<Cause<E>>), // two causes in sequence (e.g. a dying finalizer)
}
```

- `Cause::Fail(e)` — an error you declared in `E`, handled with `catch` or `map_err`.
- `Cause::Die(defect)` — a panic inside an effect step, or an explicit `die("..")`. The runtime catches panics in effect closures and turns them into `Die`; `Defect::message()` is the panic text.
- `Cause::Interrupt(fiber_id)` — the fiber was interrupted: its parent exited, it lost a race, someone called `FiberHandle::interrupt`, or its supervisor shut down.
- `Both` / `Then` — composite causes. `Cause::pretty()` renders the tree, `Cause::fail_option()` extracts a `Fail` payload, `Cause::contains(&pred)` searches the tree.

```rust
use id_effect::{Cause, Effect, Exit, die, fail};

let failed: Effect<i32, &str> = fail("no");
assert_eq!(failed.block_on(), Exit::fail("no"));

let died: Effect<i32, &str> = die("boom");
assert!(matches!(died.block_on(), Exit::Failure(Cause::Die(_))));

assert_eq!(Cause::fail(7_u8).fail_option(), Some(7));
assert_eq!(Cause::<u8>::die("x").fail_option(), None);
```

## Why This Matters

The typed-error combinators (`catch`, `map_err`) only ever see `Cause::Fail`. Defects and interrupts pass through them untouched, so a bug cannot be "recovered" by accident and an interrupted fiber really stops. Finalizers (Chapter 10) run for *every* cause, so cleanup happens however the fiber ends.

When you do need the whole picture — at a top-level handler, in a test, in a supervisor — observe the outcome as a value with `.exit()` (next section).

## Day-to-Day Usage

In application code you rarely inspect `Cause` directly. You use:

- `.catch(f)` / `.map_err(f)` for `Cause::Fail`;
- `.exit()` or `block_on()` when you need the terminal [`Exit`](./ch08-02-exit.md);
- `id_effect_cli::exit_code_for_exit` at the process edge.

`Cause` is mostly visible at infrastructure boundaries: finalizers (which receive an `ExitKind`), supervisors and top-level error reporting.
