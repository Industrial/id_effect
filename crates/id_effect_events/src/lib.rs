//! Event sourcing, projections, and CQRS boundaries for
//! [`id_effect`](https://github.com/Industrial/id_effect) programs.
//!
//! | Module | Role |
//! |--------|------|
//! | [`event_store`] | [`EventStore`], [`MemoryEventStore`], [`FileJournal`] |
//! | [`envelope`] | [`EventEnvelope`] with [`Schema`] wire bridging |
//! | [`projection`] | [`run_projection`] fold over event streams |
//! | [`cqrs`] | [`CommandHandler`] / [`QueryHandler`] dispatch helpers |

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
