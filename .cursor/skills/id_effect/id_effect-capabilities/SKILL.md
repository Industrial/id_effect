---
name: id_effect-capabilities
description: >-
  Expert in id_effect 3.0 capability DI: #[capability], caps!, Env, Needs, ProviderSpec,
  provide!, run_with, provider graphs, and service trait design. Use when wiring dependencies,
  declaring services, composing providers, or migrating from Layer/Stack/ctx! APIs.
---

# id_effect Capabilities (DI)

**Part II** of the book. id_effect 3.0 uses **capability keys + providers**, not Layers/Stack.

**Prerequisite**: `id_effect-fundamentals`.

## Core table

| Do | Pattern |
|----|---------|
| Declare key | `#[::id_effect::capability(T)] struct Name;` → `NameKey` |
| Declare provider | `#[derive(ProviderSpecDerive)]` + `#[provides(NameKey)]` |
| Typed requirements | `Effect<_, _, caps!(K1, K2)>` |
| Access in `effect!` | `~NameKey` or `require!(NameKey)` with `\|r\|` |
| Access outside macro | `Needs::<NameKey>::need(env)` |
| Wire at edge | `run_with([provide!(Live), …], effect)` |
| Build env | `build_env([provide!(…), …])?` |
| Test doubles | `mock_capability!` or `env.insert::<Key>(value)` |

## Declaring keys

```rust
#[::id_effect::capability(Counter)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Counter(pub u32);

#[::id_effect::capability(Arc<dyn UserRepository>)]
struct UserRepo;
```

Each key is a **distinct type** — `DatabaseKey` ≠ `CacheKey` even when `Value` is the same.

## Service traits

Trait methods keep **`R = ()`**. Callers carry keys in **`caps!(…)`**:

```rust
pub trait UserRepository: Send + Sync {
    fn get_user(&self, id: u64) -> Effect<User, DbError, ()>;
}
```

Dependencies are resolved **inside the Live provider**, not leaked into the trait signature.

## ProviderSpec

```rust
#[derive(::id_effect::ProviderSpecDerive)]
#[provides(UserRepoKey)]
struct UserRepoLive;

impl UserRepoLive {
    fn new(deps: &Env) -> Arc<dyn UserRepository> {
        let pool = deps.get::<DatabaseKey>().clone();
        Arc::new(PostgresUserRepository { pool })
    }
}
```

## Application wiring

```rust
run_with(
    [provide!(ConfigLive), provide!(DatabaseLive), provide!(UserRepoLive)],
    get_user_profile(42),
)?;
```

## Not this → but that

| Not this (removed / wrong) | But that (3.0) |
|----------------------------|----------------|
| `define_capability!`, `service_key!` | `#[capability(T)] struct Name;` |
| `ctx!`, `req!` | `caps!(K1, K2)`, `~Key`, `require!(Key)` |
| `Layer` / `Stack`, `Effect::provide` | `ProviderSpec`, `provide!`, `run_with` |
| Positional `(Db, Logger)` as `R` | `caps!(DatabaseKey, LoggerKey)` |

## Requirement leakage (avoid)

```rust
// BAD — trait method exposes caller's R
fn get_user(&self, id: u64) -> Effect<User, E, caps!(DatabaseKey)>;

// GOOD — trait method R = (); caller carries DatabaseKey
fn get_user(&self, id: u64) -> Effect<User, E, ()>;
```

## Next

- Integration crates: [id_effect-integration](../id_effect-integration/SKILL.md)
- Testing mocks: [id_effect-testing](../id_effect-testing/SKILL.md)
