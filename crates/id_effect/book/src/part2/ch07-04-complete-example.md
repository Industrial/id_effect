# A Complete DI Example — Putting It All Together

A small blog API with three services, a layer graph, and production vs test wiring.

## Domain

```rust
struct User { id: u64, name: String, email: String }
struct Post { id: u64, author_id: u64, title: String, body: String }

enum AppError { Db(DbError), Notify(NotifyError) }
impl From<DbError> for AppError { fn from(e: DbError) -> Self { AppError::Db(e) } }
impl From<NotifyError> for AppError { fn from(e: NotifyError) -> Self { AppError::Notify(e) } }
```

## Three service traits and their capabilities

```rust
use std::sync::Arc;
use id_effect::Effect;

pub trait UserRepository: Send + Sync + 'static {
    fn get_user(&self, id: u64) -> Effect<User, DbError>;
    fn create_user(&self, name: String, email: String) -> Effect<User, DbError>;
}
pub trait PostRepository: Send + Sync + 'static {
    fn get_posts_by_author(&self, author_id: u64) -> Effect<Vec<Post>, DbError>;
}
pub trait NotificationService: Send + Sync + 'static {
    fn send_welcome(&self, to: String) -> Effect<(), NotifyError>;
}

// Newtypes with `Deref<Target = dyn Trait>`, as in "Service Traits".
pub struct UserRepo(Arc<dyn UserRepository>);
pub struct PostRepo(Arc<dyn PostRepository>);
pub struct Notifier(Arc<dyn NotificationService>);
```

## Business logic

```rust
use id_effect::{caps, effect};

fn get_author_feed(author_id: u64) -> Effect<(User, Vec<Post>), AppError, caps!(UserRepo, PostRepo)> {
    effect!({
        let users = ~UserRepo;
        let posts = ~PostRepo;
        // `~(a, b)` runs both lookups in parallel on child fibers.
        ~(
            users.get_user(author_id).map_err(AppError::Db),
            posts.get_posts_by_author(author_id).map_err(AppError::Db),
        )
    })
}

fn register_user(name: String, email: String) -> Effect<User, AppError, caps!(UserRepo, Notifier)> {
    effect!({
        let repo = ~UserRepo;
        let notifier = ~Notifier;
        let user = ~repo.create_user(name, email); // DbError → AppError via From
        ~notifier.send_welcome(user.email.clone());
        user
    })
}
```

## Production wiring

```rust
use id_effect::{HCons, HNil, Layer, hlist, need};

fn pg_user_repo() -> Layer<HCons<UserRepo, HNil>, AppError, HCons<Database, HNil>> {
    Layer::effect(|| need::<Database>().map(|db| hlist![UserRepo(Arc::new(PgUserRepository::new(&db)))]))
}
fn pg_post_repo() -> Layer<HCons<PostRepo, HNil>, AppError, HCons<Database, HNil>> {
    Layer::effect(|| need::<Database>().map(|db| hlist![PostRepo(Arc::new(PgPostRepository::new(&db)))]))
}

fn main() {
    // Both repositories need `Database`; the merged layer builds it once and shares the `Arc`.
    let repos = pg_user_repo().merge(pg_post_repo());
    let app = database_live().provide_to(repos);
    let exit = get_author_feed(1).provide_layer(app).block_on();
    println!("{exit:?}");
}
```

`database_live()` builds `Database` from configuration (see [Configuration](./ch07-10-config.md)).
If a layer graph does not produce every capability the program needs, `provide_layer` does not
compile — there is no run-time "missing provider" error.

## Test wiring

```rust
use id_effect::{Exit, hlist};

#[test]
fn feed_includes_authors_posts() {
    let exit = get_author_feed(1)
        .provide_all(hlist![
            UserRepo(Arc::new(mock_user_repo(&[alice(), bob()]))),
            PostRepo(Arc::new(mock_post_repo(&[alice_post()]))),
        ])
        .block_on();
    let Exit::Success((_user, posts)) = exit else { panic!("feed failed") };
    assert_eq!(posts.len(), 1);
    assert_eq!(posts[0].title, "Alice's Post");
}
```

For tests that sleep or retry, run on `TestExecutor` with virtual time instead of `block_on`
(see [Testing](../part4/ch15-00-testing.md)).

## What this demonstrates

- Business logic declares `caps!(…)` and reads `~Key` — no Postgres, SMTP or concrete types in
  domain code.
- Implementations swap at the edge: a layer graph in `main`, plain values in tests.
- The dependency graph is explicit in each layer's `In` type and checked by the compiler.
