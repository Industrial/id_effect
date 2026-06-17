# Accessing Services — `Needs` and `require!`

v2 replaces `service_env`, `ServiceEnv`, and `~ ServiceKey` with [`Needs<K>`](../../src/capability/needs.rs) bounds and [`require!`](../../src/capability/run.rs).

## Single service

```rust
use id_effect::{effect, require, Needs};

fn get_user(id: u64) -> Effect<User, DbError, Env>
where
    Env: Needs<UserRepoKey>,
{
    effect!(|env: &mut Env| {
        let repo = require!(env, UserRepoKey);
        ~ repo.get_user(id)
    })
}
```

Generic over any environment that implements the bound:

```rust
fn get_user<R>(id: u64) -> Effect<User, DbError, R>
where
    R: Needs<UserRepoKey> + 'static,
{ /* same body */ }
```

## Multiple services

```rust
fn notify_user(id: u64, message: &str) -> Effect<(), AppError, Env>
where
    Env: Needs<UserRepoKey> + Needs<EmailKey>,
{
    effect!(|env: &mut Env| {
        let repo = require!(env, UserRepoKey);
        let email = require!(env, EmailKey);
        let user = ~ repo.get_user(id).map_error(AppError::Db);
        ~ email.send(&user.email, message).map_error(AppError::Email);
        ()
    })
}
```

## Direct `Env` access

Outside `effect!`, use `Needs::need` or `Env::get`:

```rust
fn pool_url(env: &Env) -> &str {
    require!(env, ConfigKey).database_url()
}
```

## `Env` vs generic `R`

| Style | When |
|-------|------|
| `Effect<_, _, Env>` | Application modules, examples |
| `Effect<_, _, R> where R: Needs<K>` | Library code that should not fix the env type |
| `caps!(K1, K2)` in docs/signatures | Documents required keys; runtime type is still `Env` |

All styles run against the same [`Env`](../../src/capability/env.rs) built by [`run_with`](../../src/capability/run.rs).
