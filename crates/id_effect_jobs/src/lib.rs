//! Background jobs for id_effect services: a Postgres `SKIP LOCKED` queue on `id_effect_sql`,
//! workers, retries with backoff and cron schedules.
//!
//! - A **[`Job`]** is a `#[derive(Schema)]` struct with a stable [`Job::NAME`], a queue, a
//!   priority, a maximum number of attempts, an optional [`UniqueKey`] and a retry
//!   [`Schedule`](id_effect::Schedule). Its payload is stored as `jsonb` through its schema.
//! - The **[`JobQueue`]** capability enqueues ([`JobQueueExt::enqueue`], [`enqueue`]) and
//!   administers jobs; [`JobStore`] adds the worker side (claim, fenced outcomes, heartbeat,
//!   rescue). [`PgJobQueue`] stores them in the table `id_effect_jobs` ([`migrator`],
//!   [`PgJobQueue::ensure_schema`]), [`MemoryJobQueue`] in memory for tests.
//! - **[`enqueue_in`]** inserts a job with the caller's
//!   [`SqlTransaction`](id_effect_sql::SqlTransaction), so it commits or rolls back with the
//!   caller's work.
//! - A **[`Worker`]** runs the handlers of a [`JobRegistry`] with a concurrency limit per queue,
//!   a heartbeat and lease, rescues the jobs of crashed workers, and drains on shutdown.
//! - With the feature `cron`, **[`enqueue_on_tick`]** makes a core cron entry enqueue one job per
//!   tick (deduplicated across schedulers by [`tick_key`]), and **[`CronLeader`]** runs the
//!   schedule on the node holding a cluster singleton.
//!
//! Every time the queue reads (`run_at`, leases, `finished_at`) comes from a
//! [`WallClock`](id_effect::WallClock), so tests run on virtual time. Delivery is at least once:
//! a job whose worker died mid-run runs again, so handlers must be idempotent.
//!
//! [`JobState`] is the lifecycle every queued job moves through; [`JobState::as_str`] is the value
//! stored in the queue table's `state` column.
//!
//! ```
//! use id_effect_jobs::JobState;
//!
//! let state: JobState = "failed".parse().unwrap();
//! assert!(!state.is_terminal());
//! assert_eq!(state.as_str(), "failed");
//! ```
//!
//! A job, its handler, and a worker draining the queue on a test executor:
//!
//! ```
//! use std::sync::Arc;
//! use std::sync::atomic::{AtomicU32, Ordering};
//!
//! use id_effect::{Executor, Schema, TestExecutor, UtcDateTime, WallClock, hlist, sync};
//! use id_effect_jobs::{
//!   Job, JobFailure, JobOptions, JobQueueExt, JobQueueService, JobRegistry, MemoryJobQueue, Worker,
//! };
//!
//! #[derive(Clone, Schema)]
//! struct Welcome {
//!   email: String,
//! }
//!
//! impl Job for Welcome {
//!   const NAME: &'static str = "app.welcome";
//! }
//!
//! let sent = Arc::new(AtomicU32::new(0));
//! let s = Arc::clone(&sent);
//! let registry = JobRegistry::new()
//!   .register::<Welcome, JobFailure>(move |job, _ctx| {
//!     let s = Arc::clone(&s);
//!     sync(move || {
//!       assert_eq!(job.email, "ada@example.com");
//!       s.fetch_add(1, Ordering::SeqCst);
//!     })
//!   })
//!   .unwrap();
//!
//! let exec = TestExecutor::new();
//! let clock = WallClock::anchored(exec.now(), UtcDateTime::unsafe_make(1_800_000_000_000));
//! let store = MemoryJobQueue::new(clock);
//! let env = hlist![JobQueueService::new(store.clone())];
//! let job = Welcome { email: "ada@example.com".into() };
//! let program = store
//!   .enqueue(job, JobOptions::default())
//!   .flat_map(move |_| Worker::new(registry).run_until_idle().provide_all(env));
//! let done = program.fork_root(Arc::new(exec.clone()));
//! exec.run_until_stalled();
//! assert!(done.is_done());
//! assert_eq!(sent.load(Ordering::SeqCst), 1);
//! ```
//!
//! Tier `platform` (ADR 0010 Amendment 6): the Ledger reference app under
//! `examples/reference/` depends on it.

#![forbid(unsafe_code)]
#![deny(missing_docs)]

use std::fmt;
use std::str::FromStr;

#[cfg(feature = "cron")]
mod cron;
mod job;
mod memory;
mod pg;
mod queue;
mod worker;

#[cfg(feature = "cron")]
pub use cron::{CronLeader, enqueue_on_tick, tick_key};
/// What [`CronLeader::new`] is built from: the singleton and its PostgreSQL lease
/// (`id_effect_node`), and the local process id (`id_effect_process`), so a service reaches them
/// through this crate.
#[cfg(feature = "cron")]
pub use id_effect_node::singleton::{ClusterSingleton, PgAdvisoryLease, SingletonError};
#[cfg(feature = "cron")]
pub use id_effect_process::{NodeName, Pid};
pub use job::{
  Job, JobContext, JobFailure, JobId, JobRegistry, Priority, Queue, UniqueKey, UniqueScope,
  default_retry, retry_delay,
};
pub use memory::{MemoryJobQueue, memory_job_queue_layer};
pub use pg::{PgJobQueue, enqueue_in, migrator, pg_job_queue_layer};
pub use queue::{
  Claimed, Enqueued, Fenced, JobError, JobFilter, JobOptions, JobQueue, JobQueueEnv, JobQueueExt,
  JobQueueService, JobRecord, JobStore, NewJob, QueueStats, RunAt, WakeSignal, enqueue,
  job_queue_env,
};
pub use worker::{
  DrainReport, JobObserver, NoopObserver, Outcome, Worker, WorkerConfig, WorkerHandle, WorkerId,
};

/// Lifecycle of a queued job.
///
/// `scheduled → running → succeeded`; a failed attempt goes `running → failed → running …` until
/// it succeeds or its attempts are exhausted (`dead`). `scheduled` and `failed` jobs can be
/// `cancelled`; `dead` and `cancelled` jobs can be retried by hand ([`JobQueue::retry_now`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum JobState {
  /// Waiting for its `run_at` time and a free worker.
  Scheduled,
  /// Claimed by a worker.
  Running,
  /// Finished successfully.
  Succeeded,
  /// Failed; runs again after its backoff unless its attempts are exhausted.
  Failed,
  /// Out of attempts (or discarded); kept for inspection and a manual retry.
  Dead,
  /// Cancelled before it ran; kept for inspection and a manual retry.
  Cancelled,
}

impl JobState {
  /// Every state, in lifecycle order.
  pub const ALL: [JobState; 6] = [
    JobState::Scheduled,
    JobState::Running,
    JobState::Succeeded,
    JobState::Failed,
    JobState::Dead,
    JobState::Cancelled,
  ];

  /// The column value: `scheduled`, `running`, `succeeded`, `failed`, `dead` or `cancelled`.
  pub const fn as_str(self) -> &'static str {
    match self {
      JobState::Scheduled => "scheduled",
      JobState::Running => "running",
      JobState::Succeeded => "succeeded",
      JobState::Failed => "failed",
      JobState::Dead => "dead",
      JobState::Cancelled => "cancelled",
    }
  }

  /// Whether no worker picks the job up again from this state without a manual retry.
  pub const fn is_terminal(self) -> bool {
    matches!(
      self,
      JobState::Succeeded | JobState::Dead | JobState::Cancelled
    )
  }

  /// Whether a worker may claim the job once its `run_at` has come: `scheduled`, or `failed`
  /// (awaiting its retry).
  pub const fn is_runnable(self) -> bool {
    matches!(self, JobState::Scheduled | JobState::Failed)
  }

  /// Whether the job holds a [`UniqueScope::WhilePending`] key: runnable or running.
  pub const fn is_pending(self) -> bool {
    matches!(
      self,
      JobState::Scheduled | JobState::Running | JobState::Failed
    )
  }
}

impl fmt::Display for JobState {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.write_str(self.as_str())
  }
}

/// A `state` column value that names no [`JobState`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UnknownJobState(pub String);

impl fmt::Display for UnknownJobState {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    write!(f, "unknown job state `{}`", self.0)
  }
}

impl std::error::Error for UnknownJobState {}

impl FromStr for JobState {
  type Err = UnknownJobState;

  fn from_str(s: &str) -> Result<Self, Self::Err> {
    JobState::ALL
      .into_iter()
      .find(|state| state.as_str() == s)
      .ok_or_else(|| UnknownJobState(s.to_owned()))
  }
}

/// Compiles and runs the README's examples as doctests.
#[cfg(doctest)]
#[doc = include_str!("../README.md")]
struct ReadmeDoctests;
