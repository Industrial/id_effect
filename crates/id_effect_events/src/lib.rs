//! Event sourcing, projections, and CQRS boundaries for
//! [`id_effect`](https://github.com/Industrial/id_effect) programs.
//!
//! | Type / fn | Role |
//! |-----------|------|
//! | [`EventStore`] | append/read event streams; [`EventStore::append_expected`] is the optimistic-concurrency append |
//! | [`SqlEventJournal`] | durable journal over an `id_effect_sql` [`SqlClient`](id_effect_sql::SqlClient) (PostgreSQL, SQLite) |
//! | [`MemoryEventStore`], [`FileJournal`] | in-memory and JSON-lines stores for tests and local runs |
//! | [`run_projection`], [`run_projection_from_store`] | fold events or a stored stream through a [`Projection`] |
//! | [`ProjectionRunner`] | rebuild several projections in dependency order |
//! | [`EventEnvelope`] | versioned payload with wire bridging via [`id_effect::schema::Schema`] |
//! | [`CommandHandler`] / [`QueryHandler`], [`dispatch_command`], [`query_projection`] | CQRS dispatch |
//!
//! Everything that touches a store returns an [`Effect`](id_effect::Effect); nothing runs until
//! the caller runs it.
//!
//! Features: `pg` and `sqlite` enable the `id_effect_sql` drivers (the journal itself works over
//! any `SqlClient`).
//!
//! ## Example
//!
//! ```rust
//! use id_effect::{Cause, Exit};
//! use id_effect_events::{EventStore, EventStoreError, MemoryEventStore};
//!
//! let store = MemoryEventStore::<String>::new();
//! let first = store.append_expected("order-1", 0, &["placed".to_owned()]);
//! assert!(matches!(first.block_on(), Exit::Success(ref s) if s[0].version == 1));
//!
//! // A writer that read the stream before that append is now stale.
//! let stale = store.append_expected("order-1", 0, &["placed again".to_owned()]);
//! match stale.block_on() {
//!   Exit::Failure(Cause::Fail(EventStoreError::VersionConflict { expected: 0, actual: 1, .. })) => {}
//!   other => panic!("expected a version conflict, got {other:?}"),
//! }
//! ```

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
mod edge;
mod envelope;
mod error;
mod event_store;
mod projection;
mod projection_runner;
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
pub use projection::{Projection, run_projection, run_projection_from_store};
pub use projection_runner::{ProjectionNode, ProjectionRunner};
pub use sql_journal::{SQL_EVENT_JOURNAL_DDL, SqlEventJournal};

/// Compiles and runs the README's example as a doctest.
#[cfg(doctest)]
#[doc = include_str!("../README.md")]
struct ReadmeDoctests;
