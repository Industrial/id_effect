# What Is a Provider?

An `Effect` describes a computation that *needs* an environment. A [`ProviderSpec`](../../src/capability/provider.rs) describes *how to build* one capability and register it in [`Env`](../../src/capability/env.rs).

```
Effect<User, DbError, Env>  where Env: Needs<DatabaseKey>
  └── "I need DatabaseKey to produce a User"

ProviderSpec for DatabaseLive
  └── "I need ConfigKey (via requires) to produce DatabaseKey"
```

Effects declare needs; providers declare construction.

## The `ProviderSpec` trait

```rust
use id_effect::{CapabilityId, Env, ProviderError, ProviderSpec};

struct DatabaseLive;

impl ProviderSpec for DatabaseLive {
    type Key = DatabaseKey;
    type Output = Pool;

    fn provider_id() -> &'static str { "database-live" }

    fn requires() -> &'static [CapabilityId] {
        // Declare ConfigKey as a dependency for graph ordering
        // (return a static slice of CapabilityId values)
        &[]
    }

    fn provide(deps: &Env) -> Result<Pool, ProviderError> {
        let config = deps.get::<ConfigKey>();
        Ok(connect_pool(config.db_url()))
    }
}
```

- **`type Key`** — which capability this provider registers
- **`type Output`** — the concrete value stored in `Env`
- **`provide(deps)`** — build the value; read dependencies with `deps.get::<K>()`
- **`requires()`** — capability ids this provider needs built first (used by [`CapabilityGraph`](../../src/capability/graph.rs))

## Providers are lazy recipes

Implementing `ProviderSpec` does nothing by itself. Construction runs when you call [`run_with`](../../src/capability/run.rs), [`build_env`](../../src/capability/run.rs), or [`CapabilityGraph::build`](../../src/capability/graph.rs).

## The key insight

- **Effects** are programs — *what to do* with capabilities
- **Providers** are constructors — *how to build* capabilities

[`provide!(DatabaseLive)`](../../src/capability/provider.rs) registers the recipe; [`run_with`](../../src/capability/run.rs) executes the graph and passes the resulting `Env` to your effect.

## Lifecycle and cleanup

Providers run synchronously at startup. For resources with async teardown, build handles in `provide` and register finalizers in effect [`Scope`](../../src/resource/scope.rs) code (see Chapter 10). The provider graph itself focuses on *construction order*, not fiber lifetimes.
