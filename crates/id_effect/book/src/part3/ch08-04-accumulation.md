# Error Accumulation — Collecting All Failures

`catch` and `map_err` handle errors one at a time: one effect, one error, one handler. Sometimes you need to run many operations and collect *all* their failures, not just the first.

## The Fail-Fast Default

Sequential `effect!` short-circuits on the first failure, and so do the parallel combinators (`zip_par`, `all`, `for_each_par`): the first failure interrupts the siblings still running.

```rust,ignore
effect!({
    let name = ~validate_name(&input.name);    // fails here →
    let email = ~validate_email(&input.email); // never runs
    let age = ~validate_age(input.age);        // never runs
    Person { name, email, age }
})
```

For form validation or batch imports you want to report every error, not just the first.

## Accumulate with `.exit()`

There is no dedicated `validate_all` in the 0.7 kernel; you don't need one. Turn each effect into one that *succeeds* with its `Exit`, run them all, then split the outcomes:

```rust
use id_effect::{Concurrency, Effect, Exit, all, fail, succeed};

fn import(n: u32) -> Effect<u32, String> {
    if n % 2 == 0 { succeed(n) } else { fail(format!("record {n} is odd")) }
}

let observed: Vec<Effect<Exit<u32, String>>> = (1..=4).map(|n| import(n).exit()).collect();
let exits = all(observed, Concurrency::Unbounded).block_on().into_result().ok().unwrap();

let (mut ok, mut errors) = (Vec::new(), Vec::new());
for exit in exits {
    match exit.into_result() {
        Ok(v) => ok.push(v),
        Err(cause) => errors.extend(cause.fail_option()),
    }
}
assert_eq!(ok, vec![2, 4]);
assert_eq!(errors.len(), 2);
```

`all` keeps input order, and because every element is infallible the batch never short-circuits. Use `Concurrency::Sequential` to run them one after another, or `Concurrency::Bounded(n)` to cap parallelism. Defects stay visible: `fail_option` returns `None` for a `Die`, so decide explicitly whether a defect aborts the batch.

## Or: Combining Two Error Types

When composing effects with different error types, `Or` (spelled with the `err!` macro) defers the decision of how to combine them:

```rust
use id_effect::{Cause, Effect, Exit, Or, err, fail};

#[derive(Debug, Clone, PartialEq, Eq)]
enum DbError { Down }
#[derive(Debug, Clone, PartialEq, Eq)]
enum NetError { Timeout }

let db: Effect<(), err!(DbError | NetError)> = fail(DbError::Down).map_err(Or::Left);
assert!(matches!(db.block_on(), Exit::Failure(Cause::Fail(Or::Left(DbError::Down)))));
```

Inside `effect!`, `~expr` widens a sub-effect's error into the block's error type, so an `Or` (or an `AppError` with `From` impls) is usually all the glue you need.

## ParseErrors

The Schema module ([Chapter 14](../part4/ch14-00-schema.md)) reports parse failures with field paths (`ParseError`, `ParseErrors`). The pattern — collect all, report all — is the same whenever you validate structured input.

## When to Accumulate vs. Short-Circuit

| Situation | Use |
|-----------|-----|
| Dependent steps (each needs the previous result) | `effect!` (short-circuit) |
| Independent steps, fail fast | `zip_par` / `all` / `for_each_par` |
| Independent validations, report all | `.exit()` per item + `all` |
| Schema parsing | `ParseErrors` |
