# Accessing Services — `~Cap` and `need`

Application effects declare the services they use in `R` and read them with `~Cap` inside
`effect!`, or with `need::<Cap>()` outside it.

## Single service

```rust
use id_effect::{Effect, caps, effect};

fn get_user(id: u64) -> Effect<User, DbError, caps!(UserRepo)> {
    effect!({
        let repo = ~UserRepo;
        ~repo.get_user(id)
    })
}
```

`~UserRepo` (an upper-case type name) reads the capability as `&Arc<UserRepo>`; `~repo.get_user(id)`
(any other expression) binds a sub-effect.

## Generic over the environment

Library code that should not fix the whole environment type takes `R` with a `Selector` bound —
"`R` contains a `UserRepo` at some index `I`". This is how the platform crates are written
(`id_effect_platform::http::execute`, `id_effect_config::Config::run`):

```rust
use id_effect::{Effect, HList, Selector, need};

fn get_user<R, I>(id: u64) -> Effect<User, DbError, R>
where
    R: Selector<UserRepo, I> + HList,
{
    need::<UserRepo>().flat_map(move |repo| repo.get_user(id).narrow())
}
```

The index `I` is always inferred; callers write `get_user(1)` inside any environment that has a
`UserRepo`, or name `R` (`get_user::<caps!(UserRepo), _>(1)`) when nothing else fixes it.

## Multiple services

```rust
fn notify_user(id: u64, message: String) -> Effect<(), AppError, caps!(UserRepo, Email)> {
    effect!({
        let repo = ~UserRepo;
        let email = ~Email;
        let user = ~repo.get_user(id).map_err(AppError::Db);
        ~email.send(&user.email, &message).map_err(AppError::Email);
    })
}
```

The order of `caps!` does not matter to providers: `provide_env`, `provide_layer` and
`Layer::provide_to` accept any environment that contains every capability (any order, extras
allowed).

## Choosing a style

| Style | When |
|-------|------|
| `Effect<_, _, caps!(K)>` + `~K` | Application modules, examples |
| `Effect<_, _, R> where R: Selector<K, I> + HList` + `need::<K>()` | Library code that should not fix `R` |
| `DynEnv` + `try_get::<K>()` | Runtime-keyed environments, e.g. per-request capabilities in [`id_effect_axum`](./ch07-08-axum-host.md) |

`DynEnv` is the escape hatch: a type-keyed map checked at run time. Prefer the typed `R`; a missing
capability there is a compile error.
