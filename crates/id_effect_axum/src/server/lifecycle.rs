//! The HTTP server's lifetime: a scoped [`Layer`] ([`server_layer`], launched by [`serve`] /
//! [`serve_until_shutdown`]) and the plain async host API ([`HostBuilder`], [`serve_router`]).
//!
//! ```rust,no_run
//! use axum::Router;
//! use id_effect::Effect;
//! use id_effect_axum::{HostConfig, HostError, ShutdownReason, provide_host_config_env, serve_until_shutdown};
//!
//! # async fn run() {
//! let app = Router::new().merge(id_effect_axum::health_router());
//! // HOST / PORT / SHUTDOWN_TIMEOUT_SECS from the process environment.
//! let config = provide_host_config_env::<HostError>().provide_to(HostConfig::layer());
//! let program: Effect<ShutdownReason, HostError> =
//!   serve_until_shutdown(app).provide_layer(config);
//! let exit = id_effect_axum::EffectExecutor::global().run_effect(program).await;
//! # let _ = exit;
//! # }
//! ```

use std::convert::Infallible;
use std::future::Future;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use axum::Router;
use id_effect::{Cause, Exit};
use id_effect::{Effect, HCons, HNil, Layer, from_async, hlist, need};
use id_effect_config::{ConfigEnv, ConfigError};
use tokio::sync::Notify;
use tokio::task::JoinHandle;

use crate::health::ReadinessState;
use crate::server::bootstrap::{HostConfig, bootstrap_env, load_host_config};
use crate::server::connection::{ConnectionLimits, serve_listener};
use crate::server::shutdown::{HostDrain, ShutdownReason, drain_with_timeout, wait_for_shutdown};

/// Host lifecycle failures.
#[derive(Debug)]
pub enum HostError {
  /// Config could not be loaded.
  Config(Box<ConfigError>),
  /// Binding or serving failed.
  Serve(String),
  /// The host program died (a defect) or was interrupted.
  Defect(String),
}

impl From<ConfigError> for HostError {
  fn from(err: ConfigError) -> Self {
    Self::Config(Box::new(err))
  }
}

impl std::fmt::Display for HostError {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    match self {
      Self::Config(e) => write!(f, "host config: {e}"),
      Self::Serve(msg) => write!(f, "host serve: {msg}"),
      Self::Defect(msg) => write!(f, "host defect: {msg}"),
    }
  }
}

impl std::error::Error for HostError {}

impl HostError {
  /// The host error for a failed host effect (`Fail(e)` → `e.into()`, otherwise
  /// [`HostError::Defect`]).
  pub fn from_cause<E: Into<HostError>>(cause: Cause<E>) -> Self {
    match crate::split_cause(cause) {
      Ok(e) => e.into(),
      Err(rest) => Self::Defect(
        crate::first_defect(&rest).map_or_else(|| "interrupted".to_owned(), str::to_owned),
      ),
    }
  }
}

/// Built host runtime context.
#[derive(Clone, Debug)]
pub struct Host {
  /// Loaded configuration.
  pub config: HostConfig,
  /// Capability environment (the config provider).
  pub env: ConfigEnv,
  /// In-flight work counter for graceful shutdown.
  pub drain: Arc<HostDrain>,
  /// Readiness (ready once built); shutdown marks it shutting down before draining. Wire it
  /// into `/ready` with [`HealthRegistry::with_readiness`](crate::HealthRegistry::with_readiness).
  pub readiness: ReadinessState,
}

/// Fluent builder for [`Host`].
#[derive(Clone, Debug, Default)]
pub struct HostBuilder {
  env: Option<ConfigEnv>,
}

impl HostBuilder {
  /// Start a new builder.
  #[inline]
  pub fn new() -> Self {
    Self::default()
  }

  /// Pre-seed the config environment (defaults to the process environment).
  #[inline]
  pub fn with_env(mut self, env: ConfigEnv) -> Self {
    self.env = Some(env);
    self
  }

  /// Load config and assemble the host context.
  pub async fn build(self) -> Result<Host, HostError> {
    let env = self
      .env
      .unwrap_or_else(|| bootstrap_env(id_effect_config::EnvConfigProvider::from_env()));
    let load = load_host_config::<ConfigEnv, _>().provide_all(env.clone());
    match crate::EffectExecutor::global().run_effect(load).await {
      Exit::Success(config) => Ok(Host {
        config,
        env,
        drain: Arc::new(HostDrain::new()),
        readiness: ReadinessState::new(true),
      }),
      Exit::Failure(cause) => Err(HostError::from_cause(cause)),
    }
  }

  /// Build the host, run `serve` concurrently with shutdown signals, then drain.
  ///
  /// On a signal, [`Host::readiness`] is marked shutting down first; `serve` keeps running
  /// while the drain waits, so `/ready` answers `503` as in-flight work finishes.
  pub async fn run_until_shutdown<F, Fut>(self, serve: F) -> Result<(), HostError>
  where
    F: FnOnce(Host) -> Fut,
    Fut: Future<Output = Result<(), HostError>>,
  {
    let host = self.build().await?;
    let timeout = host.config.shutdown_timeout;
    let drain = Arc::clone(&host.drain);
    let readiness = host.readiness.clone();
    let mut serving = std::pin::pin!(serve(host));

    let signalled = tokio::select! {
      res = &mut serving => { res?; false },
      _reason = wait_for_shutdown() => true,
    };
    readiness.mark_shutting_down();
    if signalled {
      tokio::select! {
        res = &mut serving => res?,
        () = drain_with_timeout(&drain, timeout) => return Ok(()),
      }
    }

    drain_with_timeout(&drain, timeout).await;
    Ok(())
  }
}

/// Bind address from [`HostConfig`].
#[inline]
#[allow(clippy::result_large_err)]
pub fn socket_addr(config: &HostConfig) -> Result<SocketAddr, HostError> {
  format!("{}:{}", config.bind_host, config.bind_port)
    .parse()
    .map_err(|e| HostError::Serve(format!("bind address: {e}")))
}

/// Run an Axum [`Router`] until SIGINT/SIGTERM with graceful connection drain.
///
/// On a signal, [`Host::readiness`] is marked shutting down first; the listener keeps
/// accepting while [`Host::drain`] drains, then stops. Count the requests in it with
/// [`track_in_flight`](crate::track_in_flight). Connections are bounded by the default
/// [`ConnectionLimits`] and every request carries `ConnectInfo<SocketAddr>`.
#[allow(clippy::result_large_err)]
pub async fn serve_router(host: Host, app: Router) -> Result<(), HostError> {
  serve_router_with_limits(host, app, ConnectionLimits::default()).await
}

/// [`serve_router`] within `limits`.
#[allow(clippy::result_large_err)]
pub async fn serve_router_with_limits(
  host: Host,
  app: Router,
  limits: ConnectionLimits,
) -> Result<(), HostError> {
  let addr = socket_addr(&host.config)?;
  let listener = tokio::net::TcpListener::bind(addr)
    .await
    .map_err(|e| HostError::Serve(e.to_string()))?;
  let timeout = host.config.shutdown_timeout;
  let drain = host.drain;
  let readiness = host.readiness;

  serve_listener(listener, app, limits, async move {
    wait_for_shutdown().await;
    readiness.mark_shutting_down();
    drain_with_timeout(&drain, timeout).await;
  })
  .await
  .map_err(|e| HostError::Serve(e.to_string()))
}

/// A running HTTP server: the capability [`server_layer`] provides.
#[derive(Debug)]
pub struct ServerHandle {
  local_addr: SocketAddr,
  shutdown_timeout: Duration,
  readiness: ReadinessState,
  stop: Arc<Notify>,
  task: Mutex<Option<JoinHandle<std::io::Result<()>>>>,
}

impl ServerHandle {
  /// Binds `config`'s address and serves `app` on a tokio task (call inside a tokio runtime).
  ///
  /// # Errors
  ///
  /// [`HostError::Serve`] when the address is invalid or cannot be bound.
  pub async fn start(config: &HostConfig, app: Router) -> Result<Self, HostError> {
    Self::start_with_readiness(config, app, ReadinessState::new(true)).await
  }

  /// Like [`start`](Self::start), with the [`ReadinessState`] that [`shutdown`](Self::shutdown)
  /// marks shutting down (share it with the app's `/ready` route).
  ///
  /// # Errors
  ///
  /// [`HostError::Serve`] when the address is invalid or cannot be bound.
  pub async fn start_with_readiness(
    config: &HostConfig,
    app: Router,
    readiness: ReadinessState,
  ) -> Result<Self, HostError> {
    Self::start_with_limits(config, app, readiness, ConnectionLimits::default()).await
  }

  /// Like [`start_with_readiness`](Self::start_with_readiness), serving connections within
  /// `limits` instead of the default [`ConnectionLimits`].
  ///
  /// # Errors
  ///
  /// [`HostError::Serve`] when the address is invalid or cannot be bound.
  pub async fn start_with_limits(
    config: &HostConfig,
    app: Router,
    readiness: ReadinessState,
    limits: ConnectionLimits,
  ) -> Result<Self, HostError> {
    let addr = socket_addr(config)?;
    let listener = tokio::net::TcpListener::bind(addr)
      .await
      .map_err(|e| HostError::Serve(format!("bind {addr}: {e}")))?;
    let local_addr = listener
      .local_addr()
      .map_err(|e| HostError::Serve(e.to_string()))?;
    let stop = Arc::new(Notify::new());
    let signal = Arc::clone(&stop);
    let task = tokio::spawn(serve_listener(listener, app, limits, async move {
      signal.notified().await;
    }));
    Ok(Self {
      local_addr,
      shutdown_timeout: config.shutdown_timeout,
      readiness,
      stop,
      task: Mutex::new(Some(task)),
    })
  }

  /// The bound address (useful with port `0`).
  pub fn local_addr(&self) -> SocketAddr {
    self.local_addr
  }

  /// The server's readiness (marked shutting down by [`shutdown`](Self::shutdown)).
  pub fn readiness(&self) -> &ReadinessState {
    &self.readiness
  }

  /// Marks the readiness shutting down, then stops accepting connections and waits up to the
  /// configured shutdown timeout for open ones to finish; aborts the server task after that.
  /// Idempotent.
  pub async fn shutdown(&self) {
    self.readiness.mark_shutting_down();
    self.stop.notify_one();
    let task = self
      .task
      .lock()
      .unwrap_or_else(PoisonError::into_inner)
      .take();
    if let Some(mut task) = task
      && tokio::time::timeout(self.shutdown_timeout, &mut task)
        .await
        .is_err()
    {
      task.abort();
      let _ = task.await;
    }
  }
}

/// The HTTP server as a scoped layer (Effect.ts `Layer.scoped`): acquiring it binds the
/// [`HostConfig`] address and starts serving `app`; releasing it (when the owning scope closes:
/// after a [`provide_layer`](Effect::provide_layer)ed effect, or when a [`serve`] is interrupted)
/// shuts the server down gracefully within [`HostConfig::shutdown_timeout`].
pub fn server_layer(
  app: Router,
) -> Layer<HCons<ServerHandle, HNil>, HostError, HCons<HostConfig, HNil>> {
  server_layer_with_readiness(app, ReadinessState::new(true))
}

/// [`server_layer`] whose [`ServerHandle`] uses `readiness` (see
/// [`ServerHandle::start_with_readiness`]): releasing the layer marks it shutting down first.
pub fn server_layer_with_readiness(
  app: Router,
  readiness: ReadinessState,
) -> Layer<HCons<ServerHandle, HNil>, HostError, HCons<HostConfig, HNil>> {
  server_layer_with_limits(app, readiness, ConnectionLimits::default())
}

/// [`server_layer_with_readiness`] serving connections within `limits` (see
/// [`ServerHandle::start_with_limits`]).
pub fn server_layer_with_limits(
  app: Router,
  readiness: ReadinessState,
  limits: ConnectionLimits,
) -> Layer<HCons<ServerHandle, HNil>, HostError, HCons<HostConfig, HNil>> {
  Layer::acquire_release(
    move || {
      let app = app.clone();
      let readiness = readiness.clone();
      let limits = limits.clone();
      need::<HostConfig>().flat_map(move |config| {
        from_async(move || async move {
          ServerHandle::start_with_limits(&config, app, readiness, limits)
            .await
            .map(|handle| hlist![handle])
        })
      })
    },
    |out: HCons<ServerHandle, HNil>, _kind| {
      let handle = Arc::clone(&out.0);
      from_async(move || async move {
        handle.shutdown().await;
        Ok(())
      })
    },
  )
}

/// Serves `app` until interrupted ([`Layer::launch`] of [`server_layer`]); interruption shuts the
/// server down gracefully. Fails when the address cannot be bound.
pub fn serve(app: Router) -> Effect<Infallible, HostError, HCons<HostConfig, HNil>> {
  server_layer(app).launch()
}

/// [`serve`] within `limits` instead of the default [`ConnectionLimits`].
pub fn serve_with_limits(
  app: Router,
  limits: ConnectionLimits,
) -> Effect<Infallible, HostError, HCons<HostConfig, HNil>> {
  server_layer_with_limits(app, ReadinessState::new(true), limits).launch()
}

/// Serves `app` until `stop` completes (then shuts the server down and returns `stop`'s value),
/// or fails when serving fails.
pub fn serve_until<A>(
  app: Router,
  stop: Effect<A, HostError, HCons<HostConfig, HNil>>,
) -> Effect<A, HostError, HCons<HostConfig, HNil>>
where
  A: Send + 'static,
{
  serve(app).map(|never| match never {}).race_first(stop)
}

/// Serves `app` until SIGINT / SIGTERM ([`wait_for_shutdown`]), then shuts down gracefully.
pub fn serve_until_shutdown(
  app: Router,
) -> Effect<ShutdownReason, HostError, HCons<HostConfig, HNil>> {
  serve_until(app, from_async(|| async { Ok(wait_for_shutdown().await) }))
}

/// [`serve_until_shutdown`] within `limits` instead of the default [`ConnectionLimits`].
pub fn serve_until_shutdown_with_limits(
  app: Router,
  limits: ConnectionLimits,
) -> Effect<ShutdownReason, HostError, HCons<HostConfig, HNil>> {
  serve_with_limits(app, limits)
    .map(|never| match never {})
    .race_first(from_async(|| async { Ok(wait_for_shutdown().await) }))
}

#[cfg(test)]
mod tests {
  use super::*;
  use id_effect::{sleep, succeed};
  use id_effect_config::MapConfigProvider;
  use tokio::io::{AsyncReadExt, AsyncWriteExt};

  #[tokio::test]
  async fn build_loads_defaults() {
    let env = bootstrap_env(MapConfigProvider::from_pairs(std::iter::empty::<(
      &str,
      &str,
    )>()));
    let host = HostBuilder::new().with_env(env).build().await.unwrap();
    assert_eq!(host.config.bind_port, 8080);
  }

  #[tokio::test]
  async fn custom_bind_port_from_config() {
    let env = bootstrap_env(MapConfigProvider::from_pairs([("PORT", "9090")]));
    let host = HostBuilder::new().with_env(env).build().await.unwrap();
    assert_eq!(host.config.bind_port, 9090);
  }

  #[tokio::test]
  async fn build_rejects_invalid_port() {
    let env = bootstrap_env(MapConfigProvider::from_pairs([("PORT", "0")]));
    let err = HostBuilder::new().with_env(env).build().await.unwrap_err();
    assert!(matches!(err, HostError::Config(_)));
  }

  #[test]
  fn host_error_conversions_and_display() {
    let cfg_err = ConfigError::Invalid {
      path: "PORT".into(),
      value: "0".into(),
      reason: "bad".into(),
    };
    let host_err = HostError::from(cfg_err);
    assert!(matches!(host_err, HostError::Config(_)));
    assert!(host_err.to_string().starts_with("host config"));
    let died = HostError::from_cause::<HostError>(Cause::die("boom"));
    assert_eq!(died.to_string(), "host defect: boom");
    let failed = HostError::from_cause(Cause::Fail(HostError::Serve("x".into())));
    assert_eq!(failed.to_string(), "host serve: x");
  }

  #[test]
  fn socket_addr_formats_host_and_port() {
    let cfg = HostConfig {
      bind_host: "127.0.0.1".into(),
      bind_port: 9090,
      shutdown_timeout: Duration::from_secs(1),
    };
    let addr = socket_addr(&cfg).unwrap();
    assert_eq!(addr.port(), 9090);
    let bad = HostConfig {
      bind_host: "not an ip".into(),
      ..cfg
    };
    assert!(matches!(socket_addr(&bad), Err(HostError::Serve(_))));
  }

  fn local(port: u16) -> Layer<HCons<HostConfig, HNil>, HostError> {
    Layer::succeed(hlist![HostConfig {
      bind_host: "127.0.0.1".into(),
      bind_port: port,
      shutdown_timeout: Duration::from_secs(5),
    }])
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
  async fn server_layer_serves_while_provided_and_stops_on_release() {
    let app = crate::health_router();
    let probe: Effect<(SocketAddr, String), HostError, HCons<ServerHandle, HNil>> =
      need::<ServerHandle>().flat_map(|h| {
        let addr = h.local_addr();
        from_async(move || async move {
          get(addr, "/health")
            .await
            .map(|body| (addr, body))
            .map_err(|e| HostError::Serve(e.to_string()))
        })
      });
    let program = probe.provide_layer(local(0).provide_to(server_layer(app)));
    let exit = crate::EffectExecutor::global().run_effect(program).await;
    let Exit::Success((addr, response)) = exit else {
      panic!("server probe failed: {exit:?}");
    };
    assert!(response.starts_with("HTTP/1.1 200"), "{response}");
    // Released with the scope: the listener is gone.
    assert!(get(addr, "/health").await.is_err());
  }

  #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
  async fn serve_until_returns_the_stop_value() {
    let program = serve_until(
      crate::health_router(),
      sleep(Duration::from_millis(20)).then(succeed("stopped")),
    )
    .provide_layer(local(0));
    let exit = crate::EffectExecutor::global().run_effect(program).await;
    assert!(matches!(exit, Exit::Success("stopped")), "{exit:?}");
  }

  #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
  async fn serve_fails_when_the_address_is_taken() {
    let taken = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = taken.local_addr().unwrap().port();
    let program = serve(crate::health_router()).provide_layer(local(port));
    let exit = crate::EffectExecutor::global().run_effect(program).await;
    assert!(
      matches!(&exit, Exit::Failure(Cause::Fail(HostError::Serve(_)))),
      "{exit:?}"
    );
  }
}
