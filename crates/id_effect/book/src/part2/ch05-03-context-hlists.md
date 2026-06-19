# `Env` — The Runtime Capability Container

Multi-capability effects use [`Env`](../../src/capability/env.rs) at runtime: a map from capability identity to service value. **Insertion order does not matter**.

## Structure

```rust
use id_effect::Env;

let mut env = Env::new();
env.insert::<DatabaseKey>(pool);
env.insert::<LoggerKey>(logger);

assert!(env.has::<DatabaseKey>());
let pool = env.get::<DatabaseKey>();
```

`Env` stores cloneable, `Send + Sync` values keyed by [`CapabilityId`](../../src/capability/id.rs) (derived from the key type). Lookups are O(1); there is no positional indexing.

## Building `Env`

Three common paths:

**1. Application entry — providers + graph**

```rust
run_with([provide!(ConfigLive), provide!(DatabaseLive)], app())?;
```

**2. Providers only — reuse in tests**

```rust
let env = build_env([provide!(MockDatabaseLive)])?;
```

**3. Manual — fast unit tests**

```rust
let mut env = Env::new();
env.insert::<DatabaseKey>(MockPool::new());
```

## Why not a plain `HashMap<TypeId, Box<dyn Any>>`?

You could store `dyn Any` and downcast. `Env` + `CapabilityKey` keeps:

- **Compile-time requirements** via `Needs<K>` bounds and `caps!`
- **Typed access** — `get::<DatabaseKey>()` returns `&Pool`, not `&dyn Any`
- **Stable diagnostics** — missing capabilities produce [`CapabilityError::Missing`](../../src/capability/error.rs) with the key name

Application code should think in **`Env` and capability keys**, not positional tuples.

## Order independence

These two sequences produce equivalent lookup behaviour:

```rust
env.insert::<DatabaseKey>(db).insert::<LoggerKey>(log);
// vs
env.insert::<LoggerKey>(log).insert::<DatabaseKey>(db);
```

Adding a new capability never changes how existing keys are accessed — refactor-safe in a way tuples never were.

## When you touch `Env` directly

- Test fixtures with one or two mocks
- Tokio/async examples that pass `Env` to [`run_async`](../../src/runtime/mod.rs)
- HTTP hosts that store `State<Env>` (see [Axum host](./ch07-08-axum-host.md))

Production apps usually list [`provide!(…)`](../../src/capability/provider.rs) values once at the top level and let [`CapabilityGraph`](../../src/capability/graph.rs) assemble `Env`.
