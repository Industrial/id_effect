# Providing Services — Layers

You have a trait, an implementation and a capability type. A [`Layer`](./ch06-00-layers.md)
builds the capability; the edge feeds it to the program.

## A closed layer

A layer that needs nothing (`In = HNil`) can be a plain value or a derive:

```rust
use id_effect::{HCons, HNil, Layer, ProviderSpecDerive, hlist};
# pub struct AppError;
# pub trait UserRepository: Send + Sync + 'static {}
# #[derive(Clone)] pub struct UserRepo(pub std::sync::Arc<dyn UserRepository>);
# impl UserRepo { pub fn new(repo: impl UserRepository) -> Self { Self(std::sync::Arc::new(repo)) } }
# pub struct MockUserRepository;
# impl MockUserRepository { fn default_fixture() -> Self { MockUserRepository } }
# impl UserRepository for MockUserRepository {}

// By hand: one shared value, built once per layer build.
fn mock_user_repo() -> Layer<HCons<UserRepo, HNil>, AppError> {
    Layer::succeed(hlist![UserRepo::new(MockUserRepository::default_fixture())])
}

// Or derived: `UserRepoMock::layer::<E>()` is a `Layer<HCons<UserRepo, HNil>, E>`.
#[derive(ProviderSpecDerive)]
#[provides(UserRepo, with = || UserRepo::new(MockUserRepository::default_fixture()))]
struct UserRepoMock;
```

`ProviderSpecDerive` emits only the inherent `layer()`; `#[provides(K, release = f)]` turns it into
an `acquire_release` layer whose finalizer runs when the layer's scope closes. The 0.6
`|deps: &Env|` constructors are gone: a provider with dependencies is a layer with an input.

## A layer with dependencies

The production repository needs a `Database`. Read it with `need` inside `Layer::effect`; the
layer's `In` lists what it needs:

```rust
use id_effect::{HCons, HNil, Layer, hlist, need};
# use id_effect::{Effect, Scope, sync};
# pub struct AppError;
# #[derive(Clone)] pub struct Pool;
# pub struct Database { pool: Pool }
# pub trait UserRepository: Send + Sync + 'static {}
# #[derive(Clone)] pub struct UserRepo(pub std::sync::Arc<dyn UserRepository>);
# impl UserRepo { pub fn new(repo: impl UserRepository) -> Self { Self(std::sync::Arc::new(repo)) } }
# pub struct PostgresUserRepository { pool: Pool }
# impl UserRepository for PostgresUserRepository {}
# fn open_pool(scope: Scope) -> Effect<Database, AppError> {
#     scope.add_finalizer(sync(|| ())).map(|()| Database { pool: Pool })
# }

fn user_repo_live() -> Layer<HCons<UserRepo, HNil>, AppError, HCons<Database, HNil>> {
    Layer::effect(|| {
        need::<Database>().map(|db| {
            hlist![UserRepo::new(PostgresUserRepository { pool: db.pool.clone() })]
        })
    })
}

fn database_live() -> Layer<HCons<Database, HNil>, AppError> {
    // `open_pool` adds a finalizer to `scope` that closes the pool when the layer's scope closes.
    Layer::scoped(|scope| open_pool(scope).map(|db| hlist![db]))
}
```

Compose the graph with the Effect.ts operators:

| Operator | Meaning |
|----------|---------|
| `a.merge(b)` | side by side; outputs and inputs concatenated |
| `base.provide_to(upper)` | feed `base`'s output into `upper`; keep only `upper`'s output |
| `upper.provide_merge(base)` | feed `base` into `upper`; keep both outputs |

```rust
# use id_effect::{HCons, HNil, Layer, hlist, need};
# pub struct AppError; pub struct Database; pub struct UserRepo;
# fn user_repo_live() -> Layer<HCons<UserRepo, HNil>, AppError, HCons<Database, HNil>> {
#     Layer::effect(|| need::<Database>().map(|_| hlist![UserRepo]))
# }
# fn database_live() -> Layer<HCons<Database, HNil>, AppError> { Layer::succeed(hlist![Database]) }
// Database → UserRepo, and keep Database for code that also needs it.
let app = user_repo_live().provide_merge(database_live());
# let _: Layer<HCons<Database, HCons<UserRepo, HNil>>, AppError> = app;
```

A layer value is memoised within one build: a layer used twice in a graph is constructed once.

## At the edge

```rust
# use id_effect::{Effect, HCons, HNil, Layer, ProviderSpecDerive, caps, effect, hlist, need, succeed};
# #[derive(Clone)] pub struct User { pub name: String }
# pub struct DbError;
# pub enum AppError { Db(DbError) }
# impl From<DbError> for AppError { fn from(e: DbError) -> Self { AppError::Db(e) } }
# pub struct UserProfile { pub name: String }
# pub trait UserRepository: Send + Sync + 'static {
#     fn get_user(&self, id: u64) -> Effect<User, DbError>;
# }
# #[derive(Clone)] pub struct UserRepo(pub std::sync::Arc<dyn UserRepository>);
# impl UserRepo { pub fn new(repo: impl UserRepository) -> Self { Self(std::sync::Arc::new(repo)) } }
# pub struct MockUserRepository { users: Vec<User> }
# impl MockUserRepository { fn default_fixture() -> Self { MockUserRepository { users: test_data() } } }
# impl UserRepository for MockUserRepository {
#     fn get_user(&self, id: u64) -> Effect<User, DbError> {
#         self.users.get(id as usize).cloned().map_or_else(|| id_effect::fail(DbError), succeed)
#     }
# }
# fn test_data() -> Vec<User> { (0..50).map(|i| User { name: format!("user{i}") }).collect() }
# pub struct Database;
# fn database_live() -> Layer<HCons<Database, HNil>, AppError> { Layer::succeed(hlist![Database]) }
# fn user_repo_live() -> Layer<HCons<UserRepo, HNil>, AppError, HCons<Database, HNil>> {
#     Layer::effect(|| need::<Database>().map(|_| hlist![UserRepo::new(MockUserRepository::default_fixture())]))
# }
# #[derive(ProviderSpecDerive)]
# #[provides(UserRepo, with = || UserRepo::new(MockUserRepository::default_fixture()))]
# struct UserRepoMock;
# fn get_user_profile(id: u64) -> Effect<UserProfile, AppError, caps!(UserRepo)> {
#     effect!({
#         let repo = ~UserRepo.clone();
#         let user = ~repo.0.get_user(id);
#         UserProfile { name: user.name }
#     })
# }
// Production
let exit = get_user_profile(42).provide_layer(user_repo_live().provide_merge(database_live())).block_on();

// Test with a mock layer
let exit = get_user_profile(42).provide_layer(UserRepoMock::layer()).block_on();

// Test with a plain value — no layer needed
let exit = get_user_profile(42)
    .provide(UserRepo::new(MockUserRepository { users: test_data() }))
    .block_on();
```

`provide(v)` removes one capability from `R`; `provide_all(hlist![..])` supplies the whole list;
`provide_env(env)` accepts any environment that contains `R`; `provide_layer(layer)` builds the
layer under a fresh scope and closes it (running finalizers) when the effect completes. If the
layer's output misses a capability of `R`, the call does not compile.

Application code using `caps!(UserRepo)` and `~UserRepo` stays identical across all three.
