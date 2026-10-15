//! The id_effect 0.7 showcase: an axum HTTP service whose whole lifetime is one effect on the
//! fiber runtime.
//!
//! The program is a layer graph (Effect.ts `Layer`):
//!
//! ```text
//! config provider (env)  ──►  AppConfig (Config<T> layer)
//!                                 │
//!                                 ├──►  HostConfig  ──►  server (scoped: bind / graceful drain)
//!                                 └──►  PgSqlClient pool (scoped, only with DATABASE_URL)
//! ```
//!
//! - [`AppConfig::layer`] loads the configuration once, through the `Config<T>` descriptor
//!   ([`config`] module), and provides it as a capability.
//! - [`run_until`] reads that capability, builds the server's [`HostConfig`] layer and, when a
//!   database is configured, the scoped [`PgSqlClient`] pool layer ([`database_layer`]), then
//!   serves [`router`] until its stop effect completes. Leaving the scope drains the server and
//!   closes the pool.
//! - Every request runs as one interruptible fiber on tokio (`id_effect_axum`).
//!
//! Seams for the following leaves: the domain routes (`app`, `routes`, `repo`, `error` modules)
//! extend [`router`] and read the [`SqlClientService`](id_effect_sql::SqlClientService) that
//! [`database_layer`] provides; telemetry (`telemetry` module) wraps [`router`].
//!
//! ```rust,no_run
//! use id_effect::Exit;
//! use id_effect_axum::EffectExecutor;
//!
//! # async fn run() {
//! let program = http_showcase::run_until_shutdown().provide_layer(http_showcase::live_layer());
//! match EffectExecutor::global().run_effect(program).await {
//!   Exit::Success(reason) => println!("stopped: {reason:?}"),
//!   Exit::Failure(cause) => eprintln!("failed: {cause:?}"),
//! }
//! # }
//! ```

#![deny(missing_docs)]

pub mod config;

use axum::Router;
use id_effect::next::{Effect, Layer, caps_next, effect_next, hlist};
use id_effect_axum::{HostConfig, HostError, ShutdownReason, health_router, wait_for_shutdown};
use id_effect_config::{ConfigError, EnvConfigProviderLive};
use id_effect_sql::SqlError;
use id_effect_sql::pg::{PgPoolConfig, PgSqlClient, PgSqlEnv};

pub use config::{AppConfig, DatabaseConfig};

/// Why the service could not start (or stopped serving).
#[derive(Debug)]
pub enum StartupError {
  /// The configuration could not be loaded.
  Config(Box<ConfigError>),
  /// Binding or serving failed.
  Host(HostError),
  /// The database pool could not be opened.
  Sql(SqlError),
}

impl From<ConfigError> for StartupError {
  fn from(err: ConfigError) -> Self {
    Self::Config(Box::new(err))
  }
}

impl From<HostError> for StartupError {
  fn from(err: HostError) -> Self {
    Self::Host(err)
  }
}

impl From<SqlError> for StartupError {
  fn from(err: SqlError) -> Self {
    Self::Sql(err)
  }
}

impl std::fmt::Display for StartupError {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    match self {
      Self::Config(e) => write!(f, "config: {e}"),
      Self::Host(e) => write!(f, "{e}"),
      Self::Sql(e) => write!(f, "database: {e}"),
    }
  }
}

impl std::error::Error for StartupError {}

/// The HTTP routes: `GET /health` (liveness, always `200 ok`).
pub fn router() -> Router {
  Router::new().merge(health_router())
}

/// [`AppConfig`] loaded from the process environment ([`EnvConfigProviderLive`]).
pub fn live_layer() -> Layer<caps_next!(AppConfig), StartupError> {
  EnvConfigProviderLive::layer().provide_to(AppConfig::layer())
}

/// The scoped PostgreSQL pool for `config` ([`PgSqlClient::layer`]): opened and checked when
/// built, closed when the owning scope closes. Provides `SqlClientService` and `PgPool`.
pub fn database_layer(config: &DatabaseConfig) -> Layer<PgSqlEnv, StartupError> {
  PgSqlClient::layer(
    PgPoolConfig::from_url(config.url.expose().clone()).with_max_size(config.max_connections),
  )
}

/// Serves [`router`] with the [`AppConfig`] capability until `stop` completes (then drains the
/// server and closes the pool, returning `stop`'s value), or fails when the server cannot bind
/// or the database cannot be opened.
pub fn run_until<A>(
  stop: Effect<A, HostError, caps_next!(HostConfig)>,
) -> Effect<A, StartupError, caps_next!(AppConfig)>
where
  A: Send + 'static,
{
  effect_next!(|_r: &caps_next!(AppConfig)| {
    let config = AppConfig::clone(~AppConfig);
    ~serve(&config, stop)
  })
}

/// [`run_until`] SIGINT / SIGTERM.
pub fn run_until_shutdown() -> Effect<ShutdownReason, StartupError, caps_next!(AppConfig)> {
  run_until(id_effect::next::from_async(|| async {
    Ok(wait_for_shutdown().await)
  }))
}

/// The closed program for one loaded config: the server (and the pool, when configured) as
/// layers around `id_effect_axum::server::serve_until`.
fn serve<A>(
  config: &AppConfig,
  stop: Effect<A, HostError, caps_next!(HostConfig)>,
) -> Effect<A, StartupError>
where
  A: Send + 'static,
{
  let host: Layer<caps_next!(HostConfig), StartupError> =
    Layer::succeed(hlist![config.host.clone()]);
  let serving = id_effect_axum::server::serve_until(router(), stop).map_err(StartupError::from);
  match &config.database {
    None => serving.provide_layer(host),
    Some(db) => serving.provide_layer(host.merge(database_layer(db))),
  }
}

#[cfg(test)]
mod tests {
  use std::net::SocketAddr;
  use std::time::Duration;

  use id_effect::next::{from_async, need};
  use id_effect::{Cause, Exit};
  use id_effect_axum::{EffectExecutor, ServerHandle};
  use id_effect_config::{ConfigProvider, MapConfigProvider};
  use tokio::io::{AsyncReadExt, AsyncWriteExt};

  use super::*;

  fn local_config(port: u16) -> AppConfig {
    AppConfig {
      host: HostConfig {
        bind_host: "127.0.0.1".into(),
        bind_port: port,
        shutdown_timeout: Duration::from_secs(5),
      },
      request_timeout: config::DEFAULT_REQUEST_TIMEOUT,
      database: None,
    }
  }

  async fn get(addr: SocketAddr, path: &str) -> std::io::Result<String> {
    let mut stream = tokio::net::TcpStream::connect(addr).await?;
    stream
      .write_all(
        format!("GET {path} HTTP/1.1\r\nHost: test\r\nConnection: close\r\n\r\n").as_bytes(),
      )
      .await?;
    let mut out = String::new();
    stream.read_to_string(&mut out).await?;
    Ok(out)
  }

  #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
  async fn get_health_is_200() {
    let host: Layer<caps_next!(HostConfig), HostError> =
      Layer::succeed(hlist![local_config(0).host]);
    let probe: Effect<String, HostError, caps_next!(ServerHandle)> = need::<ServerHandle>()
      .flat_map(|h| {
        let addr = h.local_addr();
        from_async(move || async move {
          get(addr, "/health")
            .await
            .map_err(|e| HostError::Serve(e.to_string()))
        })
      });
    let program = probe.provide_layer(host.provide_to(id_effect_axum::server_layer(router())));
    let Exit::Success(response) = EffectExecutor::global().run_effect(program).await else {
      panic!("server probe failed");
    };
    assert!(response.starts_with("HTTP/1.1 200"), "{response}");
    assert!(response.ends_with("ok"), "{response}");
  }

  /// `GET /health` until the server (started concurrently) accepts the connection.
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

  #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
  async fn get_health_is_200_through_the_config_layer() {
    let port = {
      let probe = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
      probe.local_addr().unwrap().port()
    };
    let port_str = port.to_string();
    let app = MapConfigProvider::from_pairs([("HOST", "127.0.0.1"), ("PORT", port_str.as_str())])
      .layer()
      .provide_to(AppConfig::layer());
    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    let stop = from_async(move || async move {
      get_when_up(addr, "/health")
        .await
        .map_err(|e| HostError::Serve(e.to_string()))
    });
    let exit = EffectExecutor::global()
      .run_effect(run_until(stop).provide_layer(app))
      .await;
    let Exit::Success(response) = exit else {
      panic!("health probe failed: {exit:?}");
    };
    assert!(response.starts_with("HTTP/1.1 200"), "{response}");
    // Released with the scope: the listener is gone.
    assert!(get(addr, "/health").await.is_err());
  }

  #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
  async fn an_invalid_config_fails_the_build() {
    let app = MapConfigProvider::from_pairs([("PORT", "nope")])
      .layer()
      .provide_to(AppConfig::layer());
    let program = run_until(id_effect::next::succeed(())).provide_layer(app);
    let exit = EffectExecutor::global().run_effect(program).await;
    assert!(
      matches!(&exit, Exit::Failure(Cause::Fail(StartupError::Config(_)))),
      "{exit:?}"
    );
  }

  #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
  async fn an_unreachable_database_fails_startup() {
    let mut config = local_config(0);
    config.database = Some(DatabaseConfig {
      url: id_effect_config::Secret::new("postgres://127.0.0.1:1/none".into()),
      max_connections: 1,
    });
    let program =
      run_until(id_effect::next::succeed(())).provide_layer(Layer::succeed(hlist![config]));
    let exit = EffectExecutor::global().run_effect(program).await;
    assert!(
      matches!(&exit, Exit::Failure(Cause::Fail(StartupError::Sql(_)))),
      "{exit:?}"
    );
  }

  #[test]
  fn startup_error_display() {
    let err = StartupError::from(HostError::Serve("bind".into()));
    assert_eq!(err.to_string(), "host serve: bind");
    let err = StartupError::from(ConfigError::Invalid {
      path: "PORT".into(),
      value: "x".into(),
      reason: "bad".into(),
    });
    assert!(err.to_string().starts_with("config:"), "{err}");
  }
}
