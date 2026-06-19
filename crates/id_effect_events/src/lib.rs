//! Event sourcing, projections, and CQRS boundaries for
//! [`id_effect`](https://github.com/Industrial/id_effect) programs.
//!
//! | Type / fn | Role |
//! |-----------|------|
//! | [`EventStore`], [`MemoryEventStore`], [`FileJournal`] | append/read event journals |
//! | [`EsEntityEventStore`] (feature `es-entity`) | production PG journal via es-entity |
//! | [`ProjectionRunner`] | multi-projection rebuild order via [`id_effect_graph`] |
//! | [`EventEnvelope`] | versioned payload with wire bridging via [`id_effect::schema::Schema`] |
//! | [`run_projection`] | fold streams or stores through a [`Projection`] |
//! | [`CommandHandler`] / [`QueryHandler`] | CQRS dispatch helpers |

#![forbid(unsafe_code)]
#![deny(missing_docs)]
#![cfg_attr(
  test,
  allow(
    clippy::bool_assert_comparison,
    clippy::unwrap_used,
    clippy::expect_used
  )
)]

mod cqrs;
mod envelope;
mod error;
mod event_store;
mod projection;
mod projection_runner;
mod sql_journal;

#[cfg(feature = "es-entity")]
mod es_entity;

#[cfg(feature = "es-entity")]
mod providers;

pub use cqrs::{
  CommandHandler, DispatchError, QueryDispatchError, QueryHandler, dispatch_command,
  query_projection,
};

#[cfg(feature = "es-entity")]
pub use cqrs::dispatch_command_es_entity;

pub use envelope::{
  EventEnvelope, WireEventEnvelope, envelope_from_wire, envelope_schema, envelope_to_wire,
  schema_error,
};
pub use error::EventStoreError;
pub use event_store::{EventStore, FileJournal, MemoryEventStore, StoredEvent};
pub use projection::{Projection, run_projection, run_projection_from_store};
pub use projection_runner::{ProjectionNode, ProjectionRunner};
pub use sql_journal::{SqlEventJournal, SqlJournalBackend, TestSqlJournalBackend};

#[cfg(feature = "es-entity")]
pub use es_entity::{
  ES_ENTITY_EVENT_JOURNAL_DDL, EsEntityEventStore, EsEntityPgBackend, apply_es_entity_journal_ddl,
};

#[cfg(feature = "es-entity")]
pub use providers::{EventStoreKey, provide_es_entity_events, provide_es_entity_events_from_pool};
