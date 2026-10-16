# Mocking Services — Test Doubles as Capabilities

In id_effect, "mocking" isn't a special testing concept. An effect names what it needs in `R`;
production provides the real values (usually through a `Layer`), a test provides fakes. Business
logic never knows the difference, and there is no macro or registry to set up.

## The Pattern

Put the service behind a trait and make the capability a newtype over `Arc<dyn Trait>`:

```rust
use std::sync::Arc;
use id_effect::{Effect, caps, effect};
# #[derive(Clone, Debug, PartialEq, Eq, Hash)]
# pub struct User { pub id: u64, pub name: String }
# #[derive(Debug, PartialEq, Eq, Hash)]
# pub enum DbError { Unavailable }
# #[derive(Debug, PartialEq, Eq, Hash)]
# pub enum AppError { NotFound(u64), Db(DbError) }
# impl From<DbError> for AppError { fn from(e: DbError) -> Self { AppError::Db(e) } }

pub trait UserRepo: Send + Sync {
  fn find(&self, id: u64) -> Result<Option<User>, DbError>;
  fn save(&self, user: &User) -> Result<(), DbError>;
}

/// The capability effects ask for.
#[derive(Clone)]
pub struct Users(pub Arc<dyn UserRepo>);

fn rename(id: u64, name: String) -> Effect<User, AppError, caps!(Users)> {
  effect!({
    let mut user = ~Users.0.find(id)?.ok_or(AppError::NotFound(id))?;
    user.name = name;
    ~Users.0.save(&user)?;
    user
  })
}
# struct Empty;
# impl UserRepo for Empty {
#   fn find(&self, _: u64) -> Result<Option<User>, DbError> { Ok(None) }
#   fn save(&self, _: &User) -> Result<(), DbError> { Ok(()) }
# }
# let exit = rename(7, "x".into()).provide(Users(Arc::new(Empty))).block_on();
# assert_eq!(exit, id_effect::Exit::fail(AppError::NotFound(7)));
```

`~Users` reads the capability inside `effect!`; `?` fails the effect with the `Result`'s error,
converted with `From` (here `From<DbError> for AppError`). Outside `effect!`, `need::<Users>()`
yields an `Arc<Users>`.

## Injecting the Test Double

```rust
# use std::collections::HashMap;
# use std::sync::{Arc, Mutex};
# use id_effect::{Effect, caps, effect};
# #[derive(Clone, Debug, PartialEq, Eq, Hash)]
# pub struct User { pub id: u64, pub name: String }
# impl User { fn new(id: u64, name: &str) -> Self { User { id, name: name.to_owned() } } }
# #[derive(Debug, PartialEq, Eq, Hash)]
# pub enum DbError { Unavailable }
# #[derive(Debug, PartialEq, Eq, Hash)]
# pub enum AppError { NotFound(u64), Db(DbError) }
# impl From<DbError> for AppError { fn from(e: DbError) -> Self { AppError::Db(e) } }
# pub trait UserRepo: Send + Sync {
#   fn find(&self, id: u64) -> Result<Option<User>, DbError>;
#   fn save(&self, user: &User) -> Result<(), DbError>;
# }
# #[derive(Clone)]
# pub struct Users(pub Arc<dyn UserRepo>);
# fn rename(id: u64, name: String) -> Effect<User, AppError, caps!(Users)> {
#   effect!({
#     let mut user = ~Users.0.find(id)?.ok_or(AppError::NotFound(id))?;
#     user.name = name;
#     ~Users.0.save(&user)?;
#     user
#   })
# }
#[derive(Default)]
struct InMemoryUsers(Mutex<HashMap<u64, User>>);

impl UserRepo for InMemoryUsers {
  // …
#   fn find(&self, id: u64) -> Result<Option<User>, DbError> { Ok(self.0.lock().unwrap().get(&id).cloned()) }
#   fn save(&self, user: &User) -> Result<(), DbError> {
#     self.0.lock().unwrap().insert(user.id, user.clone());
#     Ok(())
#   }
}
# impl InMemoryUsers {
#   fn with<const N: usize>(users: [User; N]) -> Self {
#     InMemoryUsers(Mutex::new(users.into_iter().map(|u| (u.id, u)).collect()))
#   }
#   fn get(&self, id: u64) -> Option<User> { self.0.lock().unwrap().get(&id).cloned() }
# }

#[test]
fn rename_saves_the_new_name() {
  let repo = Arc::new(InMemoryUsers::with([User::new(1, "ada")]));
  let exit = rename(1, "Ada".into()).provide(Users(repo.clone())).block_on();
  assert_eq!(exit.into_result().map(|u| u.name).ok(), Some("Ada".to_string()));
  assert_eq!(repo.get(1).unwrap().name, "Ada");
}
# let repo = Arc::new(InMemoryUsers::with([User::new(1, "ada")]));
# let exit = rename(1, "Ada".into()).provide(Users(repo.clone())).block_on();
# assert_eq!(exit.into_result().map(|u| u.name).ok(), Some("Ada".to_string()));
# assert_eq!(repo.get(1).unwrap().name, "Ada");
```

`provide(value)` removes `Users` from `R`. With several capabilities use
`provide_env(hlist![a, b, c])` or chain `provide` calls. A forgotten capability is a compile
error, not a test that silently hits production.

## Asserting on Calls

Keep a handle to the fake's shared state and inspect it after the run, like the `Mailer` fixture
in `crates/id_effect/tests/macros.rs`:

```rust
# use std::sync::{Arc, Mutex};
# use id_effect::{Effect, caps, fail, need, succeed};
# #[derive(Debug)]
# struct MailError;
# trait Mailer: Send + Sync { fn send(&self, to: &str) -> Result<(), MailError>; }
# struct Mail(Arc<dyn Mailer>);
# fn signup(email: &str) -> Effect<(), MailError, caps!(Mail)> {
#   let to = email.to_owned();
#   need::<Mail>().flat_map(move |mail| match mail.0.send(&to) { Ok(()) => succeed(()), Err(e) => fail(e) })
# }
#[derive(Clone, Default)]
struct RecordingMailer(Arc<Mutex<Vec<String>>>);

impl Mailer for RecordingMailer {
  fn send(&self, to: &str) -> Result<(), MailError> {
    self.0.lock().unwrap().push(to.to_string());
    Ok(())
  }
}

#[test]
fn signup_sends_one_welcome_mail() {
  let mailer = RecordingMailer::default();
  signup("ada@example.com").provide(Mail(Arc::new(mailer.clone()))).block_on();
  assert_eq!(*mailer.0.lock().unwrap(), vec!["ada@example.com".to_string()]);
}
# let mailer = RecordingMailer::default();
# signup("ada@example.com").provide(Mail(Arc::new(mailer.clone()))).block_on();
# assert_eq!(*mailer.0.lock().unwrap(), vec!["ada@example.com".to_string()]);
```

## Failing Services

A fake that always fails tests the error paths:

```rust
# use std::sync::Arc;
# use id_effect::{Effect, Exit, caps, effect};
# #[derive(Clone, Debug, PartialEq, Eq, Hash)]
# pub struct User { pub id: u64, pub name: String }
# #[derive(Debug, PartialEq, Eq, Hash)]
# pub enum DbError { Unavailable }
# #[derive(Debug, PartialEq, Eq, Hash)]
# pub enum AppError { NotFound(u64), Db(DbError) }
# impl From<DbError> for AppError { fn from(e: DbError) -> Self { AppError::Db(e) } }
# pub trait UserRepo: Send + Sync {
#   fn find(&self, id: u64) -> Result<Option<User>, DbError>;
#   fn save(&self, user: &User) -> Result<(), DbError>;
# }
# #[derive(Clone)]
# pub struct Users(pub Arc<dyn UserRepo>);
# fn rename(id: u64, name: String) -> Effect<User, AppError, caps!(Users)> {
#   effect!({
#     let mut user = ~Users.0.find(id)?.ok_or(AppError::NotFound(id))?;
#     user.name = name;
#     ~Users.0.save(&user)?;
#     user
#   })
# }
struct DownRepo;
impl UserRepo for DownRepo {
  fn find(&self, _: u64) -> Result<Option<User>, DbError> { Err(DbError::Unavailable) }
  fn save(&self, _: &User) -> Result<(), DbError> { Err(DbError::Unavailable) }
}

#[test]
fn rename_surfaces_db_errors() {
  let exit = rename(1, "x".into()).provide(Users(Arc::new(DownRepo))).block_on();
  assert_eq!(exit, Exit::fail(AppError::Db(DbError::Unavailable)));
}
# let exit = rename(1, "x".into()).provide(Users(Arc::new(DownRepo))).block_on();
# assert_eq!(exit, Exit::fail(AppError::Db(DbError::Unavailable)));
```

## Layer-Based Test Setup

When production wires the app with `Layer`s, a test can swap one layer for a fake and keep the
rest of the graph:

```rust
use id_effect::{Layer, hlist};
# use std::sync::Arc;
# use id_effect::{Effect, caps, need, succeed};
# #[derive(Debug)]
# pub enum AppError {}
# pub trait UserRepo: Send + Sync {}
# #[derive(Default)]
# struct InMemoryUsers;
# impl UserRepo for InMemoryUsers {}
# #[derive(Clone)]
# pub struct Users(pub Arc<dyn UserRepo>);
# #[derive(Clone)]
# pub struct Mail(pub &'static str);
# struct MailerLive;
# impl MailerLive { fn layer() -> Layer<caps!(Mail), AppError> { Layer::succeed(hlist![Mail("smtp")]) } }
# fn signup_and_rename(email: &str) -> Effect<String, AppError, caps!(Users, Mail)> {
#   let email = email.to_owned();
#   need::<Mail>().flat_map(move |mail| succeed(format!("{email} via {}", mail.0)))
# }

// `signup_and_rename` needs `caps!(Users, Mail)`; the mailer comes from production wiring.
let fake_db: Layer<caps!(Users), AppError> =
  Layer::succeed(hlist![Users(Arc::new(InMemoryUsers::default()))]);
let app = fake_db.merge(MailerLive::layer()); // Layer<caps!(Users, Mail), AppError>
let exit = signup_and_rename("ada@example.com").provide_layer(app).block_on();
# assert_eq!(exit.into_result().ok().as_deref(), Some("ada@example.com via smtp"));
```

The layer's output must match the effect's `R` (merge order included); `narrow()` adapts an
effect that needs only part of a wider environment.

`Layer::succeed` hands out the same values on every build; `Layer::effect(|| ..)` builds them
lazily; `merge` combines layers and `provide_to` feeds one layer's output into another. Layers are
memoised within one build and their scoped finalizers run when the program ends.

## What You Don't Need

- no mocking framework and no global registry: a fake is a value of the capability type;
- no runtime "missing service" errors: `R` is checked at compile time;
- no special harness: the effect runs with `block_on`, or on a `TestExecutor` when time matters.
