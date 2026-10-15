# The R Parameter — Your Dependencies, Encoded in Types

Chapter 1 introduced `R` as "what an effect needs to run". This chapter makes that precise.

`R` is a **type-level list of capabilities** (an HList): `HNil` is the empty list and
`HCons<A, HCons<B, HNil>>` is "an `A` and a `B`". [`caps!`](./ch05-02-tags.md) spells it:
`caps!(A, B)` is exactly `HCons<A, HCons<B, HNil>>`. A capability is any `Send + Sync + 'static`
type; its type is its identity. Effects that need nothing have `R = HNil`, the default.

## What R means

```rust
use id_effect::{Effect, caps, effect};

fn get_user(id: u64) -> Effect<User, DbError, caps!(Database)> {
  effect!({ ~Database.fetch_user(id)? })
}
```

`R = caps!(Database)` is a promise checked by the compiler: this effect can only run where a
`Database` has been provided. Inside `effect!`, `~Database` reads it (an `&Arc<Database>`); outside
the macro, [`need::<Database>()`](./ch05-04-get-getmut.md) does the same.

Library code can also stay generic over any environment that contains the capability:

```rust
use id_effect::{Effect, HList, Selector, need};

fn get_user_in<R, I>(id: u64) -> Effect<User, DbError, R>
where
  R: HList + Selector<Database, I> + Send + Sync + 'static,
{
  need::<Database>().flat_map(move |db| db.fetch_user_effect(id))
}
```

## How requirements are satisfied

Library code does **not** wire dependencies. The program edge removes requirements from `R` until
nothing is left, then runs the effect:

```rust
use id_effect::hlist;

// One capability at a time
let exit = get_user(42).provide(Database::connect("mem://")).block_on();

// A whole environment at once (any order, extra capabilities allowed)
let exit = app().provide_env(hlist![Database::connect("mem://"), AuditLog::stdout()]).block_on();

// From a Layer graph (Chapter 6): built, memoised and released around the effect
let exit = app().provide_layer(app_layer()).block_on();
```

`block_on` (and `Runtime::run`) only accept `Effect<A, E, HNil>`, so a requirement that was never
provided is a compile error, not a runtime failure.

The next sections cover how `R` flows through composition, how to provide it, how to widen and
narrow it, and why it doubles as documentation.
