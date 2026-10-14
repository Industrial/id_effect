# What Is a Provider?

An `Effect` describes a computation that *needs* an environment. A [`ProviderSpec`](https://github.com/Industrial/id_effect/blob/main/crates/id_effect/src/capability/provider.rs) describes *how to build* one capability and register it in [`Env`](https://github.com/Industrial/id_effect/blob/main/crates/id_effect/src/capability/env.rs).

```
Effect<User, DbError, caps!(Database)>
  └── "I need Database to produce a User"

ProviderSpec for DatabaseLive
  └── "I need Config (via requires) to produce Database"
```

Effects declare needs; providers declare construction.

## The `ProviderSpec` trait

```rust
use id_effect::{CapabilityId, Env, ProviderError, ProviderSpec};

struct DatabaseLive;

impl ProviderSpec for DatabaseLive {
    type Key = Database;
    type Output = Pool;

    fn provider_id() -> &'static str { "database-live" }

    fn requires() -> &'static [CapabilityId] {
        // Declare Config as a dependency for graph ordering
        // (return a static slice of CapabilityId values)
        &[]
    }

    fn provide(deps: &Env) -> Result<Pool, ProviderError> {
        let config = deps.get::<Cap<Config>>();
        Ok(connect_pool(config.db_url()))
    }
}
```

- **`type Key`** — which capability this provider registers
- **`type Output`** — the concrete value stored in `Env`
- **`provide(deps)`** — build the value; read dependencies with `deps.get::<Cap<K>>()`
- **`requires()`** — capability ids this provider needs built first (used by [`CapabilityGraph`](https://github.com/Industrial/id_effect/blob/main/crates/id_effect/src/capability/graph.rs))

## Providers are lazy recipes

Implementing `ProviderSpec` does nothing by itself. Construction runs when you call [`run_with`](https://github.com/Industrial/id_effect/blob/main/crates/id_effect/src/capability/run.rs), [`build_env`](https://github.com/Industrial/id_effect/blob/main/crates/id_effect/src/capability/run.rs), or [`CapabilityGraph::build`](https://github.com/Industrial/id_effect/blob/main/crates/id_effect/src/capability/graph.rs).

## The key insight

- **Effects** are programs — *what to do* with capabilities
- **Providers** are constructors — *how to build* capabilities

[`provide!(DatabaseLive)`](https://github.com/Industrial/id_effect/blob/main/crates/id_effect/src/capability/provider.rs) registers the recipe; [`run_with`](https://github.com/Industrial/id_effect/blob/main/crates/id_effect/src/capability/run.rs) executes the graph and passes the resulting `Env` to your effect.

## Lifecycle and cleanup

Providers run synchronously at startup. For resources with async teardown, build handles in `provide` and register finalizers in effect [`Scope`](https://github.com/Industrial/id_effect/blob/main/crates/id_effect/src/resource/scope.rs) code (see Chapter 10). The provider graph itself focuses on *construction order*, not fiber lifetimes.
