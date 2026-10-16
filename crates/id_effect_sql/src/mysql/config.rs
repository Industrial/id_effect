//! Pool configuration and construction via sqlx.

use std::str::FromStr;
use std::time::Duration;

use sqlx::MySqlPool;
use sqlx::mysql::{MySqlConnectOptions, MySqlPoolOptions};

use super::error::MySqlSqlError;

/// The session time zone every connection is set to, so `TIMESTAMP` values are read and written
/// in UTC.
const UTC: &str = "+00:00";

/// sqlx MySQL pool sizing and connection string.
#[derive(Clone, Debug)]
pub struct MySqlPoolConfig {
  /// MySQL connection URL (`mysql://user:password@host:3306/database`, with sqlx's query
  /// parameters: `?ssl-mode=…`, `?socket=…`, …).
  pub url: String,
  /// Maximum pooled connections.
  pub max_size: u32,
  /// Pool acquire timeout, which includes opening a connection (default 5s).
  pub acquire_timeout: Duration,
}

impl MySqlPoolConfig {
  /// Config for a connection URL, with a pool of up to 16 connections.
  #[inline]
  pub fn from_url(url: impl Into<String>) -> Self {
    Self {
      url: url.into(),
      max_size: 16,
      acquire_timeout: Duration::from_secs(5),
    }
  }

  /// Override maximum pool size.
  #[inline]
  #[must_use]
  pub fn with_max_size(mut self, max_size: u32) -> Self {
    self.max_size = max_size;
    self
  }

  /// The URL's options, with the session time zone forced to UTC.
  fn connect_options(&self) -> Result<MySqlConnectOptions, MySqlSqlError> {
    Ok(
      MySqlConnectOptions::from_str(&self.url)
        .map_err(MySqlSqlError::from_sqlx)?
        .timezone(Some(UTC.to_owned())),
    )
  }

  fn pool_options(&self) -> MySqlPoolOptions {
    MySqlPoolOptions::new()
      .max_connections(self.max_size)
      .acquire_timeout(self.acquire_timeout)
      .idle_timeout(None)
  }
}

/// Create a [`MySqlPool`] from `config` (opens a first connection).
pub async fn mysql_pool_from_config(config: MySqlPoolConfig) -> Result<MySqlPool, MySqlSqlError> {
  let options = config.connect_options()?;
  config
    .pool_options()
    .connect_with(options)
    .await
    .map_err(MySqlSqlError::from_sqlx)
}

/// A [`MySqlPool`] that opens connections on first use.
pub fn mysql_pool_from_config_lazy(config: MySqlPoolConfig) -> Result<MySqlPool, MySqlSqlError> {
  let options = config.connect_options()?;
  Ok(config.pool_options().connect_lazy_with(options))
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::SqlError;

  #[test]
  fn builders_set_their_fields() {
    let config = MySqlPoolConfig::from_url("mysql://u:p@db:3307/app").with_max_size(3);
    assert_eq!(
      (config.url.as_str(), config.max_size, config.acquire_timeout),
      ("mysql://u:p@db:3307/app", 3, Duration::from_secs(5))
    );
    let options = config.connect_options().expect("options");
    assert_eq!(
      (
        options.get_host(),
        options.get_port(),
        options.get_database()
      ),
      ("db", 3307, Some("app"))
    );
  }

  #[test]
  fn invalid_urls_are_unsupported() {
    let err = mysql_pool_from_config_lazy(MySqlPoolConfig::from_url("mysql://h/db?ssl-mode=nope"))
      .unwrap_err();
    assert!(matches!(err.0, SqlError::Unsupported(_)), "{:?}", err.0);
  }
}
