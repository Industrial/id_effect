//! Durable workflows for id_effect services (ADR 0010 Amendment 7): a history journal replayed
//! on resume, on `id_effect_sql` (Postgres), with any number of engines against one database,
//! activities and durable timers as `id_effect_jobs` jobs, and signals (release 0.11.0 decisions
//! D6 and D7).
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
//! timers and signals in r11-e7-5-activities-timers; sagas follow in r11-e7-5-sagas-promote).
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
//! | [`ctx.activity::<A>(input)`](WorkflowContext::activity) | enqueues a job, parks; once delivered, journals `{"ok"}` / `{"err"}` | the journaled result |
//! | [`ctx.sleep(d)`](WorkflowContext::sleep), [`sleep_until(t)`](WorkflowContext::sleep_until) | enqueues a timer job at `run_at`, parks; once fired, journals `{"due"}` | returns at once |
//! | [`ctx.wait_signal::<P>(name)`](WorkflowContext::wait_signal) | takes the next stored signal of `name` or parks; journals `{"number", "payload"}` | the journaled signal |
//!
//! Replay is keyed by `seq` and checked by kind and step name (the Temporal / Durable Task
//! model): a call whose kind or name differs from the journaled one at its `seq`, or a body that
//! ends with history left over, fails with [`WorkflowError::NonDeterministic`] and fails the
//! instance; the divergence is sticky (every later call of the run fails with it), so a body
//! that catches it cannot journal past it. Steps are at least once: a step whose result was
//! not journaled (crash, lease lost) runs again on resume, so its effect must be idempotent.
//! Calls run one after another.
//! [`VERSION`](Workflow::VERSION) is recorded on each instance, not checked; routing old
//! instances to old code is E13.5 (0.16).
//!
//! ## Waiting: park and wake
//!
//! An activity, a timer or a signal that has not arrived yet **parks** the run: the call fails
//! with [`WorkflowError::Waiting`] (and so does every later call of that run), the engine sets
//! `waiting` on the instance and frees its lease ([`WorkflowStore::wait`],
//! [`RunOutcome::Waiting`]). A parked instance is not claimable. What it waits for arrives as a
//! [`Completion`] ([`WorkflowStore::complete`]: an activity's result or a fired timer, keyed by
//! `(instance, seq)`) or a signal ([`WorkflowStore::signal`]); each clears `waiting` and
//! increments the instance's `wakeups` in the statement that stores it. The next run replays
//! the history, reaches the waiting call at the same `seq`, finds the completion or signal and
//! journals it there, under the lease and fenced like any append. Only the lease holder writes
//! history; workers and senders write completions and signals.
//!
//! No wake-up is lost: a run reads `wakeups` when it claims the instance and parks only while
//! it is unchanged (one fenced `UPDATE … AND wakeups = $seen`, serialised with the wake-ups on
//! the instance row). A completion or signal that lands after the claim makes the park match
//! nothing; the engine frees the lease instead and the instance runs again at once.
//!
//! ## Activities (D6)
//!
//! An [`Activity`] is a type with a [`NAME`](Activity::NAME), schema-encoded `Input` and
//! `Output`, and `run(ctx, input) -> Effect<Output, JobFailure>`. The workflow side only names
//! the type; workers get the implementation from
//! [`WorkflowEngine::register_activity`] on their `id_effect_jobs::JobRegistry`.
//!
//! - **Scheduling** ([`ctx.activity`](WorkflowContext::activity) at a fresh `seq`): when no
//!   completion is delivered for `seq`, a fenced lease renewal (a stale run schedules nothing),
//!   then one job on [`Activity::QUEUE`] (default [`ACTIVITY_QUEUE`]) with the unique key
//!   `<instance>#<seq>` held until the job row is pruned. Scheduling the same call again (a
//!   crash before the park, a replay woken by an unrelated signal, two engines) inserts
//!   nothing, so one call is one job.
//! - **Execution**: at least once, by any `id_effect_jobs::Worker` serving the queue, on any
//!   node. A typed failure is retried after [`Activity::retry`] (a core
//!   [`Schedule`](id_effect::Schedule)) up to [`Activity::MAX_ATTEMPTS`]; a worker that dies
//!   mid-run loses its job lease and another worker rescues and reruns the job.
//! - **Delivery**: the handler records `{"ok": output}`, or `{"err": message}` once the
//!   failure is final (a discard, the last attempt, a done schedule), as the completion of
//!   `(instance, seq)`: `INSERT … ON CONFLICT DO NOTHING`, so a duplicate completion (a rescued
//!   attempt that ran twice) is ignored and the first result wins. The result reaches the
//!   history only through the next run of the instance, at `seq`.
//! - A job that dies without a recorded result (its attempts used up by rescues of crashed
//!   workers, or a defect on the last attempt) leaves the instance parked; retrying the job by
//!   hand (`JobQueue::retry_now`) resumes it.
//!
//! ## Durable timers (D6)
//!
//! [`ctx.sleep(d)`](WorkflowContext::sleep) / [`sleep_until(t)`](WorkflowContext::sleep_until)
//! at a fresh `seq` enqueue one job on [`TIMER_QUEUE`] with `run_at` the due instant (the
//! engine's clock at the first schedule plus `d`) and the unique key `<instance>#<seq>`, and
//! park. Workers run it once `run_at` has come on the queue's clock
//! ([`WorkflowEngine::register_timers`]); the handler delivers `{"due": …}` as the completion.
//! The timer lives in the job table, not in an engine, so it survives engine restarts and
//! moves between nodes; with a test clock (`WallClock::anchored`, `jump`) waits of days
//! complete at once.
//!
//! ## Signals (D6)
//!
//! [`WorkflowClient::signal(instance, name, payload)`](WorkflowClient::signal) stores the
//! encoded payload in `workflow_signals` numbered by the instance's incremented `wakeups`, so
//! numbers grow in commit order (the instance row lock serialises senders).
//! [`ctx.wait_signal(name)`](WorkflowContext::wait_signal) takes the first signal of `name`
//! numbered after the last one this instance consumed (known from the replayed history), so
//! signals are received in arrival order, one per call, and a signal sent before the wait is
//! buffered until it is taken.
//!
//! ## Schema (`migrations/0001_*.up.sql`, `0002_*.up.sql`, [`migrator`])
//!
//! - `workflow_instances (id text PK, name, version, status 'running'|'completed'|'failed',
//!   input jsonb, output jsonb, error, lease_owner, lease_expires_at, fencing_token bigint,
//!   created_at, updated_at)`, partial index `(name, created_at, id) WHERE status = 'running'`.
//! - `workflow_history (instance_id → workflow_instances, seq bigint, kind, name, payload jsonb,
//!   recorded_at, UNIQUE (instance_id, seq))`.
//!
//! `migrations/0002_activities_timers_signals.up.sql` adds:
//!
//! - `workflow_instances.waiting boolean` and `wakeups bigint`; the claim index becomes
//!   `(name, created_at, id) WHERE status = 'running' AND NOT waiting`.
//! - `workflow_completions (instance_id, seq, kind 'activity'|'timer', name, payload jsonb,
//!   completed_at, PRIMARY KEY (instance_id, seq))`: one delivery per idempotency key.
//! - `workflow_signals (instance_id, number, name, payload jsonb, received_at, PRIMARY KEY
//!   (instance_id, number))` and an index `(instance_id, name, number)`.
//!
//! Activity and timer jobs live in `id_effect_jobs`' own table (its `migrator`).
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
//! [`run_once`](WorkflowEngine::run_once) and [`resume`](WorkflowEngine::resume). Activities and
//! timers: [`with_job_queue`](WorkflowEngine::with_job_queue) on the engines,
//! [`register_activity`](WorkflowEngine::register_activity) and
//! [`register_timers`](WorkflowEngine::register_timers) on the workers' registry. Signals:
//! [`WorkflowEngine::client`] or [`WorkflowClient`].
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

mod activity;
mod engine;
mod error;
mod history;
pub mod lease;
mod memory;
mod pg;
mod signal;
mod timer;
mod workflow;

pub use activity::{ACTIVITY_QUEUE, Activity, ActivityContext};
pub use engine::{EngineConfig, RunOutcome, Started, WorkflowEngine};
pub use error::WorkflowError;
pub use history::{
  Claimed, Completion, EventKind, Finish, HistoryEvent, InstanceRecord, NewInstance, SignalRecord,
  WorkflowStatus, WorkflowStore,
};
/// What activities fail with and are retried by: an activity's `run` returns a [`JobFailure`]
/// and its [`Activity::retry`] schedule steps on it.
pub use id_effect_jobs::JobFailure;
pub use lease::{EngineId, Fenced, Lease};
pub use memory::MemoryWorkflowStore;
pub use pg::{PgWorkflowStore, migrator};
pub use signal::WorkflowClient;
pub use timer::TIMER_QUEUE;
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
