# Capability Types

A **capability** is any `Send + Sync + 'static` type placed in an effect's `R`. Its Rust type is
its name: there is no separate key type, registry or tag.

## Declaring a capability

```rust
use std::sync::Arc;
# use id_effect::Effect;
# pub struct Pool; pub struct User; pub struct DbError;

// A concrete value
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Counter(pub u32);

// Two roles of the same underlying type: one newtype each
pub struct MainDb(pub Pool);
pub struct CacheDb(pub Pool);

// A trait-backed service (ports and adapters): a newtype over `Arc<dyn Trait>`
pub trait UserRepository: Send + Sync {
  fn get_user(&self, id: u64) -> Effect<User, DbError>;
}
#[derive(Clone)]
pub struct UserRepo(pub Arc<dyn UserRepository>);
```

`MainDb` and `CacheDb` both wrap a `Pool` but are distinct capabilities: providing one where the
other is required is a type error.

## Spelling R: `caps!`

`caps!(A, B, C)` is the type `HCons<A, HCons<B, HCons<C, HNil>>>`:

```rust
# use id_effect::{Effect, caps, effect};
# struct Report; struct DbError; struct MainDb; struct CacheDb; struct UserRepo;
fn needs_main() -> Effect<Report, DbError, caps!(MainDb)> {
  // ...
#   effect!({ let _ = ~MainDb; Report })
}
fn needs_both() -> Effect<Report, DbError, caps!(MainDb, CacheDb, UserRepo)> {
  // ...
#   effect!({ let _ = (~MainDb, ~CacheDb, ~UserRepo); Report })
}
```

Listing a capability twice is a compile error at the repeat.

## Service traits

A trait method returns an effect that needs nothing (`R = HNil`); the **caller** carries the
service in its `R`:

```rust
# use std::sync::Arc;
# use id_effect::{Effect, Exit, caps, effect, succeed};
# pub struct User { name: String }
# pub struct DbError;
# pub trait UserRepository: Send + Sync {
#   fn get_user(&self, id: u64) -> Effect<User, DbError>;
# }
# #[derive(Clone)]
# pub struct UserRepo(pub Arc<dyn UserRepository>);
# struct InMemory;
# impl UserRepository for InMemory {
#   fn get_user(&self, _id: u64) -> Effect<User, DbError> { succeed(User { name: "Ada".into() }) }
# }
fn show_user(id: u64) -> Effect<String, DbError, caps!(UserRepo)> {
  effect!({
    let repo = ~UserRepo.clone(); // Arc<UserRepo>
    let user = ~repo.0.get_user(id);
    user.name.clone()
  })
}
# let exit = show_user(1).provide(UserRepo(Arc::new(InMemory))).block_on();
# assert!(matches!(exit, Exit::Success(ref n) if n == "Ada"));
```

`~UserRepo` reads the capability (an `&Arc<UserRepo>`) and `.clone()` applies to that reference;
`~repo.0.get_user(id)` is an ordinary bind that runs the effect the method returns. (`~X` reads a
capability only when `X` is an upper-case identifier; anything else after `~` is bound as an
effect.) In tests, provide a `UserRepo` wrapping an in-memory implementation.

## Values are shared

Capabilities are stored as `Arc<T>` and handed out as `&Arc<T>` (inside `effect!`) or `Arc<T>`
(from `need`). They are shared, not owned by one effect, so mutable state inside a capability uses
interior mutability (`Mutex`, atomics, or the effect-aware `Ref` / `SynchronizedRef`).

## Summary

| Item | Role |
|------|------|
| capability type `T` | a dependency; the type is its identity |
| `caps!(T1, T2, ..)` | the environment type `HCons<T1, HCons<T2, .., HNil>>` |
| newtype `struct X(Inner)` | names a role, separates same-type capabilities |
| `struct S(Arc<dyn Trait>)` | a swappable service |
