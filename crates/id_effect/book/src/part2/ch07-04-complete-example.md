# A Complete DI Example — Putting It All Together

A small blog API with three services, a provider graph, and production vs test wiring.

## Domain

```rust
struct User { id: u64, name: String, email: String }
struct Post { id: u64, author_id: u64, title: String, body: String }
enum AppError { Db(DbError), Notify(NotifyError) }
```

## Three service traits + keys

```rust
use id_effect::{Effect, define_capability};
use std::sync::Arc;

pub trait UserRepository: Send + Sync {
    fn get_user(&self, id: u64) -> Effect<User, DbError, ()>;
}

pub trait PostRepository: Send + Sync {
    fn get_posts_by_author(&self, author_id: u64) -> Effect<Vec<Post>, DbError, ()>;
}

pub trait NotificationService: Send + Sync {
    fn send_welcome(&self, to: &str) -> Effect<(), NotifyError, ()>;
}

define_capability!(UserRepoKey, Arc<dyn UserRepository>);
define_capability!(PostRepoKey, Arc<dyn PostRepository>);
define_capability!(NotifierKey, Arc<dyn NotificationService>);
```

## Business logic

```rust
use id_effect::{effect, require, Needs};

fn get_author_feed(author_id: u64) -> Effect<(User, Vec<Post>), AppError, Env>
where
    Env: Needs<UserRepoKey> + Needs<PostRepoKey>,
{
    effect!(|env: &mut Env| {
        let user_repo = require!(env, UserRepoKey);
        let post_repo = require!(env, PostRepoKey);
        let user  = ~ user_repo.get_user(author_id).map_error(AppError::Db);
        let posts = ~ post_repo.get_posts_by_author(author_id).map_error(AppError::Db);
        (user, posts)
    })
}

fn register_user(name: &str, email: &str) -> Effect<User, AppError, Env>
where
    Env: Needs<UserRepoKey> + Needs<NotifierKey>,
{
    effect!(|env: &mut Env| {
        let repo     = require!(env, UserRepoKey);
        let notifier = require!(env, NotifierKey);
        let user = ~ repo.create_user(name, email).map_error(AppError::Db);
        ~ notifier.send_welcome(&user.email).map_error(AppError::Notify);
        user
    })
}
```

## Production wiring

```rust
use id_effect::{provide, run_with};

fn main() {
    run_with(
        [
            provide!(ConfigLive),
            provide!(DatabaseLive),
            provide!(PgUserRepoLive),
            provide!(PgPostRepoLive),
            provide!(SmtpNotifierLive),
        ],
        get_author_feed(1),
    )
    .expect("app failed");
}
```

[`CapabilityGraph`](../../src/capability/graph.rs) ensures `DatabaseLive` runs before repo providers that call `deps.get::<DatabaseKey>()`.

## Test wiring

```rust
#[test]
fn feed_includes_authors_posts() {
    let mut env = Env::new();
    env.insert::<UserRepoKey>(Arc::new(mock_user_repo(&[alice(), bob()])));
    env.insert::<PostRepoKey>(Arc::new(mock_post_repo(&[alice_post()])));

    let (_user, posts) = run_test(get_author_feed(1), env).unwrap();
    assert_eq!(posts.len(), 1);
    assert_eq!(posts[0].title, "Alice's Post");
}
```

## What this demonstrates

- Business logic declares `Needs<K>` and uses `require!` — no Postgres, SMTP, or concrete types in domain code.
- Providers swap at the edge via `provide!(…)`.
- The dependency graph is explicit in provider `requires()` + the effect's `Needs` bounds.

That's compile-time dependency injection in v2: requirements are typed; wiring is centralized at `main` and in tests.
