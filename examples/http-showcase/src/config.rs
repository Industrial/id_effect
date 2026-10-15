//! [`AppConfig`]: every setting of the showcase as one `Config<T>` descriptor
//! ([`AppConfig::config`]), provided to the program as a capability by [`AppConfig::layer`].
//!
//! | Variable | Default | Meaning |
//! |---|---|---|
//! | `HOST` | `0.0.0.0` | bind address ([`HostConfig`]) |
//! | `PORT` | `8080` | listen port ([`HostConfig`]) |
//! | `SHUTDOWN_TIMEOUT_SECS` | `30` | graceful drain on shutdown ([`HostConfig`]) |
//! | `REQUEST_TIMEOUT` | `2s` | per-request handler timeout (a duration: `500ms`, `2s`, ...) |
//! | `DATABASE_URL` | unset | PostgreSQL URL; unset runs without a database |
//! | `DATABASE_MAX_CONNECTIONS` | `16` | pool size when `DATABASE_URL` is set |
//!
//! Every key is read together ([`Config::all`]), so a misconfigured process reports all of its
//! bad keys at once.

use std::time::Duration;

use id_effect::next::{Layer, caps_next};
use id_effect_axum::HostConfig;
use id_effect_config::{Config, ConfigEnv, ConfigError, Secret};

/// Default per-request handler timeout (`REQUEST_TIMEOUT`).
pub const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(2);

/// Default pool size (`DATABASE_MAX_CONNECTIONS`).
pub const DEFAULT_MAX_CONNECTIONS: u32 = 16;

/// PostgreSQL settings, present when `DATABASE_URL` is set.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DatabaseConfig {
  /// Connection URL (`postgres://…`); redacted in `Debug` output.
  pub url: Secret<String>,
  /// Maximum pooled connections.
  pub max_connections: u32,
}

/// The showcase's configuration.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AppConfig {
  /// Bind address, port and shutdown drain of the HTTP server.
  pub host: HostConfig,
  /// Per-request handler timeout.
  pub request_timeout: Duration,
  /// The database, when one is configured.
  pub database: Option<DatabaseConfig>,
}

impl AppConfig {
  /// The descriptor: [`HostConfig::config`] plus the showcase's own keys (module docs), loaded
  /// together and validated.
  pub fn config() -> Config<AppConfig> {
    Config::all((
      HostConfig::config(),
      Config::duration("REQUEST_TIMEOUT").with_default(DEFAULT_REQUEST_TIMEOUT),
      Config::optional_string("DATABASE_URL"),
      Config::integer("DATABASE_MAX_CONNECTIONS").with_default(i64::from(DEFAULT_MAX_CONNECTIONS)),
    ))
    .map_attempt(Self::from_parts)
  }

  /// The loaded [`AppConfig`] as a capability, read from the
  /// [`ConfigProviderService`](id_effect_config::ConfigProviderService) capability when the
  /// layer is built. A load failure fails the build with `E::from(err)`.
  pub fn layer<E>() -> Layer<caps_next!(AppConfig), E, ConfigEnv>
  where
    E: From<ConfigError> + Send + 'static,
  {
    Self::config().layer()
  }

  fn from_parts(
    (host, request_timeout, database_url, max_connections): (
      HostConfig,
      Duration,
      Option<String>,
      i64,
    ),
  ) -> Result<Self, ConfigError> {
    if request_timeout.is_zero() {
      return Err(ConfigError::Invalid {
        path: "REQUEST_TIMEOUT".into(),
        value: format!("{request_timeout:?}"),
        reason: "the request timeout must be positive".into(),
      });
    }
    let max_connections = u32::try_from(max_connections)
      .ok()
      .filter(|n| *n > 0)
      .ok_or_else(|| ConfigError::Invalid {
        path: "DATABASE_MAX_CONNECTIONS".into(),
        value: max_connections.to_string(),
        reason: "the pool size must be between 1 and 4294967295".into(),
      })?;
    let database = database_url
      .filter(|url| !url.trim().is_empty())
      .map(|url| DatabaseConfig {
        url: Secret::new(url),
        max_connections,
      });
    Ok(Self {
      host,
      request_timeout,
      database,
    })
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use id_effect::Exit;
  use id_effect::next::{Effect, need};
  use id_effect_config::{ConfigProvider, MapConfigProvider};

  fn load(pairs: &[(&str, &str)]) -> Result<AppConfig, ConfigError> {
    AppConfig::config().load(&MapConfigProvider::from_pairs(pairs.iter().copied()))
  }

  #[test]
  fn defaults_without_a_database() {
    let cfg = load(&[]).unwrap();
    assert_eq!(cfg.host.bind_port, 8080);
    assert_eq!(cfg.request_timeout, DEFAULT_REQUEST_TIMEOUT);
    assert_eq!(cfg.database, None);
  }

  #[test]
  fn reads_overrides_and_the_database() {
    let cfg = load(&[
      ("PORT", "9000"),
      ("REQUEST_TIMEOUT", "500ms"),
      ("DATABASE_URL", "postgres://u@db/app"),
      ("DATABASE_MAX_CONNECTIONS", "4"),
    ])
    .unwrap();
    assert_eq!(cfg.host.bind_port, 9000);
    assert_eq!(cfg.request_timeout, Duration::from_millis(500));
    let db = cfg.database.unwrap();
    assert_eq!(db.url.expose(), "postgres://u@db/app");
    assert_eq!(db.max_connections, 4);
    assert!(!format!("{db:?}").contains("postgres://"));
  }

  #[test]
  fn blank_database_url_means_no_database() {
    assert_eq!(load(&[("DATABASE_URL", " ")]).unwrap().database, None);
  }

  #[test]
  fn reports_every_invalid_key() {
    let err = load(&[("PORT", "0"), ("DATABASE_MAX_CONNECTIONS", "0")]).unwrap_err();
    assert_eq!(err.errors().len(), 1, "{err}");
    let err = load(&[("PORT", "0"), ("REQUEST_TIMEOUT", "soon")]).unwrap_err();
    assert_eq!(err.errors().len(), 2, "{err}");
  }

  #[test]
  fn rejects_a_zero_request_timeout() {
    assert!(load(&[("REQUEST_TIMEOUT", "0s")]).is_err());
  }

  #[test]
  fn layer_provides_the_capability() {
    let app = MapConfigProvider::from_pairs([("PORT", "7070")])
      .layer()
      .provide_to(AppConfig::layer::<ConfigError>());
    let port: Effect<u16, ConfigError, caps_next!(AppConfig)> =
      need::<AppConfig>().map(|c| c.host.bind_port);
    assert!(matches!(
      port.provide_layer(app).block_on(),
      Exit::Success(7070)
    ));
  }
}
