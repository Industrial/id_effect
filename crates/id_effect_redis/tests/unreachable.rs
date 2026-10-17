//! Without a server (no Docker needed): every operation fails with a typed
//! `RedisError::Connection`, and a `Cache` on Redis degrades to its lookup.

mod common;

use std::time::Duration;

use common::{ok, run};
use id_effect::{Cache, Never, sync};
use id_effect_redis::{RedisCacheBackend, RedisClient, RedisConfig, RedisError};

async fn unreachable() -> RedisClient {
  let config = RedisConfig::new("redis://127.0.0.1:1")
    .pool_size(1)
    .reconnect_retries(0)
    .connection_timeout(Duration::from_millis(200));
  ok(RedisClient::connect_lazy(config)).await
}

fn down<A: std::fmt::Debug>(r: Result<A, RedisError>) -> bool {
  matches!(r, Err(RedisError::Connection(_)))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn every_operation_fails_with_a_connection_error() {
  let client = unreachable().await;
  let kv = client.kv::<u32>("k");
  assert!(down(run(kv.get("a")).await));
  assert!(down(run(kv.set("a", &1)).await));
  assert!(down(run(kv.keys()).await));
  assert!(down(run(kv.clear()).await));
  let hash = client.hash::<u32>("h");
  assert!(down(run(hash.get_all("a")).await));
  let json = client.json::<u32>("j");
  assert!(down(run(json.get("a")).await));
  assert!(down(run(client.ping()).await));
  assert!(down(run(client.publish("c", "x")).await));
  assert!(down(run(client.subscribe(["c"])).await.map(|_| ())));
  assert!(down(run(client.psubscribe(["c*"])).await.map(|_| ())));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_cache_on_an_unreachable_redis_still_answers() {
  let client = unreachable().await;
  let cache: Cache<u32, String> =
    Cache::with_backend(RedisCacheBackend::new(client, "users", None), |id: u32| {
      sync(move || format!("user-{id}"))
    });
  assert_eq!(ok(cache.get(1)).await, "user-1");
  assert_eq!(ok(cache.size::<Never, id_effect::HNil>()).await, 0);
  // The read, the write and the size each failed and were counted.
  assert_eq!(
    ok(cache.backend_errors::<Never, id_effect::HNil>()).await,
    3
  );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_malformed_url_is_a_config_error() {
  let built = run(RedisClient::connect_lazy(RedisConfig::new("not-a-url"))).await;
  assert!(matches!(built, Err(RedisError::Config(_))), "{built:?}");
}
