# Mocking Services — Test Doubles as Capabilities

In id_effect, "mocking" isn't a special testing concept. An effect names what it needs in `R`;
production provides the real values (usually through a `Layer`), a test provides fakes. Business
logic never knows the difference, and there is no macro or registry to set up.

## The Pattern

Put the service behind a trait and make the capability a newtype over `Arc<dyn Trait>`:

```rust,ignore
use std::sync::Arc;
use id_effect::{Effect, caps, effect};

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
```

`~Users` reads the capability inside `effect!`; `?` fails the effect with the `Result`'s error,
converted with `From` (here `From<DbError> for AppError`). Outside `effect!`, `need::<Users>()`
yields an `Arc<Users>`.

## Injecting the Test Double

```rust,ignore
#[derive(Default)]
struct InMemoryUsers(Mutex<HashMap<u64, User>>);

impl UserRepo for InMemoryUsers { /* … */ }

#[test]
fn rename_saves_the_new_name() {
  let repo = Arc::new(InMemoryUsers::with([User::new(1, "ada")]));
  let exit = rename(1, "Ada".into()).provide(Users(repo.clone())).block_on();
  assert_eq!(exit.into_result().map(|u| u.name).ok(), Some("Ada".to_string()));
  assert_eq!(repo.get(1).unwrap().name, "Ada");
}
```

`provide(value)` removes `Users` from `R`. With several capabilities use
`provide_env(hlist![a, b, c])` or chain `provide` calls. A forgotten capability is a compile
error, not a test that silently hits production.

## Asserting on Calls

Keep a handle to the fake's shared state and inspect it after the run, like the `Mailer` fixture
in `crates/id_effect/tests/macros.rs`:

```rust,ignore
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
```

## Failing Services

A fake that always fails tests the error paths:

```rust,ignore
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
```

## Layer-Based Test Setup

When production wires the app with `Layer`s, a test can swap one layer for a fake and keep the
rest of the graph:

```rust,ignore
use id_effect::{Layer, hlist};

// `signup_and_rename` needs `caps!(Users, Mail)`; the mailer comes from production wiring.
let fake_db: Layer<caps!(Users), AppError> =
  Layer::succeed(hlist![Users(Arc::new(InMemoryUsers::default()))]);
let app = fake_db.merge(MailerLive::layer()); // Layer<caps!(Users, Mail), AppError>
let exit = signup_and_rename("ada@example.com").provide_layer(app).block_on();
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
