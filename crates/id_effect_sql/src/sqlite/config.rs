//! Pool configuration and construction via sqlx.

use std::path::Path;
use std::str::FromStr;
use std::time::Duration;

use sqlx::SqlitePool;
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions};

use super::error::SqliteSqlError;

/// sqlx SQLite pool sizing and database location.
#[derive(Clone, Debug)]
pub struct SqlitePoolConfig {
  /// SQLite connection URL: `sqlite::memory:`, `sqlite://path/to/file.db`, with sqlx's query
  /// parameters (`?mode=ro`, `?mode=rwc`, …).
  pub url: String,
  /// Maximum pooled connections.
  pub max_size: u32,
  /// Pool acquire timeout, which includes opening a connection (default 10s: opening a new
  /// database file in WAL mode syncs it to disk several times).
  pub acquire_timeout: Duration,
  /// How long a statement waits for a locked database before failing with `SQLITE_BUSY`
  /// (default 5s).
  pub busy_timeout: Duration,
  /// Create the database file if it does not exist.
  pub create_if_missing: bool,
  /// Switch the database to write-ahead logging, so readers do not wait for a writer.
  pub wal: bool,
}

impl SqlitePoolConfig {
  /// Config for a connection URL, with a pool of up to 16 connections. The file must exist
  /// unless the URL says `?mode=rwc`.
  #[inline]
  pub fn from_url(url: impl Into<String>) -> Self {
    Self {
      url: url.into(),
      max_size: 16,
      acquire_timeout: Duration::from_secs(10),
      busy_timeout: Duration::from_secs(5),
      create_if_missing: false,
      wal: false,
    }
  }

  /// A private in-memory database: every pool built from this config gets its own, shared by
  /// the pool's connections, and gone when the pool closes.
  #[inline]
  pub fn memory() -> Self {
    Self::from_url("sqlite::memory:")
  }

  /// The database file at `path`, created if missing, in WAL mode.
  pub fn file(path: impl AsRef<Path>) -> Self {
    // sqlx reads the part after the scheme as a percent-encoded path ending at `?`.
    let path = path
      .as_ref()
      .to_string_lossy()
      .replace('%', "%25")
      .replace('?', "%3F")
      .replace('#', "%23");
    Self {
      create_if_missing: true,
      wal: true,
      ..Self::from_url(format!("sqlite://{path}"))
    }
  }

  /// Override maximum pool size.
  #[inline]
  #[must_use]
  pub fn with_max_size(mut self, max_size: u32) -> Self {
    self.max_size = max_size;
    self
  }

  /// Override how long a statement waits for a locked database.
  #[inline]
  #[must_use]
  pub fn with_busy_timeout(mut self, busy_timeout: Duration) -> Self {
    self.busy_timeout = busy_timeout;
    self
  }

  fn connect_options(&self) -> Result<SqliteConnectOptions, SqliteSqlError> {
    let mut options = SqliteConnectOptions::from_str(&self.url)
      .map_err(SqliteSqlError::from_sqlx)?
      .busy_timeout(self.busy_timeout);
    if self.create_if_missing {
      options = options.create_if_missing(true);
    }
    if self.wal {
      options = options.journal_mode(SqliteJournalMode::Wal);
    }
    Ok(options)
  }

  /// Connections are never retired for age or idleness: an in-memory database lives only as
  /// long as one of its connections.
  fn pool_options(&self) -> SqlitePoolOptions {
    SqlitePoolOptions::new()
      .max_connections(self.max_size)
      .acquire_timeout(self.acquire_timeout)
      .idle_timeout(None)
      .max_lifetime(None)
  }
}

/// Create a [`SqlitePool`] from `config` (opens a first connection).
pub async fn sqlite_pool_from_config(
  config: SqlitePoolConfig,
) -> Result<SqlitePool, SqliteSqlError> {
  let options = config.connect_options()?;
  config
    .pool_options()
    .connect_with(options)
    .await
    .map_err(SqliteSqlError::from_sqlx)
}

/// A [`SqlitePool`] that opens connections on first use.
pub fn sqlite_pool_from_config_lazy(
  config: SqlitePoolConfig,
) -> Result<SqlitePool, SqliteSqlError> {
  let options = config.connect_options()?;
  Ok(config.pool_options().connect_lazy_with(options))
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::SqlError;

  #[test]
  fn builders_set_their_fields() {
    let memory = SqlitePoolConfig::memory()
      .with_max_size(3)
      .with_busy_timeout(Duration::from_millis(10));
    assert_eq!(memory.url, "sqlite::memory:");
    assert_eq!(
      (memory.max_size, memory.busy_timeout),
      (3, Duration::from_millis(10))
    );
    assert!(!memory.create_if_missing && !memory.wal);
    let file = SqlitePoolConfig::file("/tmp/a%b?c#d.db");
    assert_eq!(file.url, "sqlite:///tmp/a%25b%3Fc%23d.db");
    assert!(file.create_if_missing && file.wal);
    let options = file.connect_options().expect("options");
    assert_eq!(options.get_filename(), Path::new("/tmp/a%b?c#d.db"));
  }

  #[test]
  fn invalid_urls_are_unsupported() {
    let err = SqlitePoolConfig::from_url("sqlite://x.db?mode=nope")
      .connect_options()
      .unwrap_err();
    assert!(matches!(err.0, SqlError::Unsupported(m) if m.contains("mode")));
  }
}
