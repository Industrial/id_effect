# Capability Keys — `define_capability!`

A **capability key** is a zero-sized type that identifies a service in [`Env`](../../src/capability/env.rs). [`define_capability!`](../../src/capability/key.rs) generates the key and wires it to a stored value type.

## Declaring a key

```rust
use id_effect::define_capability;

// Key + concrete value type
define_capability!(CounterKey, Counter);

// Key for a trait object (typical for services)
define_capability!(DatabaseKey, Pool);
define_capability!(CacheKey, Pool);      // same Pool, different identity
define_capability!(UserRepoKey, Arc<dyn UserRepository>);
```

Each key implements [`CapabilityKey`](../../src/capability/key.rs) with an associated `Value` type. `DatabaseKey` and `CacheKey` both store `Pool` but are unrelated types — you cannot pass one where the other is expected.

## Registering values

At runtime, values live in `Env`:

```rust
use id_effect::Env;

let mut env = Env::new();
env.insert::<DatabaseKey>(main_pool);
env.insert::<CacheKey>(cache_pool);
```

Or let a [`ProviderSpec`](../../src/capability/provider.rs) insert them during `run_with`.

## Why keys help the compiler

```rust
fn needs_database<R: Needs<DatabaseKey>>() -> Effect<A, E, R> { ... }
fn needs_cache<R: Needs<CacheKey>>() -> Effect<A, E, R> { ... }
```

Providing the wrong key is a type error at the call site, not a silent runtime swap.

## Service traits

Define a focused trait, then a key for its handle:

```rust
pub trait UserRepository: Send + Sync {
    fn get_user(&self, id: u64) -> Effect<User, DbError, ()>;
}

define_capability!(UserRepoKey, Arc<dyn UserRepository>);
```

Trait methods keep `R = ()` — the *caller* carries `Needs<UserRepoKey>` in its environment.

## Summary

| Item | Role |
|------|------|
| `define_capability!(K, V)` | Generate `K: CapabilityKey` with `Value = V` |
| `Env::insert::<K>(v)` | Register a service |
| `Needs<K>` | Bound: environment contains `K` |
| `K::Value` | Concrete type stored for `K` |

Keys eliminate the positional problem. The next section introduces `Env` — how those keys are stored at runtime.
