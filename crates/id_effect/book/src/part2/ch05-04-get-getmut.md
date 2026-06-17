# `Needs` and `require!` — Reading from `Env`

To use a capability inside an effect, bound `R` with [`Needs<K>`](../../src/capability/needs.rs) and borrow with [`require!`](../../src/capability/run.rs) or `Needs::need`.

## `Needs<K>` — the trait bound

```rust
use id_effect::{Effect, Env, Needs};

fn use_database<R>(env: &R) -> &Pool
where
    R: Needs<DatabaseKey>,
{
    Needs::<DatabaseKey>::need(env)
}
```

For [`Env`](../../src/capability/env.rs), `Needs<K>::need` delegates to `env.get::<K>()`. Effect signatures typically write:

```rust
fn get_user<R>(id: u64) -> Effect<User, DbError, R>
where
    R: Needs<DatabaseKey> + 'static,
{ ... }
```

Compose requirements with `+`:

```rust
fn notify_user<R>(id: u64, msg: &str) -> Effect<(), AppError, R>
where
    R: Needs<UserRepoKey> + Needs<EmailServiceKey> + 'static,
{ ... }
```

## `require!` — inside effect bodies

```rust
use id_effect::{effect, require};

fn get_user(id: u64) -> Effect<User, DbError, Env> {
    effect!(|env: &mut Env| {
        let db = require!(env, DatabaseKey);
        ~ db.fetch_user(id)
    })
}
```

With [`Effect::new`](../../src/kernel/effect.rs) closures:

```rust
Effect::new(|env: &mut Env| {
    let db = require!(env, DatabaseKey);
    db.fetch_user(id)
})
```

`require!` expands to `Needs::<K>::need(env)` — the v2 replacement for tag-based `Get` + `~ ServiceKey` lookup.

## Compile-time guarantees

If `get_user` requires `DatabaseKey` but you run it with an empty `Env`, you get a **runtime** missing-capability error when the effect executes — not a silent `None`. For static verification, keep `Needs<K>` bounds on public APIs so callers must wire providers before `run_with`.

When building tests manually:

```rust
let mut env = Env::new();
// forgot env.insert::<DatabaseKey>(...)
run_blocking(get_user(42), env); // panics on require! / get
```

Prefer `build_env` or typed test helpers so incomplete wiring fails at setup time.

## Summary

| Tool | Use |
|------|-----|
| `R: Needs<K>` | Declare dependency in signature |
| `require!(env, K)` | Borrow inside `effect!` / `Effect::new` |
| `env.get::<K>()` | Direct access when you hold `&Env` |
| `env.try_get::<K>()` | Fallible lookup without panic |

An application that satisfies all `Needs` bounds at the edge and passes a complete provider list to `run_with` is an application where every dependency is explicit — no service-locator globals.
