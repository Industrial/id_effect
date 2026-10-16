# Reading Capabilities — `~Cap`, `need` and `Ctx`

Declare what an effect needs in `R` with `caps!`, then read it in one of three ways. All three are
checked at compile time against `R`.

## `~Cap` inside `effect!`

```rust
use id_effect::{Effect, Exit, Never, caps, effect};

#[derive(Debug)]
struct Greeting(&'static str);

fn answer() -> Effect<u32> {
  id_effect::succeed(42)
}

fn hello() -> Effect<String, Never, caps!(Greeting)> {
  effect!({ format!("{}:{}", ~Greeting.0, ~answer()) })
}

let out = hello().provide(Greeting("hi")).block_on();
assert!(matches!(out, Exit::Success(ref s) if s == "hi:42"));
```

`~Greeting` is an `&Arc<Greeting>` borrowed from the environment; what follows it (`.0`, a method
call) is plain Rust on that reference. The closure form names the whole environment:
`effect!(|r: &caps!(Greeting, Counter)| { .. })` binds `r` and fixes `R`.

## `need::<T>()` outside the macro

`need::<T>()` builds a lookup that you finish with `.effect()`, `.map(..)` or `.flat_map(..)`:

```rust
use std::sync::Arc;
use id_effect::{Effect, HCons, HNil, hlist, need};

struct Db(u32);
struct Logger(&'static str);
type DbLog = HCons<Db, HCons<Logger, HNil>>;

let db: Effect<Arc<Db>, (), DbLog> = need::<Db>().effect();
let len: Effect<u32, (), DbLog> = need::<Logger>().map(|l| l.0.len() as u32);
let sum: Effect<u32, (), DbLog> =
  need::<Db>().flat_map(|db| need::<Logger>().map(move |l| db.0 + l.0.len() as u32));

let exit = sum.provide_all(hlist![Db(10), Logger("ab")]).block_on(); // Success(12)
```

`need` is the tool for library code that is generic over its environment:

```rust
use id_effect::{Effect, HList, Selector, need};
# struct AppError;
# struct Logger;
# impl Logger { fn info(&self, _line: String) {} }

fn audit<R, I>(line: String) -> Effect<(), AppError, R>
where
  R: HList + Selector<Logger, I> + Send + Sync + 'static,
{
  need::<Logger>().map(move |log| log.info(line))
}
```

## `Ctx` in hand-written async steps

`Effect::from_ctx` gives an async step direct access to its context: `ctx.get::<T, _>()` reads a
capability and `ctx.env()` is the whole environment.

```rust
# use id_effect::{Effect, Exit, HCons, HNil, hlist};
# struct Db(u32);
# struct Logger(&'static str);
# type DbLog = HCons<Db, HCons<Logger, HNil>>;
fn describe() -> Effect<String, (), DbLog> {
  Effect::from_ctx(|ctx| async move {
    Ok(format!("{}:{}", ctx.get::<Db, _>().0, ctx.get::<Logger, _>().0))
  })
}
# let exit = describe().provide_all(hlist![Db(1), Logger("app")]).block_on();
# assert_eq!(exit, Exit::succeed("1:app".to_string()));
```

## No `get_mut`

Capabilities are shared `Arc`s, so there is no mutable lookup. State that changes lives behind
interior mutability inside the capability: a `Mutex`, an atomic, or an effect-aware `Ref` /
`SynchronizedRef` from the coordination module.

## When a capability is missing

A missing capability never reaches run time with a typed `R`:

```text
error[E0277]: capability `Metrics` is not in this effect's environment
  --> src/handler.rs:14:14
   |
14 |     let m = ~Metrics;
   |              ^^^^^^^ `Metrics` is required here
   = note: add `Metrics` to the effect's `caps!(..)`, or remove it with `.provide(..)` / `.provide_env(..)` before running
```

Only `DynEnv` lookups (`try_need`) can fail at run time, and they fail with a typed
`MissingCapability` error rather than a panic.

## Summary

| Tool | Use |
|------|-----|
| `caps!(K1, K2, ..)` | declare dependencies in `R` |
| `~Cap` | read inside `effect!` |
| `need::<Cap>()` | read in combinator code or generic helpers |
| `ctx.get::<Cap, _>()` | read inside `Effect::from_ctx` |
| `try_need::<Cap>()` | fallible lookup in a `DynEnv` |
