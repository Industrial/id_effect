//! Redis: balances cached in Redis (shared by every instance) and dropped by postings, and the
//! login rate limit counted in Redis across instances.

mod common;

use std::sync::Arc;

use common::{run, with_ledger};
use id_effect_kit::redis::{RedisClient, RedisConfig, redis};
use id_effect_kit::test::http::{StatusCode, TestClient};
use id_effect_kit::test::{ContainerError, ContainerImage, Redis};
use ledger::reports::BALANCE_NAMESPACE;
use ledger::{Database, Services};
use serde_json::{Value, json};

/// A Redis server for this test, removed when the returned container is dropped.
async fn redis() -> Option<(id_effect_kit::test::RedisContainer, RedisClient)> {
  if !id_effect_kit::test::require_docker() {
    return None;
  }
  let container = run(Redis::new().start::<ContainerError, id_effect::HNil>()).await;
  let client = run(RedisClient::connect(RedisConfig::new(container.url()))).await;
  Some((container, client))
}

async fn cached_keys(client: &RedisClient) -> Vec<String> {
  let mut keys: Vec<String> = run(
    client.query(
      redis::cmd("KEYS")
        .arg(format!("{BALANCE_NAMESPACE}:*"))
        .clone(),
    ),
  )
  .await;
  keys.sort();
  keys
}

#[tokio::test]
async fn balances_are_cached_in_redis_until_a_posting() {
  let Some((_container, client)) = redis().await else {
    return;
  };
  let shared = client.clone();
  with_ledger(
    move |_, services| services.redis = Some(shared),
    |l| async move {
      let team = l.team("acme").await;
      let accounts = l.accounts("acme", "acme-accountant", 2).await;
      let (cash, sales) = (accounts[0].id, accounts[1].id);
      l.transfer(&team.accountant, cash, sales, 300).await;
      assert!(cached_keys(&client).await.is_empty());

      let balance: Value = l
        .get(&format!("/accounts/{cash}/balance"), &team.viewer)
        .await
        .json()
        .unwrap();
      assert_eq!(balance["balance"], 300);
      assert_eq!(
        cached_keys(&client).await,
        [format!("{BALANCE_NAMESPACE}:acme:{cash}")]
      );

      // A posting on the account drops its cached balance; the next read refills it.
      l.transfer(&team.accountant, cash, sales, 50).await;
      assert!(cached_keys(&client).await.is_empty());
      let balance: Value = l
        .get(&format!("/accounts/{cash}/balance"), &team.viewer)
        .await
        .json()
        .unwrap();
      assert_eq!(balance["balance"], 350);
      assert_eq!(cached_keys(&client).await.len(), 1);
    },
  )
  .await;
}

#[tokio::test]
async fn the_login_limit_holds_across_instances() {
  let Some((_container, client)) = redis().await else {
    return;
  };
  let shared = client.clone();
  with_ledger(
    move |config, services| {
      config.auth.login_burst = 3.0;
      config.auth.login_per_second = 0.01;
      services.redis = Some(shared);
    },
    |l| async move {
      // A second instance of the service over the same database and Redis.
      let second = ledger::router(
        &l.config,
        Services {
          redis: Some(client),
          ..Services::new(Database::new(Arc::clone(&l.db), vec![]))
        },
      )
      .expect("the app builds");
      let second = TestClient::new(second);
      let body = json!({"username": "nobody", "password": "wrong password"});
      let from = |client: &TestClient<axum::Router>| {
        client
          .post_json("/auth/login", &body)
          .header("x-forwarded-for", "198.51.100.9")
      };
      for _ in 0..3 {
        l.send(from(&l.client))
          .await
          .assert_status(StatusCode::UNAUTHORIZED);
      }
      // The bucket lives in Redis: the other instance has no attempts left for this client.
      l.send(from(&second))
        .await
        .assert_status(StatusCode::TOO_MANY_REQUESTS);
    },
  )
  .await;
}
