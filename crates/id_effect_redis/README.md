# `id_effect_redis`

Redis for id_effect services, over [redis-rs](https://crates.io/crates/redis):

- `RedisClient`: a pool of reconnecting, multiplexed connections, provided as a capability by
  `RedisClient::layer(RedisConfig)`; raw commands through `query`.
- Typed stores with `serde` values: `RedisKv` (key → value, time-to-live, set-if-absent,
  counters), `RedisHash` (key → field → value) and `RedisJson` (documents updated at a JSONPath,
  on the RedisJSON module of Redis 8).
- `RedisCacheBackend`: Redis behind the core `Cache` API (`Cache::with_backend`), so the same
  cache code runs in process or shared by every instance.
- Pub/sub: `publish`, and `subscribe` / `psubscribe` as an `id_effect` `Stream`.
- Feature `session`: `RedisSessionStore`, `id_effect_auth`'s `SessionStore` in Redis.
- Feature `rate-limit`: `RedisRateLimitStore`, `id_effect_axum`'s `RateLimitStore`, so a rate
  limit holds across instances.

```rust,no_run
use std::time::Duration;

use id_effect::{Cache, Effect, sync};
use id_effect_redis::{RedisCacheBackend, RedisClient, RedisConfig, RedisError};

// A price cache shared by every instance: the same `Cache` API as the in-process one.
fn prices(redis: &RedisClient) -> Cache<String, u64> {
  let backend = RedisCacheBackend::new(redis.clone(), "prices", Some(Duration::from_secs(30)));
  Cache::with_backend(backend, |sku: String| sync(move || sku.len() as u64 * 100))
}

let program: Effect<u64, RedisError> = RedisClient::connect(RedisConfig::default())
  .flat_map(|redis| prices(&redis).get("tea".to_owned()).widen());
# let _ = program;
```

Every operation is a lazy `Effect` failing with `RedisError`; run it on a tokio runtime. The
integration tests start a Redis 8 container (testcontainers): without Docker they print `SKIP`,
and with `ID_EFFECT_REQUIRE_DOCKER=1` (the CI `containers-integration` job) they fail instead.

Tier: `platform` ([ADR 0010](../../docs/adrs/0010-crate-tiers.md) Amendment 5): the Ledger reference app ([`examples/reference/ledger`](../../examples/reference/ledger)) depends on it.
