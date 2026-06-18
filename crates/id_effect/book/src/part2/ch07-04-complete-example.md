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
use id_effect::Effect;
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

#[::id_effect::capability(Arc<dyn UserRepository>)]
struct UserRepo;

#[::id_effect::capability(Arc<dyn PostRepository>)]
struct PostRepo;

#[::id_effect::capability(Arc<dyn NotificationService>)]
struct Notifier;
```

## Business logic

```rust
use id_effect::{effect, require, caps, succeed};

fn get_author_feed(author_id: u64) -> Effect<(User, Vec<Post>), AppError, caps!(UserRepoKey, PostRepoKey)> {
    effect!(|r| {
        let user_repo = ~UserRepoKey;
        let post_repo = ~PostRepoKey;
        let user  = ~ user_repo.get_user(author_id).map_error(AppError::Db);
        let posts = ~ post_repo.get_posts_by_author(author_id).map_error(AppError::Db);
        (user, posts)
    })
}

fn register_user(name: &str, email: &str) -> Effect<User, AppError, caps!(UserRepoKey, NotifierKey)> {
    effect!(|r| {
        let repo = ~UserRepoKey;
        let notifier = ~NotifierKey;
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

[`CapabilityGraph`](../../src/capability/graph.rs) ensures `DatabaseLive` runs before repo providers that read `DatabaseKey` from `Env`.

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

- Business logic declares `caps!(…)` and uses `~Key` — no Postgres, SMTP, or concrete types in domain code.
- Providers swap at the edge via `provide!(…)`.
- The dependency graph is explicit in provider `requires()` + the effect's capability list.

That's compile-time dependency injection: requirements are typed; wiring is centralized at `main` and in tests.
