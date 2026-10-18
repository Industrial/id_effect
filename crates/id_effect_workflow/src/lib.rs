//! Durable workflows for id_effect services (ADR 0010 Amendment 7): a history journal replayed
//! on resume, on `id_effect_sql` (Postgres), with any number of engines against one database
//! (release 0.11.0 decisions D6 and D7).
//!
//! ```
//! use id_effect::{Effect, Exit, Schema, succeed};
//! use id_effect_workflow::{
//!   MemoryWorkflowStore, RunOutcome, Workflow, WorkflowContext, WorkflowEngine, WorkflowError,
//!   WorkflowId,
//! };
//!
//! #[derive(Schema)]
//! struct Order {
//!   id: i64,
//! }
//!
//! #[derive(Schema)]
//! struct Receipt {
//!   order: i64,
//!   charged: i64,
//! }
//!
//! struct Checkout;
//!
//! impl Workflow for Checkout {
//!   type Input = Order;
//!   type Output = Receipt;
//!   const NAME: &'static str = "checkout";
//!
//!   fn run(&self, ctx: WorkflowContext, order: Order) -> Effect<Receipt, WorkflowError> {
//!     // Journaled: after a crash the engine replays this result instead of charging again.
//!     let receipt = Receipt { order: order.id, charged: 42 };
//!     ctx.step("charge", succeed::<_, String, _>(receipt))
//!   }
//! }
//!
//! let engine = WorkflowEngine::new(MemoryWorkflowStore::new()).register(Checkout).unwrap();
//! let id = WorkflowId::new("checkout:7").unwrap();
//! let run = engine
//!   .start::<Checkout>(id.clone(), Order { id: 7 })
//!   .flat_map({
//!     let engine = engine.clone();
//!     let id = id.clone();
//!     move |_| engine.resume(&id)
//!   });
//! assert!(matches!(run.block_on(), Exit::Success(Some(RunOutcome::Completed))));
//! let Exit::Success(Some(receipt)) = engine.result::<Checkout>(&id).block_on() else { panic!() };
//! assert_eq!((receipt.order, receipt.charged), (7, 42));
//! ```
//!
//! # Design
//!
//! This section is the design the 0.11 workflow leaves build on (r11-e7-5-engine; activities,
//! timers and signals follow in r11-e7-5-activities-timers, sagas in r11-e7-5-sagas-promote).
//!
//! ## Model: journal and replay (D7)
//!
//! A [`Workflow`] is a type with a [`NAME`](Workflow::NAME), a [`VERSION`](Workflow::VERSION),
//! a schema-encoded [`Input`](Workflow::Input) and [`Output`](Workflow::Output) (both
//! [`Journaled`]: `HasSchema<A = Self, I = Unknown, E = ()>`, `#[derive(Schema)]`), and a body
//! `run(ctx, input) -> Effect<Output, WorkflowError>`. An instance is one execution of it under a
//! [`WorkflowId`]. The body is replayed from the start every time an engine resumes the
//! instance; everything non-deterministic goes through the [`WorkflowContext`], whose calls each
//! take the next history position `seq` (1, 2, …):
//!
//! | Call | First run | Replay |
//! |---|---|---|
//! | [`ctx.step(name, effect)`](WorkflowContext::step) | runs `effect`, journals `{"ok": value}` or `{"err": message}` | returns the journaled result; `effect` does not run |
//! | [`ctx.now()`](WorkflowContext::now) | reads the engine's [`WallClock`](id_effect::WallClock), journals it | the journaled instant |
//! | [`ctx.random()`](WorkflowContext::random) | draws from the engine's [`Random`](id_effect::Random), journals it | the journaled value |
//!
//! Replay is keyed by `seq` and checked by kind and step name (the Temporal / Durable Task
//! model): a call whose kind or name differs from the journaled one at its `seq`, or a body that
//! ends with history left over, fails with [`WorkflowError::NonDeterministic`] and fails the
//! instance. Steps are at least once: a step whose result was not journaled (crash, lease lost)
//! runs again on resume, so its effect must be idempotent. Steps run one after another.
//! [`VERSION`](Workflow::VERSION) is recorded on each instance; routing old instances to old
//! code is E13.5 (0.16).
//!
//! ## Schema (`migrations/0001_id_effect_workflow.up.sql`, [`migrator`])
//!
//! - `workflow_instances (id text PK, name, version, status 'running'|'completed'|'failed',
//!   input jsonb, output jsonb, error, lease_owner, lease_expires_at, fencing_token bigint,
//!   created_at, updated_at)`, partial index `(name, created_at, id) WHERE status = 'running'`.
//! - `workflow_history (instance_id → workflow_instances, seq bigint, kind, name, payload jsonb,
//!   recorded_at, UNIQUE (instance_id, seq))`.
//!
//! Times are bound from the engine's clock, never the database's `now()` (as in
//! `id_effect_jobs`); [`migrator`] tracks its versions in `_id_effect_workflow_migrations`.
//!
//! ## Multi-writer: leases and fencing (D7)
//!
//! Any number of [`WorkflowEngine`]s run against one database. An engine advances an instance
//! only while it holds the instance's lease:
//!
//! 1. **Acquire** ([`WorkflowStore::claim`] with `FOR UPDATE SKIP LOCKED`, or
//!    [`WorkflowStore::acquire`] for one id): only when `lease_owner` is null or
//!    `lease_expires_at` has passed; sets the owner and expiry and increments `fencing_token`.
//! 2. **Append** ([`WorkflowStore::append`]): one statement whose `UPDATE` of the instance row
//!    checks owner, token, `status = 'running'` and `lease_expires_at > now` (and extends the
//!    lease), and inserts the history row only when it matched; `ON CONFLICT (instance_id, seq)
//!    DO NOTHING`. A takeover locks the same row, so a stale writer's append either happened
//!    while its lease was valid or is rejected ([`Fenced::Lost`]) — it can never append after
//!    another engine took over, nor after its own lease ran out.
//! 3. **Heartbeat**: while a run is in flight the engine renews the lease every third of
//!    [`EngineConfig::lease`]; a rejected renewal stops the run ([`RunOutcome::LeaseLost`]).
//! 4. **Finish** ([`WorkflowStore::finish`]): fenced like the append; stores the output or the
//!    error and frees the lease.
//!
//! A crashed engine's lease runs out; another engine claims the instance (a new token) and
//! resumes it from its history, re-running only the step that was in flight.
//!
//! ## Stores
//!
//! [`WorkflowStore`] is the persistence trait; [`PgWorkflowStore`] implements it on any
//! `id_effect_sql` [`SqlClient`](id_effect_sql::SqlClient) (D6: no duroxide, no SQLite), and
//! [`MemoryWorkflowStore`] in memory with the same rules for unit tests.
//!
//! ## API
//!
//! [`WorkflowEngine::register`], [`start`](WorkflowEngine::start) (idempotent per id),
//! [`status`](WorkflowEngine::status), [`result`](WorkflowEngine::result),
//! [`instance`](WorkflowEngine::instance); drivers [`run`](WorkflowEngine::run) (forever),
//! [`run_once`](WorkflowEngine::run_once) and [`resume`](WorkflowEngine::resume).
//!
//! # Names
//!
//! [`WorkflowName`] is the validated name a workflow is registered and started under: 1 to
//! [`WorkflowName::MAX_LEN`] ASCII letters, digits, `.`, `_` or `-`.
//!
//! ```
//! use id_effect_workflow::{WorkflowName, WorkflowNameError};
//!
//! let name = WorkflowName::new("close-account").unwrap();
//! assert_eq!(name.to_string(), "close-account");
//! assert_eq!(WorkflowName::new(""), Err(WorkflowNameError::Empty));
//! ```

#![deny(missing_docs)]

mod engine;
mod error;
mod history;
pub mod lease;
mod memory;
mod pg;
mod workflow;

pub use engine::{EngineConfig, RunOutcome, Started, WorkflowEngine};
pub use error::WorkflowError;
pub use history::{
  Claimed, EventKind, Finish, HistoryEvent, InstanceRecord, NewInstance, WorkflowStatus,
  WorkflowStore,
};
pub use lease::{EngineId, Fenced, Lease};
pub use memory::MemoryWorkflowStore;
pub use pg::{PgWorkflowStore, migrator};
pub use workflow::{Journaled, Workflow, WorkflowContext, WorkflowId};

use std::fmt;

/// The name a workflow is registered and started under.
///
/// 1 to [`WorkflowName::MAX_LEN`] ASCII letters, digits, `.`, `_` or `-`, so it is safe as a
/// database key and in log lines.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct WorkflowName(String);

impl WorkflowName {
  /// The longest accepted name, in bytes.
  pub const MAX_LEN: usize = 128;

  /// Validates `name`.
  pub fn new(name: impl Into<String>) -> Result<Self, WorkflowNameError> {
    let name = name.into();
    if name.is_empty() {
      return Err(WorkflowNameError::Empty);
    }
    if let Some((index, ch)) = name
      .char_indices()
      .find(|&(_, ch)| !(ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-')))
    {
      return Err(WorkflowNameError::InvalidChar { ch, index });
    }
    if name.len() > Self::MAX_LEN {
      return Err(WorkflowNameError::TooLong { len: name.len() });
    }
    Ok(Self(name))
  }

  /// The name as a string slice.
  pub fn as_str(&self) -> &str {
    &self.0
  }
}

impl fmt::Display for WorkflowName {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.pad(&self.0)
  }
}

/// Why [`WorkflowName::new`] rejected a name.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum WorkflowNameError {
  /// The name is empty.
  Empty,
  /// The name is longer than [`WorkflowName::MAX_LEN`] bytes.
  TooLong {
    /// The length of the rejected name, in bytes.
    len: usize,
  },
  /// The name contains a character other than an ASCII letter, digit, `.`, `_` or `-`.
  InvalidChar {
    /// The first rejected character.
    ch: char,
    /// Its byte index in the name.
    index: usize,
  },
}

impl fmt::Display for WorkflowNameError {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    match self {
      Self::Empty => f.write_str("workflow name is empty"),
      Self::TooLong { len } => write!(
        f,
        "workflow name is {len} bytes, longer than {}",
        WorkflowName::MAX_LEN
      ),
      Self::InvalidChar { ch, index } => {
        write!(
          f,
          "workflow name has {ch:?} at byte {index}; use ASCII letters, digits, '.', '_' or '-'"
        )
      }
    }
  }
}

impl std::error::Error for WorkflowNameError {}
