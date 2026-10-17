//! Ex 010 — a Redis-backed `Cache`, a typed key-value store and pub/sub.
//!
//! Needs a Redis server at `REDIS_URL` (default `redis://127.0.0.1:6379`), e.g.
//! `docker run --rm -p 6379:6379 redis:8.2`. Without one it says so and exits successfully.
//!
//! Run: `cargo run -p id_effect_redis --example 010_cache_and_store`

use std::time::Duration;

use id_effect::{Cache, Effect, Exit, sync};
use id_effect_redis::{RedisCacheBackend, RedisClient, RedisConfig, RedisError};
use id_effect_tokio::TokioRuntime;

/// Looks a user's display name up (here: computes it), cached in Redis for a minute.
fn user_names(redis: &RedisClient) -> Cache<u32, String> {
  let backend = RedisCacheBackend::new(
    redis.clone(),
    "example:users",
    Some(Duration::from_secs(60)),
  );
  Cache::with_backend(backend, |id: u32| sync(move || format!("user #{id}")))
}

fn program(redis: RedisClient) -> Effect<(), RedisError> {
  let names = user_names(&redis);
  let visits = redis.kv::<i64>("example:visits");
  let messages = redis.subscribe(["example:events"]);
  let publisher = redis.clone();
  names
    .get(42)
    .widen()
    .zip(names.get(42).widen())
    .flat_map(move |(first, again)| {
      println!("cache: {first} / {again}");
      visits.incr_by("home", 1)
    })
    .flat_map(move |n| {
      println!("visits of home: {n}");
      messages
    })
    .flat_map(move |events| {
      publisher
        .publish("example:events", "hello")
        .flat_map(move |_| events.take(1).run_collect())
    })
    .map(|got| println!("received {:?} on {}", got[0].text(), got[0].channel()))
}

#[tokio::main(flavor = "multi_thread", worker_threads = 2)]
async fn main() {
  let url = std::env::var("REDIS_URL").unwrap_or_else(|_| RedisConfig::DEFAULT_URL.to_owned());
  let config = RedisConfig::new(url.clone())
    .connection_timeout(Duration::from_millis(500))
    .reconnect_retries(0);
  let runtime = TokioRuntime::current().expect("tokio runtime");
  let redis = match runtime.run(RedisClient::connect(config)).await {
    Exit::Success(redis) => redis,
    Exit::Failure(cause) => {
      println!("skipped: no Redis at {url} ({cause:?})");
      return;
    }
  };
  match runtime.run(program(redis)).await {
    Exit::Success(()) => {}
    Exit::Failure(cause) => panic!("example failed: {cause:?}"),
  }
}
