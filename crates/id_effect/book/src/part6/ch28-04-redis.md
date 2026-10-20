# Redis (`id_effect_redis`)

`id_effect_redis` puts Redis behind the same effect-and-layer shape as the SQL client: a pooled `RedisClient` capability, typed stores for key-value, hash and JSON data, a backend for the core `Cache`, pub/sub as a `Stream`, a Redis Streams `MessageBroker` for `id_effect_messaging`, and Redis-backed session and rate-limit stores for `id_effect_auth` and `id_effect_axum`.

```toml
id_effect_redis = { version = "0.12", features = ["streams", "session", "rate-limit"] }
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

## Typed stores: key-value and JSON documents

Besides SQL, id_effect ships a first-party NoSQL integration: Redis as a **typed key-value store** (`RedisKv`, `RedisHash`) and a **JSON document store** (`RedisJson`, on the RedisJSON module). The type parameter is the value or document type — any `serde` type, stored as JSON — so reads come back as your Rust type, and a stored value that no longer matches it is a `RedisError::Decode`. Each store lives in a **namespace** (a key prefix, `namespace:key`), so `keys()` / `ids()` and `clear()` touch only their own data.

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

### A document collection

A `RedisJson<D>` is a collection of documents of one type, addressed by id: whole-document `set` / `get` / `delete`, partial reads and writes at a JSONPath, and `ids()` to list the collection. Wrapped in a small type of your own, it reads like a repository; values a document embeds (lists, nested objects) stay typed on the way back:

```rust,no_run
use id_effect::{Effect, Exit, succeed};
use id_effect_redis::{RedisClient, RedisConfig, RedisError, RedisJson};
use id_effect_tokio::TokioRuntime;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct Profile {
    name: String,
    plan: String,
    tags: Vec<String>,
}

/// User profiles as JSON documents, keyed by user id (keys `profiles:<id>`).
#[derive(Clone)]
struct Profiles(RedisJson<Profile>);

impl Profiles {
    fn new(redis: &RedisClient) -> Self {
        Self(redis.json("profiles"))
    }

    fn save(&self, id: &str, profile: &Profile) -> Effect<(), RedisError> {
        self.0.set(id, profile)
    }

    fn find(&self, id: &str) -> Effect<Option<Profile>, RedisError> {
        self.0.get(id)
    }

    /// One field, without reading the whole document.
    fn plan(&self, id: &str) -> Effect<Option<String>, RedisError> {
        self.0.get_path(id, "$.plan")
    }

    /// Upgrades in place; `false` when there is no such profile.
    fn upgrade(&self, id: &str) -> Effect<bool, RedisError> {
        self.0.set_path(id, "$.plan", &"pro")
    }

    /// Every profile id in the collection.
    fn ids(&self) -> Effect<Vec<String>, RedisError> {
        self.0.ids()
    }
}

/// Saves a profile, upgrades it in place, and reads it back: one field, then the document.
fn upgrade_flow(redis: &RedisClient) -> Effect<(Option<String>, Option<Profile>, Vec<String>), RedisError> {
    let profiles = Profiles::new(redis);
    let ada = Profile { name: "Ada".into(), plan: "free".into(), tags: vec!["beta".into()] };
    let p = profiles.clone();
    profiles
        .save("u1", &ada)
        .flat_map(move |()| p.upgrade("u1").map(move |_| p))
        .flat_map(|p| {
            let q = p.clone();
            p.plan("u1").zip(q.find("u1")).zip(q.ids())
        })
        .flat_map(|((plan, found), ids)| succeed((plan, found, ids)))
}

# async fn run() -> std::io::Result<()> {
let program = RedisClient::connect_lazy(RedisConfig::new("redis://127.0.0.1:6379/0"))
    .flat_map(|redis| upgrade_flow(&redis));
let exit = TokioRuntime::current()?.run(program).await;
if let Exit::Success((plan, found, ids)) = exit {
    assert_eq!(plan.as_deref(), Some("pro"));
    assert_eq!(found.map(|p| p.tags), Some(vec!["beta".to_owned()]));
    assert_eq!(ids, ["u1"]);
}
# Ok(())
# }
# fn main() {}
```

`crates/id_effect_redis/tests/redis_container.rs` runs the three stores against a Redis 8 container: `key_value_store`, `hash_store` and `json_document_store` (documents, JSONPath reads and writes, `incr_by`, decoding failures).

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

Dropping the stream unsubscribes. If the connection is lost, the stream fails with `RedisError::Connection`; subscribe again to resume (Redis pub/sub does not replay what was published meanwhile; use the Redis Streams broker below when every message must arrive).

## A message broker on Redis Streams (feature `streams`)

`RedisStreamsBroker` implements `id_effect_messaging`'s `MessageBroker` (publish, subscribe a consumer group, ack or nack each delivery) on Redis Streams, over the same `RedisClient`. Code written against `MessageBroker` and tested on `MemoryBroker` runs on it unchanged, and the `Relay` of an outbox publishes through it like through any broker.

```rust,no_run
use std::time::Duration;

use id_effect::{Effect, succeed};
use id_effect_messaging::{
    BrokerError, ConsumerGroup, Delivery, MessageBrokerExt, SubscribeOptions, Topic, ack_on_success,
};
use id_effect_redis::{RedisClient, RedisStreamsBroker, RedisStreamsConfig};

fn bill_orders(redis: RedisClient) -> Effect<(), BrokerError> {
    let broker = RedisStreamsBroker::new(
        redis,
        RedisStreamsConfig::new()
            .consumer("billing-7f9c")              // stable and unique per instance
            .min_idle(Duration::from_secs(60))     // reclaim deliveries unsettled for a minute
            .max_len(1_000_000),                   // XADD … MAXLEN ~ 1000000
    );
    let orders = Topic::new("orders").unwrap();
    let billing = ConsumerGroup::new("billing").unwrap();
    let options = SubscribeOptions::default().prefetch(32).concurrency(4).max_deliveries(5);
    broker.consume(&orders, &billing, options, ack_on_success(|d: &Delivery| {
        println!("billing {} (attempt {})", d.message().id(), d.attempt());
        succeed::<(), (), _>(())
    }))
}
```

How the broker semantics map onto Redis:

| `MessageBroker` | Redis Streams |
|---|---|
| a topic | a stream, keyed by the topic name |
| `publish` | `XADD <topic> [MAXLEN ~ n] * id <id> [key <key>] h:<name> <value>… payload <bytes>` |
| `subscribe` | `XGROUP CREATE <topic> <group> 0` (`StartFrom::Earliest`) or `$` (`Latest`) `MKSTREAM`; an existing group keeps its position |
| pulling deliveries | `XAUTOCLAIM` of entries idle for `min_idle` (redeliveries first), else `XREADGROUP GROUP <group> <consumer> COUNT <credit> BLOCK <block> STREAMS <topic> >` |
| `prefetch` | the credit: prefetch minus the deliveries still unsettled; at zero the subscription waits for a settlement |
| `Delivery::attempt` | the entry's delivery count in the group (`XPENDING`) |
| `ack` | `XACK` |
| `nack(Requeue)` | `XCLAIM … IDLE <min_idle> JUSTID`: the next pull redelivers it, without counting a delivery |
| a dropped delivery | stays pending; reclaimed once idle for `min_idle` |
| dead-lettering | one Lua script: `XACK`, then `XADD` of `Message::dead_lettered` to `<topic>.dlq` (or `SubscribeOptions::dead_letter`) if the group still had the entry pending |

A message is dead-lettered when it is nacked with `Nack::DeadLetter` (reason `rejected`), nacked with `Requeue` on its `max_deliveries`-th delivery, or reclaimed after its `max_deliveries`-th delivery was dropped (reason `max-deliveries`). The dead-letter stream is a topic like any other: subscribe a group to it to inspect or replay.

Things to know:

- **At least once.** A delivery is redelivered if it is nacked, dropped, or its consumer crashes; handlers must be idempotent (`Inbox` in `id_effect_messaging` deduplicates on the message id). A delivery still being handled after `min_idle` is reclaimed by another consumer, so keep `min_idle` above the longest handler run.
- **Consumers.** Give each instance a stable `consumer` name, unique among the instances. The default is generated per start, which leaves an empty consumer behind in each group.
- **Connections.** Each subscription opens its own reconnecting connection for its blocking reads (the response timeout grows by `block`); publishing and settling use the client's pool. `close()` ends every subscription within `block`.
- **Trimming.** Acked entries stay in the stream. `max_len` trims it on every publish, approximately; trimming also drops entries a slow group has not read yet. Unbounded by default.
- Redis 6.2 or later (`XAUTOCLAIM`). The keys are not hash-tagged: use one Redis node (with replicas if you like), not a cluster.

`cargo run -p id_effect_redis --features streams --example 030_streams` runs the whole cycle (a redelivery and a dead letter) against a local server.

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
