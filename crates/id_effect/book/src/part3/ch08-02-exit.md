# Exit — Terminal Outcomes

Every effect run ends with an `Exit`. It's the final word on what happened.

## The Exit Type

<!-- ignore: excerpt of the id_effect::Exit definition, not a standalone program -->
```rust,ignore
pub enum Exit<A, E> {
    Success(A),         // the effect completed with A
    Failure(Cause<E>),  // the effect failed, died or was interrupted
}
```

`Exit` pairs the success type with the full failure taxonomy. It is what the run edges return:

- **`eff.block_on()`** (and `Runtime::block_on`, `Runtime::run(eff).await`) returns **`Exit<A, E>`**, so typed failures, defects and interrupts are all values you can match on — in application code and in tests alike.
- **`FiberHandle::join_future()`** resolves to the fiber's `Exit`; `FiberHandle::interrupt()` is an effect that succeeds with the interrupted fiber's `Exit`.
- **`eff.exit()`** turns any effect into one that *succeeds* with its `Exit` (Effect.ts `Effect.exit`), so you can inspect an outcome inside a larger program.

```rust
use id_effect::{Cause, Effect, Exit, fail, succeed};

let ok: Effect<i32, ()> = succeed(42);
assert_eq!(ok.block_on(), Exit::succeed(42));

// `.exit()` never fails: the outcome becomes the success value.
let seen: Effect<Exit<i32, &str>> = fail::<i32, &str, _>("no").exit();
assert_eq!(seen.block_on(), Exit::succeed(Exit::Failure(Cause::Fail("no"))));
```

## Converting Exit to Result

`Exit::into_result` gives `Result<A, Cause<E>>`; combine it with `Cause::fail_option` when you only care about typed failures:

```rust
# use id_effect::{Effect, fail};
# let program: Effect<i32, String> = fail("boom".to_owned());
match program.block_on().into_result() {
    Ok(value) => println!("got {value}"),
    Err(cause) => match cause.fail_option() {
        Some(e) => eprintln!("failed: {e:?}"),
        None => eprintln!("defect or interrupt"),
    },
}
```

`Exit` also has `map`, `map_error` (typed errors only) and `to_either` / `from_either`.

## Practical Rule

Run at the edge with `block_on` and match on the `Exit`. In tests, assert on the `Exit` directly (`assert_eq!(eff.block_on(), Exit::succeed(..))`, or `TestExecutor` for virtual time — see [Testing](../part4/ch15-00-testing.md)). At the **process edge** (binaries), use **`run_main`** / **`exit_code_for_exit`** from **`id_effect_cli`**; the mapping is in the [CLI exit codes](./ch16-01-cli-exit-codes.md) chapter.
