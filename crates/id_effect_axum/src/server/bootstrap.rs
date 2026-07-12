//! Config bootstrap via [`id_effect_config`].

use std::time::Duration;

use id_effect::Needs;
use id_effect::kernel::Effect;
use id_effect::{Env, ProviderBox};
use id_effect_config::{
  Config, ConfigError, ConfigProviderService, config, config_env, provide_env_config_provider,
};

/// Host bind and shutdown settings loaded from the environment.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HostConfig {
  /// Bind address (e.g. `0.0.0.0`).
  pub bind_host: String,
  /// Listen port.
  pub bind_port: u16,
  /// Graceful shutdown drain timeout.
  pub shutdown_timeout: Duration,
}

impl HostConfig {
  /// Compose the lazy config descriptor for host settings.
  #[inline]
  pub fn descriptor() -> Config<(String, i64, i64)> {
    config::all3(
      Config::string("HOST").with_default("0.0.0.0".to_string()),
      Config::integer("PORT").with_default(8080),
      Config::integer("SHUTDOWN_TIMEOUT_SECS").with_default(30),
    )
  }

  #[allow(clippy::result_large_err)]
  fn from_tuple((bind_host, port, timeout_secs): (String, i64, i64)) -> Result<Self, ConfigError> {
    if !(1..=u16::MAX as i64).contains(&port) {
      return Err(ConfigError::Invalid {
        path: "PORT".into(),
        value: port.to_string(),
        reason: "port must be between 1 and 65535".into(),
      });
    }
    if timeout_secs < 0 {
      return Err(ConfigError::Invalid {
        path: "SHUTDOWN_TIMEOUT_SECS".into(),
        value: timeout_secs.to_string(),
        reason: "timeout must be non-negative".into(),
      });
    }
    Ok(Self {
      bind_host,
      bind_port: port as u16,
      shutdown_timeout: Duration::from_secs(timeout_secs as u64),
    })
  }
}

/// Load [`HostConfig`] using the installed [`id_effect_config::ConfigProvider`] capability.
#[inline]
pub fn load_host_config<R>() -> Effect<HostConfig, ConfigError, R>
where
  R: Needs<ConfigProviderService> + 'static,
{
  HostConfig::descriptor()
    .map_attempt(HostConfig::from_tuple)
    .run::<HostConfig, ConfigError, R>()
}

/// Register the process environment as the config provider.
#[inline]
pub fn provide_host_config_env() -> ProviderBox {
  provide_env_config_provider(id_effect_config::ProviderOptions::default())
}

/// Build a minimal [`Env`] with only the config provider (tests / CLI bootstrap).
#[inline]
pub fn bootstrap_env<P: id_effect_config::ConfigProvider + 'static>(provider: P) -> Env {
  config_env(provider)
}

#[cfg(test)]
mod tests {
  use super::*;
  use id_effect::run_blocking;
  use id_effect_config::MapConfigProvider;

  #[test]
  fn load_host_config_uses_defaults() {
    let provider = MapConfigProvider::from_pairs(std::iter::empty::<(&str, &str)>());
    let env = bootstrap_env(provider);
    let cfg = run_blocking(load_host_config(), env).unwrap();
    assert_eq!(cfg.bind_host, "0.0.0.0");
    assert_eq!(cfg.bind_port, 8080);
    assert_eq!(cfg.shutdown_timeout, Duration::from_secs(30));
  }

  #[test]
  fn load_host_config_reads_overrides() {
    let provider = MapConfigProvider::from_pairs([
      ("HOST", "127.0.0.1"),
      ("PORT", "9090"),
      ("SHUTDOWN_TIMEOUT_SECS", "5"),
    ]);
    let env = bootstrap_env(provider);
    let cfg = run_blocking(load_host_config(), env).unwrap();
    assert_eq!(cfg.bind_host, "127.0.0.1");
    assert_eq!(cfg.bind_port, 9090);
    assert_eq!(cfg.shutdown_timeout, Duration::from_secs(5));
  }

  #[test]
  fn load_host_config_rejects_invalid_port() {
    let provider = MapConfigProvider::from_pairs([("PORT", "99999")]);
    let env = bootstrap_env(provider);
    assert!(run_blocking(load_host_config(), env).is_err());
  }

  #[test]
  fn load_host_config_rejects_negative_shutdown_timeout() {
    let provider = MapConfigProvider::from_pairs([("SHUTDOWN_TIMEOUT_SECS", "-1")]);
    let env = bootstrap_env(provider);
    assert!(run_blocking(load_host_config(), env).is_err());
  }
}
