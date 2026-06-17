# Providing Services — `ProviderSpec` Implementations

You have a trait and an implementation. A [`ProviderSpec`](../../src/capability/provider.rs) wires the impl into [`Env`](../../src/capability/env.rs).

## Production provider

```rust
use id_effect::{Env, ProviderError, ProviderSpec, define_capability, provide, run_with};
use std::sync::Arc;

struct PostgresUserRepository { pool: Pool }

impl UserRepository for PostgresUserRepository {
    fn get_user(&self, id: u64) -> Effect<User, DbError, ()> {
        effect! { /* query via self.pool */ }
    }
}

struct UserRepoLive;

impl ProviderSpec for UserRepoLive {
    type Key = UserRepoKey;
    type Output = Arc<dyn UserRepository>;

    fn provider_id() -> &'static str { "user-repo-postgres" }

    fn provide(deps: &Env) -> Result<Arc<dyn UserRepository>, ProviderError> {
        let pool = deps.get::<DatabaseKey>().clone();
        Ok(Arc::new(PostgresUserRepository { pool }))
    }
}
```

Register alongside dependencies:

```rust
run_with(
    [provide!(ConfigLive), provide!(DatabaseLive), provide!(UserRepoLive)],
    get_user_profile(42),
)?;
```

## Mock provider

[`provide!`](../../src/capability/provider.rs) takes a **type** (`provide!(MockUserRepoLive)`), not a value. For custom fixture data, insert into `Env` directly:

```rust
let mut env = Env::new();
env.insert::<UserRepoKey>(Arc::new(MockUserRepository { users: test_data() }));
run_blocking(get_user(1), env)?;
```

For a fixed zero-config mock, use a unit struct:

```rust
struct MockUserRepoLive;

impl ProviderSpec for MockUserRepoLive {
    type Key = UserRepoKey;
    type Output = Arc<dyn UserRepository>;

    fn provider_id() -> &'static str { "user-repo-mock" }

    fn provide(_deps: &Env) -> Result<Arc<dyn UserRepository>, ProviderError> {
        Ok(Arc::new(MockUserRepository::default_fixture()))
    }
}
```

Same `UserRepoKey`, no real database — swap at the edge:

```rust
// Production
run_with([provide!(DatabaseLive), provide!(UserRepoLive)], app())?;

// Test (fixed fixture)
run_with([provide!(MockUserRepoLive)], get_user(1))?;
```

Application code using `Needs<UserRepoKey>` and `require!(env, UserRepoKey)` stays identical.
