# Redis (`id_effect_redis`)

`id_effect_redis` puts Redis behind the same effect-and-layer shape as the SQL client: a pooled `RedisClient` capability, typed stores for key-value, hash and JSON data, a backend for the core `Cache`, pub/sub as a `Stream`, and Redis-backed session and rate-limit stores for `id_effect_auth` and `id_effect_axum`.

```toml
id_effect_redis = { version = "0.9", features = ["session", "rate-limit"] }
```

It is built on [redis-rs](https://crates.io/crates/redis) with tokio connections, so its effects run on a tokio runtime (`id_effect_tokio::TokioRuntime`, or the one your axum server runs on). Every operation is a lazy `Effect<_, RedisError>`; `RedisError` separates `Config`, `Connection` (unreachable, dropped, timed out), `Command` (the server said no) and `Decode` (a reply or a stored value is not the type you asked for).

## The client and its layer

`RedisConfig` says where to connect and how many connections to open; `RedisClient::layer` connects (and `PING`s) when the layer is built, failing the build with `E::from(RedisError)`:

```rust,no_run
use std::time::Duration;

use id_effect::{Effect, need};
use id_effect_redis::{RedisClient, RedisConfig, RedisEnv, RedisError};
use id_effect_tokio::TokioRuntime;

# async fn start() -> std::io::Result<()> {
let config = RedisConfig::new("redis://cache.internal:6379/0")
    .pool_size(4)                                   // connections, used round-robin
    .connection_timeout(Duration::from_secs(2))
    .response_timeout(Duration::from_millis(500));

let program: Effect<i64, RedisError, RedisEnv> =
    need::<RedisClient>().flat_map(|redis| redis.kv::<i64>("visits").incr_by("/", 1).narrow());
let exit = TokioRuntime::current()?
    .run(program.provide_layer(RedisClient::layer(config)))
    .await;
# let _ = exit;
# Ok(())
# }
# fn main() {}
```

Each pooled connection is a redis-rs `ConnectionManager`: multiplexed (many commands in flight at once) and reconnecting by itself, with `reconnect_retries` attempts before a command fails. `RedisClient::connect_lazy` builds a client whose connections open on first use, for services that must start while Redis is still coming up. `Debug` of the config and the client hides the URL's password.

Anything the typed API does not cover goes through `query` (one command) or `query_pipeline`, with the reply decoded by redis-rs (`id_effect_redis::redis` re-exports it):

```rust,no_run
# use id_effect::Effect;
# use id_effect_redis::{RedisClient, RedisError};
# fn f(redis: RedisClient) -> Effect<(i64, i64), RedisError> {
use id_effect_redis::redis;

redis.query_pipeline(redis::pipe().atomic().cmd("INCR").arg("a").cmd("INCR").arg("b").clone())
# }
```

## Typed stores

Three stores give Redis a typed API, with values as any `serde` type, stored as JSON. Each lives in a **namespace** (a key prefix, `namespace:key`), so `keys()` and `clear()` touch only their own data.

| Store | Shape | Highlights |
|---|---|---|
| `redis.kv::<V>("ns")` → `RedisKv<V>` | key → value | `get`, `get_many` (one `MGET`), `set`, `set_ex(key, v, ttl)`, `set_nx` (set if absent: locks, idempotency keys), `incr_by`, `expire`, `ttl`, `delete`, `keys`, `clear` |
| `redis.hash::<V>("ns")` → `RedisHash<V>` | key → field → value | `get`, `get_all`, `set`, `set_all`, `delete_field`, `incr_by`, `len`, `expire` |
| `redis.json::<D>("ns")` → `RedisJson<D>` | id → JSON document | `set`, `get`, `get_path` / `set_path` / `delete_path` at a JSONPath, `incr_by` on a number, `ids`, `clear` |

```rust,no_run
use std::time::Duration;

use id_effect::Effect;
use id_effect_redis::{RedisClient, RedisError};
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize)]
struct Order {
    status: String,
    total_cents: i64,
}

fn place(redis: &RedisClient, id: &str) -> Effect<Option<String>, RedisError> {
    let orders = redis.json::<Order>("orders");
    let again = orders.clone();
    let id = id.to_owned();
    orders
        .set(&id, &Order { status: "new".into(), total_cents: 1250 })
        .flat_map({
            let (orders, id) = (again.clone(), id.clone());
            move |()| orders.set_path(&id, "$.status", &"paid")
        })
        .flat_map(move |_| again.get_path::<String>(&id, "$.status"))
}

fn remember_once(redis: &RedisClient, request_id: &str) -> Effect<bool, RedisError> {
    // `true` the first time this request id is seen within a day.
    redis.kv::<u8>("idempotency").set_nx(request_id, &1, Some(Duration::from_secs(86_400)))
}
```

`RedisJson` uses the RedisJSON module, which Redis 8 includes (as does Redis Stack); against a server without it, its commands fail with `RedisError::Command`. A value that does not decode as the requested type is a `RedisError::Decode`, not a panic. Note that `RedisJson::incr_by` with a fractional delta leaves a float in the document.

## The cache, on Redis

The core `Cache` (see the resources chapter) memoizes an effectful lookup with a time-to-live, single-flight misses and statistics. `Cache::with_backend(backend, lookup)` keeps the values in a `CacheBackend` instead of in the process, **with the same API**: code written against `Cache` runs unchanged on either. Two backends exist: another in-process `Cache`, and `RedisCacheBackend`, which shares the values between every instance using the same namespace.

The in-process backend needs no server, so this runs as is:

```rust
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use id_effect::{Cache, CacheStats, Effect, Exit, sync};

// The same code, whatever holds the values.
fn twice(cache: &Cache<u32, String>) -> Effect<(String, CacheStats)> {
    let stats = cache.clone();
    cache.get(7).zip(cache.get(7)).flat_map(move |(a, _)| stats.stats().map(move |s| (a, s)))
}

let calls = Arc::new(AtomicUsize::new(0));
let lookup = {
    let calls = Arc::clone(&calls);
    move |id: u32| {
        calls.fetch_add(1, Ordering::SeqCst);
        sync(move || format!("user-{id}"))
    }
};

let local: Cache<u32, String> = Cache::new(1_000, Some(Duration::from_secs(60)), lookup.clone());
// Its own lookup is never used: as a backend, only its stored values are.
let store: Cache<u32, String> = Cache::new(1_000, Some(Duration::from_secs(60)), |id: u32| sync(move || id.to_string()));
let backed: Cache<u32, String> = Cache::with_backend(store, lookup);

for cache in [&local, &backed] {
    let Exit::Success((value, stats)) = twice(cache).block_on() else { panic!() };
    assert_eq!(value, "user-7");
    assert_eq!((stats.hits, stats.misses, stats.size), (1, 1, 1));
}
assert_eq!(calls.load(Ordering::SeqCst), 2); // one lookup per cache
```

On Redis, only the backend changes:

```rust,no_run
use std::time::Duration;

use id_effect::{Cache, sync};
use id_effect_redis::{RedisCacheBackend, RedisClient};

fn user_names(redis: &RedisClient) -> Cache<u32, String> {
    let backend = RedisCacheBackend::new(redis.clone(), "users", Some(Duration::from_secs(60)));
    Cache::with_backend(backend, |id: u32| sync(move || format!("user-{id}")))
}
```

What a backend changes:

- **Sharing.** A value one instance looked up is a hit for every other instance; `invalidate(&key)` and `invalidate_all()` reach all of them. Keys are the key type's `Display`, values are JSON, each expiring after the backend's time-to-live (or evicted by the server's `maxmemory-policy`).
- **Stampedes.** Concurrent misses on one key *in a process* still share one lookup (the leader reads the backend, runs the lookup on a miss, writes the value, then wakes the others). Across processes, each instance may run the lookup once.
- **Failures.** Only successes are stored: a typed failure goes to the callers of that lookup and is not cached. If Redis is down, a read counts as a miss (the lookup runs) and a write or invalidation is skipped; `cache.backend_errors()` counts both, so a cache outage slows callers down instead of failing them.
- **Statistics.** `hits` are values read from the backend, `misses` are lookups run or joined, and `size` asks the backend (a namespace scan for Redis).

## Pub/sub as a stream

`publish(channel, payload)` returns how many subscribers received the message; `publish_json` sends a `serde` value. `subscribe(channels)` (or `psubscribe(patterns)`) opens a dedicated connection and completes **once the server confirmed the subscription**, so anything published afterwards is delivered; it yields an `id_effect` `Stream<RedisMessage, RedisError>`, consumed with the usual stream operators:

```rust,no_run
use id_effect::Effect;
use id_effect_redis::{RedisClient, RedisError};
use serde::Deserialize;

#[derive(Deserialize)]
struct Shipped {
    order: String,
}

fn next_shipments(redis: &RedisClient, n: usize) -> Effect<Vec<String>, RedisError> {
    redis.subscribe(["orders.shipped"]).flat_map(move |messages| {
        messages
            .take(n)
            .map(|m| m.json::<Shipped>().map(|s| s.order).unwrap_or_default())
            .run_collect()
    })
}
```

Dropping the stream unsubscribes. If the connection is lost, the stream fails with `RedisError::Connection`; subscribe again to resume (Redis pub/sub does not replay what was published meanwhile; use Redis streams through `query` when every message must arrive).

## Sessions in Redis (feature `session`)

`RedisSessionStore` implements `id_effect_auth`'s `SessionStore`, so `Sessions` (login, rotation, idle and absolute timeouts, logout everywhere) runs on Redis unchanged. It passes the same behaviour suite as `MemorySessionStore` and `PgSessionStore`.

```rust,no_run
use id_effect::{Effect, need};
use id_effect_auth::session::{SessionConfig, SessionError, SessionStoreEnv, SessionStoreService, Sessions};
use id_effect_redis::{RedisClient, RedisConfig, redis_session_store_layer};

let layer = RedisClient::layer::<SessionError>(RedisConfig::default())
    .provide_to(redis_session_store_layer());
let program: Effect<String, SessionError, SessionStoreEnv> =
    need::<SessionStoreService>().flat_map(|store| {
        let sessions = Sessions::from_service((*store).clone(), SessionConfig::default());
        sessions.login("user-42", None).map(|s| s.id().as_str().to_owned()).narrow()
    });
let runnable = program.provide_layer(layer);
# let _ = runnable;
```

A record is a hash keyed by the SHA-256 of the session id (the id, which is the cookie value, is never written), and it **expires in Redis with the session**, so abandoned sessions need no cleanup job. `remove_user` (logout everywhere) uses a per-user index, and each operation is one Lua script, so a record and its indexes change together. `with_prefix` keeps applications sharing a server apart. The keys are not hash-tagged: use one Redis node (with replicas if you like), not a cluster.

## Rate limits across instances (feature `rate-limit`)

`RateLimitLayer` keeps its token buckets in a `RateLimitStore`. The default `MemoryRateLimitStore` is per process, so three instances behind a load balancer admit three times the limit. `RedisRateLimitStore` keeps the buckets in Redis, so the limit holds for the service as a whole:

```rust,no_run
use axum::{Router, routing::get};
use id_effect_axum::middleware::{RateLimitKey, RateLimitLayer};
use id_effect_redis::{RedisClient, RedisRateLimitStore};

fn app(redis: RedisClient) -> Router {
    Router::new()
        .route("/api", get(|| async { "ok" }))
        .layer(
            RateLimitLayer::new(RedisRateLimitStore::new(redis, 10.0, 20.0)) // 10/s, bursts of 20
                .key(RateLimitKey::peer_ip()),
        )
}
```

Each check is one Lua script that refills the bucket on the **Redis server's clock** (so instances with skewed clocks agree) and takes a token; a bucket expires once it would be full again. If Redis fails, the check dies and the layer lets the request through: the limiter protects capacity, it is not an access control.

## Testing

The crate's integration tests start a Redis 8 container with testcontainers. Without Docker they print `SKIP` and pass; with `ID_EFFECT_REQUIRE_DOCKER=1` (the CI `containers-integration` job) a missing Docker daemon fails them. Code that only needs *a* cache can be tested with the in-process backend shown above.
