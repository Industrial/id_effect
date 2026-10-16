# When Not to Use the Macro

`effect!` is the idiomatic choice for most multi-step computations, but it is a macro, and it has
edges. Knowing when to reach for a combinator instead saves debugging time.

## Single-step transforms

```rust
# use id_effect::{Effect, effect, succeed};
# struct UserId(u64);
# fn parse_id(_: &str) -> Effect<u64, String> { succeed(7) }
# let raw = "7";
// Unnecessarily verbose
# let _: Effect<u64, String> =
effect!({ ~parse_id(raw) })
# ;

// Clear and direct
# let _ =
parse_id(raw)
# ;
# let _ =
parse_id(raw).map(UserId)
# ;
```

Use `effect!` for two or more steps; for one, `map` / `flat_map` is enough.

## Named combinators for structural patterns

```rust
# use id_effect::{Effect, effect, succeed, zip_par};
# fn step_a() -> Effect<u32, String> { succeed(1) }
# fn step_b() -> Effect<u32, String> { succeed(2) }
# type Pair = Effect<(u32, u32), String>;
// Sequential, even though the steps are independent:
# let _: Pair =
effect!({
  let a = ~step_a();
  let b = ~step_b();
  (a, b)
})
# ;

// Says what you mean:
# let _: Pair =
step_a().zip(step_b())       // sequential pair
# ;
# let _: Pair =
zip_par(step_a(), step_b())  // concurrent pair
# ;
```

For concurrent independent steps inside a block, `~(step_a(), step_b())` is the same join.
`race`, `timeout`, `all` and `for_each_par` are combinators too
([Chapter 9](../part3/ch09-00-concurrency.md)).

## Avoid nesting effect! inside effect!

```rust,compile_fail
# use id_effect::{Effect, effect, succeed};
# fn inner_step() -> Effect<i32, String> { succeed(1) }
// CONFUSING: nested macro bodies. It does not even compile: the inner `effect!` has no
// expected type, so its error type is not the outer one and `String` cannot widen into it.
# let _: Effect<i32, String> =
effect!({
  let result = ~effect!({ let x = ~inner_step(); x * 2 });
  result + 1
})
# ;
```

Flatten instead:

```rust
# use id_effect::{Effect, effect, succeed};
# fn inner_step() -> Effect<i32, String> { succeed(1) }
// BETTER: flatten
# let _: Effect<i32, String> =
effect!({
  let x = ~inner_step();
  x * 2 + 1
})
# ;
```

## The macro and type inference

`effect!({ .. })` takes `E` and `R` from the **expected type**. If inference fails inside a block:

1. give the enclosing function an explicit return type (`-> Effect<A, E, caps!(..)>`);
2. bind typed helpers rather than bare constructors (`~number(1)`, not `~succeed(1)`);
3. pin `R` with the closure form, `effect!(|r: &caps!(Greeting, Counter)| { .. })`, which also
   binds `r` to the environment;
4. bind a helper that is generic in `R` with `.same_env()` (see [The ~ Operator](./ch03-02-bind-operator.md));
5. as a last resort, move the inner logic into a named helper function.

## Generic code over R

Library code that should work in any environment containing its capability is generic over `R`
with a `Selector` bound, and uses `need` instead of the macro where that is simpler:

```rust
use id_effect::{Effect, HList, Selector, need};
# #[derive(Clone, Copy)] struct UserId(u64);
# struct AuthError;
# struct Session { user_id: UserId }

pub fn current_user<R, I>() -> Effect<UserId, AuthError, R>
where
  R: HList + Selector<Session, I> + Send + Sync + 'static,
{
  need::<Session>().map(|s| s.user_id)
}
```

## `!Send` state

The body of `effect!` must be `Send`. To hold an `Rc` or a `RefCell` across a bind, use
`effect_local!`, which builds a `LocalEffect` (run it with its own `block_on`, or `run_on_local`
on a local worker); a `LocalEffect` body can still bind ordinary `Send` effects.

## Summary

| Situation | Prefer |
|-----------|--------|
| 2+ sequential steps | `effect!({ ~ .. })` |
| 1 step, simple transform | `.map` / `.flat_map` |
| Independent steps | `.zip`, `zip_par`, `~(a, b)` |
| Generic over `R` | `need::<T>()` with a `Selector` bound, or `.same_env()` |
| `!Send` body | `effect_local!` |
