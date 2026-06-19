//! Capability DI providers for [`PgSqlClient`] and [`PgPool`](sqlx::PgPool).

use std::sync::Arc;

use id_effect::{CapabilityId, CapabilityKey, Env, ProviderBox, ProviderError, ProviderNode};
use sqlx::PgPool;

use crate::PgSqlClient;
use crate::pool_key::PgPoolKey;
use id_effect_sql::client::SqlClientKey;

/// Register `pool` as both [`PgPoolKey`] and [`SqlClient`](id_effect_sql::SqlClient).
#[inline]
pub fn provide_pg_sql_client(pool: PgPool) -> ProviderBox {
  struct Node {
    pool: Arc<PgPool>,
    client: Arc<dyn id_effect_sql::SqlClient>,
  }

  impl ProviderNode for Node {
    fn id(&self) -> &str {
      "sql-pg/client"
    }

    fn requires(&self) -> &[CapabilityId] {
      &[]
    }

    fn provides(&self) -> CapabilityId {
      SqlClientKey::id()
    }

    fn cap_name(&self) -> &str {
      "SqlClientKey"
    }

    fn build(&self, deps: &Env) -> Result<Env, ProviderError> {
      let mut out = deps.clone();
      out.insert::<PgPoolKey>(Arc::clone(&self.pool));
      out.insert::<SqlClientKey>(Arc::clone(&self.client));
      Ok(out)
    }
  }

  let pool_arc = Arc::new(pool);
  let client: Arc<dyn id_effect_sql::SqlClient> = Arc::new(PgSqlClient::new((*pool_arc).clone()));
  ProviderBox(Arc::new(Node {
    pool: pool_arc,
    client,
  }))
}
