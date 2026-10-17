//! The queue capability: [`JobQueue`] (producers and administration) and [`JobStore`] (the
//! worker side), the values they exchange, the [`JobQueueService`] capability and the typed
//! enqueue sugar ([`JobQueueExt`], [`enqueue`]).

use std::collections::BTreeMap;
use std::fmt;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use id_effect::schema::serde_json_from_unknown;
use id_effect::{Deferred, Effect, HCons, HList, HNil, Never, Selector, UtcDateTime, hlist, need};
use id_effect_sql::SqlError;

use crate::JobState;
use crate::job::{Job, JobFailure, JobId, Priority, Queue, UniqueKey};

/// The most bytes of a failure's text kept as a job's `last_error`.
pub(crate) const MAX_ERROR_BYTES: usize = 4096;

/// When an enqueued job may first run. Resolved on the store's
/// [`WallClock`](id_effect::WallClock) when the job is inserted.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum RunAt {
  /// As soon as a worker is free.
  #[default]
  Now,
  /// At this instant (or as soon as a worker is free after it).
  At(UtcDateTime),
  /// This long after it is enqueued.
  After(Duration),
}

impl RunAt {
  /// The instant this resolves to when `now` is the time of the insert.
  pub fn resolve(self, now: UtcDateTime) -> UtcDateTime {
    match self {
      RunAt::Now => now,
      RunAt::At(at) => at,
      RunAt::After(delay) => later(now, delay),
    }
  }
}

/// Per-enqueue choices that override the [`Job`]'s own: queue, priority, unique key, and when it
/// may run (default: now, on [`Job::QUEUE`] with the job's priority and key).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct JobOptions {
  /// The queue (default [`Job::QUEUE`]).
  pub queue: Option<Queue>,
  /// The priority (default [`Job::priority`]).
  pub priority: Option<Priority>,
  /// When it may run (default [`RunAt::Now`]).
  pub run_at: Option<RunAt>,
  /// The unique key (default [`Job::unique_key`]).
  pub unique: Option<UniqueKey>,
}

impl JobOptions {
  /// No overrides.
  pub fn new() -> Self {
    Self::default()
  }

  /// Enqueue on `queue`.
  pub fn queue(mut self, queue: impl Into<Queue>) -> Self {
    self.queue = Some(queue.into());
    self
  }

  /// With `priority`.
  pub fn priority(mut self, priority: Priority) -> Self {
    self.priority = Some(priority);
    self
  }

  /// First run at `run_at`.
  pub fn run_at(mut self, run_at: RunAt) -> Self {
    self.run_at = Some(run_at);
    self
  }

  /// With `key`.
  pub fn unique(mut self, key: UniqueKey) -> Self {
    self.unique = Some(key);
    self
  }
}

/// A job ready to insert: [`NewJob::of`] encodes a [`Job`] with its options.
#[derive(Clone, Debug, PartialEq)]
pub struct NewJob {
  /// [`Job::NAME`].
  pub name: String,
  /// The queue.
  pub queue: Queue,
  /// The payload, encoded with the job's schema.
  pub payload: serde_json::Value,
  /// The priority.
  pub priority: Priority,
  /// The maximum number of attempts.
  pub max_attempts: u16,
  /// The unique key, if any.
  pub unique: Option<UniqueKey>,
  /// When it may first run.
  pub run_at: RunAt,
}

impl NewJob {
  /// `job` encoded with its schema, with `opts` overriding its own queue, priority and key.
  pub fn of<J: Job>(job: J, opts: JobOptions) -> Self {
    let priority = opts.priority.unwrap_or_else(|| job.priority());
    let unique = opts.unique.or_else(|| job.unique_key());
    let max_attempts = job.max_attempts();
    let payload = serde_json_from_unknown(&J::schema().encode(job));
    Self {
      name: J::NAME.to_owned(),
      queue: opts.queue.unwrap_or_else(|| Queue::new(J::QUEUE)),
      payload,
      priority,
      max_attempts,
      unique,
      run_at: opts.run_at.unwrap_or_default(),
    }
  }

  /// [`JobError::InvalidOptions`] for an empty name or queue or zero `max_attempts`.
  pub fn validate(&self) -> Result<(), JobError> {
    if self.name.is_empty() {
      return Err(JobError::InvalidOptions("the job name is empty".into()));
    }
    if self.queue.as_str().is_empty() {
      return Err(JobError::InvalidOptions(format!(
        "job `{}`: the queue name is empty",
        self.name
      )));
    }
    if self.max_attempts == 0 {
      return Err(JobError::InvalidOptions(format!(
        "job `{}`: max_attempts must be at least 1",
        self.name
      )));
    }
    Ok(())
  }
}

/// The result of an enqueue.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Enqueued {
  /// A new job was inserted.
  New(JobId),
  /// A job holding the same [`UniqueKey`] in the same queue exists, so nothing was inserted.
  /// `existing` is `None` when that job was committed by a concurrent transaction after this one
  /// started.
  Duplicate {
    /// The job that holds the key.
    existing: Option<JobId>,
  },
}

impl Enqueued {
  /// The new job's id, or the id of the job that holds the key.
  pub fn id(&self) -> Option<JobId> {
    match *self {
      Enqueued::New(id) => Some(id),
      Enqueued::Duplicate { existing } => existing,
    }
  }

  /// Whether a job was inserted.
  pub fn is_new(&self) -> bool {
    matches!(self, Enqueued::New(_))
  }
}

/// One row of the queue, as [`JobQueue::list`] returns it.
#[derive(Clone, Debug, PartialEq)]
pub struct JobRecord {
  /// The id.
  pub id: JobId,
  /// [`Job::NAME`].
  pub name: String,
  /// The queue.
  pub queue: Queue,
  /// The encoded payload.
  pub payload: serde_json::Value,
  /// The state.
  pub state: JobState,
  /// The priority.
  pub priority: Priority,
  /// Attempts started so far (counted when a worker claims the job).
  pub attempts: u16,
  /// The maximum number of attempts.
  pub max_attempts: u16,
  /// The unique key, if any.
  pub unique: Option<UniqueKey>,
  /// When it may run next.
  pub run_at: UtcDateTime,
  /// When it was enqueued.
  pub inserted_at: UtcDateTime,
  /// When the last attempt started.
  pub attempted_at: Option<UtcDateTime>,
  /// When it reached `succeeded`, `dead` or `cancelled`.
  pub finished_at: Option<UtcDateTime>,
  /// The worker running it.
  pub locked_by: Option<String>,
  /// When that worker's lease runs out.
  pub locked_until: Option<UtcDateTime>,
  /// The text of the last failure.
  pub last_error: Option<String>,
}

/// A job a worker claimed: it is `running`, leased to that worker, with its attempt counted.
#[derive(Clone, Debug, PartialEq)]
pub struct Claimed {
  /// The id.
  pub id: JobId,
  /// [`Job::NAME`].
  pub name: String,
  /// The queue.
  pub queue: Queue,
  /// The encoded payload.
  pub payload: serde_json::Value,
  /// This attempt's number, from 1.
  pub attempt: u16,
  /// The maximum number of attempts.
  pub max_attempts: u16,
  /// The priority.
  pub priority: Priority,
  /// When it became runnable.
  pub run_at: UtcDateTime,
  /// When it was enqueued.
  pub inserted_at: UtcDateTime,
}

/// Whether a worker's outcome was recorded. Outcomes are fenced on the worker's lease: once the
/// lease expired and the job was rescued, the late outcome is dropped.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fenced {
  /// Recorded.
  Applied,
  /// The job is no longer `running` under this worker; nothing changed.
  Lost,
}

/// Per-queue counts, as [`JobQueue::stats`] returns them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QueueStats {
  /// The queue.
  pub queue: Queue,
  /// Jobs per state (states without jobs are absent).
  pub counts: BTreeMap<JobState, u64>,
  /// The earliest `run_at` among the `scheduled` and `failed` jobs.
  pub oldest_runnable: Option<UtcDateTime>,
}

impl QueueStats {
  /// The number of jobs in `state`.
  pub fn count(&self, state: JobState) -> u64 {
    self.counts.get(&state).copied().unwrap_or(0)
  }
}

/// Which jobs [`JobQueue::list`] returns: newest first, at most `limit`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JobFilter {
  /// Only this queue.
  pub queue: Option<Queue>,
  /// Only this state.
  pub state: Option<JobState>,
  /// At most this many (default 100).
  pub limit: u32,
  /// Only ids below this one (the next page after a last id).
  pub before: Option<JobId>,
}

impl Default for JobFilter {
  fn default() -> Self {
    Self {
      queue: None,
      state: None,
      limit: 100,
      before: None,
    }
  }
}

impl JobFilter {
  /// Whether `record` passes the filter (ignoring `limit`).
  pub fn matches(&self, record: &JobRecord) -> bool {
    self.queue.as_ref().is_none_or(|q| *q == record.queue)
      && self.state.is_none_or(|s| s == record.state)
      && self.before.is_none_or(|b| record.id < b)
  }
}

/// A queue operation's failure.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum JobError {
  /// The database failed (or a row did not decode).
  Sql(SqlError),
  /// A payload could not be encoded.
  Encode(String),
  /// A [`JobRegistry`](crate::JobRegistry) already has a handler for this name.
  DuplicateJob(String),
  /// A job's options are invalid (zero `max_attempts`, an empty queue name).
  InvalidOptions(String),
  /// No job has this id.
  NotFound(JobId),
}

impl fmt::Display for JobError {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    match self {
      JobError::Sql(e) => write!(f, "job queue: {e}"),
      JobError::Encode(m) => write!(f, "job payload: {m}"),
      JobError::DuplicateJob(name) => write!(f, "job `{name}` is already registered"),
      JobError::InvalidOptions(m) => write!(f, "invalid job options: {m}"),
      JobError::NotFound(id) => write!(f, "no job with id {id}"),
    }
  }
}

impl std::error::Error for JobError {
  fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
    match self {
      JobError::Sql(e) => Some(e),
      _ => None,
    }
  }
}

impl From<SqlError> for JobError {
  fn from(e: SqlError) -> Self {
    JobError::Sql(e)
  }
}

/// The producer and administration side of a job queue. Object safe; the typed enqueue methods
/// are in [`JobQueueExt`].
pub trait JobQueue: Send + Sync + 'static {
  /// Inserts `job`, unless a job holding its [`UniqueKey`] exists ([`Enqueued::Duplicate`]).
  fn enqueue_raw(&self, job: NewJob) -> Effect<Enqueued, JobError>;

  /// `scheduled` or `failed` → `cancelled`; `false` for a job in another state (a `running`
  /// job is not interrupted) or no job.
  fn cancel(&self, id: JobId) -> Effect<bool, JobError>;

  /// `dead` or `cancelled` → `scheduled` now, with its attempts reset; `false` for a job in
  /// another state or no job.
  fn retry_now(&self, id: JobId) -> Effect<bool, JobError>;

  /// Per-queue counts per state, by queue name.
  fn stats(&self) -> Effect<Vec<QueueStats>, JobError>;

  /// The jobs that pass `filter`, newest first.
  fn list(&self, filter: JobFilter) -> Effect<Vec<JobRecord>, JobError>;
}

/// The worker side of a job queue.
pub trait JobStore: JobQueue {
  /// Claims up to `limit` runnable jobs of `queue` whose `run_at` has come (higher priority
  /// first, then earlier `run_at`), skipping rows other workers are claiming: each becomes
  /// `running`, leased to `worker` for `lease`, with its attempt counted.
  fn claim(
    &self,
    queue: &Queue,
    worker: &str,
    limit: u16,
    lease: Duration,
  ) -> Effect<Vec<Claimed>, JobError>;

  /// `running` under `worker` → `succeeded`.
  fn complete(&self, id: JobId, worker: &str) -> Effect<Fenced, JobError>;

  /// `running` under `worker` → `failed`, runnable again after `retry_in`; or `dead` when
  /// `failure` is a discard, `retry_in` is `None`, or the attempts are exhausted. Records the
  /// failure's text (at most 4 KiB) as `last_error`.
  fn fail(
    &self,
    id: JobId,
    worker: &str,
    failure: &JobFailure,
    retry_in: Option<Duration>,
  ) -> Effect<Fenced, JobError>;

  /// Extends the lease of every job `running` under `worker` to `lease` from now; how many.
  fn heartbeat(&self, worker: &str, lease: Duration) -> Effect<u64, JobError>;

  /// Puts every job `running` under `worker` back to `scheduled`, refunding the attempt (a
  /// draining worker); how many.
  fn release(&self, worker: &str) -> Effect<u64, JobError>;

  /// Re-queues every `running` job whose lease expired (its worker crashed or hung): `failed`
  /// and runnable now, or `dead` when its attempts are exhausted; how many.
  fn rescue_expired(&self) -> Effect<u64, JobError>;

  /// Deletes the `succeeded`, `dead` and `cancelled` jobs finished before `finished_before`; how
  /// many.
  fn prune(&self, finished_before: UtcDateTime) -> Effect<u64, JobError>;

  /// The store's in-process wake-up: notified on every new job inserted through this store, so
  /// a worker in the same process polls at once instead of after its poll interval.
  fn wake_signal(&self) -> Option<WakeSignal> {
    None
  }
}

/// Typed enqueue on any [`JobQueue`].
pub trait JobQueueExt: JobQueue {
  /// Enqueues `job` with `opts`.
  fn enqueue<J: Job>(&self, job: J, opts: JobOptions) -> Effect<Enqueued, JobError> {
    self.enqueue_raw(NewJob::of(job, opts))
  }

  /// Enqueues `job` to run at `at`.
  fn enqueue_at<J: Job>(&self, job: J, at: UtcDateTime) -> Effect<Enqueued, JobError> {
    self.enqueue(job, JobOptions::new().run_at(RunAt::At(at)))
  }

  /// Enqueues `job` to run `delay` from now.
  fn enqueue_after<J: Job>(&self, job: J, delay: Duration) -> Effect<Enqueued, JobError> {
    self.enqueue(job, JobOptions::new().run_at(RunAt::After(delay)))
  }
}

impl<Q: JobQueue + ?Sized> JobQueueExt for Q {}

/// Capability: an `Arc<dyn JobStore>` in the typed environment (derefs to `dyn JobStore`).
#[derive(Clone)]
pub struct JobQueueService(pub Arc<dyn JobStore>);

impl JobQueueService {
  /// Wrap `store` as the capability.
  pub fn new(store: impl JobStore) -> Self {
    Self(Arc::new(store))
  }
}

impl std::ops::Deref for JobQueueService {
  type Target = dyn JobStore;

  fn deref(&self) -> &Self::Target {
    &*self.0
  }
}

impl fmt::Debug for JobQueueService {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_tuple("JobQueueService")
      .field(&"<dyn JobStore>")
      .finish()
  }
}

/// The minimal typed environment holding only [`JobQueueService`].
pub type JobQueueEnv = HCons<JobQueueService, HNil>;

/// A [`JobQueueEnv`] wrapping `store`.
pub fn job_queue_env(store: impl JobStore) -> JobQueueEnv {
  hlist![JobQueueService::new(store)]
}

/// Enqueues `job` with `opts` on the [`JobQueueService`] in `R`.
pub fn enqueue<J, R, I>(job: J, opts: JobOptions) -> Effect<Enqueued, JobError, R>
where
  J: Job,
  R: Selector<JobQueueService, I> + HList,
{
  need::<JobQueueService>().flat_map(move |queue| queue.enqueue(job, opts).narrow())
}

/// In-process wake-ups from a store to the workers polling it (see [`JobStore::wake_signal`]).
/// Cloning gives another handle to the same signal.
#[derive(Clone, Default)]
pub struct WakeSignal {
  inner: Arc<Mutex<WakeState>>,
}

#[derive(Default)]
struct WakeState {
  generation: u64,
  waiters: Deferred<(), Never>,
}

impl fmt::Debug for WakeSignal {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_struct("WakeSignal")
      .field("generation", &self.generation())
      .finish()
  }
}

impl WakeSignal {
  /// A signal nobody waits on yet.
  pub fn new() -> Self {
    Self::default()
  }

  fn state(&self) -> MutexGuard<'_, WakeState> {
    self.inner.lock().unwrap_or_else(PoisonError::into_inner)
  }

  /// Wakes every current waiter.
  pub fn notify(&self) {
    let woken = {
      let mut st = self.state();
      st.generation = st.generation.wrapping_add(1);
      std::mem::take(&mut st.waiters)
    };
    woken.try_succeed(());
  }

  /// How many notifications so far (read before the work a later [`wait_since`](Self::wait_since)
  /// waits after).
  pub fn generation(&self) -> u64 {
    self.state().generation
  }

  /// Completes at the first notification after `seen` ([`generation`](Self::generation)), at
  /// once if there was one already.
  pub fn wait_since<R: Send + Sync + 'static>(&self, seen: u64) -> Effect<(), Never, R> {
    let st = self.state();
    if st.generation != seen {
      return id_effect::succeed(());
    }
    let waiters = st.waiters.clone();
    drop(st);
    waiters.wait()
  }
}

/// `at + delay`, saturating at the latest representable instant.
pub(crate) fn later(at: UtcDateTime, delay: Duration) -> UtcDateTime {
  UtcDateTime(at.0.checked_add(delay).unwrap_or(jiff::Timestamp::MAX))
}

/// `at - delay`, saturating at the earliest representable instant.
pub(crate) fn earlier(at: UtcDateTime, delay: Duration) -> UtcDateTime {
  UtcDateTime(at.0.checked_sub(delay).unwrap_or(jiff::Timestamp::MIN))
}

/// `message`, cut to at most [`MAX_ERROR_BYTES`] at a character boundary.
pub(crate) fn truncate_error(message: &str) -> String {
  if message.len() <= MAX_ERROR_BYTES {
    return message.to_owned();
  }
  let mut end = MAX_ERROR_BYTES;
  while !message.is_char_boundary(end) {
    end -= 1;
  }
  message[..end].to_owned()
}

/// One `(queue, state, jobs, earliest run_at)` group per row, folded into [`QueueStats`] by queue.
pub(crate) fn fold_stats(
  groups: impl IntoIterator<Item = (Queue, JobState, u64, Option<UtcDateTime>)>,
) -> Vec<QueueStats> {
  let mut by_queue: BTreeMap<Queue, QueueStats> = BTreeMap::new();
  for (queue, state, jobs, oldest) in groups {
    let stats = by_queue.entry(queue.clone()).or_insert_with(|| QueueStats {
      queue,
      counts: BTreeMap::new(),
      oldest_runnable: None,
    });
    *stats.counts.entry(state).or_insert(0) += jobs;
    if state.is_runnable() {
      stats.oldest_runnable = match (stats.oldest_runnable, oldest) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (a, b) => a.or(b),
      };
    }
  }
  by_queue.into_values().collect()
}

#[cfg(test)]
mod tests {
  use std::sync::Arc;

  use id_effect::{Exit, Schema, TestExecutor};

  use super::*;

  #[derive(Clone, Schema)]
  struct Report {
    id: i64,
  }

  impl Job for Report {
    const NAME: &'static str = "test.report";
    const QUEUE: &'static str = "reports";

    fn priority(&self) -> Priority {
      Priority::LOW
    }

    fn max_attempts(&self) -> u16 {
      3
    }

    fn unique_key(&self) -> Option<UniqueKey> {
      Some(UniqueKey::while_pending(format!("report-{}", self.id)))
    }
  }

  fn at(secs: i64) -> UtcDateTime {
    UtcDateTime::unsafe_make(secs * 1000)
  }

  #[test]
  fn new_job_encodes_the_payload_and_applies_overrides() {
    let job = NewJob::of(Report { id: 4 }, JobOptions::new());
    assert_eq!(job.name, "test.report");
    assert_eq!(job.queue.as_str(), "reports");
    assert_eq!(job.payload, serde_json::json!({"id": 4}));
    assert_eq!(
      (job.priority, job.max_attempts, job.run_at),
      (Priority::LOW, 3, RunAt::Now)
    );
    assert_eq!(job.unique, Some(UniqueKey::while_pending("report-4")));
    let opts = JobOptions::new()
      .queue("urgent")
      .priority(Priority::HIGH)
      .run_at(RunAt::After(Duration::from_secs(5)))
      .unique(UniqueKey::until_pruned("x"));
    let job = NewJob::of(Report { id: 4 }, opts);
    assert_eq!(job.queue.as_str(), "urgent");
    assert_eq!(job.priority, Priority::HIGH);
    assert_eq!(job.unique, Some(UniqueKey::until_pruned("x")));
    assert_eq!(job.run_at.resolve(at(10)), at(15));
    assert_eq!(RunAt::At(at(3)).resolve(at(10)), at(3));
    assert_eq!(job.validate(), Ok(()));
  }

  #[test]
  fn invalid_options_are_rejected() {
    let mut job = NewJob::of(Report { id: 1 }, JobOptions::new());
    job.max_attempts = 0;
    assert!(
      matches!(job.validate(), Err(JobError::InvalidOptions(m)) if m.contains("max_attempts"))
    );
    job.max_attempts = 1;
    job.queue = Queue::new("");
    assert!(matches!(job.validate(), Err(JobError::InvalidOptions(m)) if m.contains("queue")));
    job.name.clear();
    assert!(job.validate().is_err());
  }

  #[test]
  fn errors_display_and_enqueued_reports_ids() {
    assert_eq!(
      JobError::from(SqlError::NotConnected).to_string(),
      "job queue: sql client not connected"
    );
    assert!(std::error::Error::source(&JobError::Sql(SqlError::NotConnected)).is_some());
    assert!(std::error::Error::source(&JobError::NotFound(JobId(1))).is_none());
    assert_eq!(JobError::NotFound(JobId(9)).to_string(), "no job with id 9");
    assert!(JobError::Encode("x".into()).to_string().contains("payload"));
    assert_eq!(Enqueued::New(JobId(1)).id(), Some(JobId(1)));
    let dup = Enqueued::Duplicate { existing: None };
    assert!(!dup.is_new() && dup.id().is_none());
  }

  #[test]
  fn last_error_is_truncated_at_a_char_boundary() {
    assert_eq!(truncate_error("short"), "short");
    let long = "é".repeat(3000);
    let cut = truncate_error(&long);
    assert!(cut.len() <= MAX_ERROR_BYTES && cut.len() >= MAX_ERROR_BYTES - 1);
    assert!(cut.chars().all(|c| c == 'é'));
    assert_eq!(earlier(at(10), Duration::from_secs(4)), at(6));
  }

  #[test]
  fn stats_fold_counts_by_queue_and_the_oldest_runnable() {
    let a = Queue::new("a");
    let stats = fold_stats([
      (a.clone(), JobState::Scheduled, 2, Some(at(5))),
      (a.clone(), JobState::Failed, 1, Some(at(3))),
      (a.clone(), JobState::Succeeded, 4, Some(at(1))),
      (Queue::new("b"), JobState::Dead, 1, Some(at(0))),
    ]);
    assert_eq!(stats.len(), 2);
    assert_eq!(stats[0].count(JobState::Scheduled), 2);
    assert_eq!(stats[0].count(JobState::Running), 0);
    assert_eq!(
      stats[0].oldest_runnable,
      Some(at(3)),
      "succeeded rows do not count"
    );
    assert_eq!(stats[1].oldest_runnable, None);
  }

  #[test]
  fn wake_signal_wakes_waiters_after_a_generation() {
    let exec = TestExecutor::new();
    let signal = WakeSignal::new();
    let seen = signal.generation();
    let waiter = signal
      .wait_since::<HNil>(seen)
      .fork_root(Arc::new(exec.clone()));
    exec.run_until_stalled();
    assert!(!waiter.is_done());
    signal.notify();
    exec.run_until_stalled();
    assert!(waiter.is_done());
    assert!(matches!(
      signal.wait_since::<HNil>(seen).block_on(),
      Exit::Success(())
    ));
    assert!(format!("{signal:?}").contains("generation: 1"));
  }
}
