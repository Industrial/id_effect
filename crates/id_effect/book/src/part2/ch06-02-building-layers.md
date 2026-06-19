# Building Providers — From Simple to Complex

## Leaf providers — no dependencies

```rust
struct ConfigLive;

impl ProviderSpec for ConfigLive {
    type Key = ConfigKey;
    type Output = Config;

    fn provider_id() -> &'static str { "config-from-env" }

    fn provide(_deps: &Env) -> Result<Config, ProviderError> {
        Config::from_env().map_err(|e| ProviderError {
            provider: Self::provider_id(),
            message: e.to_string(),
        })
    }
}
```

## Dependent providers

Read already-built capabilities from `deps`:

```rust
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

Override `requires()` to return `[DatabaseKey::id(), …]` so the graph builds the database before the repository.

## Test doubles

Same key, different provider type:

```rust
struct MockUserRepoLive;

impl ProviderSpec for MockUserRepoLive {
    type Key = UserRepoKey;
    type Output = Arc<dyn UserRepository>;

    fn provider_id() -> '&'static str { "user-repo-mock" }

    fn provide(_deps: &Env) -> Result<Arc<dyn UserRepository>, ProviderError> {
        Ok(Arc::new(MockUserRepository::default_fixture()))
    }
}
```

When a test needs custom data, use `Env::insert` instead of `provide!`. Business logic still uses `Needs<UserRepoKey>` — only the wiring at the edge changes.

## Custom values

Workspace crates expose helpers that return [`ProviderBox`](../../src/capability/provider.rs) directly — e.g. `id_effect_config::provide_config_provider`, `id_effect_platform::http::reqwest::provide_reqwest_client`. Use these when you already hold a concrete handle and don't need a zero-sized `ProviderSpec` type.

## The pattern in practice

```
ConfigLive          (no deps)
  → DatabaseLive    (needs ConfigKey)
  → CacheLive       (needs ConfigKey)
  → UserRepoLive    (needs DatabaseKey)
```

The next section shows how [`CapabilityGraph`](../../src/capability/graph.rs) wires the list together.
