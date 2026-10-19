//! Config bootstrap via [`id_effect_config`]: [`HostConfig`] as a `Config<T>` descriptor and as a
//! [`Layer`].

use std::time::Duration;

use id_effect::observability::posture::record_dangerous;
use id_effect::{Effect, HCons, HNil, Layer, Selector};
use id_effect_config::{
  Config, ConfigEnv, ConfigError, ConfigProvider, ConfigProviderService, EnvConfigProviderLive,
  config_env,
};

use crate::middleware::LoadShed;

/// Host bind, shutdown and load-shedding settings loaded from the environment (`HOST`, `PORT`,
/// `SHUTDOWN_TIMEOUT_SECS`, `MAX_IN_FLIGHT`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HostConfig {
  /// Bind address (e.g. `0.0.0.0`).
  pub bind_host: String,
  /// Listen port.
  pub bind_port: u16,
  /// Graceful shutdown drain timeout.
  pub shutdown_timeout: Duration,
  /// The in-flight request cap the server installs ([`LoadShedLayer`](crate::middleware::LoadShedLayer));
  /// on by default, off only through [`Self::dangerous_no_load_shedding`].
  pub load_shed: LoadShed,
}

impl Default for HostConfig {
  /// `0.0.0.0:8080`, a 30 s shutdown timeout and the default [`LoadShed`] (what
  /// [`Self::config`] loads from an empty environment).
  fn default() -> Self {
    Self {
      bind_host: "0.0.0.0".to_owned(),
      bind_port: 8080,
      shutdown_timeout: Duration::from_secs(30),
      load_shed: LoadShed::default(),
    }
  }
}

impl HostConfig {
  /// The raw descriptor: `HOST` (default `0.0.0.0`), `PORT` (default `8080`),
  /// `SHUTDOWN_TIMEOUT_SECS` (default `30`) and `MAX_IN_FLIGHT` (default: none, i.e.
  /// [`LoadShed::default_max_in_flight`]), loaded together so every failure is reported.
  #[inline]
  pub fn descriptor() -> Config<(String, i64, i64, Option<i64>)> {
    Config::all((
      Config::string("HOST").with_default("0.0.0.0".to_string()),
      Config::integer("PORT").with_default(8080),
      Config::integer("SHUTDOWN_TIMEOUT_SECS").with_default(30),
      Config::integer("MAX_IN_FLIGHT")
        .map(Some)
        .with_default(None),
    ))
  }

  /// No load shedding: the server admits every request, however many are in flight.
  ///
  /// **Re-opens unbounded in-flight requests** (finding M5 of the 0.11 security review): a burst
  /// of slow requests queues work and memory without limit instead of being answered `503`.
  /// Only for a server behind a proxy that caps concurrency itself. Recorded in the posture
  /// registry ([`id_effect::observability::posture`]).
  #[must_use]
  pub fn dangerous_no_load_shedding(mut self) -> Self {
    record_dangerous(
      "id_effect_axum::HostConfig::dangerous_no_load_shedding",
      "M5: no in-flight request cap; overload queues work and memory without bound",
    );
    self.load_shed = LoadShed::disabled();
    self
  }

  /// The validated descriptor (port in `1..=65535`, non-negative timeout, `MAX_IN_FLIGHT` at
  /// least 1).
  #[inline]
  pub fn config() -> Config<HostConfig> {
    Self::descriptor().map_attempt(Self::from_tuple)
  }

  /// A layer providing [`HostConfig`] from the [`ConfigProviderService`] capability (feed it a
  /// provider layer such as [`provide_host_config_env`] with `provide_to`).
  #[inline]
  pub fn layer<E>() -> Layer<HCons<HostConfig, HNil>, E, ConfigEnv>
  where
    E: From<ConfigError> + Send + 'static,
  {
    Self::config().layer()
  }

  #[allow(clippy::result_large_err)]
  fn from_tuple(
    (bind_host, port, timeout_secs, max_in_flight): (String, i64, i64, Option<i64>),
  ) -> Result<Self, ConfigError> {
    if !(1..=i64::from(u16::MAX)).contains(&port) {
      return Err(ConfigError::Invalid {
        path: "PORT".into(),
        value: port.to_string(),
        reason: "port must be between 1 and 65535".into(),
      });
    }
    let Ok(timeout_secs) = u64::try_from(timeout_secs) else {
      return Err(ConfigError::Invalid {
        path: "SHUTDOWN_TIMEOUT_SECS".into(),
        value: timeout_secs.to_string(),
        reason: "timeout must be non-negative".into(),
      });
    };
    let load_shed = match max_in_flight {
      None => LoadShed::default(),
      Some(n) => match usize::try_from(n) {
        Ok(n) if n >= 1 => LoadShed::new(n),
        _ => {
          return Err(ConfigError::Invalid {
            path: "MAX_IN_FLIGHT".into(),
            value: n.to_string(),
            reason: "the in-flight cap must be at least 1".into(),
          });
        }
      },
    };
    Ok(Self {
      bind_host,
      bind_port: u16::try_from(port).unwrap_or(u16::MAX),
      shutdown_timeout: Duration::from_secs(timeout_secs),
      load_shed,
    })
  }
}

/// Load [`HostConfig`] using the [`ConfigProviderService`] capability in `R` (index `I` inferred).
#[inline]
pub fn load_host_config<R, I>() -> Effect<HostConfig, ConfigError, R>
where
  R: Selector<ConfigProviderService, I> + Send + Sync + 'static,
{
  HostConfig::config().run::<HostConfig, ConfigError, R, I>()
}

/// A layer providing the process environment as the config provider.
#[inline]
pub fn provide_host_config_env<E: Send + 'static>() -> Layer<ConfigEnv, E> {
  EnvConfigProviderLive::layer()
}

/// A minimal environment with only `provider` as the config provider (tests / CLI bootstrap).
#[inline]
pub fn bootstrap_env<P: ConfigProvider + 'static>(provider: P) -> ConfigEnv {
  config_env(provider)
}

#[cfg(test)]
mod tests {
  use super::*;
  use id_effect::Exit;
  use id_effect::need;
  use id_effect_config::{MapConfigProvider, provide_config_provider};

  #[allow(clippy::result_large_err)]
  fn load(pairs: &[(&str, &str)]) -> Result<HostConfig, id_effect::Cause<ConfigError>> {
    let env = bootstrap_env(MapConfigProvider::from_pairs(pairs.iter().copied()));
    load_host_config().provide_all(env).block_on().into_result()
  }

  #[test]
  fn load_host_config_uses_defaults() {
    let cfg = load(&[]).unwrap();
    assert_eq!(cfg.bind_host, "0.0.0.0");
    assert_eq!(cfg.bind_port, 8080);
    assert_eq!(cfg.shutdown_timeout, Duration::from_secs(30));
    assert_eq!(cfg.load_shed, LoadShed::default());
    assert_eq!(cfg, HostConfig::default());
  }

  #[test]
  fn load_host_config_reads_and_validates_max_in_flight() {
    let cfg = load(&[("MAX_IN_FLIGHT", "64")]).unwrap();
    assert_eq!(cfg.load_shed.max_in_flight(), Some(64));
    assert!(load(&[("MAX_IN_FLIGHT", "0")]).is_err());
    assert!(load(&[("MAX_IN_FLIGHT", "-3")]).is_err());
  }

  #[test]
  fn dangerous_no_load_shedding_disables_and_records() {
    let cfg = HostConfig::default().dangerous_no_load_shedding();
    assert_eq!(cfg.load_shed.max_in_flight(), None);
    assert!(
      id_effect::observability::posture::dangerous_opt_outs()
        .iter()
        .any(|(id, _)| *id == "id_effect_axum::HostConfig::dangerous_no_load_shedding")
    );
  }

  #[test]
  fn load_host_config_reads_overrides() {
    let cfg = load(&[
      ("HOST", "127.0.0.1"),
      ("PORT", "9090"),
      ("SHUTDOWN_TIMEOUT_SECS", "5"),
    ])
    .unwrap();
    assert_eq!(cfg.bind_host, "127.0.0.1");
    assert_eq!(cfg.bind_port, 9090);
    assert_eq!(cfg.shutdown_timeout, Duration::from_secs(5));
  }

  #[test]
  fn load_host_config_rejects_invalid_port() {
    assert!(load(&[("PORT", "99999")]).is_err());
  }

  #[test]
  fn load_host_config_rejects_negative_shutdown_timeout() {
    assert!(load(&[("SHUTDOWN_TIMEOUT_SECS", "-1")]).is_err());
  }

  #[test]
  fn host_config_layer_provides_the_capability() {
    let app = provide_config_provider(MapConfigProvider::from_pairs([("PORT", "7070")]))
      .provide_to(HostConfig::layer::<ConfigError>());
    let port: Effect<u16, ConfigError, HCons<HostConfig, HNil>> =
      need::<HostConfig>().map(|c| c.bind_port);
    assert!(matches!(
      port.provide_layer(app).block_on(),
      Exit::Success(7070)
    ));
    // The env-backed provider layer builds too.
    let env_app = provide_host_config_env::<ConfigError>().provide_to(HostConfig::layer());
    let any: Effect<(), ConfigError, HCons<HostConfig, HNil>> = need::<HostConfig>().map(|_| ());
    let _ = any.provide_layer(env_app).block_on();
  }
}
