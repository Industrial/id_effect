//! Health checks as effects: success is "up", a [`SqlError`] is "down".
//!
//! Wrap them in a readiness indicator, e.g. with `id_effect_axum`:
//!
//! ```rust,ignore
//! use id_effect_axum::{HealthIndicator, HealthRegistry};
//!
//! let env = sql_client_env(client);
//! let registry = HealthRegistry::new()
//!   .with_indicator(HealthIndicator::from_effect("db", move || sql_ping().provide_all(env.clone())));
//! ```
//!
//! [`sql_ping`] works over any [`SqlClient`]; with the `pg` feature, `pg_pool_health` also
//! reports the pool's size and idle connections (a map, so it serializes as the check's JSON
//! detail).

use id_effect::{Effect, HList, Selector, need};

use crate::client::{SqlClient, SqlClientService};
use crate::error::SqlError;

/// The PostgreSQL pool health check (feature `pg`): `SELECT 1` on a pooled connection, then the
/// pool's `size`, `idle` and `max_size` as detail (see
/// [`PgSqlClient::pool_health`](crate::pg::PgSqlClient::pool_health)).
#[cfg(feature = "pg")]
pub fn pg_pool_health(
  pool: sqlx::PgPool,
) -> Effect<std::collections::BTreeMap<&'static str, u64>, SqlError> {
  crate::pg::PgSqlClient::new(pool).pool_health()
}

/// The statement [`sql_ping`] runs.
pub const PING_SQL: &str = "SELECT 1";

/// Runs [`PING_SQL`] on the [`SqlClientService`] in `R`: succeeds (up) when the database
/// answers, fails with its [`SqlError`] (down) otherwise.
pub fn sql_ping<R, I>() -> Effect<(), SqlError, R>
where
  R: Selector<SqlClientService, I> + HList,
{
  need::<SqlClientService>().flat_map(|c| c.query(PING_SQL, &[]).map(|_| ()).narrow())
}

/// Runs [`PING_SQL`] on `client` (see [`sql_ping`]).
pub fn ping(client: &dyn SqlClient) -> Effect<(), SqlError> {
  client.query(PING_SQL, &[]).map(|_| ())
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::client::tests::run;
  use crate::{SqlClientEnv, TestSqlClient, sql_client_env};

  #[test]
  fn sql_ping_is_up_when_the_client_answers() {
    let test = TestSqlClient::new();
    run(test.connect()).unwrap();
    let check: Effect<(), SqlError, SqlClientEnv> = sql_ping();
    run(check.provide_all(sql_client_env(test.clone()))).unwrap();
    assert_eq!(test.recorded_queries()[0].0, PING_SQL);
  }

  #[test]
  fn sql_ping_is_down_when_queries_fail_or_the_client_is_closed() {
    let test = TestSqlClient::new();
    run(test.connect()).unwrap();
    test.fail_queries("connection reset");
    assert!(matches!(
      run(sql_ping::<SqlClientEnv, _>().provide_all(sql_client_env(test))),
      Err(SqlError::QueryFailed { .. })
    ));
    let closed = TestSqlClient::new();
    assert_eq!(run(ping(&closed)).unwrap_err(), SqlError::NotConnected);
  }
}
