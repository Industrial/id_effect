//! Host builder and run-until-shutdown entry point.

use std::future::Future;
use std::net::SocketAddr;
use std::sync::Arc;

use axum::Router;
use id_effect::Env;

use crate::server::bootstrap::{HostConfig, bootstrap_env, load_host_config};
use crate::server::shutdown::{HostDrain, drain_with_timeout, wait_for_shutdown};

/// Host lifecycle failures.
#[derive(Debug)]
pub enum HostError {
  /// Config could not be loaded.
  Config(Box<id_effect_config::ConfigError>),
  /// The serve future failed.
  Serve(String),
}

impl From<id_effect_config::ConfigError> for HostError {
  fn from(err: id_effect_config::ConfigError) -> Self {
    Self::Config(Box::new(err))
  }
}

impl std::fmt::Display for HostError {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    match self {
      Self::Config(e) => write!(f, "host config: {e}"),
      Self::Serve(msg) => write!(f, "host serve: {msg}"),
    }
  }
}

impl std::error::Error for HostError {}

/// Built host runtime context.
#[derive(Clone, Debug)]
pub struct Host {
  /// Loaded configuration.
  pub config: HostConfig,
  /// Capability environment (config provider at minimum).
  pub env: Env,
  /// In-flight work counter for graceful shutdown.
  pub drain: Arc<HostDrain>,
}

/// Fluent builder for [`Host`].
#[derive(Clone, Debug, Default)]
pub struct HostBuilder {
  env: Option<Env>,
}

impl HostBuilder {
  /// Start a new builder.
  #[inline]
  pub fn new() -> Self {
    Self::default()
  }

  /// Pre-seed the capability environment (defaults to env-only bootstrap).
  #[inline]
  pub fn with_env(mut self, env: Env) -> Self {
    self.env = Some(env);
    self
  }

  /// Load config and assemble the host context.
  pub async fn build(self) -> Result<Host, HostError> {
    let env = self
      .env
      .unwrap_or_else(|| bootstrap_env(id_effect_config::EnvConfigProvider::from_env()));
    let config = id_effect::run_async(load_host_config(), env.clone())
      .await
      .map_err(|e| HostError::Config(Box::new(e)))?;
    Ok(Host {
      config,
      env,
      drain: Arc::new(HostDrain::new()),
    })
  }

  /// Build the host, run `serve` concurrently with shutdown signals, then drain.
  pub async fn run_until_shutdown<F, Fut>(self, serve: F) -> Result<(), HostError>
  where
    F: FnOnce(Host) -> Fut,
    Fut: Future<Output = Result<(), HostError>>,
  {
    let host = self.build().await?;
    let timeout = host.config.shutdown_timeout;
    let drain = Arc::clone(&host.drain);

    tokio::select! {
      res = serve(host) => res?,
      _reason = wait_for_shutdown() => {},
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
#[allow(clippy::result_large_err)]
pub async fn serve_router(host: Host, app: Router) -> Result<(), HostError> {
  let addr = socket_addr(&host.config)?;
  let listener = tokio::net::TcpListener::bind(addr)
    .await
    .map_err(|e| HostError::Serve(e.to_string()))?;
  let timeout = host.config.shutdown_timeout;
  let drain = host.drain;

  axum::serve(listener, app)
    .with_graceful_shutdown(async move {
      wait_for_shutdown().await;
      drain_with_timeout(&drain, timeout).await;
    })
    .await
    .map_err(|e| HostError::Serve(e.to_string()))
}

#[cfg(test)]
mod tests {
  use super::*;
  use id_effect_config::MapConfigProvider;

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
  fn host_error_from_config_error() {
    let cfg_err = id_effect_config::ConfigError::Invalid {
      path: "PORT".into(),
      value: "0".into(),
      reason: "bad".into(),
    };
    let host_err = HostError::from(cfg_err);
    assert!(matches!(host_err, HostError::Config(_)));
    assert!(!format!("{host_err:?}").is_empty());
  }

  #[test]
  fn socket_addr_formats_host_and_port() {
    let cfg = HostConfig {
      bind_host: "127.0.0.1".into(),
      bind_port: 9090,
      shutdown_timeout: std::time::Duration::from_secs(1),
    };
    let addr = socket_addr(&cfg).unwrap();
    assert_eq!(addr.port(), 9090);
  }
}
