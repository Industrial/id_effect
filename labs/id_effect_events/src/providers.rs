//! [`Layer`](id_effect::Layer)s providing the es-entity event journal backend.

#[cfg(feature = "es-entity")]
mod es_entity_provider {
  use crate::es_entity::EsEntityPgBackend;
  use id_effect::{HCons, HNil, Layer, hlist};
  use sqlx::PgPool;
  use std::sync::Arc;

  /// Shared es-entity PostgreSQL event journal backend (the capability the layers provide).
  pub type EventJournalBackend = Arc<EsEntityPgBackend>;

  /// The typed environment holding only [`EventJournalBackend`] (the output of the layers).
  pub type EventJournalEnv = HCons<EventJournalBackend, HNil>;

  /// A layer providing `backend` as the [`EventJournalBackend`] capability. It needs nothing
  /// (`In = HNil`) and never fails, so `E` is the error type of the graph it joins.
  #[inline]
  pub fn provide_es_entity_events<E: Send + 'static>(
    backend: EsEntityPgBackend,
  ) -> Layer<EventJournalEnv, E> {
    Layer::succeed(hlist![Arc::new(backend)])
  }

  /// A layer providing an [`EsEntityPgBackend`] over the shared `pool`.
  #[inline]
  pub fn provide_es_entity_events_from_pool<E: Send + 'static>(
    pool: PgPool,
  ) -> Layer<EventJournalEnv, E> {
    provide_es_entity_events(EsEntityPgBackend::new(pool))
  }

  #[cfg(test)]
  mod tests {
    use super::*;
    use id_effect::Never;
    use id_effect::{Effect, need};

    #[test]
    fn layer_provides_the_journal_backend() {
      let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");
      // `connect_lazy` opens no connection, so no database is needed. The tokio context is left
      // before `block_on` (which refuses to run inside an async context).
      let pool = {
        let _guard = rt.enter();
        PgPool::connect_lazy("postgres://localhost/unused").expect("lazy pool")
      };
      // `need` hands out the environment's `Arc` around the capability value.
      let program: Effect<usize, Never, EventJournalEnv> =
        need::<EventJournalBackend>().map(|slot| Arc::strong_count(&*slot));
      let exit = program
        .provide_layer(provide_es_entity_events_from_pool(pool))
        .block_on();
      assert_eq!(exit.into_result().ok(), Some(1));
    }
  }
}

#[cfg(feature = "es-entity")]
pub use es_entity_provider::{
  EventJournalBackend, EventJournalEnv, provide_es_entity_events,
  provide_es_entity_events_from_pool,
};
