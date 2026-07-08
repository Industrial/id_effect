---
name: id_effect-capabilities
description: >-
  Expert in id_effect 3.0 capability DI: Cap<T>, caps!, Env, Needs, ProviderSpec,
  provide!, run_with, provider graphs, and service trait design. Use when wiring dependencies,
  declaring services, composing providers, or migrating from Layer/Stack/ctx! APIs.
---

# id_effect Capabilities (DI)

**Part II** of the book. id_effect 3.0 uses **service-named capabilities (`Cap<T>`) + providers**, not Layers/Stack.

**Prerequisite**: `id_effect-fundamentals`.

## Core table

| Do | Pattern |
|----|---------|
| Declare service | `struct Counter(u32);` or `type HttpClientService = Arc<dyn HttpClient>;` |
| Declare provider | `#[derive(ProviderSpecDerive)]` + `#[provides(Counter)]` |
| Typed requirements | `Effect<_, _, caps!(K1, K2)>` |
| Access in `effect!` | `~Counter` or `require!(Counter)` with `\|r\|` |
| Access outside macro | `Needs::<Counter>::need(env)` |
| Wire at edge | `run_with([provide!(Live), …], effect)` |
| Build env | `build_env([provide!(…), …])?` |
| Test doubles | `mock_capability!` or `env.insert::<Cap<Counter>>(value)` |

## Declaring services

```rust
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Counter(pub u32);
pub type UserRepo = Arc<dyn UserRepository>;
```

Each service is a **distinct type** — `Database` ≠ `Cache` even when the stored value type is the same.

## Service traits

Trait methods keep **`R = ()`**. Callers carry services in **`caps!(…)`**:

```rust
pub trait UserRepository: Send + Sync {
    fn get_user(&self, id: u64) -> Effect<User, DbError, ()>;
}
```

Dependencies are resolved **inside the Live provider**, not leaked into the trait signature.

## ProviderSpec

```rust
#[derive(::id_effect::ProviderSpecDerive)]
#[provides(UserRepo)]
struct UserRepoLive;

impl UserRepoLive {
    fn new(deps: &Env) -> UserRepo {
        let pool = deps.get::<Cap<Database>>().clone();
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
| `service_key!`, `ctx!`, `req!` | `caps!(Service)`, `~Service`, `require!(Service)` |
| `Layer` / `Stack`, `Effect::provide` | `ProviderSpec`, `provide!`, `run_with` |
| `#[capability]` / `*Key` types | service type `T` + `Cap<T>` |
| Positional `(Db, Logger)` as `R` | `caps!(Database, EffectLogger)` |

## Requirement leakage (avoid)

```rust
// BAD — trait method exposes caller's R
fn get_user(&self, id: u64) -> Effect<User, E, caps!(Database)>;

// GOOD — trait method R = (); caller carries Database
fn get_user(&self, id: u64) -> Effect<User, E, ()>;
```

## Next

- Integration crates: [id_effect-integration](../id_effect-integration/SKILL.md)
- Testing mocks: [id_effect-testing](../id_effect-testing/SKILL.md)
