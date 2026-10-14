# `Needs` and `~Key` — Reading from `Env`

To use a capability inside an effect, declare required keys in `R` with [`caps!`](https://github.com/Industrial/id_effect/blob/main/crates/id_effect/src/capability/set.rs) and borrow with `~Key`, [`require!`](https://github.com/Industrial/id_effect/blob/main/crates/id_effect/src/capability/run.rs), or `Needs::need`.

## Declaring requirements with `caps!`

Put the keys your effect needs in the third type parameter:

```rust
fn get_user(id: u64) -> Effect<User, DbError, caps!(Database)> { ... }
fn notify_user(id: u64, msg: &str) -> Effect<(), AppError, caps!(UserRepo, EmailService)> { ... }
```

Library helpers can stay generic when callers supply a wider `Env`:

```rust
fn get_user<R>(id: u64) -> Effect<User, DbError, R>
where
    R: id_effect::Needs<Database> + 'static,
{
    effect!(|r: &mut R| {
        let db = ~Database;
        db.fetch_user(id)
    })
}
```

## `~Key` — capability lookup inside effect bodies

```rust
use id_effect::{effect, require, caps};

fn get_user(id: u64) -> Effect<User, DbError, caps!(Database)> {
    effect!(|r| {
        let db = ~Database;
        db.fetch_user(id)
    })
}
```

`~Key` inside `effect!` expands to a typed borrow from `r`. `require!(K)` is equivalent sugar.

If `get_user` requires `Database` but you run it with an empty `Env`, you get a **runtime** missing-capability error when the effect executes — not a silent `None`. For static verification, keep `caps!(…)` or `Needs<K>` bounds on public APIs so callers must wire providers before `run_with`.

When building tests manually:

```rust
let mut env = Env::new();
// forgot env.insert::<Cap<Database>>(...)
run_blocking(get_user(42), env); // panics on ~Key / get
```

The lookups are `#[track_caller]`, so the panic points at **your** code, not at `id_effect` internals:

```text
missing capability `my_app::Database` (required at src/users.rs:14:19)
  hint: register a provider with `provide!(…)` in `run_with` / `build_env`, and list the service in the effect's `caps!(…)` so `run_with` verifies it up front
```

`Env::try_get` returns the same information as [`CapabilityError::MissingAt`](https://github.com/Industrial/id_effect/blob/main/crates/id_effect/src/capability/error.rs) (`err.location()` is the caller's `file:line:column`), and binding a narrower effect with `~inner()` names the `~` call site when the wider environment lacks a key. `run_with` reports an unprovided service before the program starts as `RunError::Capability(CapabilityError::Missing(…))`, displayed as ``missing capability `my_app::Database` ``.

Compile-time mistakes get targeted messages too — for example, looking up a service in an effect whose environment is `()`:

```text
error[E0277]: environment `()` cannot provide capability `Database`
  = note: run the effect in a capability environment: `Effect<_, _, caps!(Database)>` (or `Env`), not `()`
```

Prefer `build_env` or typed test helpers so incomplete wiring fails at setup time.

## Summary

| Tool | Use |
|------|-----|
| `caps!(K1, K2, …)` | Declare dependencies in `R` |
| `~Key` / `require!(K)` | Borrow inside `effect!` |
| `env.get::<Cap<K>>()` | Direct access when you hold `&Env` |
| `env.try_get::<Cap<K>>()` | Fallible lookup without panic |

An application that satisfies all `Needs` bounds at the edge and passes a complete provider list to `run_with` is an application where every dependency is explicit — no service-locator globals.
