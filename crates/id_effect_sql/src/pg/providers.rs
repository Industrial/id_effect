//! Layers providing [`PgSqlClient`] as [`SqlClientService`] together with its [`PgPool`].

use id_effect::next::{HCons, HNil, Layer, hlist};

use super::PgSqlClient;
use super::pool_key::PgPool;
use crate::client::SqlClientService;

/// The environment of the PostgreSQL layers: the client capability and its pool.
pub type PgSqlEnv = HCons<SqlClientService, HCons<PgPool, HNil>>;

/// A [`Layer`] providing `pool` as both [`SqlClientService`] (a [`PgSqlClient`]) and
/// [`PgPool`]. It neither checks nor closes the pool (the caller owns it); use
/// [`PgSqlClient::layer`] for the scoped variant.
#[inline]
pub fn provide_pg_sql_client<E: Send + 'static>(pool: sqlx::PgPool) -> Layer<PgSqlEnv, E> {
  Layer::succeed(hlist![
    SqlClientService::new(PgSqlClient::new(pool.clone())),
    PgPool(pool)
  ])
}
