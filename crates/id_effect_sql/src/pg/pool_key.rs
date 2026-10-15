//! Shared [`sqlx::PgPool`] capability for Apalis, obix, and event journal adapters.

use id_effect::next::{HCons, HNil, Layer, hlist};

/// Shared PostgreSQL pool in the typed environment (derefs to [`sqlx::PgPool`], which is itself
/// a cheap handle: clone it to hand the pool to an adapter).
#[derive(Clone, Debug)]
pub struct PgPool(pub sqlx::PgPool);

impl std::ops::Deref for PgPool {
  type Target = sqlx::PgPool;

  fn deref(&self) -> &Self::Target {
    &self.0
  }
}

/// The minimal typed environment holding only [`PgPool`].
pub type PgPoolEnv = HCons<PgPool, HNil>;

/// A [`Layer`] providing `pool` as the workspace-wide [`PgPool`] capability. The pool is not
/// closed by the layer (the caller owns it); see [`PgSqlClient::layer`](super::PgSqlClient::layer)
/// for a layer that owns its pool.
#[inline]
pub fn provide_pg_pool<E: Send + 'static>(pool: sqlx::PgPool) -> Layer<PgPoolEnv, E> {
  Layer::succeed(hlist![PgPool(pool)])
}
