# Capability services

A **capability service** is a Rust type that names a dependency in [`Env`](../../src/capability/env.rs). The [`Cap<T>`](../../src/capability/key.rs) wrapper implements [`CapabilityKey`](../../src/capability/key.rs) for any cloneable `T`, so you use the service type directly in `caps!`, `require!`, and `#[provides]`.

## Declaring a service

```rust
// Concrete value type
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Counter(pub u32);

// Trait-backed service (typical for ports/adapters)
pub type Database = Arc<dyn DbClient>;
pub type UserRepo = Arc<dyn UserRepository>;
```

`Database` and `Cache` can both wrap a `Pool` but remain **distinct capability identities** — you cannot pass one where the other is required.

## Registering values

At runtime, values live in `Env`:

```rust
use id_effect::{Cap, Env};

let mut env = Env::new();
env.insert::<Cap<Database>>(main_pool);
env.insert::<Cap<Cache>>(cache_pool);
```

Or let a [`ProviderSpec`](../../src/capability/provider.rs) insert them during `run_with`.

## Why named services help the compiler

```rust
fn needs_database() -> Effect<A, E, caps!(Database)> { ... }
fn needs_cache() -> Effect<A, E, caps!(Cache)> { ... }
```

Providing the wrong service is a type error at the call site, not a silent runtime swap.

## Service traits

Define a focused trait, then a type alias for the handle stored in `Env`:

```rust
pub trait UserRepository: Send + Sync {
    fn get_user(&self, id: u64) -> Effect<User, DbError, ()>;
}
pub type UserRepo = Arc<dyn UserRepository>;
```

Trait methods keep `R = ()` — the *caller* carries `Database` / `UserRepo` in its `caps!` list.

## Summary

| Item | Role |
|------|------|
| Service type `T` | Name used in `caps!(T)` and `require!(T)` |
| `Cap<T>` | Internal `CapabilityKey` with `Value = T` |
| `Env::insert::<Cap<T>>(v)` | Register a service |
| `Needs<T>` | Bound: environment contains `T` |

Named services eliminate the positional problem. The next section introduces `Env` — how those services are stored at runtime.
