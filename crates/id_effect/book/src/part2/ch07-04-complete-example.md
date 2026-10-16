# A Complete DI Example — Putting It All Together

A small blog API with three services, a layer graph, and production vs test wiring.

## Domain

```rust
# struct DbError; struct NotifyError;
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
# pub struct User; pub struct Post; pub struct DbError; pub struct NotifyError;

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
# use std::sync::Arc;
# use id_effect::Effect;
# #[derive(Clone, Debug)] pub struct User { id: u64, name: String, email: String }
# #[derive(Clone, Debug)] pub struct Post { id: u64, author_id: u64, title: String, body: String }
# #[derive(Debug)] pub struct DbError;
# #[derive(Debug)] pub struct NotifyError;
# #[derive(Debug)] pub enum AppError { Db(DbError), Notify(NotifyError) }
# impl From<DbError> for AppError { fn from(e: DbError) -> Self { AppError::Db(e) } }
# impl From<NotifyError> for AppError { fn from(e: NotifyError) -> Self { AppError::Notify(e) } }
# pub trait UserRepository: Send + Sync + 'static {
#     fn get_user(&self, id: u64) -> Effect<User, DbError>;
#     fn create_user(&self, name: String, email: String) -> Effect<User, DbError>;
# }
# pub trait PostRepository: Send + Sync + 'static {
#     fn get_posts_by_author(&self, author_id: u64) -> Effect<Vec<Post>, DbError>;
# }
# pub trait NotificationService: Send + Sync + 'static {
#     fn send_welcome(&self, to: String) -> Effect<(), NotifyError>;
# }
# pub struct UserRepo(Arc<dyn UserRepository>);
# pub struct PostRepo(Arc<dyn PostRepository>);
# pub struct Notifier(Arc<dyn NotificationService>);
# impl std::ops::Deref for UserRepo { type Target = dyn UserRepository; fn deref(&self) -> &Self::Target { &*self.0 } }
# impl std::ops::Deref for PostRepo { type Target = dyn PostRepository; fn deref(&self) -> &Self::Target { &*self.0 } }
# impl std::ops::Deref for Notifier { type Target = dyn NotificationService; fn deref(&self) -> &Self::Target { &*self.0 } }
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
# use std::sync::Arc;
# use id_effect::Effect;
# #[derive(Clone, Debug)] pub struct User { id: u64, name: String, email: String }
# #[derive(Clone, Debug)] pub struct Post { id: u64, author_id: u64, title: String, body: String }
# #[derive(Debug)] pub struct DbError;
# #[derive(Debug)] pub enum AppError { Db(DbError) }
# pub trait UserRepository: Send + Sync + 'static {
#     fn get_user(&self, id: u64) -> Effect<User, DbError>;
# }
# pub trait PostRepository: Send + Sync + 'static {
#     fn get_posts_by_author(&self, author_id: u64) -> Effect<Vec<Post>, DbError>;
# }
# pub struct UserRepo(Arc<dyn UserRepository>);
# pub struct PostRepo(Arc<dyn PostRepository>);
# impl std::ops::Deref for UserRepo { type Target = dyn UserRepository; fn deref(&self) -> &Self::Target { &*self.0 } }
# impl std::ops::Deref for PostRepo { type Target = dyn PostRepository; fn deref(&self) -> &Self::Target { &*self.0 } }
# fn get_author_feed(author_id: u64) -> Effect<(User, Vec<Post>), AppError, id_effect::caps!(UserRepo, PostRepo)> {
#     id_effect::effect!({
#         let users = ~UserRepo;
#         let posts = ~PostRepo;
#         ~(users.get_user(author_id).map_err(AppError::Db), posts.get_posts_by_author(author_id).map_err(AppError::Db))
#     })
# }
# pub struct Database;
# fn database_live() -> Layer<HCons<Database, HNil>, AppError> { Layer::succeed(hlist![Database]) }
# struct PgUserRepository;
# impl PgUserRepository { fn new(_db: &Database) -> Self { PgUserRepository } }
# impl UserRepository for PgUserRepository {
#     fn get_user(&self, id: u64) -> Effect<User, DbError> {
#         id_effect::succeed(User { id, name: "Ada".into(), email: "ada@example.com".into() })
#     }
# }
# struct PgPostRepository;
# impl PgPostRepository { fn new(_db: &Database) -> Self { PgPostRepository } }
# impl PostRepository for PgPostRepository {
#     fn get_posts_by_author(&self, _: u64) -> Effect<Vec<Post>, DbError> { id_effect::succeed(vec![]) }
# }
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
# use std::sync::Arc;
# use id_effect::Effect;
# #[derive(Clone, Debug)] pub struct User { id: u64, name: String, email: String }
# #[derive(Clone, Debug)] pub struct Post { id: u64, author_id: u64, title: String, body: String }
# #[derive(Debug)] pub struct DbError;
# #[derive(Debug)] pub enum AppError { Db(DbError) }
# pub trait UserRepository: Send + Sync + 'static {
#     fn get_user(&self, id: u64) -> Effect<User, DbError>;
# }
# pub trait PostRepository: Send + Sync + 'static {
#     fn get_posts_by_author(&self, author_id: u64) -> Effect<Vec<Post>, DbError>;
# }
# pub struct UserRepo(Arc<dyn UserRepository>);
# pub struct PostRepo(Arc<dyn PostRepository>);
# impl std::ops::Deref for UserRepo { type Target = dyn UserRepository; fn deref(&self) -> &Self::Target { &*self.0 } }
# impl std::ops::Deref for PostRepo { type Target = dyn PostRepository; fn deref(&self) -> &Self::Target { &*self.0 } }
# fn get_author_feed(author_id: u64) -> Effect<(User, Vec<Post>), AppError, id_effect::caps!(UserRepo, PostRepo)> {
#     id_effect::effect!({
#         let users = ~UserRepo;
#         let posts = ~PostRepo;
#         ~(users.get_user(author_id).map_err(AppError::Db), posts.get_posts_by_author(author_id).map_err(AppError::Db))
#     })
# }
# struct MockUsers(Vec<User>);
# impl UserRepository for MockUsers {
#     fn get_user(&self, id: u64) -> Effect<User, DbError> {
#         self.0.iter().find(|u| u.id == id).cloned().map_or_else(|| id_effect::fail(DbError), id_effect::succeed)
#     }
# }
# struct MockPosts(Vec<Post>);
# impl PostRepository for MockPosts {
#     fn get_posts_by_author(&self, author_id: u64) -> Effect<Vec<Post>, DbError> {
#         id_effect::succeed(self.0.iter().filter(|p| p.author_id == author_id).cloned().collect())
#     }
# }
# fn mock_user_repo(users: &[User]) -> MockUsers { MockUsers(users.to_vec()) }
# fn mock_post_repo(posts: &[Post]) -> MockPosts { MockPosts(posts.to_vec()) }
# fn alice() -> User { User { id: 1, name: "Alice".into(), email: "alice@example.com".into() } }
# fn bob() -> User { User { id: 2, name: "Bob".into(), email: "bob@example.com".into() } }
# fn alice_post() -> Post { Post { id: 10, author_id: 1, title: "Alice's Post".into(), body: String::new() } }

// In a #[cfg(test)] module, under #[test]:
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
# feed_includes_authors_posts();
```

For tests that sleep or retry, run on `TestExecutor` with virtual time instead of `block_on`
(see [Testing](../part4/ch15-00-testing.md)).

## What this demonstrates

- Business logic declares `caps!(…)` and reads `~Key` — no Postgres, SMTP or concrete types in
  domain code.
- Implementations swap at the edge: a layer graph in `main`, plain values in tests.
- The dependency graph is explicit in each layer's `In` type and checked by the compiler.
