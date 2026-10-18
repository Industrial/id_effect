//! M17: `clear()` never targets the whole database by accident. A store with an empty namespace
//! refuses with `RedisError::EmptyNamespace` (no server needed); a namespaced `clear()` deletes
//! only its own keys, and `clear_all_dangerous()` is the recorded flush (Docker Redis, see
//! `common` for SKIP / `ID_EFFECT_REQUIRE_DOCKER`).

mod common;

use std::time::Duration;

use common::{ok, redis_client, run};
use id_effect::observability::posture::dangerous_opt_outs;
use id_effect_redis::{RedisClient, RedisConfig, RedisError};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn m17_clear_with_empty_namespace_is_an_error() {
  // Nothing listens on port 1: the refusal happens before any command is sent.
  let config = RedisConfig::new("redis://127.0.0.1:1")
    .pool_size(1)
    .reconnect_retries(0)
    .connection_timeout(Duration::from_millis(200));
  let client = ok(RedisClient::connect_lazy(config)).await;
  assert_eq!(
    run(client.kv::<u32>("").clear()).await,
    Err(RedisError::EmptyNamespace)
  );
  assert_eq!(
    run(client.hash::<u32>("").clear()).await,
    Err(RedisError::EmptyNamespace)
  );
  assert_eq!(
    run(client.json::<u32>("").clear()).await,
    Err(RedisError::EmptyNamespace)
  );
  // A namespaced store does reach the (absent) server.
  assert!(matches!(
    run(client.kv::<u32>("k").clear()).await,
    Err(RedisError::Connection(_))
  ));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn m17_clear_only_deletes_namespace_keys() {
  let Some((_redis, client)) = redis_client("m17_clear_only_deletes_namespace_keys").await else {
    return;
  };
  let users = client.kv::<u32>("users");
  let user_tags = client.kv::<u32>("users-tags");
  let carts = client.hash::<u32>("carts");
  ok(users.set("a", &1)).await;
  ok(users.set("b", &2)).await;
  ok(user_tags.set("a", &3)).await;
  ok(carts.set("u1", "apple", &4)).await;
  ok(client.query::<()>(redis::cmd("SET").arg("plain").arg("5").clone())).await;

  // Only `users:*`: neither a sibling prefix nor other stores nor plain keys.
  assert_eq!(ok(users.clear()).await, 2);
  assert_eq!(ok(users.keys()).await, Vec::<String>::new());
  let root = client.kv::<u32>("");
  assert_eq!(ok(root.keys()).await, ["carts:u1", "plain", "users-tags:a"]);

  // The empty namespace refuses, and deletes nothing.
  assert_eq!(run(root.clear()).await, Err(RedisError::EmptyNamespace));
  assert_eq!(ok(root.keys()).await.len(), 3);

  // The explicit flush deletes everything and is recorded as a posture opt-out.
  assert_eq!(ok(root.clear_all_dangerous()).await, 3);
  assert_eq!(ok(root.keys()).await, Vec::<String>::new());
  assert!(
    dangerous_opt_outs()
      .iter()
      .any(|(id, _)| *id == "id_effect_redis::RedisKv::clear_all_dangerous")
  );
}
