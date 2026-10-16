//! [`run_until`]: the whole service for the loaded configuration.

use id_effect::{Effect, Layer, caps, effect, hlist};
use id_effect_axum::{HostConfig, HostError};

use super::{StartupError, repository_layer, serve_router};
use crate::application::{RequestPolicy, new_app_env};
use crate::domain::todo::TodoRepositoryService;
use crate::infrastructure::config::AppConfig;
use crate::infrastructure::telemetry::instrument;
use crate::interface::http::router;

/// Serves the [`router`] with the [`AppConfig`] capability until `stop` completes, then drains
/// the server and closes the store, returning `stop`'s value. Fails when the server cannot bind
/// or the store cannot be opened.
///
/// The layer graph: `HostConfig` (from the config) merged with the [`repository_layer`] the
/// config selects; inside, the [`router`] over [`new_app_env`] (the store and a
/// [`RequestPolicy`] with `REQUEST_TIMEOUT`), wrapped in request telemetry ([`instrument`]).
pub fn run_until<A: Send + 'static>(
  stop: Effect<A, HostError, caps!(HostConfig)>,
) -> Effect<A, StartupError, caps!(AppConfig)> {
  effect!(|_r: &caps!(AppConfig)| {
    let config = AppConfig::clone(~AppConfig);
    let host: Layer<caps!(HostConfig), StartupError> = Layer::succeed(hlist![config.host.clone()]);
    let policy = RequestPolicy::with_timeout(config.request_timeout);
    let store = if config.database.is_some() { "postgres" } else { "memory" };
    let serving = effect!(|_r: &caps!(TodoRepositoryService, HostConfig)| {
      let repository = TodoRepositoryService::clone(~TodoRepositoryService);
      let app = instrument(router(new_app_env(repository, policy)));
      ~serve_router(app, store, stop).map_err(StartupError::from)
    });
    ~serving.provide_layer(host.merge(repository_layer(&config)))
  })
}

#[cfg(test)]
mod tests {
  use std::net::SocketAddr;
  use std::time::Duration;

  use id_effect::{Cause, Exit, succeed};
  use id_effect_axum::EffectExecutor;
  use id_effect_config::{ConfigProvider, MapConfigProvider, Secret};
  use tokio::io::{AsyncReadExt, AsyncWriteExt};

  use super::*;
  use crate::infrastructure::config::DatabaseConfig;

  async fn get(addr: SocketAddr, path: &str) -> std::io::Result<String> {
    let mut stream = tokio::net::TcpStream::connect(addr).await?;
    let request = format!("GET {path} HTTP/1.1\r\nHost: test\r\nConnection: close\r\n\r\n");
    stream.write_all(request.as_bytes()).await?;
    let mut out = String::new();
    stream.read_to_string(&mut out).await?;
    Ok(out)
  }

  /// `get` until the server (started concurrently) accepts the connection.
  async fn get_when_up(addr: SocketAddr, path: &str) -> std::io::Result<String> {
    let mut last = None;
    for _ in 0..100 {
      match get(addr, path).await {
        Ok(body) => return Ok(body),
        Err(e) => last = Some(e),
      }
      tokio::time::sleep(Duration::from_millis(20)).await;
    }
    Err(last.unwrap_or_else(|| std::io::Error::other("no attempt")))
  }

  fn free_port() -> u16 {
    let probe = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    probe.local_addr().unwrap().port()
  }

  #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
  async fn serves_health_and_memory_todos_through_the_config_layer() {
    let port = free_port();
    let port_str = port.to_string();
    let app = MapConfigProvider::from_pairs([("HOST", "127.0.0.1"), ("PORT", port_str.as_str())])
      .layer()
      .provide_to(AppConfig::layer());
    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    let stop = id_effect::from_async(move || async move {
      let health = get_when_up(addr, "/health").await;
      // Without DATABASE_URL the todos are served from memory.
      let todos = get(addr, "/todos").await;
      health
        .and_then(|h| todos.map(|t| (h, t)))
        .map_err(|e| HostError::Serve(e.to_string()))
    });
    let exit = EffectExecutor::global()
      .run_effect(run_until(stop).provide_layer(app))
      .await;
    let Exit::Success((health, todos)) = exit else {
      panic!("probe failed: {exit:?}");
    };
    assert!(health.starts_with("HTTP/1.1 200"), "{health}");
    assert!(todos.starts_with("HTTP/1.1 200"), "{todos}");
    assert!(todos.ends_with("[]"), "{todos}");
    // Released with the scope: the listener is gone.
    assert!(get(addr, "/health").await.is_err());
  }

  #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
  async fn an_invalid_config_fails_the_build() {
    let app = MapConfigProvider::from_pairs([("PORT", "nope")])
      .layer()
      .provide_to(AppConfig::layer());
    let program = run_until(succeed(())).provide_layer(app);
    let exit = EffectExecutor::global().run_effect(program).await;
    assert!(
      matches!(&exit, Exit::Failure(Cause::Fail(StartupError::Config(_)))),
      "{exit:?}"
    );
  }

  #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
  async fn an_unreachable_database_fails_startup() {
    let config = AppConfig {
      host: HostConfig {
        bind_host: "127.0.0.1".into(),
        bind_port: 0,
        shutdown_timeout: Duration::from_secs(5),
      },
      request_timeout: AppConfig::DEFAULT_REQUEST_TIMEOUT,
      database: Some(DatabaseConfig {
        url: Secret::new("postgres://127.0.0.1:1/none".into()),
        max_connections: 1,
      }),
    };
    let program = run_until(succeed(())).provide_layer(Layer::succeed(hlist![config]));
    let exit = EffectExecutor::global().run_effect(program).await;
    assert!(
      matches!(&exit, Exit::Failure(Cause::Fail(StartupError::Sql(_)))),
      "{exit:?}"
    );
  }
}
