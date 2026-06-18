# Capability Keys — `#[capability]`

A **capability key** is a zero-sized type that identifies a service in [`Env`](../../src/capability/env.rs). The [`#[capability]`](../../src/capability/key.rs) attribute generates the key and wires it to a stored value type.

## Declaring a key

```rust
// Key + concrete value type
#[::id_effect::capability(Counter)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Counter(pub u32);

// Key for a trait object (typical for services)
#[::id_effect::capability(Pool)]
struct Database;

#[::id_effect::capability(Pool)]
struct Cache; // same Pool, different identity

#[::id_effect::capability(Arc<dyn UserRepository>)]
struct UserRepo;
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
fn needs_database() -> Effect<A, E, caps!(DatabaseKey)> { ... }
fn needs_cache() -> Effect<A, E, caps!(CacheKey)> { ... }
```

Providing the wrong key is a type error at the call site, not a silent runtime swap.

## Service traits

Define a focused trait, then a key for its handle:

```rust
pub trait UserRepository: Send + Sync {
    fn get_user(&self, id: u64) -> Effect<User, DbError, ()>;
}

#[::id_effect::capability(Arc<dyn UserRepository>)]
struct UserRepo;
```

Trait methods keep `R = ()` — the *caller* carries `DatabaseKey` / `UserRepoKey` in its `caps!` list.

## Summary

| Item | Role |
|------|------|
| `#[capability(V)]` on a struct | Generate `StructKey: CapabilityKey` with `Value = V` |
| `Env::insert::<K>(v)` | Register a service |
| `Needs<K>` | Bound: environment contains `K` |
| `K::Value` | Concrete type stored for `K` |

Keys eliminate the positional problem. The next section introduces `Env` — how those keys are stored at runtime.
