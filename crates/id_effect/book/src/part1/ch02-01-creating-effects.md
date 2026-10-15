# Creating Effects — succeed, fail, sync and from_async

Every effect starts from a constructor. The constructors are free functions at the crate root.

## succeed

`succeed` wraps a value into an effect that, when run, produces that value:

```rust
use id_effect::{Effect, succeed};

let answer: Effect<i32, String> = succeed(42);
let greeting: Effect<String> = succeed("Hello, world!".to_string());
```

The type parameters:

- `A = i32`: the value we produce;
- `E = String`: the error type (unused here, but chosen by the annotation). Leave it out and it
  is `Never`: the effect cannot fail;
- `R = HNil` (the default): no environment needed.

## fail

`fail` builds an effect that, when run, fails with a typed error:

```rust
use id_effect::{Cause, Effect, Exit, fail};

let oops: Effect<i32, &'static str> = fail("not_ready");
assert_eq!(oops.block_on(), Exit::Failure(Cause::Fail("not_ready")));
```

`die(defect)` is the third outcome: an unexpected failure (a *defect*), reported as
`Cause::Die`. A panic inside an effect becomes a defect too.

## From a closure: sync and suspend

`sync` defers a synchronous computation until the effect runs:

```rust
use id_effect::{Effect, sync};

let computed: Effect<i32> = sync(|| expensive_calculation() * 2);
```

`suspend` defers *building* an effect, which is handy when the effect to run depends on state read
at run time:

```rust
use id_effect::{Effect, fail, succeed, suspend};

let checked: Effect<u8, &'static str> = suspend(|| {
  if feature_enabled() { succeed(1) } else { fail("disabled") }
});
```

## From async code: from_async

`from_async` wraps an `async` block that returns a `Result<A, E>`:

```rust
use id_effect::{Effect, Exit, from_async};

let eff: Effect<u8, ()> = from_async(|| async move {
  core::future::ready(()).await;
  Ok(42)
});
assert_eq!(eff.block_on(), Exit::succeed(42));
```

The closure is called when the effect runs, so nothing starts early.

## The effect! macro

`effect!({ .. })` builds an effect from an async-style body; `~expr` inside it runs a sub-effect.
It is the workhorse of the library, covered in [Chapter 3](./ch03-00-effect-macro.md):

```rust
use id_effect::{Effect, effect, succeed};

fn number(n: i32) -> Effect<i32> {
  succeed(n)
}

let sum: Effect<i32> = effect!({
  let a = ~number(40);
  let b = ~number(2);
  a + b
});
```

A bound sub-effect needs a known error and environment type, which is why the examples bind small
typed helpers such as `number` rather than a bare `succeed(..)`.

## Type Inference

```rust
let answer = succeed(42);    // Effect<i32, _, _>: E and R come from how it is used
let greeting = succeed("hi"); // Effect<&str, _, _>
```

`E` and `R` are usually inferred from the surrounding code (a function's return type, the effect it
is combined with). Annotate when the compiler asks.

## Quick Reference

```rust
succeed(value)           // produces value
fail(error)              // fails with a typed error
die(defect)              // fails with a defect
sync(|| value)           // runs a closure when run
suspend(|| effect)       // builds the effect when run
from_async(|| async {..}) // wraps an async block returning Result<A, E>
effect!({ .. })          // do-notation (Chapter 3)
```
