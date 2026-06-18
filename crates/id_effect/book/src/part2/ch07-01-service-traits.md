# Service Traits — Defining Interfaces

The first step in defining a service is the trait — the contract between implementation and callers.

## Define the interface

```rust
use id_effect::Effect;

pub trait UserRepository: Send + Sync {
    fn get_user(&self, id: u64) -> Effect<User, DbError, ()>;
    fn save_user(&self, user: &User) -> Effect<(), DbError, ()>;
}
```

Conventions:

- Methods return `Effect<_, _, ()>` — the service method itself has no extra environment; callers carry `UserRepoKey` in `caps!`.
- `Send + Sync` on the trait so handles like `Arc<dyn UserRepository>` work across fibers.
- Small, verb-oriented methods (`get_user`, not `users`).

## Define the capability key

```rust
#[::id_effect::capability(Arc<dyn UserRepository>)]
struct UserRepo;
```

This generates `UserRepoKey: CapabilityKey` with `Value = Arc<dyn UserRepository>`.

## Use `~Key` in callers

```rust
use id_effect::{effect, require, caps, succeed};

fn get_user_profile(id: u64) -> Effect<UserProfile, AppError, caps!(UserRepoKey)> {
    effect!(|r| {
        let repo = ~UserRepoKey;
        let user = ~ repo.get_user(id).map_error(AppError::Db);
        UserProfile::from(user)
    })
}
```

## Keep traits focused

```rust
// BAD — one god trait
trait AppService {
    fn get_user(&self, id: u64) -> Effect<User, AppError, ()>;
    fn send_email(&self, to: &str, body: &str) -> Effect<(), AppError, ()>;
}

// GOOD — separate capabilities
trait UserRepository { /* … */ }
trait EmailService { /* … */ }

#[::id_effect::capability(Arc<dyn UserRepository>)]
struct UserRepo;

#[::id_effect::capability(Arc<dyn EmailService>)]
struct Email;
```

Functions declare exactly what they need: `caps!(UserRepoKey, EmailKey)`.
