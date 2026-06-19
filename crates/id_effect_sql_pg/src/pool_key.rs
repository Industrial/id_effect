//! Shared [`PgPool`](sqlx::PgPool) capability for Apalis, obix, and event journal adapters.

use std::sync::Arc;

use id_effect::{CapabilityId, CapabilityKey, Env, ProviderBox, ProviderError, ProviderNode};
use sqlx::PgPool;

mod pg_pool_cap {
  use std::sync::Arc;

  /// Tag for the shared sqlx PostgreSQL pool in the capability environment.
  #[::id_effect::capability(Arc<sqlx::PgPool>)]
  #[allow(dead_code)]
  pub struct PgPool;
}

pub use pg_pool_cap::PgPoolKey;

/// Register `pool` as the workspace-wide [`PgPoolKey`] capability.
#[inline]
pub fn provide_pg_pool(pool: PgPool) -> ProviderBox {
  struct Node {
    pool: Arc<PgPool>,
  }

  impl ProviderNode for Node {
    fn id(&self) -> &str {
      "sql-pg/pool"
    }

    fn requires(&self) -> &[CapabilityId] {
      &[]
    }

    fn provides(&self) -> CapabilityId {
      PgPoolKey::id()
    }

    fn cap_name(&self) -> &str {
      "PgPoolKey"
    }

    fn build(&self, deps: &Env) -> Result<Env, ProviderError> {
      let mut out = deps.clone();
      out.insert::<PgPoolKey>(Arc::clone(&self.pool));
      Ok(out)
    }
  }

  ProviderBox(Arc::new(Node {
    pool: Arc::new(pool),
  }))
}
