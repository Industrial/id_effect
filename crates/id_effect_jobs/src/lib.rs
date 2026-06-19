//! Background jobs and transactional outbox stubs for
//! [`id_effect`](https://github.com/Industrial/id_effect) platform async messaging.
//!
//! | Type / fn | Role |
//! |-----------|------|
//! | [`JobRunner`], [`MemoryJobRunner`] | FIFO job queue as [`Effect`](id_effect::Effect) values |
//! | [`OutboxTable`], [`MemoryOutbox`] | transactional outbox insert + relay stub |
//! | [`drain_jobs`] / [`relay_outbox`] | process loops for in-memory adapters |

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

mod broker;
mod error;
mod outbox;
mod runner;

pub use broker::{KafkaBrokerStub, MemoryBroker, MessageBroker};
pub use error::{JobError, OutboxError};
pub use outbox::{MemoryOutbox, OutboxRecord, OutboxTable, relay_outbox};
pub use runner::{JobRecord, JobRunner, JobSpec, JobState, MemoryJobRunner, drain_jobs};
