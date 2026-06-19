//! Pool configuration and construction.

use deadpool_postgres::{Manager, ManagerConfig, Pool, RecyclingMethod, Runtime};
use tokio_postgres::NoTls;

use crate::error::PgSqlError;

/// deadpool-postgres pool sizing and connection string.
#[derive(Clone, Debug)]
pub struct PgPoolConfig {
  /// PostgreSQL connection URL (`postgres://…`).
  pub url: String,
  /// Maximum pooled connections.
  pub max_size: usize,
}

impl PgPoolConfig {
  /// Build config from a connection URL with default pool size (16).
  #[inline]
  pub fn from_url(url: impl Into<String>) -> Self {
    Self {
      url: url.into(),
      max_size: 16,
    }
  }

  /// Override maximum pool size.
  #[inline]
  pub fn with_max_size(mut self, max_size: usize) -> Self {
    self.max_size = max_size;
    self
  }
}

/// Create a [`Pool`] from `config` (synchronous — pool acquire is async).
pub fn pg_pool_from_config(config: PgPoolConfig) -> Result<Pool, PgSqlError> {
  let pg_config = config.url.parse::<tokio_postgres::Config>().map_err(|e| {
    PgSqlError(id_effect_sql::SqlError::Unsupported(format!(
      "invalid postgres url: {e}"
    )))
  })?;
  let manager = Manager::from_config(
    pg_config,
    NoTls,
    ManagerConfig {
      recycling_method: RecyclingMethod::Fast,
    },
  );
  Pool::builder(manager)
    .max_size(config.max_size)
    .runtime(Runtime::Tokio1)
    .build()
    .map_err(PgSqlError::from_build)
}
