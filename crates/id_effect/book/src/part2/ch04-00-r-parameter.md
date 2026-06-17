# The R Parameter — Your Dependencies, Encoded in Types

Chapter 1 introduced `R` as "what an effect needs to run." We kept it vague on purpose — you needed to understand effects before worrying about their environment.

In **capability DI v2**, `R` is almost always [`Env`](../../src/capability/env.rs): an order-independent runtime container keyed by capability identity. Pure effects use `R = ()`.

## What R means in v2

```rust
use id_effect::{Effect, Env, Needs};

fn get_user(id: u64) -> Effect<User, DbError, Env>
where
    Env: Needs<DatabaseKey>,
{ ... }
```

Or keep the environment generic:

```rust
fn get_user<R>(id: u64) -> Effect<User, DbError, R>
where
    R: Needs<DatabaseKey> + 'static,
{ ... }
```

`R` is a *promise to the compiler*: this effect may only run where `DatabaseKey` is available. The [`caps!`](../../src/capability/set.rs) macro documents which capabilities an effect touches; at runtime every multi-capability effect still uses `Env`.

## How requirements are satisfied

v2 does **not** call `.provide()` on effects. Wire dependencies at the program edge with [`run_with`](../../src/capability/run.rs):

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
let mut env = Env::new();
env.insert::<DatabaseKey>(mock_db);
run_blocking(get_user(42), env)?;
```

Or use [`build_env`](../../src/capability/run.rs) when you still want provider types but not a full app run.

The next sections cover how `R` flows through composition, how to wire dependencies at the edge, and how capability keys replace positional tuples.
