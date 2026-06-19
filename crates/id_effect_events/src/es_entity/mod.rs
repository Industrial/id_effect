//! es-entity backed PostgreSQL event journal (production path).
//!
//! [`EsEntityPgBackend`] persists stream events inside `es_entity::DbOp` transactions on the
//! shared [`PgPoolKey`](id_effect_sql_pg::PgPoolKey). [`EsEntityEventStore`] exposes the slim
//! [`EventStore`](crate::EventStore) trait via `Effect::new_async`.

mod store;

pub use store::{
  ES_ENTITY_EVENT_JOURNAL_DDL, EsEntityEventStore, EsEntityPgBackend, apply_es_entity_journal_ddl,
};
