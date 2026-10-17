//! [`Job`]: a typed, schema-encoded unit of background work; its identity, options and
//! failures; and the [`JobRegistry`] of handlers a [`Worker`](crate::Worker) runs.

use std::collections::BTreeMap;
use std::fmt;
use std::sync::Arc;
use std::time::{Duration, Instant};

use id_effect::schema::{HasSchema, ParseErrors, Unknown};
use id_effect::{Effect, HNil, Never, Schedule, UtcDateTime, fail, succeed, suspend};
use id_effect_sql::SqlError;

use crate::queue::JobError;

/// A job type: its payload is the type itself, encoded with its [`HasSchema`] schema (derive it
/// with `#[derive(Schema)]`) and stored as JSON.
///
/// [`NAME`](Self::NAME) is the registry key and the `name` column: keep it stable across deploys
/// (renaming it strands the queued jobs of the old name). The per-instance options are read when
/// the job is enqueued ([`priority`](Self::priority), [`max_attempts`](Self::max_attempts),
/// [`unique_key`](Self::unique_key)) or run ([`timeout`](Self::timeout)); when it runs is a
/// per-enqueue choice ([`JobOptions`](crate::JobOptions)).
///
/// Delivery is at least once: a job whose worker died mid-run runs again, so a handler must be
/// idempotent.
pub trait Job: HasSchema<A = Self, I = Unknown, E = ()> + Send + Sync + 'static {
  /// The job's stable name.
  const NAME: &'static str;
  /// The queue it is enqueued on unless [`JobOptions`](crate::JobOptions) says otherwise.
  const QUEUE: &'static str = Queue::DEFAULT;

  /// Higher runs first within a queue (default [`Priority::NORMAL`]).
  fn priority(&self) -> Priority {
    Priority::NORMAL
  }

  /// How many attempts before the job is `dead` (default 20; at least 1).
  fn max_attempts(&self) -> u16 {
    20
  }

  /// Deduplicates the job within its queue (default none; see [`UniqueKey`]).
  fn unique_key(&self) -> Option<UniqueKey> {
    None
  }

  /// The time limit of one attempt; an attempt that exceeds it fails and is retried (default
  /// none: until it finishes).
  fn timeout(&self) -> Option<Duration> {
    None
  }

  /// The backoff between attempts, stepped once per failed attempt with the failure (default
  /// [`default_retry`]). A schedule that is done makes the job `dead` before its attempts are
  /// exhausted.
  fn retry() -> Schedule<(), JobFailure> {
    default_retry()
  }
}

/// The default [`Job::retry`]: exponential from 1 s, capped at 1 h, jittered by ±20 %.
pub fn default_retry() -> Schedule<(), JobFailure> {
  Schedule::exponential(Duration::from_secs(1))
    // `union` takes the shorter delay: at most one hour.
    .union(Schedule::spaced(Duration::from_secs(3600)))
    .jittered()
    .map(|_| ())
}

/// The delay before the attempt after attempt number `attempt` (1-based) failed with `failure`,
/// or `None` when `schedule` is done.
///
/// A [`Schedule`]'s state lives in memory only, so the delay is replayed: the schedule is stepped
/// `attempt` times from [`Schedule::start`] with `failure` as the input, on a clock that advances
/// by each delay, and the last delay is returned. Clock-free schedules replay exactly; a jittered
/// one draws a fresh jitter per replay.
pub fn retry_delay(
  schedule: &Schedule<(), JobFailure>,
  failure: &JobFailure,
  attempt: u16,
) -> Option<Duration> {
  let mut now = Instant::now();
  let mut state = schedule.start();
  let mut delay = None;
  for _ in 0..attempt.max(1) {
    let (next, (), step) = schedule.step(now, failure, state).into_parts();
    let step = step?;
    now = now.checked_add(step).unwrap_or(now);
    state = next;
    delay = Some(step);
  }
  delay
}

/// A job's priority within its queue: higher runs first (the `priority` column).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Priority(pub i16);

impl Priority {
  /// Runs after [`NORMAL`](Self::NORMAL) jobs (`-10`).
  pub const LOW: Priority = Priority(-10);
  /// The default (`0`).
  pub const NORMAL: Priority = Priority(0);
  /// Runs before [`NORMAL`](Self::NORMAL) jobs (`10`).
  pub const HIGH: Priority = Priority(10);
}

/// How long a [`UniqueKey`] blocks another job with the same key in the same queue.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum UniqueScope {
  /// While the job is pending: `scheduled`, `running`, or `failed` and awaiting a retry. Once it
  /// succeeds, dies or is cancelled, the key is free again.
  #[default]
  WhilePending,
  /// Until the job's row is pruned, whatever its state: a cron tick (`name@tick`) runs once even
  /// when a second scheduler enqueues it after the first run finished.
  UntilPruned,
}

impl UniqueScope {
  /// The `unique_scope` column value: `pending` or `pruned`.
  pub const fn as_str(self) -> &'static str {
    match self {
      UniqueScope::WhilePending => "pending",
      UniqueScope::UntilPruned => "pruned",
    }
  }

  /// The scope of a `unique_scope` column value.
  pub fn parse(column: &str) -> Option<Self> {
    match column {
      "pending" => Some(UniqueScope::WhilePending),
      "pruned" => Some(UniqueScope::UntilPruned),
      _ => None,
    }
  }
}

/// Deduplicates jobs within a queue: while a job holds `key` (see [`UniqueScope`]), enqueuing
/// another job with the same key in the same queue inserts nothing and reports
/// [`Enqueued::Duplicate`](crate::Enqueued::Duplicate).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct UniqueKey {
  /// The key (the `unique_key` column).
  pub key: String,
  /// How long it is held.
  pub scope: UniqueScope,
}

impl UniqueKey {
  /// `key`, held while the job is pending.
  pub fn while_pending(key: impl Into<String>) -> Self {
    Self {
      key: key.into(),
      scope: UniqueScope::WhilePending,
    }
  }

  /// `key`, held until the job's row is pruned.
  pub fn until_pruned(key: impl Into<String>) -> Self {
    Self {
      key: key.into(),
      scope: UniqueScope::UntilPruned,
    }
  }
}

/// Why an attempt failed, and whether to try again.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum JobFailure {
  /// Try again after the [`Job::retry`] delay, unless the attempts are exhausted.
  Retry(String),
  /// Retrying cannot help (a payload that no longer decodes, a permanent rejection): the job is
  /// `dead` at once.
  Discard(String),
}

impl JobFailure {
  /// A [`Retry`](Self::Retry) with `error`'s text.
  pub fn retry(error: impl fmt::Display) -> Self {
    JobFailure::Retry(error.to_string())
  }

  /// A [`Discard`](Self::Discard) with `error`'s text.
  pub fn discard(error: impl fmt::Display) -> Self {
    JobFailure::Discard(error.to_string())
  }

  /// The failure's text (stored as the job's `last_error`).
  pub fn message(&self) -> &str {
    match self {
      JobFailure::Retry(m) | JobFailure::Discard(m) => m,
    }
  }

  /// Whether this is a [`Discard`](Self::Discard).
  pub fn is_discard(&self) -> bool {
    matches!(self, JobFailure::Discard(_))
  }
}

impl fmt::Display for JobFailure {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    match self {
      JobFailure::Retry(m) => write!(f, "{m}"),
      JobFailure::Discard(m) => write!(f, "{m} (discarded)"),
    }
  }
}

impl std::error::Error for JobFailure {}

impl From<String> for JobFailure {
  fn from(error: String) -> Self {
    JobFailure::Retry(error)
  }
}

impl From<&str> for JobFailure {
  fn from(error: &str) -> Self {
    JobFailure::Retry(error.to_owned())
  }
}

impl From<SqlError> for JobFailure {
  fn from(error: SqlError) -> Self {
    JobFailure::Retry(error.to_string())
  }
}

impl From<Never> for JobFailure {
  fn from(never: Never) -> Self {
    match never {}
  }
}

/// A job's id (the `id` column).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct JobId(pub i64);

impl fmt::Display for JobId {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    write!(f, "{}", self.0)
  }
}

/// A queue name (the `queue` column). Cloning is cheap.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Queue(Arc<str>);

impl Queue {
  /// The default queue's name, `default`.
  pub const DEFAULT: &'static str = "default";

  /// The queue named `name`.
  pub fn new(name: impl Into<Arc<str>>) -> Self {
    Self(name.into())
  }

  /// The name.
  pub fn as_str(&self) -> &str {
    &self.0
  }
}

impl Default for Queue {
  fn default() -> Self {
    Self::new(Self::DEFAULT)
  }
}

impl fmt::Debug for Queue {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    write!(f, "Queue({:?})", &*self.0)
  }
}

impl fmt::Display for Queue {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.write_str(&self.0)
  }
}

impl From<&str> for Queue {
  fn from(name: &str) -> Self {
    Self::new(name)
  }
}

impl From<String> for Queue {
  fn from(name: String) -> Self {
    Self::new(name)
  }
}

/// What a handler knows about the attempt it runs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JobContext {
  /// The job's id.
  pub id: JobId,
  /// The job's [`Job::NAME`].
  pub name: String,
  /// Its queue.
  pub queue: Queue,
  /// This attempt's number, from 1.
  pub attempt: u16,
  /// The job's maximum number of attempts.
  pub max_attempts: u16,
  /// When it was enqueued.
  pub inserted_at: UtcDateTime,
}

type Handler<R> = Arc<dyn Fn(Unknown, JobContext) -> Effect<(), JobFailure, R> + Send + Sync>;

/// One registered job type, erased: its handler over the decoded payload and its retry schedule.
pub(crate) struct Registered<R> {
  pub(crate) run: Handler<R>,
  pub(crate) retry: Schedule<(), JobFailure>,
}

/// The handlers a [`Worker`](crate::Worker) runs, keyed by [`Job::NAME`]: typed when registered,
/// erased by name, with handlers in the environment `R` (provided the normal way to the worker's
/// effect). Cloning shares the handlers.
pub struct JobRegistry<R = HNil> {
  entries: BTreeMap<&'static str, Arc<Registered<R>>>,
}

impl<R> Clone for JobRegistry<R> {
  fn clone(&self) -> Self {
    Self {
      entries: self.entries.clone(),
    }
  }
}

impl<R> Default for JobRegistry<R> {
  fn default() -> Self {
    Self {
      entries: BTreeMap::new(),
    }
  }
}

impl<R> fmt::Debug for JobRegistry<R> {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_set().entries(self.entries.keys()).finish()
  }
}

impl<R: Send + Sync + 'static> JobRegistry<R> {
  /// An empty registry.
  pub fn new() -> Self {
    Self::default()
  }

  /// Registers `handler` for `J`; fails with [`JobError::DuplicateJob`] when `J::NAME` is taken.
  ///
  /// The worker decodes the payload with `J`'s schema (a payload that no longer decodes is
  /// discarded, naming every field error), bounds the attempt by [`Job::timeout`], and maps the
  /// handler's error with `Into<JobFailure>` (a plain error is a [`JobFailure::Retry`]).
  pub fn register<J, E>(
    mut self,
    handler: impl Fn(J, JobContext) -> Effect<(), E, R> + Send + Sync + 'static,
  ) -> Result<Self, JobError>
  where
    J: Job,
    E: Into<JobFailure> + Send + 'static,
  {
    if self.entries.contains_key(J::NAME) {
      return Err(JobError::DuplicateJob(J::NAME.to_owned()));
    }
    let handler = Arc::new(handler);
    let run: Handler<R> = Arc::new(move |payload: Unknown, ctx: JobContext| {
      let job = match J::schema().decode_unknown_all(&payload) {
        Ok(job) => job,
        Err(errors) => {
          return fail(JobFailure::Discard(format!(
            "payload of `{}` does not decode: {}",
            J::NAME,
            describe(&errors)
          )));
        }
      };
      let limit = job.timeout();
      let handler = Arc::clone(&handler);
      let attempt = suspend(move || handler(job, ctx)).map_err(Into::<JobFailure>::into);
      match limit {
        None => attempt,
        Some(limit) => attempt.timeout(limit).flat_map(move |done| match done {
          Some(()) => succeed(()),
          None => fail(JobFailure::Retry(format!("timed out after {limit:?}"))),
        }),
      }
    });
    let entry = Registered {
      run,
      retry: J::retry(),
    };
    self.entries.insert(J::NAME, Arc::new(entry));
    Ok(self)
  }

  /// The registered names, in order.
  pub fn names(&self) -> impl Iterator<Item = &'static str> + '_ {
    self.entries.keys().copied()
  }

  /// Whether a handler is registered for `name`.
  pub fn contains(&self, name: &str) -> bool {
    self.entries.contains_key(name)
  }

  /// The number of registered job types.
  pub fn len(&self) -> usize {
    self.entries.len()
  }

  /// `true` when nothing is registered.
  pub fn is_empty(&self) -> bool {
    self.entries.is_empty()
  }

  pub(crate) fn get(&self, name: &str) -> Option<Arc<Registered<R>>> {
    self.entries.get(name).cloned()
  }
}

/// Every issue of `errors` on one line: `path: message; …`.
fn describe(errors: &ParseErrors) -> String {
  errors
    .issues
    .iter()
    .map(|e| {
      if e.path.is_empty() {
        e.message.clone()
      } else {
        format!("{}: {}", e.path, e.message)
      }
    })
    .collect::<Vec<_>>()
    .join("; ")
}

#[cfg(test)]
mod tests {
  use std::collections::BTreeMap;

  use id_effect::schema::Schema;
  use id_effect::{Cause, Exit};

  use super::*;

  #[derive(Clone, Debug, PartialEq, id_effect::Schema)]
  struct Mail {
    to: String,
    tries: i64,
  }

  impl Job for Mail {
    const NAME: &'static str = "test.mail";
    const QUEUE: &'static str = "mail";
  }

  fn ctx() -> JobContext {
    JobContext {
      id: JobId(7),
      name: Mail::NAME.into(),
      queue: Queue::new("mail"),
      attempt: 1,
      max_attempts: 20,
      inserted_at: UtcDateTime::unsafe_make(0),
    }
  }

  fn run(eff: Effect<(), JobFailure>) -> Result<(), JobFailure> {
    match eff.block_on() {
      Exit::Success(()) => Ok(()),
      Exit::Failure(Cause::Fail(f)) => Err(f),
      Exit::Failure(c) => panic!("unexpected cause: {c:?}"),
    }
  }

  #[test]
  fn job_defaults_and_constants() {
    let mail = Mail {
      to: "a".into(),
      tries: 0,
    };
    assert_eq!(mail.priority(), Priority::NORMAL);
    assert_eq!(mail.max_attempts(), 20);
    assert_eq!(mail.unique_key(), None);
    assert_eq!(mail.timeout(), None);
    assert_eq!(Mail::QUEUE, "mail");
    assert!(Priority::HIGH > Priority::NORMAL && Priority::NORMAL > Priority::LOW);
    assert_eq!(Queue::default().as_str(), "default");
    assert_eq!(
      format!("{:?} {}", Queue::from("q"), JobId(3)),
      "Queue(\"q\") 3"
    );
    assert_eq!(UniqueKey::until_pruned("k").scope, UniqueScope::UntilPruned);
    for scope in [UniqueScope::WhilePending, UniqueScope::UntilPruned] {
      assert_eq!(UniqueScope::parse(scope.as_str()), Some(scope));
    }
    assert_eq!(UniqueScope::parse("forever"), None);
  }

  #[test]
  fn retry_delay_replays_the_schedule_per_attempt() {
    let exp: Schedule<(), JobFailure> = Schedule::exponential(Duration::from_secs(1)).map(|_| ());
    let f = JobFailure::retry("boom");
    let delays: Vec<_> = (1..=4).map(|n| retry_delay(&exp, &f, n)).collect();
    assert_eq!(
      delays,
      [1, 2, 4, 8].map(|s| Some(Duration::from_secs(s))),
      "attempt n waits base * 2^(n-1)"
    );
    let twice: Schedule<(), JobFailure> = Schedule::recurs(2).map(|_| ());
    assert_eq!(retry_delay(&twice, &f, 2), Some(Duration::ZERO));
    assert_eq!(
      retry_delay(&twice, &f, 3),
      None,
      "a done schedule means dead"
    );
    let default = default_retry();
    let d = retry_delay(&default, &f, 30).unwrap();
    assert!(
      d <= Duration::from_secs(4320),
      "capped at 1 h (+20 % jitter): {d:?}"
    );
    assert!(d >= Duration::from_secs(2880), "{d:?}");
  }

  #[test]
  fn failures_convert_and_display() {
    assert_eq!(JobFailure::from("x"), JobFailure::Retry("x".into()));
    assert_eq!(JobFailure::from(String::from("y")).message(), "y");
    let sql = JobFailure::from(SqlError::NotConnected);
    assert!(!sql.is_discard());
    assert!(sql.message().contains("not connected"));
    let d = JobFailure::discard("gone");
    assert!(d.is_discard());
    assert_eq!(d.to_string(), "gone (discarded)");
    assert_eq!(JobFailure::retry(7).to_string(), "7");
  }

  #[test]
  fn registry_rejects_a_duplicate_name() {
    let reg = JobRegistry::<HNil>::new()
      .register::<Mail, JobFailure>(|_, _| succeed(()))
      .unwrap();
    assert!(reg.contains("test.mail") && reg.len() == 1 && !reg.is_empty());
    assert_eq!(reg.names().collect::<Vec<_>>(), ["test.mail"]);
    let again = reg.register::<Mail, JobFailure>(|_, _| succeed(()));
    assert!(matches!(again, Err(JobError::DuplicateJob(n)) if n == "test.mail"));
  }

  #[test]
  fn registered_handler_decodes_the_payload() {
    let reg = JobRegistry::<HNil>::new()
      .register::<Mail, &str>(|mail, ctx| {
        assert_eq!(ctx.attempt, 1);
        if mail.to == "ok" {
          succeed(())
        } else {
          fail("refused")
        }
      })
      .unwrap();
    let entry = reg.get("test.mail").unwrap();
    let ok = Mail::schema().encode(Mail {
      to: "ok".into(),
      tries: 1,
    });
    assert_eq!(run((entry.run)(ok, ctx())), Ok(()));
    let refused = Mail::schema().encode(Mail {
      to: "no".into(),
      tries: 1,
    });
    assert_eq!(
      run((entry.run)(refused, ctx())),
      Err(JobFailure::Retry("refused".into()))
    );
    assert!(reg.get("other").is_none());
    assert!(format!("{reg:?}").contains("test.mail"));
  }

  #[test]
  fn a_payload_that_no_longer_decodes_is_discarded_with_every_field_error() {
    let reg = JobRegistry::<HNil>::new()
      .register::<Mail, JobFailure>(|_, _| succeed(()))
      .unwrap();
    let entry = reg.get("test.mail").unwrap();
    let bad = Unknown::Object(BTreeMap::from([("tries".to_owned(), Unknown::Bool(true))]));
    let Err(JobFailure::Discard(m)) = run((entry.run)(bad, ctx())) else {
      panic!("expected a discard");
    };
    assert!(m.contains("test.mail"), "{m}");
    assert!(
      m.contains("to: missing field") && m.contains("tries:"),
      "{m}"
    );
  }

  #[derive(Clone, Debug, id_effect::Schema)]
  struct Slow {
    secs: i64,
  }

  impl Job for Slow {
    const NAME: &'static str = "test.slow";

    fn timeout(&self) -> Option<Duration> {
      Some(Duration::from_secs(1))
    }
  }

  #[test]
  fn an_attempt_over_its_timeout_fails_with_retry() {
    use id_effect::{TestExecutor, sleep};
    let reg = JobRegistry::<HNil>::new()
      .register::<Slow, Never>(|job, _| sleep(Duration::from_secs(job.secs as u64)))
      .unwrap();
    let entry = reg.get("test.slow").unwrap();
    let exec = TestExecutor::new();
    let payload = Slow::schema().encode(Slow { secs: 5 });
    let h = (entry.run)(payload, ctx()).fork_root(std::sync::Arc::new(exec.clone()));
    exec.advance(Duration::from_secs(2));
    assert!(h.is_done());
    let fast = Slow::schema().encode(Slow { secs: 0 });
    assert_eq!(run((entry.run)(fast, ctx())), Ok(()));
  }

  #[test]
  fn schema_round_trips_the_payload() {
    let s: Schema<Mail, Unknown, ()> = Mail::schema();
    let m = Mail {
      to: "x".into(),
      tries: 2,
    };
    assert_eq!(s.decode_unknown(&s.encode(m.clone())).unwrap(), m);
  }
}
