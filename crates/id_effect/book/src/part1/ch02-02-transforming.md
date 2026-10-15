# Transforming Success — map and map_err

You have an effect that produces some value, but you want a different value or a different error.
That is what `map` and `map_err` are for.

## map

`map` transforms the success value without running any new effects:

```rust
use id_effect::{Effect, Exit, succeed};

let number: Effect<i32, String> = succeed(21);
let doubled: Effect<i32, String> = number.map(|n| n * 2);
let text: Effect<String, String> = doubled.map(|n| n.to_string());
assert_eq!(text.block_on(), Exit::succeed("42".to_string()));
```

None of the `.map()` calls executes anything; each one wraps the previous description in "and then
transform the result with this function". Only `A` changes; `E` and `R` stay the same.

## map_err

`map_err` transforms the typed failure and leaves the success path untouched:

```rust
use id_effect::{Cause, Effect, Exit, fail};

#[derive(Debug, PartialEq)]
struct AppError(String);

let db_err: Effect<String, String> = fail("db connection failed".to_string());
let app_err: Effect<String, AppError> = db_err.map_err(AppError);
assert_eq!(
  app_err.block_on(),
  Exit::Failure(Cause::Fail(AppError("db connection failed".into())))
);
```

This is the usual tool at module boundaries: a database module returns `DbError`, the application
works with `AppError`. When `AppError: From<DbError>`, `.map_err(Into::into)` is enough, and inside
`effect!` the conversion happens on its own (Chapter 3). Defects and interrupts pass through
`map_err` unchanged.

## Why These Don't Execute Anything

```rust
let effect = succeed::<i32, Never, HNil>(42)
  .map(|n| {
    println!("mapping!");
    n + 1
  })
  .map(|n| n * 2);

// Nothing has printed yet: we have a description of three steps.

assert_eq!(effect.block_on(), Exit::succeed(86));
// NOW the effect ran: "mapping!" printed once.
```

## Combining map and map_err

A common pattern normalises an effect into your domain's types:

```rust
fn fetch_user_record(id: u64) -> Effect<User, AppError> {
  raw_db_fetch(id)
    .map(User::from_row)
    .map_err(AppError::Database)
}
```

## Other small combinators

- `a.then(b)` runs `a`, discards its value and runs `b`.
- `a.zip(b)` runs `a` then `b` and pairs their values (`zip_par(a, b)` runs them concurrently).
- `eff.exit()` turns the outcome into a value: `Effect<Exit<A, E>, E2, R>` never fails with a
  typed error, so you can inspect the `Exit` in a later step.
- `eff.catch(|e| ..)` recovers from a typed failure ([Chapter 8](../part3/ch08-03-recovery.md)).

## Summary

| Method | Changes | Does not change |
|--------|---------|-----------------|
| `.map(f)` | `A` (success type) | `E`, `R` |
| `.map_err(f)` | `E` (error type) | `A`, `R` |
| `.then(b)` / `.zip(b)` | `A` | `E`, `R` |

None of them run the effect. They all return new, larger descriptions.
