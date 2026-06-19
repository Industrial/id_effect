//! Event sourcing, projections, and CQRS boundaries for
//! [`id_effect`](https://github.com/Industrial/id_effect) programs.
//!
//! | Type / fn | Role |
//! |-----------|------|
//! | [`EventStore`], [`MemoryEventStore`], [`FileJournal`] | append/read event journals |
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
mod sql_journal;

pub use cqrs::{
  CommandHandler, DispatchError, QueryDispatchError, QueryHandler, dispatch_command,
  query_projection,
};
pub use envelope::{
  EventEnvelope, WireEventEnvelope, envelope_from_wire, envelope_schema, envelope_to_wire,
  schema_error,
};
pub use error::EventStoreError;
pub use event_store::{EventStore, FileJournal, MemoryEventStore, StoredEvent};
pub use sql_journal::{
  POSTGRES_JOURNAL_DDL, SqlEventJournal, SqlJournalBackend, TestSqlJournalBackend,
};

pub use projection::{Projection, run_projection, run_projection_from_store};
