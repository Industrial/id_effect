//! [`StartupError`]: why the service could not start, or stopped serving.

use std::fmt;

use id_effect_axum::HostError;
use id_effect_config::ConfigError;
use id_effect_sql::SqlError;

/// Why the service could not start (or stopped serving). Every layer of the startup graph fails
/// with it (a `Layer` has one error type).
#[derive(Debug)]
pub enum StartupError {
  /// The configuration could not be loaded.
  Config(Box<ConfigError>),
  /// Binding or serving failed.
  Host(HostError),
  /// The database pool could not be opened, or the migration failed.
  Sql(SqlError),
}

impl From<ConfigError> for StartupError {
  fn from(err: ConfigError) -> Self {
    Self::Config(Box::new(err))
  }
}

impl From<HostError> for StartupError {
  fn from(err: HostError) -> Self {
    Self::Host(err)
  }
}

impl From<SqlError> for StartupError {
  fn from(err: SqlError) -> Self {
    Self::Sql(err)
  }
}

impl fmt::Display for StartupError {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    match self {
      Self::Config(e) => write!(f, "config: {e}"),
      Self::Host(e) => write!(f, "{e}"),
      Self::Sql(e) => write!(f, "database: {e}"),
    }
  }
}

impl std::error::Error for StartupError {}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn displays_its_source() {
    let err = StartupError::from(HostError::Serve("bind".into()));
    assert_eq!(err.to_string(), "host serve: bind");
    let err = StartupError::from(ConfigError::Invalid {
      path: "PORT".into(),
      value: "x".into(),
      reason: "bad".into(),
    });
    assert!(err.to_string().starts_with("config:"), "{err}");
    let err = StartupError::from(SqlError::NotConnected);
    assert!(err.to_string().starts_with("database:"), "{err}");
  }
}
