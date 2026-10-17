//! `RedisRateLimitStore` against a Redis container: token buckets shared by every instance,
//! behind id_effect_axum's `RateLimitLayer`.

#![cfg(feature = "rate-limit")]

mod common;

use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::extract::Request;
use axum::http::StatusCode;
use axum::routing::get;
use common::{ok, redis_client};
use id_effect::{Cause, Exit};
use id_effect_axum::middleware::{RateLimitError, RateLimitKey, RateLimitLayer, RateLimitStore};
use id_effect_redis::{RedisClient, RedisConfig, RedisRateLimitStore};
use id_effect_tokio::TokioRuntime;
use tower::ServiceExt;

async fn acquire(store: &RedisRateLimitStore, key: &str) -> Exit<(), RateLimitError> {
  TokioRuntime::current()
    .expect("tokio")
    .run(store.acquire(key))
    .await
}

fn admitted(exit: &Exit<(), RateLimitError>) -> bool {
  match exit {
    Exit::Success(()) => true,
    Exit::Failure(Cause::Fail(RateLimitError)) => false,
    other => panic!("unexpected exit: {other:?}"),
  }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn buckets_are_shared_and_refill() {
  let Some((_redis, client)) = redis_client("buckets_are_shared_and_refill").await else {
    return;
  };
  // Two instances of a service: one bucket per key between them.
  let a = RedisRateLimitStore::new(client.clone(), 5.0, 2.0);
  let b = RedisRateLimitStore::new(client.clone(), 5.0, 2.0);
  assert!(admitted(&acquire(&a, "ip:1").await));
  assert!(admitted(&acquire(&b, "ip:1").await));
  assert!(!admitted(&acquire(&a, "ip:1").await), "burst used up");
  assert!(admitted(&acquire(&b, "ip:2").await), "another key");

  // 5 tokens per second: one is back after 200 ms, not two.
  tokio::time::sleep(Duration::from_millis(260)).await;
  assert!(admitted(&acquire(&a, "ip:1").await));
  assert!(!admitted(&acquire(&b, "ip:1").await));

  // The bucket expires once it would be full again.
  let ttl =
    ok(client.query::<i64>(redis::cmd("PTTL").arg("id_effect:rate-limit:ip:1").clone())).await;
  assert!((1000..=1400).contains(&ttl), "{ttl}");
  let custom = RedisRateLimitStore::new(client, 1.0, 1.0).with_prefix("api");
  assert!(admitted(&acquire(&custom, "k").await));
  assert!(!admitted(&acquire(&custom, "k").await));
  assert!(format!("{custom:?}").contains("api"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_layer_answers_429_from_the_shared_bucket() {
  let Some((_redis, client)) = redis_client("the_layer_answers_429_from_the_shared_bucket").await
  else {
    return;
  };
  let app = |client: RedisClient| {
    Router::new().route("/", get(|| async { "ok" })).layer(
      RateLimitLayer::new(RedisRateLimitStore::new(client, 0.5, 2.0)).key(RateLimitKey::global()),
    )
  };
  let (one, two) = (app(client.clone()), app(client));
  let request = || Request::builder().uri("/").body(Body::empty()).unwrap();
  assert_eq!(
    one.clone().oneshot(request()).await.unwrap().status(),
    StatusCode::OK
  );
  assert_eq!(
    two.clone().oneshot(request()).await.unwrap().status(),
    StatusCode::OK
  );
  let limited = one.oneshot(request()).await.unwrap();
  assert_eq!(limited.status(), StatusCode::TOO_MANY_REQUESTS);
  assert!(limited.headers().contains_key("retry-after"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_unreachable_server_fails_open() {
  let config = RedisConfig::new("redis://127.0.0.1:1")
    .pool_size(1)
    .reconnect_retries(0)
    .connection_timeout(Duration::from_millis(200));
  let client = ok(RedisClient::connect_lazy(config)).await;
  let store = RedisRateLimitStore::new(client.clone(), 1.0, 1.0);
  assert!(matches!(
    acquire(&store, "k").await,
    Exit::Failure(Cause::Die(_))
  ));
  let app = Router::new()
    .route("/", get(|| async { "ok" }))
    .layer(RateLimitLayer::new(store).key(RateLimitKey::global()));
  let request = Request::builder().uri("/").body(Body::empty()).unwrap();
  assert_eq!(app.oneshot(request).await.unwrap().status(), StatusCode::OK);
}
