//! Pool configuration and construction via sqlx.

use std::time::Duration;

use sqlx::PgPool;
use sqlx::postgres::PgPoolOptions;

use crate::error::PgSqlError;

/// sqlx PostgreSQL pool sizing and connection string.
#[derive(Clone, Debug)]
pub struct PgPoolConfig {
  /// PostgreSQL connection URL (`postgres://…`).
  pub url: String,
  /// Maximum pooled connections.
  pub max_size: u32,
  /// Pool acquire timeout (default 2s).
  pub acquire_timeout: Duration,
}

impl PgPoolConfig {
  /// Build config from a connection URL with default pool size (16).
  #[inline]
  pub fn from_url(url: impl Into<String>) -> Self {
    Self {
      url: url.into(),
      max_size: 16,
      acquire_timeout: Duration::from_secs(2),
    }
  }

  /// Override maximum pool size.
  #[inline]
  pub fn with_max_size(mut self, max_size: u32) -> Self {
    self.max_size = max_size;
    self
  }

  fn options(&self) -> PgPoolOptions {
    PgPoolOptions::new()
      .max_connections(self.max_size)
      .acquire_timeout(self.acquire_timeout)
      .idle_timeout(None)
  }
}

/// Create a [`PgPool`] from `config` (connects eagerly).
pub async fn pg_pool_from_config(config: PgPoolConfig) -> Result<PgPool, PgSqlError> {
  config
    .options()
    .connect(&config.url)
    .await
    .map_err(PgSqlError::from_sqlx)
}

/// Lazy pool for unit tests and provider wiring without immediate connect.
pub fn pg_pool_from_config_lazy(config: PgPoolConfig) -> Result<PgPool, PgSqlError> {
  config
    .options()
    .connect_lazy(&config.url)
    .map_err(PgSqlError::from_sqlx)
}
