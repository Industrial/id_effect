//! Capability DI provider for [`PgSqlClient`].

use std::sync::Arc;

use deadpool_postgres::Pool;
use id_effect::{CapabilityId, CapabilityKey, Env, ProviderBox, ProviderError, ProviderNode};

use crate::PgSqlClient;
use id_effect_sql::client::SqlClientKey;

/// Register `pool` as the [`SqlClient`](id_effect_sql::SqlClient) capability.
#[inline]
pub fn provide_pg_sql_client(pool: Pool) -> ProviderBox {
  struct Node {
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
      out.insert::<SqlClientKey>(Arc::clone(&self.client));
      Ok(out)
    }
  }

  let client: Arc<dyn id_effect_sql::SqlClient> = Arc::new(PgSqlClient::new(pool));
  ProviderBox(Arc::new(Node { client }))
}
