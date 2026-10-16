# Recovery Combinators — catch, map_err and exit

Knowing about `Cause` and `Exit` is only useful if you can act on them. The 0.7 kernel keeps the recovery surface deliberately small: three methods on `Effect`, plus ordinary `flat_map` over an observed `Exit`.

## catch: Handle Expected Errors

`catch` intercepts `Cause::Fail(e)` and gives you a chance to recover (it may also change the error type):

```rust
# use id_effect::{Effect, caps, fail, succeed};
# struct Db;
# #[derive(Debug)]
# enum DbError { NotFound, Timeout }
# struct User;
# impl User { fn anonymous() -> Self { User } }
# fn find_user(_id: u64) -> Effect<User, DbError, caps!(Db)> { fail(DbError::NotFound) }
# let id = 7;
let resilient: Effect<User, DbError, caps!(Db)> = find_user(id).catch(|error: DbError| match error {
    DbError::NotFound => succeed(User::anonymous()),
    other => fail(other), // re-raise anything else
});
```

If `find_user` fails with `Cause::Fail(e)`, the closure runs. `Cause::Die` and `Cause::Interrupt` pass through unchanged — `catch` only handles typed failures. A runnable version (`examples/012_catch.rs`):

```rust
use id_effect::{Effect, Exit, effect, succeed};

fn parse_i32(raw: &'static str) -> Effect<i32, &'static str> {
    effect!({ raw.parse::<i32>().map_err(|_| "parse_failed")? })
}

let program: Effect<i32, &'static str> = effect!({
    let v = ~parse_i32("x").catch(|_| succeed(0));
    v + 42
});
assert_eq!(program.block_on(), Exit::succeed(42));
```

## map_err: Rewrite the Error

`map_err` transforms the typed error and leaves defects and interrupts alone. Use it to lift a component's error into your application's error type, or into an `Or` union (next section):

```rust
# use id_effect::{Effect, effect};
# #[derive(Debug)]
# enum AppError { BadInput }
# fn parse_i32(raw: &'static str) -> Effect<i32, &'static str> {
#     effect!({ raw.parse::<i32>().map_err(|_| "parse_failed")? })
# }
# let program: Effect<i32, AppError> = effect!({
let v = ~parse_i32("nope").map_err(|_| AppError::BadInput);
# v
# });
# assert!(program.block_on().into_result().is_err());
```

## exit: Handle Everything

When you genuinely need to react to defects or interruption — at a top-level handler, around a plugin call, in a supervisor — observe the outcome as a value and branch on it:

```rust
use id_effect::{Cause, Exit, succeed};
# use id_effect::{Effect, die};
# fn risky() -> Effect<i32, String> { die("plugin panicked") }
# fn handle_expected_error(e: String) -> Effect<i32, String> { id_effect::fail(e) }
# fn log_defect(report: String) { eprintln!("{report}") }
# fn fallback_value() -> i32 { 0 }

let bulletproof = risky().exit().flat_map(|exit| match exit {
    Exit::Success(v) => succeed(v),
    Exit::Failure(Cause::Fail(e)) => handle_expected_error(e),
    Exit::Failure(cause) => {
        log_defect(cause.pretty());
        succeed(fallback_value())
    }
});
# assert_eq!(bulletproof.block_on(), Exit::succeed(0));
```

Don't use this to swallow defects silently, and remember that swallowing a `Cause::Interrupt` means your fiber keeps running after someone asked it to stop.

## Common recipes

| Effect.ts / 0.6 name | 0.7 spelling |
|---|---|
| `catchAll` / `catch_all` | `.exit().flat_map(\|exit\| ..)` |
| `orElse(alt)` | `.catch(\|_\| alt())` |
| `fold(on_e, on_a)` | `.map(on_a).catch(\|e\| succeed(on_e(e)))` |
| `ignore` / `option` | `.map(Some).catch(\|_\| succeed(None))` |
| `retry` | `Effect::retry(\|\| eff, schedule)` ([Chapter 11](./ch11-03-retry-repeat.md)) |

Prefer the narrowest tool that solves your problem: `catch` for expected errors, `.exit()` only when you need to see defects or interrupts.
