# Providing Services — `ProviderSpec` Implementations

You have a trait and an implementation. A provider wires the impl into [`Env`](../../src/capability/env.rs).

## Production provider

```rust
use id_effect::{Env, ProviderSpecDerive, caps, effect, provide, run_with};
use std::sync::Arc;

struct PostgresUserRepository { pool: Pool }

impl UserRepository for PostgresUserRepository {
    fn get_user(&self, id: u64) -> Effect<User, DbError, ()> {
        effect! { /* query via self.pool */ }
    }
}

#[derive(ProviderSpecDerive)]
#[provides(UserRepo)]
struct UserRepoLive;

impl UserRepoLive {
    fn new(deps: &Env) -> Arc<dyn UserRepository> {
        let pool = deps.get::<Cap<Database>>().clone();
        Arc::new(PostgresUserRepository { pool })
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
env.insert::<Cap<UserRepo>>(Arc::new(MockUserRepository { users: test_data() }));
run_blocking(get_user(1), env)?;
```

For a fixed zero-config mock, use a unit struct:

```rust
#[derive(ProviderSpecDerive)]
#[provides(UserRepo)]
struct MockUserRepoLive;

impl MockUserRepoLive {
    fn new() -> Arc<dyn UserRepository> {
        Arc::new(MockUserRepository::default_fixture())
    }
}
```

Same `UserRepo`, no real database — swap at the edge:

```rust
// Production
run_with([provide!(DatabaseLive), provide!(UserRepoLive)], app())?;

// Test (fixed fixture)
run_with([provide!(MockUserRepoLive)], get_user(1))?;
```

Application code using `caps!(UserRepo)` and `~UserRepo` stays identical.
