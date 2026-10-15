# Service Traits — Defining Interfaces

The first step in defining a service is the trait: the contract between implementations and
callers.

## Define the interface

```rust
use id_effect::Effect;

pub trait UserRepository: Send + Sync + 'static {
    fn get_user(&self, id: u64) -> Effect<User, DbError>;
    fn save_user(&self, user: User) -> Effect<(), DbError>;
}
```

Conventions:

- Methods return `Effect<A, E>` — `R` defaults to `HNil`: the method needs nothing from the
  environment; callers carry the *service itself* in their `R`. A sub-effect with `R = HNil`
  binds inside any `effect!`.
- `Send + Sync + 'static` on the trait, so `Arc<dyn UserRepository>` can live in `R` and cross
  fibers (effects are `Send`).
- Small, verb-oriented methods (`get_user`, not `users`).

## Define the capability type

A capability is any `Send + Sync + 'static` type; `R` stores it behind an `Arc`. For a trait
object, wrap it in a newtype and deref to the trait (this is how
`id_effect_platform::http::HttpClientService` is built):

```rust
use std::ops::Deref;
use std::sync::Arc;

#[derive(Clone)]
pub struct UserRepo(pub Arc<dyn UserRepository>);

impl UserRepo {
    pub fn new(repo: impl UserRepository) -> Self {
        Self(Arc::new(repo))
    }
}

impl Deref for UserRepo {
    type Target = dyn UserRepository;
    fn deref(&self) -> &Self::Target {
        &*self.0
    }
}
```

The type is the key: there is no separate tag, and two services with the same implementation
type are told apart by their newtypes.

## Use `~UserRepo` in callers

```rust
use id_effect::{Effect, caps, effect};

fn get_user_profile(id: u64) -> Effect<UserProfile, AppError, caps!(UserRepo)> {
    effect!({
        let repo = ~UserRepo; // &Arc<UserRepo>, derefs to dyn UserRepository
        let user = ~repo.get_user(id).map_err(AppError::Db);
        UserProfile::from(user)
    })
}
```

Inside `effect!`, `~UserRepo` reads the capability and `~expr` binds a sub-effect. `~expr` also
widens the sub-effect's error with `From`, so with `impl From<DbError> for AppError` the
`.map_err` can go. Outside the macro, `need::<UserRepo>()` is the same read as an effect; a
`flat_map` keeps one `R`, so lift the method's `HNil` effect with `.narrow()`:
`need::<UserRepo>().flat_map(move |repo| repo.get_user(id).narrow())`.

## Keep traits focused

```rust
// BAD — one god trait
trait AppService {
    fn get_user(&self, id: u64) -> Effect<User, AppError>;
    fn send_email(&self, to: &str, body: &str) -> Effect<(), AppError>;
}

// GOOD — separate capabilities
trait UserRepository { /* … */ }
trait EmailService { /* … */ }
struct UserRepo(Arc<dyn UserRepository>);
struct Email(Arc<dyn EmailService>);
```

Functions declare exactly what they need: `caps!(UserRepo, Email)`.
