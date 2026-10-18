//! The [`Workflow`] trait, the [`WorkflowContext`] its body journals through, and the instance
//! id [`WorkflowId`].

use std::collections::BTreeMap;
use std::fmt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use id_effect::schema::{serde_json_from_unknown, unknown_from_serde_json};
use id_effect::{
  Effect, HasSchema, Random, Unknown, UtcDateTime, WallClock, fail, succeed, suspend,
};

use id_effect_jobs::JobQueue;

use crate::error::WorkflowError;
use crate::history::{EventKind, HistoryEvent, WorkflowStore};
use crate::lease::{Fenced, Lease, later};

/// A value a workflow journals: its input, its output and every step result. Encoded with its
/// [`HasSchema`] schema (`#[derive(Schema)]`) and stored as JSON.
pub trait Journaled: HasSchema<A = Self, I = Unknown, E = ()> + Send + 'static {}

impl<T: HasSchema<A = T, I = Unknown, E = ()> + Send + 'static> Journaled for T {}

/// `value` encoded with its schema, as JSON.
pub(crate) fn encode<A: Journaled>(value: A) -> serde_json::Value {
  serde_json_from_unknown(&A::schema().encode(value))
}

/// `value` decoded with `A`'s schema; `what` names it in the error.
pub(crate) fn decode<A: Journaled>(
  what: &str,
  value: &serde_json::Value,
) -> Result<A, WorkflowError> {
  A::schema()
    .decode_unknown_all(&unknown_from_serde_json(value.clone()))
    .map_err(|errors| WorkflowError::Decode {
      what: what.to_owned(),
      message: errors.to_string(),
    })
}

/// `r` as an effect.
pub(crate) fn ready<A: Send + 'static>(r: Result<A, WorkflowError>) -> Effect<A, WorkflowError> {
  match r {
    Ok(a) => succeed(a),
    Err(e) => fail(e),
  }
}

/// A durable workflow: a body that is replayed from its journal whenever an engine resumes the
/// instance (after a restart, a crash, or a takeover by another engine).
///
/// The body must be deterministic given its input and the values the [`WorkflowContext`]
/// returns: every side effect goes through [`WorkflowContext::step`] or
/// [`WorkflowContext::activity`], time through [`WorkflowContext::now`] and the durable timers
/// [`WorkflowContext::sleep`] / [`sleep_until`](WorkflowContext::sleep_until), randomness through
/// [`WorkflowContext::random`], input from outside through [`WorkflowContext::wait_signal`], and
/// these calls run one after another (a call started while another is in flight gets a position
/// that depends on scheduling). Code outside them runs again on every replay.
///
/// [`NAME`](Self::NAME) is the registry key and the `name` column (a valid
/// [`WorkflowName`](crate::WorkflowName)); keep it stable. [`VERSION`](Self::VERSION) is recorded
/// on every instance when it is started; this release does not route instances by version
/// (versioning across code upgrades is E13.5), so a change that reorders or renames steps makes
/// the replay of an older instance fail with [`WorkflowError::NonDeterministic`].
pub trait Workflow: Send + Sync + 'static {
  /// The input it is started with.
  type Input: Journaled;
  /// What it completes with.
  type Output: Journaled;

  /// The workflow's stable name.
  const NAME: &'static str;
  /// The version recorded on each instance (default `"1"`).
  const VERSION: &'static str = "1";

  /// The body. Fail it with [`WorkflowError::failed`] (or let a [`WorkflowError::Step`] or
  /// [`WorkflowError::Activity`] propagate) to fail the instance; [`WorkflowError::LeaseLost`]
  /// stops the run without touching the instance, and [`WorkflowError::Waiting`] parks it.
  fn run(&self, ctx: WorkflowContext, input: Self::Input) -> Effect<Self::Output, WorkflowError>;
}

/// The id of one workflow instance (the `id` column): 1 to [`WorkflowId::MAX_LEN`] bytes with no
/// control characters. Choose it to make starting idempotent (`close-account:42`), or use
/// [`WorkflowId::random`].
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct WorkflowId(String);

impl WorkflowId {
  /// The longest accepted id, in bytes.
  pub const MAX_LEN: usize = 200;

  /// Validates `id`.
  pub fn new(id: impl Into<String>) -> Result<Self, WorkflowError> {
    let id = id.into();
    if id.is_empty() {
      return Err(WorkflowError::InvalidId("empty".into()));
    }
    if id.len() > Self::MAX_LEN {
      return Err(WorkflowError::InvalidId(format!(
        "{} bytes, longer than {}",
        id.len(),
        Self::MAX_LEN
      )));
    }
    if id.chars().any(char::is_control) {
      return Err(WorkflowError::InvalidId(
        "contains a control character".into(),
      ));
    }
    Ok(Self(id))
  }

  /// `wf-<32 hex digits>` from OS entropy.
  pub fn random() -> Self {
    let r = Random::live();
    Self(format!("wf-{:016x}{:016x}", r.next_u64(), r.next_u64()))
  }

  /// The id as a string slice.
  pub fn as_str(&self) -> &str {
    &self.0
  }

  /// An id read back from the store (validated when it was written).
  pub(crate) fn stored(id: String) -> Self {
    Self(id)
  }
}

impl fmt::Display for WorkflowId {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.pad(&self.0)
  }
}

/// Where the next journal position stands relative to the history.
pub(crate) enum Position {
  /// Replaying: the history holds this payload at the position.
  Replayed(serde_json::Value),
  /// Past the history: run the work and append at `seq`.
  Fresh(i64),
}

/// The replay cursor over the history loaded when the run started.
struct Replay {
  history: Vec<HistoryEvent>,
  next: usize,
  /// Per signal name, the number of the last signal received (replayed or fresh).
  signals: BTreeMap<String, i64>,
}

pub(crate) struct Inner {
  pub(crate) lease: Lease,
  pub(crate) store: Arc<dyn WorkflowStore>,
  pub(crate) jobs: Option<Arc<dyn JobQueue>>,
  pub(crate) clock: WallClock,
  random: Random,
  lease_for: Duration,
  replay: Mutex<Replay>,
  /// Set once a call parked the run ([`WorkflowError::Waiting`]).
  parked: AtomicBool,
  /// The first [`WorkflowError::NonDeterministic`] of the run: it fails the instance even when
  /// the body catches it.
  diverged: Mutex<Option<WorkflowError>>,
}

/// What a workflow body journals through: [`step`](Self::step), [`now`](Self::now),
/// [`random`](Self::random), [`activity`](Self::activity), [`sleep`](Self::sleep) /
/// [`sleep_until`](Self::sleep_until) and [`wait_signal`](Self::wait_signal). Cloning shares the
/// run's replay cursor.
///
/// Each call takes the next history position (`seq`). While the position is in the history the
/// journaled value is returned and the work is not run again; the call must match the journaled
/// one (same kind, same name) or it fails with [`WorkflowError::NonDeterministic`]. Past the
/// history the work runs and its result is appended under the run's lease, fenced: when the
/// lease is gone the append is rejected and the call fails with [`WorkflowError::LeaseLost`].
///
/// An activity, a timer or a signal that has not arrived yet parks the run: the call fails with
/// [`WorkflowError::Waiting`], so does every later call of the run, and the engine frees the
/// instance until the result, the timer or the signal arrives.
#[derive(Clone)]
pub struct WorkflowContext {
  pub(crate) inner: Arc<Inner>,
}

impl fmt::Debug for WorkflowContext {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    let replay = self.replay();
    f.debug_struct("WorkflowContext")
      .field("instance", &self.inner.lease.instance)
      .field("token", &self.inner.lease.token)
      .field("history", &replay.history.len())
      .field("next_seq", &(replay.next + 1))
      .finish_non_exhaustive()
  }
}

impl WorkflowContext {
  pub(crate) fn new(
    lease: Lease,
    history: Vec<HistoryEvent>,
    store: Arc<dyn WorkflowStore>,
    jobs: Option<Arc<dyn JobQueue>>,
    clock: WallClock,
    random: Random,
    lease_for: Duration,
  ) -> Self {
    Self {
      inner: Arc::new(Inner {
        lease,
        store,
        jobs,
        clock,
        random,
        lease_for,
        replay: Mutex::new(Replay {
          history,
          next: 0,
          signals: BTreeMap::new(),
        }),
        parked: AtomicBool::new(false),
        diverged: Mutex::new(None),
      }),
    }
  }

  fn replay(&self) -> MutexGuard<'_, Replay> {
    self
      .inner
      .replay
      .lock()
      .unwrap_or_else(PoisonError::into_inner)
  }

  /// The instance this run advances.
  pub fn instance(&self) -> &crate::WorkflowId {
    &self.inner.lease.instance
  }

  /// Whether the next call replays a journaled value (`false` once past the history).
  pub fn is_replaying(&self) -> bool {
    let replay = self.replay();
    replay.next < replay.history.len()
  }

  /// Whether a call parked the run.
  pub(crate) fn is_parked(&self) -> bool {
    self.inner.parked.load(Ordering::SeqCst)
  }

  /// Parks the run: this call and every later one fail with [`WorkflowError::Waiting`].
  pub(crate) fn park<A: Send + 'static>(&self) -> Effect<A, WorkflowError> {
    self.inner.parked.store(true, Ordering::SeqCst);
    fail(WorkflowError::Waiting)
  }

  /// The number of the last signal `name` this run received (0: none).
  pub(crate) fn last_signal(&self, name: &str) -> i64 {
    self.replay().signals.get(name).copied().unwrap_or(0)
  }

  /// Notes that this run received signal `name` numbered `number`.
  pub(crate) fn received_signal(&self, name: &str, number: i64) {
    self.replay().signals.insert(name.to_owned(), number);
  }

  /// Records `error` (a [`WorkflowError::NonDeterministic`]) as the run's divergence, unless
  /// one is recorded already, and returns it.
  pub(crate) fn diverge(&self, error: WorkflowError) -> WorkflowError {
    let mut diverged = self
      .inner
      .diverged
      .lock()
      .unwrap_or_else(PoisonError::into_inner);
    diverged.get_or_insert(error).clone()
  }

  /// The run's first divergence from its history, if any.
  pub(crate) fn diverged(&self) -> Option<WorkflowError> {
    self
      .inner
      .diverged
      .lock()
      .unwrap_or_else(PoisonError::into_inner)
      .clone()
  }

  /// Takes the next position for a `kind` call named `name`. After a divergence every call
  /// fails with it, and after a park with [`WorkflowError::Waiting`], so a body that catches
  /// either can never journal past the position it stopped at.
  pub(crate) fn position(&self, kind: EventKind, name: &str) -> Result<Position, WorkflowError> {
    if let Some(e) = self.diverged() {
      return Err(e);
    }
    if self.is_parked() {
      return Err(WorkflowError::Waiting);
    }
    let mut replay = self.replay();
    let index = replay.next;
    replay.next += 1;
    let seq = index as i64 + 1;
    match replay.history.get(index) {
      None => Ok(Position::Fresh(seq)),
      Some(event) if event.kind == kind && event.name == name => {
        Ok(Position::Replayed(event.payload.clone()))
      }
      Some(event) => {
        let error = WorkflowError::NonDeterministic {
          seq,
          history: HistoryEvent::describe(event.kind, &event.name),
          code: HistoryEvent::describe(kind, name),
        };
        drop(replay);
        Err(self.diverge(error))
      }
    }
  }

  /// The error for history the body did not replay to its end, if any.
  pub(crate) fn leftover(&self) -> Option<WorkflowError> {
    let replay = self.replay();
    replay
      .history
      .get(replay.next)
      .map(|event| WorkflowError::NonDeterministic {
        seq: event.seq,
        history: HistoryEvent::describe(event.kind, &event.name),
        code: "the end of the workflow".into(),
      })
  }

  /// Appends `payload` at `seq` under the lease, extending it; `LeaseLost` when rejected.
  pub(crate) fn record(
    &self,
    seq: i64,
    kind: EventKind,
    name: String,
    payload: serde_json::Value,
  ) -> Effect<(), WorkflowError> {
    let inner = Arc::clone(&self.inner);
    self.inner.clock.now().flat_map(move |now| {
      let event = HistoryEvent {
        seq,
        kind,
        name,
        payload,
        recorded_at: now,
      };
      inner
        .store
        .append(&inner.lease, event, later(now, inner.lease_for))
        .flat_map(|fenced| {
          if fenced.is_applied() {
            succeed(())
          } else {
            fail(WorkflowError::LeaseLost)
          }
        })
    })
  }

  /// At the fresh position `seq` of a `kind` call named `name` (an activity or a timer): the
  /// payload of the delivered [`Completion`](crate::Completion), journaled at `seq`; or, when
  /// nothing was delivered yet, `schedule` (enqueue the job; idempotent per `(instance, seq)`)
  /// after a fenced check that the lease is still held, and the run parks.
  pub(crate) fn delivered(
    &self,
    seq: i64,
    kind: EventKind,
    name: String,
    schedule: Effect<(), WorkflowError>,
  ) -> Effect<serde_json::Value, WorkflowError> {
    let ctx = self.clone();
    let store = Arc::clone(&self.inner.store);
    store
      .completion(&self.inner.lease.instance, seq)
      .flat_map(move |found| match found {
        Some(done) if done.kind == kind && done.name == name => {
          let payload = done.payload;
          ctx
            .record(seq, kind, name, payload.clone())
            .map(move |()| payload)
        }
        Some(done) => fail(ctx.diverge(WorkflowError::NonDeterministic {
          seq,
          history: format!(
            "a delivered {}",
            HistoryEvent::describe(done.kind, &done.name)
          ),
          code: HistoryEvent::describe(kind, &name),
        })),
        None => ctx.renewed().then(schedule).flat_map(move |()| ctx.park()),
      })
  }

  /// Extends the lease (fenced); `LeaseLost` when it is gone, so a stale run schedules nothing.
  fn renewed(&self) -> Effect<(), WorkflowError> {
    let inner = Arc::clone(&self.inner);
    self.inner.clock.now().flat_map(move |now| {
      inner
        .store
        .renew(&inner.lease, now, later(now, inner.lease_for))
        .flat_map(|fenced| match fenced {
          Fenced::Applied => succeed(()),
          Fenced::Lost => fail(WorkflowError::LeaseLost),
        })
    })
  }

  /// The engine's job queue, for scheduling activities and timers.
  pub(crate) fn jobs(&self) -> Result<Arc<dyn JobQueue>, WorkflowError> {
    self.inner.jobs.clone().ok_or(WorkflowError::NoJobQueue)
  }

  /// Runs `effect` once and journals its result as the step `name`.
  ///
  /// On replay the journaled result is returned and `effect` does not run. A typed failure of
  /// `effect` is journaled too (its `Display` text) and returned as [`WorkflowError::Step`], on
  /// this run and every replay; retry inside `effect` (`retry(schedule)`) for transient errors.
  /// A defect or interruption journals nothing, so the step runs again when the instance
  /// resumes: a step is at least once, and its effect must be idempotent.
  pub fn step<A, E>(
    &self,
    name: impl Into<String>,
    effect: Effect<A, E>,
  ) -> Effect<A, WorkflowError>
  where
    A: Journaled,
    E: fmt::Display + Send + 'static,
  {
    let ctx = self.clone();
    let name = name.into();
    suspend(move || match ctx.position(EventKind::Step, &name) {
      Err(e) => fail(e),
      Ok(Position::Replayed(payload)) => match step_result::<A>(&name, &payload) {
        Ok(a) => succeed(a),
        Err(e) => fail(e),
      },
      Ok(Position::Fresh(seq)) => effect
        .map(|a| Ok(encode(a)))
        .catch(|e| succeed(Err(e.to_string())))
        .flat_map(move |result| {
          let payload = match &result {
            Ok(value) => serde_json::json!({ "ok": value }),
            Err(message) => serde_json::json!({ "err": message }),
          };
          ctx
            .record(seq, EventKind::Step, name.clone(), payload.clone())
            .flat_map(move |()| match step_result::<A>(&name, &payload) {
              Ok(a) => succeed(a),
              Err(e) => fail(e),
            })
        }),
    })
  }

  /// The current time, journaled: the engine's clock on the first run, the same instant on every
  /// replay.
  pub fn now(&self) -> Effect<UtcDateTime, WorkflowError> {
    let ctx = self.clone();
    suspend(
      move || match ctx.position(EventKind::Now, EventKind::Now.as_str()) {
        Err(e) => fail(e),
        Ok(Position::Replayed(payload)) => match decode_now(&payload) {
          Ok(at) => succeed(at),
          Err(e) => fail(e),
        },
        Ok(Position::Fresh(seq)) => {
          let clock = ctx.inner.clock.clone();
          clock.now().flat_map(move |at: UtcDateTime| {
            let payload = serde_json::json!({ "at": at.0.to_string() });
            ctx
              .record(seq, EventKind::Now, EventKind::Now.as_str().into(), payload)
              .map(move |()| at)
          })
        }
      },
    )
  }

  /// A uniformly distributed `u64`, journaled: drawn from the engine's [`Random`] on the first
  /// run, the same value on every replay.
  pub fn random(&self) -> Effect<u64, WorkflowError> {
    let ctx = self.clone();
    suspend(
      move || match ctx.position(EventKind::Random, EventKind::Random.as_str()) {
        Err(e) => fail(e),
        Ok(Position::Replayed(payload)) => match decode_random(&payload) {
          Ok(n) => succeed(n),
          Err(e) => fail(e),
        },
        Ok(Position::Fresh(seq)) => {
          let n = ctx.inner.random.next_u64();
          // A decimal string: JSON readers that hold numbers as f64 would round a u64.
          let payload = serde_json::json!({ "u64": n.to_string() });
          ctx
            .record(
              seq,
              EventKind::Random,
              EventKind::Random.as_str().into(),
              payload,
            )
            .map(move |()| n)
        }
      },
    )
  }
}

/// A journaled step payload as the step's result.
fn step_result<A: Journaled>(name: &str, payload: &serde_json::Value) -> Result<A, WorkflowError> {
  if let Some(value) = payload.get("ok") {
    return decode(&format!("step {name}"), value);
  }
  match payload.get("err").and_then(serde_json::Value::as_str) {
    Some(message) => Err(WorkflowError::Step {
      step: name.to_owned(),
      message: message.to_owned(),
    }),
    None => Err(malformed(&format!("step {name}"), payload)),
  }
}

fn malformed(what: &str, payload: &serde_json::Value) -> WorkflowError {
  WorkflowError::Decode {
    what: what.to_owned(),
    message: format!("malformed journal payload {payload}"),
  }
}

fn decode_now(payload: &serde_json::Value) -> Result<UtcDateTime, WorkflowError> {
  payload
    .get("at")
    .and_then(serde_json::Value::as_str)
    .and_then(|s| s.parse::<jiff::Timestamp>().ok())
    .map(UtcDateTime)
    .ok_or_else(|| malformed("now", payload))
}

fn decode_random(payload: &serde_json::Value) -> Result<u64, WorkflowError> {
  payload
    .get("u64")
    .and_then(serde_json::Value::as_str)
    .and_then(|s| s.parse::<u64>().ok())
    .ok_or_else(|| malformed("random", payload))
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn ids_are_validated() {
    assert_eq!(
      WorkflowId::new("close-account:42").unwrap().as_str(),
      "close-account:42"
    );
    assert!(matches!(
      WorkflowId::new(""),
      Err(WorkflowError::InvalidId(_))
    ));
    assert!(WorkflowId::new("a\nb").is_err());
    assert!(WorkflowId::new("a".repeat(WorkflowId::MAX_LEN)).is_ok());
    assert!(WorkflowId::new("a".repeat(WorkflowId::MAX_LEN + 1)).is_err());
    let (a, b) = (WorkflowId::random(), WorkflowId::random());
    assert!(a != b && a.as_str().len() == 35 && a.to_string().starts_with("wf-"));
  }

  #[test]
  fn journal_payloads_decode_or_report_malformed() {
    let at = UtcDateTime::unsafe_make(1_700_000_000_123);
    assert_eq!(
      decode_now(&serde_json::json!({ "at": at.0.to_string() })),
      Ok(at)
    );
    assert_eq!(
      decode_random(&serde_json::json!({ "u64": u64::MAX.to_string() })),
      Ok(u64::MAX)
    );
    assert!(matches!(
      decode_now(&serde_json::json!({ "at": 5 })),
      Err(WorkflowError::Decode { .. })
    ));
    assert!(decode_random(&serde_json::json!({})).is_err());
    assert_eq!(
      step_result::<Unit>("s", &serde_json::json!({ "err": "boom" })),
      Err(WorkflowError::Step {
        step: "s".into(),
        message: "boom".into()
      })
    );
    assert!(matches!(
      step_result::<Unit>("s", &serde_json::json!(3)),
      Err(WorkflowError::Decode { .. })
    ));
  }

  #[derive(Debug, PartialEq, id_effect::Schema)]
  struct Unit {
    n: i64,
  }
}
