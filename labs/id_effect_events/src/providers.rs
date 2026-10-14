//! Capability providers for es-entity event persistence.

#[cfg(feature = "es-entity")]
mod es_entity_provider {
  use crate::es_entity::EsEntityPgBackend;
  use id_effect::{
    Cap, CapabilityId, CapabilityKey, Env, ProviderBox, ProviderError, ProviderNode,
  };
  use sqlx::PgPool;
  use std::sync::Arc;

  /// Shared es-entity PostgreSQL event journal backend.
  pub type EventJournalBackend = Arc<EsEntityPgBackend>;

  /// Register `backend` as [`EventJournalBackend`].
  #[inline]
  pub fn provide_es_entity_events(backend: EsEntityPgBackend) -> ProviderBox {
    struct Node {
      backend: EventJournalBackend,
    }

    impl ProviderNode for Node {
      fn id(&self) -> &str {
        "events/es-entity-journal"
      }

      fn requires(&self) -> &[CapabilityId] {
        &[]
      }

      fn provides(&self) -> CapabilityId {
        Cap::<EventJournalBackend>::id()
      }

      fn cap_name(&self) -> &str {
        "EventJournalBackend"
      }

      fn build(&self, deps: &Env) -> Result<Env, ProviderError> {
        let mut out = deps.clone();
        out.insert::<Cap<EventJournalBackend>>(Arc::clone(&self.backend));
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
  EventJournalBackend, provide_es_entity_events, provide_es_entity_events_from_pool,
};
