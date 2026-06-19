//! Host builder and run-until-shutdown entry point.

use std::future::Future;
use std::sync::Arc;

use id_effect::Env;

use crate::bootstrap::{HostConfig, bootstrap_env, load_host_config};
use crate::shutdown::{HostDrain, drain_with_timeout, wait_for_shutdown};

/// Host lifecycle failures.
#[derive(Debug)]
pub enum HostError {
  /// Config could not be loaded.
  Config(id_effect_config::ConfigError),
  /// The serve future failed.
  Serve(String),
}

impl From<id_effect_config::ConfigError> for HostError {
  fn from(err: id_effect_config::ConfigError) -> Self {
    Self::Config(err)
  }
}

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
      .map_err(HostError::Config)?;
    Ok(Host {
      config,
      env,
      drain: Arc::new(HostDrain::new()),
    })
  }

  /// Build the host, run `serve` until a shutdown signal, then drain.
  pub async fn run_until_shutdown<F, Fut>(self, serve: F) -> Result<(), HostError>
  where
    F: FnOnce(Host) -> Fut,
    Fut: Future<Output = Result<(), HostError>>,
  {
    let host = self.build().await?;
    let timeout = host.config.shutdown_timeout;
    let serve_result = serve(host.clone()).await;
    let _reason = wait_for_shutdown().await;
    drain_with_timeout(&host.drain, timeout).await;
    serve_result
  }
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
}
