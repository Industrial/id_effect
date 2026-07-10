//! Shared [`sqlx::PgPool`](sqlx::PgPool) capability for Apalis, obix, and event journal adapters.

use std::sync::Arc;

use id_effect::{Cap, CapabilityId, CapabilityKey, Env, ProviderBox, ProviderError, ProviderNode};
use sqlx::PgPool as SqlxPgPool;

/// Shared PostgreSQL pool in the capability environment.
pub type PgPool = Arc<SqlxPgPool>;

/// Register `pool` as the workspace-wide [`PgPool`] capability.
#[inline]
pub fn provide_pg_pool(pool: SqlxPgPool) -> ProviderBox {
  struct Node {
    pool: PgPool,
  }

  impl ProviderNode for Node {
    fn id(&self) -> &str {
      "sql-pg/pool"
    }

    fn requires(&self) -> &[CapabilityId] {
      &[]
    }

    fn provides(&self) -> CapabilityId {
      Cap::<PgPool>::id()
    }

    fn cap_name(&self) -> &str {
      "PgPool"
    }

    fn build(&self, deps: &Env) -> Result<Env, ProviderError> {
      let mut out = deps.clone();
      out.insert::<Cap<PgPool>>(Arc::clone(&self.pool));
      Ok(out)
    }
  }

  ProviderBox(Arc::new(Node {
    pool: Arc::new(pool),
  }))
}
