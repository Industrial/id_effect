# Widening and Narrowing — Environment Transformations

Your effect needs *part* of an environment, but the caller has the whole thing. Or a step cannot
fail, but the block it joins can. Typed `R` handles both with a few operations.

## The mismatch

```rust
// Needs only AuditLog
fn audit(msg: String) -> Effect<(), AppError, caps!(AuditLog)> { /* ... */ }

// The caller has Database, AuditLog and Config
fn process(data: Data) -> Effect<(), AppError, caps!(Database, AuditLog, Config)> { /* ... */ }
```

`caps!(AuditLog)` and `caps!(Database, AuditLog, Config)` are different types, so `audit(..)` has
to be lifted into the wider environment before it can be sequenced with the caller's other steps.

## Inside effect!: `~` narrows for you

A `~` bind lifts the sub-effect into the block's environment automatically, as long as every
capability it needs is in the block's `R`:

```rust
fn process(data: Data) -> Effect<(), AppError, caps!(Database, AuditLog, Config)> {
  effect!({
    ~audit("start".into());  // caps!(AuditLog) lifted into the block's R
    ~store(data);            // caps!(Database) lifted too
  })
}
```

This is the common case, and it needs no annotations.

## Outside effect!: `narrow`

With combinators, lift explicitly with `.narrow()`. It turns an `Effect<A, E, R>` into an
`Effect<A, E, Wide>` for any `Wide` that contains every capability of `R` (any order, extras
allowed):

```rust
use id_effect::{Effect, HCons, HNil, hlist, need};

struct Db(u32);
struct Logger(&'static str);
struct Cache(u8);

type DbLog = HCons<Db, HCons<Logger, HNil>>;
type Wide = HCons<Cache, HCons<Logger, HCons<Db, HNil>>>;

let small: Effect<u32, (), DbLog> = need::<Db>().map(|d| d.0);
let wide: Effect<u32, (), Wide> = small.narrow();

let exit = wide.provide_all(hlist![Cache(0), Logger("w"), Db(4)]).block_on();
```

`narrow` into the same environment is free; otherwise the sub-environment is projected when the
effect runs (one `Arc` clone per capability). It is what makes this compose:

```rust
let both: Effect<(u32, String), (), Wide> = small_effect().narrow().zip(other_effect().narrow());
```

## Shrinking R: provide

The other direction, removing requirements, is [providing](./ch04-02-providing.md):
`.provide(t)` removes one capability, `.provide_some(hlist![..])` a sub-list. Providing in the
middle of a program is how you scope a capability to one region:

```rust
// `Db(1)` is visible only inside `inner`; the outer environment is restored afterwards.
let outer: Effect<u32, (), HCons<Logger, HNil>> = inner().provide(Db(1)).narrow();
```

## Widening the error: `widen`

`Effect<A, Never, R>` cannot fail. `.widen()` gives it any error type, so it can be sequenced with
fallible effects outside `effect!` (inside `effect!`, `~` widens `Never` on its own):

```rust
let child: Effect<u8, ()> = succeed(5);
// `fork` cannot fail; `widen` lets it flat_map into the fallible `join`.
let program: Effect<u8, ()> = child.fork().widen().flat_map(|h| h.join());
```

For a fallible error type, `.map_err(Into::into)` widens through `From`.

## Generic helpers

A helper generic over `R` (with a `Selector` bound) fits any environment that has its capability.
`~` cannot infer such an `R` on its own; bind it with `.same_env()`, which fixes it to the
caller's environment:

```rust
fn audit<R, I>(msg: String) -> Effect<(), AppError, R>
where
  R: HList + Selector<Logger, I>,
{
  effect!({ ~Logger.info(msg) })
}

fn signup() -> Effect<(), AppError, caps!(Logger, Db)> {
  effect!({ ~audit("signup".into()).same_env() })
}
```

## Summary

| Operation | Effect on `R` / `E` |
|-----------|---------------------|
| `~sub` in `effect!` | lifts `sub` into the block's `R`, widens its error |
| `.narrow::<Wide, _>()` | `R` → any `Wide` containing `R` |
| `.provide(t)` / `.provide_some(..)` | removes capabilities from `R` |
| `.widen()` | `Never` → any `E` |
| `.map_err(Into::into)` | `E2` → `E` through `From` |
| `.same_env()` | fixes a generic helper's `R` to the caller's |
