//! Capability providers for es-entity event persistence.

#[cfg(feature = "es-entity")]
mod es_entity_provider {
  use crate::es_entity::EsEntityPgBackend;
  use id_effect::{CapabilityId, CapabilityKey, Env, ProviderBox, ProviderError, ProviderNode};
  use sqlx::PgPool;
  use std::sync::Arc;

  mod event_journal_cap {
    use std::sync::Arc;

    use crate::es_entity::EsEntityPgBackend;

    /// Shared es-entity PostgreSQL event journal backend.
    #[::id_effect::capability(Arc<EsEntityPgBackend>)]
    #[allow(dead_code)]
    pub struct EventJournalBackend;
  }

  pub use event_journal_cap::EventJournalBackendKey as EventStoreKey;

  /// Register `backend` as [`EventStoreKey`].
  #[inline]
  pub fn provide_es_entity_events(backend: EsEntityPgBackend) -> ProviderBox {
    struct Node {
      backend: Arc<EsEntityPgBackend>,
    }

    impl ProviderNode for Node {
      fn id(&self) -> &str {
        "events/es-entity-journal"
      }

      fn requires(&self) -> &[CapabilityId] {
        &[]
      }

      fn provides(&self) -> CapabilityId {
        EventStoreKey::id()
      }

      fn cap_name(&self) -> &str {
        "EventStoreKey"
      }

      fn build(&self, deps: &Env) -> Result<Env, ProviderError> {
        let mut out = deps.clone();
        out.insert::<EventStoreKey>(Arc::clone(&self.backend));
        Ok(out)
      }
    }

    ProviderBox(Arc::new(Node {
      backend: Arc::new(backend),
    }))
  }

  /// Build [`EsEntityPgBackend`] from a shared pool and register it.
  #[inline]
  pub fn provide_es_entity_events_from_pool(pool: PgPool) -> ProviderBox {
    provide_es_entity_events(EsEntityPgBackend::new(pool))
  }
}

#[cfg(feature = "es-entity")]
pub use es_entity_provider::{
  EventStoreKey, provide_es_entity_events, provide_es_entity_events_from_pool,
};
