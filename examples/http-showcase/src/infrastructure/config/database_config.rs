//! [`DatabaseConfig`]: the PostgreSQL settings of [`AppConfig`](super::AppConfig).

use id_effect_config::Secret;

/// PostgreSQL settings, present when `DATABASE_URL` is set.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DatabaseConfig {
  /// Connection URL (`postgres://…`); redacted in `Debug` output.
  pub url: Secret<String>,
  /// Maximum pooled connections (`DATABASE_MAX_CONNECTIONS`).
  pub max_connections: u32,
}

impl DatabaseConfig {
  /// Default pool size (`DATABASE_MAX_CONNECTIONS`).
  pub const DEFAULT_MAX_CONNECTIONS: u32 = 16;
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn debug_redacts_the_url() {
    let db = DatabaseConfig {
      url: Secret::new("postgres://u:pw@db/app".to_owned()),
      max_connections: DatabaseConfig::DEFAULT_MAX_CONNECTIONS,
    };
    let shown = format!("{db:?}");
    assert!(!shown.contains("pw"), "{shown}");
    assert!(shown.contains("16"), "{shown}");
  }
}
