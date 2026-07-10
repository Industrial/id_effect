# Accessing Services — `Needs` and `~Key`

Application effects access services with [`Needs<K>`](../../src/capability/needs.rs) bounds, [`caps!`](../../src/capability/set.rs) in `R`, and `~Key` (or [`require!`](../../src/capability/require.rs)) inside `effect!`.

## Single service

```rust
use id_effect::{effect, require, caps, succeed};

fn get_user(id: u64) -> Effect<User, DbError, caps!(UserRepo)> {
    effect!(|r| {
        let repo = ~UserRepo;
        ~ repo.get_user(id)
    })
}
```

Generic over any environment that implements the bound:

```rust
fn get_user<R>(id: u64) -> Effect<User, DbError, R>
where
    R: Needs<UserRepo> + 'static,
{
    effect!(|r: &mut R| {
        let repo = ~UserRepo;
        ~ repo.get_user(id)
    })
}
```

## Multiple services

```rust
fn notify_user(id: u64, message: &str) -> Effect<(), AppError, caps!(UserRepo, Email)> {
    effect!(|r| {
        let repo = ~UserRepo;
        let email = ~Email;
        let user = ~ repo.get_user(id).map_error(AppError::Db);
        ~ email.send(&user.email, message).map_error(AppError::Email);
        ()
    })
}
```

## Direct `Env` access

Prefer `effect!` + `~Config` in application code. For small sync helpers outside `effect!`, `Needs::<Config>::need(env)` is available.

## `caps!` vs generic `R`

| Style | When |
|-------|------|
| `Effect<_, _, caps!(K)>` | Application modules, examples |
| `Effect<_, _, R> where R: Needs<K>` | Library code that should not fix the env type |
| `Env` at HTTP boundaries | Axum `State<Env>`, then `run_with_caps` |

All styles run against the same [`Env`](../../src/capability/env.rs) built by [`run_with`](../../src/capability/run.rs) or [`build_env`](../../src/capability/run.rs).
