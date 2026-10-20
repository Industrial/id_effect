//! Durable timers (release 0.11.0 decision D6): [`WorkflowContext::sleep`] and
//! [`WorkflowContext::sleep_until`] park the instance and fire through the `id_effect_jobs`
//! queue's `run_at`, one job per timer under a unique key, so a timer survives any number of
//! engine restarts.

use std::sync::Arc;
use std::time::Duration;

use id_effect::{Effect, HList, Schema, UtcDateTime, WallClock, fail, suspend};
use id_effect_jobs::{Job, JobFailure, JobOptions, JobRegistry, RunAt, UniqueKey};

use crate::activity::idempotency_key;
use crate::error::WorkflowError;
use crate::history::{Completion, EventKind, WorkflowStore};
use crate::workflow::{Position, WorkflowContext, WorkflowId, ready};

/// The queue timer jobs are enqueued on. A worker must serve it
/// (`Worker::queue(TIMER_QUEUE, n)`) with the timers' handler
/// ([`WorkflowEngine::register_timers`](crate::WorkflowEngine::register_timers)).
pub const TIMER_QUEUE: &str = "workflow.timers";

/// The job name of a timer.
pub(crate) const TIMER_JOB: &str = "workflow.timer";

/// The history and completion name of a timer.
const TIMER: &str = "timer";

/// The job of one durable timer: due at `due` (RFC 3339), it completes position `seq` of
/// `instance`.
#[derive(Clone, Debug, Schema)]
pub(crate) struct TimerTask {
  instance: String,
  seq: i64,
  due: String,
}

impl Job for TimerTask {
  const NAME: &'static str = TIMER_JOB;
  const QUEUE: &'static str = TIMER_QUEUE;
}

impl WorkflowContext {
  /// A durable sleep of `duration` from now (the engine's clock when the timer is first
  /// scheduled).
  ///
  /// The first run enqueues one timer job with `run_at` at the due instant and the unique key
  /// `<instance>#<seq>`, and parks the run ([`WorkflowError::Waiting`]); the engine frees the
  /// instance, so the wait costs nothing and survives restarts. When the job runs (on any
  /// worker serving [`TIMER_QUEUE`]) the instance wakes and the next run journals the timer
  /// (`{"due": …}`); every replay returns at once. A replay before it fired schedules nothing
  /// new: the job of the first run keeps its due instant. Needs an engine with a job queue.
  pub fn sleep(&self, duration: Duration) -> Effect<(), WorkflowError> {
    self.timer(RunAt::After(duration))
  }

  /// A durable sleep until `at` (see [`sleep`](Self::sleep)); an instant in the past fires at
  /// once.
  pub fn sleep_until(&self, at: UtcDateTime) -> Effect<(), WorkflowError> {
    self.timer(RunAt::At(at))
  }

  fn timer(&self, due: RunAt) -> Effect<(), WorkflowError> {
    let ctx = self.clone();
    suspend(move || match ctx.position(EventKind::Timer, TIMER) {
      Err(e) => fail(e),
      Ok(Position::Replayed(payload)) => ready(decode_due(&payload).map(|_| ())),
      Ok(Position::Fresh(seq)) => {
        let schedule = schedule(&ctx, seq, due);
        ctx
          .delivered(seq, EventKind::Timer, TIMER.to_owned(), schedule)
          .flat_map(|payload| ready(decode_due(&payload).map(|_| ())))
      }
    })
  }
}

/// Enqueues the timer job of position `seq`, due at `due` resolved on the engine's clock (a
/// duplicate of its key inserts nothing).
fn schedule(ctx: &WorkflowContext, seq: i64, due: RunAt) -> Effect<(), WorkflowError> {
  let jobs = match ctx.jobs() {
    Ok(jobs) => jobs,
    Err(e) => return fail(e),
  };
  let instance = ctx.instance().clone();
  ctx.inner.clock.now().flat_map(move |now| {
    let due = microseconds(due.resolve(now));
    let task = TimerTask {
      instance: instance.as_str().to_owned(),
      seq,
      due: due.0.to_string(),
    };
    let opts = JobOptions::new()
      .run_at(RunAt::At(due))
      .unique(UniqueKey::until_pruned(idempotency_key(&instance, seq)));
    jobs
      .enqueue_raw(id_effect_jobs::NewJob::of(task, opts))
      .map(|_| ())
      .map_err(WorkflowError::Jobs)
  })
}

/// `at` truncated to whole microseconds (Postgres `timestamptz` precision), so the journaled
/// due instant is exactly the job's `run_at`.
fn microseconds(at: UtcDateTime) -> UtcDateTime {
  UtcDateTime(jiff::Timestamp::from_microsecond(at.0.as_microsecond()).unwrap_or(at.0))
}

/// The due instant of a journaled timer.
pub(crate) fn decode_due(payload: &serde_json::Value) -> Result<UtcDateTime, WorkflowError> {
  payload
    .get("due")
    .and_then(serde_json::Value::as_str)
    .and_then(|s| s.parse::<jiff::Timestamp>().ok())
    .map(UtcDateTime)
    .ok_or_else(|| WorkflowError::Decode {
      what: TIMER.into(),
      message: format!("malformed journal payload {payload}"),
    })
}

/// Adds the timer handler to `registry`: a due timer job delivers its completion to the
/// instance through `store`.
pub(crate) fn register<R>(
  registry: JobRegistry<R>,
  store: Arc<dyn WorkflowStore>,
  clock: WallClock,
) -> Result<JobRegistry<R>, WorkflowError>
where
  R: HList + Send + Sync + 'static,
{
  registry
    .register::<TimerTask, JobFailure>(move |task, _job| {
      fire(Arc::clone(&store), clock.clone(), task).narrow()
    })
    .map_err(WorkflowError::Jobs)
}

/// Records that `task` fired (ignored when it already did) and wakes the instance.
fn fire(
  store: Arc<dyn WorkflowStore>,
  clock: WallClock,
  task: TimerTask,
) -> Effect<(), JobFailure> {
  let instance = WorkflowId::stored(task.instance);
  clock.now().flat_map(move |now| {
    let completion = Completion {
      seq: task.seq,
      kind: EventKind::Timer,
      name: TIMER.to_owned(),
      payload: serde_json::json!({ "due": task.due }),
      completed_at: now,
    };
    store
      .complete(&instance, completion)
      .map(|_| ())
      .map_err(JobFailure::retry)
  })
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn due_instants_are_whole_microseconds_and_round_trip() {
    let at = UtcDateTime("2026-10-18T10:00:00.123456789Z".parse().unwrap());
    let due = microseconds(at);
    assert_eq!(due.0.to_string(), "2026-10-18T10:00:00.123456Z");
    let payload = serde_json::json!({ "due": due.0.to_string() });
    assert_eq!(decode_due(&payload), Ok(due));
    assert!(decode_due(&serde_json::json!({ "due": "soon" })).is_err());
  }

  #[test]
  fn timer_jobs_have_a_name_and_a_queue() {
    assert_eq!(
      (TimerTask::NAME, TimerTask::QUEUE),
      (TIMER_JOB, TIMER_QUEUE)
    );
    let task = TimerTask {
      instance: "i".into(),
      seq: 2,
      due: "x".into(),
    };
    let encoded = TimerTask::schema().encode(task.clone());
    let back = TimerTask::schema().decode_unknown(&encoded).unwrap();
    assert_eq!(
      (back.instance, back.seq, back.due),
      (task.instance, 2, task.due)
    );
  }
}
