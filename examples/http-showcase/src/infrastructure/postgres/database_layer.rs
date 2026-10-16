//! [`database_layer`]: the scoped PostgreSQL pool of a [`DatabaseConfig`].

use id_effect::Layer;
use id_effect_sql::SqlError;
use id_effect_sql::pg::{PgPoolConfig, PgSqlClient, PgSqlEnv};

use crate::infrastructure::config::DatabaseConfig;

/// The scoped PostgreSQL pool for `config` ([`PgSqlClient::layer`], Effect.ts `Layer.scoped`):
/// opened and checked (`SELECT 1`) when built, closed when the owning scope closes. Provides
/// `SqlClientService` and `PgPool` ([`PgSqlEnv`]); a failure to open fails the build with
/// `E::from(err)`.
pub fn database_layer<E>(config: &DatabaseConfig) -> Layer<PgSqlEnv, E>
where
  E: From<SqlError> + Send + 'static,
{
  PgSqlClient::layer(
    PgPoolConfig::from_url(config.url.expose().clone()).with_max_size(config.max_connections),
  )
}

#[cfg(test)]
mod tests {
  use id_effect::{Effect, Exit, caps, need};
  use id_effect_config::Secret;
  use id_effect_sql::SqlClientService;

  use super::*;
  use crate::infrastructure::postgres::LivePg;

  #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
  async fn live_postgres_the_layer_opens_a_pool() {
    // Skip-or-fail first when the database is unreachable.
    let Some(pg) = LivePg::connect("live_postgres_the_layer_opens_a_pool").await else {
      return;
    };
    pg.close().await;
    let url = std::env::var("DATABASE_URL").unwrap();
    let config = DatabaseConfig {
      url: Secret::new(url),
      max_connections: 2,
    };
    let one: Effect<u64, SqlError, caps!(SqlClientService)> =
      need::<SqlClientService>().flat_map(|sql| sql.execute("SELECT 1", &[]).narrow());
    let exit = LivePg::exit(one.provide_layer(database_layer::<SqlError>(&config))).await;
    assert!(matches!(exit, Exit::Success(_)), "{exit:?}");
  }
}
