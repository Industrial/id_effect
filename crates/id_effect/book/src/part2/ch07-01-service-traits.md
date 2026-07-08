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

- Methods return `Effect<_, _, ()>` — the service method itself has no extra environment; callers carry `UserRepo` in `caps!`.
- `Send + Sync` on the trait so handles like `Arc<dyn UserRepository>` work across fibers.
- Small, verb-oriented methods (`get_user`, not `users`).

## Define the capability service

```rust
struct UserRepo;
```

This generates `UserRepo: Capability` with `Value = Arc<dyn UserRepository>`.

## Use `~Key` in callers

```rust
use id_effect::{effect, require, caps, succeed};

fn get_user_profile(id: u64) -> Effect<UserProfile, AppError, caps!(UserRepo)> {
    effect!(|r| {
        let repo = ~UserRepo;
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
struct UserRepo;
struct Email;
```

Functions declare exactly what they need: `caps!(UserRepo, Email)`.
