# `Env` — The Runtime Capability Container

Multi-capability effects use [`Env`](https://github.com/Industrial/id_effect/blob/main/crates/id_effect/src/capability/env.rs) at runtime: a map from capability identity to service value. **Insertion order does not matter**.

## Structure

```rust
use id_effect::Env;

let mut env = Env::new();
env.insert::<Cap<Database>>(pool);
env.insert::<Cap<EffectLogger>>(logger);

assert!(env.has::<Cap<Database>>());
let pool = env.get::<Cap<Database>>();
```

`Env` stores cloneable, `Send + Sync` values keyed by [`CapabilityId`](https://github.com/Industrial/id_effect/blob/main/crates/id_effect/src/capability/id.rs) (derived from the key type). Lookups are O(1); there is no positional indexing.

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
env.insert::<Cap<Database>>(MockPool::new());
```

## Why not a plain `HashMap<TypeId, Box<dyn Any>>`?

You could store `dyn Any` and downcast. `Env` + `Capability` keeps:

- **Compile-time requirements** via `Needs<K>` bounds and `caps!`
- **Typed access** — `get::<Cap<Database>>()` returns `&Pool`, not `&dyn Any`
- **Stable diagnostics** — missing capabilities produce [`CapabilityError::Missing`](https://github.com/Industrial/id_effect/blob/main/crates/id_effect/src/capability/error.rs) with the key name

Application code should think in **`Env` and capability services**, not positional tuples.

## Order independence

These two sequences produce equivalent lookup behaviour:

```rust
env.insert::<Cap<Database>>(db).insert::<Cap<EffectLogger>>(log);
// vs
env.insert::<Cap<EffectLogger>>(log).insert::<Cap<Database>>(db);
```

Adding a new capability never changes how existing keys are accessed — refactor-safe in a way tuples never were.

## When you touch `Env` directly

- Test fixtures with one or two mocks
- Tokio/async examples that pass `Env` to [`run_async`](https://github.com/Industrial/id_effect/blob/main/crates/id_effect/src/runtime/mod.rs)
- HTTP hosts that store `State<Env>` (see [Axum host](./ch07-08-axum-host.md))

Production apps usually list [`provide!(…)`](https://github.com/Industrial/id_effect/blob/main/crates/id_effect/src/capability/provider.rs) values once at the top level and let [`CapabilityGraph`](https://github.com/Industrial/id_effect/blob/main/crates/id_effect/src/capability/graph.rs) assemble `Env`.
