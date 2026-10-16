//! Shared by the integration tests: a Redis 8 container (testcontainers) and an effect runner.
//!
//! Without Docker a test prints `SKIP` and passes, unless `ID_EFFECT_REQUIRE_DOCKER=1` (the CI
//! `containers-integration` job), where a missing Docker daemon fails it.

#![allow(dead_code)]

use std::fmt::Debug;

use id_effect::{Cause, Effect, Exit};
use id_effect_redis::{RedisClient, RedisConfig};
use id_effect_tokio::TokioRuntime;
use testcontainers::core::{IntoContainerPort, WaitFor};
use testcontainers::runners::AsyncRunner;
use testcontainers::{ContainerAsync, GenericImage};

/// Run `eff` on the test's tokio runtime: its success, its typed failure, or a panic.
pub async fn run<A, E>(eff: Effect<A, E>) -> Result<A, E>
where
  A: Send + 'static,
  E: Send + Debug + 'static,
{
  match TokioRuntime::current().expect("tokio").run(eff).await {
    Exit::Success(a) => Ok(a),
    Exit::Failure(Cause::Fail(e)) => Err(e),
    Exit::Failure(c) => panic!("unexpected cause: {c:?}"),
  }
}

/// [`run`], which must succeed.
pub async fn ok<A, E>(eff: Effect<A, E>) -> A
where
  A: Send + 'static,
  E: Send + Debug + 'static,
{
  run(eff).await.expect("effect failed")
}

/// A running Redis 8 server (RedisJSON included) and its URL, or `None` (after printing SKIP)
/// without Docker.
pub async fn redis_server(test: &str) -> Option<(ContainerAsync<GenericImage>, String)> {
  let started = GenericImage::new("redis", "8.2")
    .with_exposed_port(6379.tcp())
    .with_wait_for(WaitFor::message_on_stdout("Ready to accept connections"))
    .start()
    .await;
  let container = match started {
    Ok(container) => container,
    Err(e) if std::env::var("ID_EFFECT_REQUIRE_DOCKER").as_deref() == Ok("1") => {
      panic!("{test}: ID_EFFECT_REQUIRE_DOCKER=1 but the Redis container did not start: {e}")
    }
    Err(e) => {
      eprintln!("SKIP {test}: Docker is not available ({e})");
      return None;
    }
  };
  let host = container.get_host().await.expect("host");
  let port = container.get_host_port_ipv4(6379).await.expect("port");
  Some((container, format!("redis://{host}:{port}")))
}

/// A server and a client connected to it (2 pooled connections), or `None` without Docker.
pub async fn redis_client(test: &str) -> Option<(ContainerAsync<GenericImage>, RedisClient)> {
  let (container, url) = redis_server(test).await?;
  let client = ok(RedisClient::connect(RedisConfig::new(url).pool_size(2))).await;
  Some((container, client))
}
