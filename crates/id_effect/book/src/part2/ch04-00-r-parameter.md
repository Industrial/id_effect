# The R Parameter — Your Dependencies, Encoded in Types

Chapter 1 introduced `R` as "what an effect needs to run." We kept it vague on purpose — you needed to understand effects before worrying about their environment.

For effects that use dependencies, `R` is written with [`caps!`](../../src/capability/set.rs): a compile-time list of **capability keys**. Pure effects use `R = ()`.

## What R means

```rust
use id_effect::{Effect, caps, effect, require};

fn get_user(id: u64) -> Effect<User, DbError, caps!(DatabaseKey)> {
    effect!(|r| {
        let db = ~DatabaseKey;
        Ok(db.fetch_user(id))
    })
}
```


## Implicit `|r|`

When the enclosing function already returns `Effect<_, _, caps!(…)>` , you can write `effect!(|r| { … })` and omit the environment type on `r`. Rust infers `&mut caps!(…)` from the return type. Use an explicit `|r: &mut caps!(…)|` when you want the macro to validate that body keys (`~Key`, `require!(Key)`) match that list.

Library code can stay generic over any `R` that exposes the same keys:

```rust
fn get_user<R>(id: u64) -> Effect<User, DbError, R>
where
    R: id_effect::Needs<DatabaseKey> + 'static,
{
    effect!(|r: &mut R| {
        let db = ~DatabaseKey;
        Ok(db.fetch_user(id))
    })
}
```

`R` is a *promise to the compiler*: this effect may only run where `DatabaseKey` is available. [`caps!`](../../src/capability/set.rs) documents which keys the effect touches; at runtime [`run_with`](../../src/capability/run.rs) builds an [`Env`](../../src/capability/env.rs) that satisfies them.

## How requirements are satisfied

Library code does **not** wire dependencies. Provide at the program edge with [`run_with`](../../src/capability/run.rs):

```rust
use id_effect::{provide, run_with};

run_with(
    [provide!(DatabaseLive), provide!(LoggerLive)],
    get_user(42),
)?;
```

`run_with` builds a [`CapabilityGraph`](../../src/capability/graph.rs), constructs an `Env`, and runs the effect with [`run_blocking`](../../src/runtime/mod.rs).

For tests you can skip the graph and build `Env` directly:

```rust
use id_effect::{Env, caps, run_blocking};

let mut env = Env::new();
env.insert::<DatabaseKey>(mock_db);
run_blocking(get_user(42), caps!(DatabaseKey)::from_env(env))?;
```

Or use [`build_env`](../../src/capability/run.rs) when you still want provider types but not a full app run.

The next sections cover how `R` flows through composition, how to wire dependencies at the edge, and how capability keys replace positional tuples.
