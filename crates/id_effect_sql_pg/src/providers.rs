//! Capability DI providers for [`PgSqlClient`] and [`PgPool`](sqlx::PgPool).

use std::sync::Arc;

use id_effect::{Cap, CapabilityId, CapabilityKey, Env, ProviderBox, ProviderError, ProviderNode};
use sqlx::PgPool as SqlxPgPool;

use crate::PgSqlClient;
use crate::pool_key::PgPool;
use id_effect_sql::client::{SqlClient, SqlClientService};

/// Register `pool` as both [`PgPool`] and [`SqlClient`](id_effect_sql::SqlClient).
#[inline]
pub fn provide_pg_sql_client(pool: SqlxPgPool) -> ProviderBox {
  struct Node {
    pool: PgPool,
    client: SqlClientService,
  }

  impl ProviderNode for Node {
    fn id(&self) -> &str {
      "sql-pg/client"
    }

    fn requires(&self) -> &[CapabilityId] {
      &[]
    }

    fn provides(&self) -> CapabilityId {
      Cap::<SqlClientService>::id()
    }

    fn cap_name(&self) -> &str {
      "SqlClient"
    }

    fn build(&self, deps: &Env) -> Result<Env, ProviderError> {
      let mut out = deps.clone();
      out.insert::<Cap<PgPool>>(Arc::clone(&self.pool));
      out.insert::<Cap<SqlClientService>>(self.client.clone());
      Ok(out)
    }
  }

  let pool_arc = Arc::new(pool);
  let client =
    SqlClientService(Arc::new(PgSqlClient::new((*pool_arc).clone())) as Arc<dyn SqlClient>);
  ProviderBox(Arc::new(Node {
    pool: pool_arc,
    client,
  }))
}
