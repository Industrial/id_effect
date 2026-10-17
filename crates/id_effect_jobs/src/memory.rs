//! [`MemoryJobQueue`]: the queue in memory, with the same ordering, uniqueness, leases and
//! fencing as [`PgJobQueue`](crate::PgJobQueue), for tests and single-process development.

use std::collections::BTreeMap;
use std::fmt;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use id_effect::{Effect, Layer, UtcDateTime, WallClock, fail, hlist, succeed, suspend};
use id_effect_sql::{SQLSTATE_UNIQUE_VIOLATION, SqlError};

use crate::JobState;
use crate::job::{JobFailure, JobId, Queue, UniqueScope};
use crate::queue::{
  Claimed, Enqueued, Fenced, JobError, JobFilter, JobQueue, JobQueueEnv, JobQueueService,
  JobRecord, JobStore, NewJob, QueueStats, WakeSignal, fold_stats, later, truncate_error,
};

/// The `last_error` of a job rescued from an expired lease.
pub(crate) const LEASE_EXPIRED: &str = "lease expired (worker lost)";

#[derive(Default)]
struct State {
  next_id: i64,
  jobs: BTreeMap<JobId, JobRecord>,
  outage: Option<String>,
}

/// Whether `r` holds its unique key: the predicate of the table's unique index.
fn holds_key(r: &JobRecord) -> bool {
  r.unique
    .as_ref()
    .is_some_and(|u| r.state.is_pending() || u.scope == UniqueScope::UntilPruned)
}

impl State {
  /// The job other than `except` that holds `key` in `queue`.
  fn holder(&self, queue: &Queue, key: &str, except: Option<JobId>) -> Option<JobId> {
    self
      .jobs
      .values()
      .find(|r| {
        Some(r.id) != except
          && r.queue == *queue
          && holds_key(r)
          && r.unique.as_ref().is_some_and(|u| u.key == key)
      })
      .map(|r| r.id)
  }

  /// Job `id` if it is `running` under `worker` (the fence of every outcome).
  fn leased(&mut self, id: JobId, worker: &str) -> Option<&mut JobRecord> {
    self
      .jobs
      .get_mut(&id)
      .filter(|r| r.state == JobState::Running && r.locked_by.as_deref() == Some(worker))
  }
}

/// [`JobStore`] in memory. Clones share the jobs. Every time is read from its
/// [`WallClock`] (anchor it to a `TestExecutor` for virtual time); jobs do not survive a
/// restart. [`enqueue_in`](crate::enqueue_in) needs [`PgJobQueue`](crate::PgJobQueue).
#[derive(Clone)]
pub struct MemoryJobQueue {
  state: Arc<Mutex<State>>,
  clock: WallClock,
  wake: WakeSignal,
}

impl fmt::Debug for MemoryJobQueue {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_struct("MemoryJobQueue")
      .field("len", &self.len())
      .field("clock", &self.clock)
      .finish()
  }
}

impl MemoryJobQueue {
  /// An empty queue reading the time from `clock`.
  pub fn new(clock: WallClock) -> Self {
    Self {
      state: Arc::default(),
      clock,
      wake: WakeSignal::new(),
    }
  }

  /// The clock it reads.
  pub fn clock(&self) -> &WallClock {
    &self.clock
  }

  fn lock(&self) -> MutexGuard<'_, State> {
    self.state.lock().unwrap_or_else(PoisonError::into_inner)
  }

  /// The number of jobs, in any state.
  pub fn len(&self) -> usize {
    self.lock().jobs.len()
  }

  /// `true` when it holds no job.
  pub fn is_empty(&self) -> bool {
    self.len() == 0
  }

  /// Make every later operation fail with [`SqlError::ConnectionFailed`]`(message)` (a store
  /// outage), or recover with `None`.
  pub fn fail_with(&self, message: Option<&str>) {
    self.lock().outage = message.map(str::to_owned);
  }

  /// A lazy step over the state at the clock's now (or the scripted outage).
  fn with<A: Send + 'static>(
    &self,
    f: impl FnOnce(&mut State, UtcDateTime) -> Result<A, JobError> + Send + 'static,
  ) -> Effect<A, JobError> {
    let state = Arc::clone(&self.state);
    self.clock.now().flat_map(move |now| {
      suspend(move || {
        let mut st = state.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(m) = st.outage.clone() {
          return fail(JobError::Sql(SqlError::connection_failed(m)));
        }
        match f(&mut st, now) {
          Ok(a) => succeed(a),
          Err(e) => fail(e),
        }
      })
    })
  }
}

fn count(n: usize) -> u64 {
  u64::try_from(n).unwrap_or(u64::MAX)
}

impl JobQueue for MemoryJobQueue {
  fn enqueue_raw(&self, job: NewJob) -> Effect<Enqueued, JobError> {
    let wake = self.wake.clone();
    self
      .with(move |st, now| {
        job.validate()?;
        if let Some(u) = &job.unique
          && let Some(existing) = st.holder(&job.queue, &u.key, None)
        {
          return Ok(Enqueued::Duplicate {
            existing: Some(existing),
          });
        }
        st.next_id += 1;
        let id = JobId(st.next_id);
        let record = JobRecord {
          id,
          name: job.name,
          queue: job.queue,
          payload: job.payload,
          state: JobState::Scheduled,
          priority: job.priority,
          attempts: 0,
          max_attempts: job.max_attempts,
          unique: job.unique,
          run_at: job.run_at.resolve(now),
          inserted_at: now,
          attempted_at: None,
          finished_at: None,
          locked_by: None,
          locked_until: None,
          last_error: None,
        };
        st.jobs.insert(id, record);
        Ok(Enqueued::New(id))
      })
      .map(move |done| {
        if done.is_new() {
          wake.notify();
        }
        done
      })
  }

  fn cancel(&self, id: JobId) -> Effect<bool, JobError> {
    self.with(move |st, now| {
      Ok(match st.jobs.get_mut(&id) {
        Some(r) if r.state.is_runnable() => {
          r.state = JobState::Cancelled;
          r.finished_at = Some(now);
          true
        }
        _ => false,
      })
    })
  }

  fn retry_now(&self, id: JobId) -> Effect<bool, JobError> {
    self.with(move |st, now| {
      let Some(r) = st.jobs.get(&id) else {
        return Ok(false);
      };
      if !matches!(r.state, JobState::Dead | JobState::Cancelled) {
        return Ok(false);
      }
      if let Some(u) = &r.unique
        && st.holder(&r.queue, &u.key, Some(id)).is_some()
      {
        return Err(JobError::Sql(SqlError::QueryFailed {
          sql: "retry_now".into(),
          message: format!(
            "duplicate key value violates unique constraint \"id_effect_jobs_unique_idx\" \
             (SQLSTATE {SQLSTATE_UNIQUE_VIOLATION})"
          ),
        }));
      }
      let Some(r) = st.jobs.get_mut(&id) else {
        return Ok(false);
      };
      r.state = JobState::Scheduled;
      r.attempts = 0;
      r.run_at = now;
      r.finished_at = None;
      Ok(true)
    })
  }

  fn stats(&self) -> Effect<Vec<QueueStats>, JobError> {
    self.with(|st, _| {
      let mut groups: BTreeMap<(Queue, JobState), (u64, Option<UtcDateTime>)> = BTreeMap::new();
      for r in st.jobs.values() {
        let g = groups
          .entry((r.queue.clone(), r.state))
          .or_insert((0, None));
        g.0 += 1;
        g.1 = Some(g.1.map_or(r.run_at, |t| t.min(r.run_at)));
      }
      Ok(fold_stats(
        groups
          .into_iter()
          .map(|((q, s), (n, oldest))| (q, s, n, oldest)),
      ))
    })
  }

  fn list(&self, filter: JobFilter) -> Effect<Vec<JobRecord>, JobError> {
    self.with(move |st, _| {
      Ok(
        st.jobs
          .values()
          .rev()
          .filter(|r| filter.matches(r))
          .take(filter.limit as usize)
          .cloned()
          .collect(),
      )
    })
  }
}

impl JobStore for MemoryJobQueue {
  fn claim(
    &self,
    queue: &Queue,
    worker: &str,
    limit: u16,
    lease: Duration,
  ) -> Effect<Vec<Claimed>, JobError> {
    let (queue, worker) = (queue.clone(), worker.to_owned());
    self.with(move |st, now| {
      let mut ready: Vec<&JobRecord> = st
        .jobs
        .values()
        .filter(|r| r.queue == queue && r.state.is_runnable() && r.run_at <= now)
        .collect();
      ready.sort_by(|a, b| {
        b.priority
          .cmp(&a.priority)
          .then(a.run_at.cmp(&b.run_at))
          .then(a.id.cmp(&b.id))
      });
      let ids: Vec<JobId> = ready
        .iter()
        .take(usize::from(limit))
        .map(|r| r.id)
        .collect();
      let until = later(now, lease);
      let mut claimed = Vec::with_capacity(ids.len());
      for id in ids {
        let Some(r) = st.jobs.get_mut(&id) else {
          continue;
        };
        r.state = JobState::Running;
        r.attempts = r.attempts.saturating_add(1);
        r.attempted_at = Some(now);
        r.locked_by = Some(worker.clone());
        r.locked_until = Some(until);
        claimed.push(Claimed {
          id,
          name: r.name.clone(),
          queue: r.queue.clone(),
          payload: r.payload.clone(),
          attempt: r.attempts,
          max_attempts: r.max_attempts,
          priority: r.priority,
          run_at: r.run_at,
          inserted_at: r.inserted_at,
        });
      }
      Ok(claimed)
    })
  }

  fn complete(&self, id: JobId, worker: &str) -> Effect<Fenced, JobError> {
    let worker = worker.to_owned();
    self.with(move |st, now| {
      let Some(r) = st.leased(id, &worker) else {
        return Ok(Fenced::Lost);
      };
      r.state = JobState::Succeeded;
      r.finished_at = Some(now);
      r.locked_by = None;
      r.locked_until = None;
      Ok(Fenced::Applied)
    })
  }

  fn fail(
    &self,
    id: JobId,
    worker: &str,
    failure: &JobFailure,
    retry_in: Option<Duration>,
  ) -> Effect<Fenced, JobError> {
    let (worker, failure) = (worker.to_owned(), failure.clone());
    self.with(move |st, now| {
      let Some(r) = st.leased(id, &worker) else {
        return Ok(Fenced::Lost);
      };
      let dead = failure.is_discard() || retry_in.is_none() || r.attempts >= r.max_attempts;
      if dead {
        r.state = JobState::Dead;
        r.finished_at = Some(now);
      } else {
        r.state = JobState::Failed;
        r.run_at = later(now, retry_in.unwrap_or_default());
      }
      r.last_error = Some(truncate_error(failure.message()));
      r.locked_by = None;
      r.locked_until = None;
      Ok(Fenced::Applied)
    })
  }

  fn heartbeat(&self, worker: &str, lease: Duration) -> Effect<u64, JobError> {
    let worker = worker.to_owned();
    self.with(move |st, now| {
      let until = later(now, lease);
      let mut n = 0;
      for r in st.jobs.values_mut() {
        if r.state == JobState::Running && r.locked_by.as_deref() == Some(worker.as_str()) {
          r.locked_until = Some(until);
          n += 1;
        }
      }
      Ok(count(n))
    })
  }

  fn release(&self, worker: &str) -> Effect<u64, JobError> {
    let worker = worker.to_owned();
    self.with(move |st, _| {
      let mut n = 0;
      for r in st.jobs.values_mut() {
        if r.state == JobState::Running && r.locked_by.as_deref() == Some(worker.as_str()) {
          r.state = JobState::Scheduled;
          r.attempts = r.attempts.saturating_sub(1);
          r.locked_by = None;
          r.locked_until = None;
          n += 1;
        }
      }
      Ok(count(n))
    })
  }

  fn rescue_expired(&self) -> Effect<u64, JobError> {
    self.with(|st, now| {
      let mut n = 0;
      for r in st.jobs.values_mut() {
        if r.state == JobState::Running && r.locked_until.is_some_and(|t| t < now) {
          let dead = r.attempts >= r.max_attempts;
          r.state = if dead {
            JobState::Dead
          } else {
            JobState::Failed
          };
          r.finished_at = dead.then_some(now);
          r.run_at = now;
          r.locked_by = None;
          r.locked_until = None;
          r.last_error = Some(LEASE_EXPIRED.to_owned());
          n += 1;
        }
      }
      Ok(count(n))
    })
  }

  fn prune(&self, finished_before: UtcDateTime) -> Effect<u64, JobError> {
    self.with(move |st, _| {
      let before = st.jobs.len();
      st.jobs.retain(|_, r| {
        !(r.state.is_terminal() && r.finished_at.is_some_and(|t| t < finished_before))
      });
      Ok(count(before - st.jobs.len()))
    })
  }

  fn wake_signal(&self) -> Option<WakeSignal> {
    Some(self.wake.clone())
  }
}

/// A [`Layer`] providing `store` as the [`JobQueueService`] (keep a clone to inspect it).
pub fn memory_job_queue_layer<E: Send + 'static>(store: MemoryJobQueue) -> Layer<JobQueueEnv, E> {
  Layer::succeed(hlist![JobQueueService::new(store)])
}

#[cfg(test)]
mod tests {
  use id_effect::{Cause, Exit, need};

  use super::*;
  use crate::job::{Priority, UniqueKey};
  use crate::queue::RunAt;

  fn run<A: Send + 'static>(eff: Effect<A, JobError>) -> Result<A, JobError> {
    match eff.block_on() {
      Exit::Success(a) => Ok(a),
      Exit::Failure(Cause::Fail(e)) => Err(e),
      Exit::Failure(c) => panic!("unexpected cause: {c:?}"),
    }
  }

  /// A store on a fixed clock: anchored at an instant far in the past, so `block_on`'s real
  /// executor clock only moves it forward by the test's runtime.
  fn store() -> MemoryJobQueue {
    MemoryJobQueue::new(WallClock::anchored(
      std::time::Instant::now(),
      UtcDateTime::unsafe_make(1_800_000_000_000),
    ))
  }

  fn job(queue: &str, unique: Option<UniqueKey>) -> NewJob {
    NewJob {
      name: "test.job".into(),
      queue: Queue::new(queue),
      payload: serde_json::json!({}),
      priority: Priority::NORMAL,
      max_attempts: 2,
      unique,
      run_at: RunAt::Now,
    }
  }

  fn record(q: &MemoryJobQueue, id: JobId) -> JobRecord {
    run(q.list(JobFilter::default()))
      .unwrap()
      .into_iter()
      .find(|r| r.id == id)
      .expect("record")
  }

  #[test]
  fn an_outage_fails_every_operation_until_it_recovers() {
    let q = store();
    q.fail_with(Some("down"));
    let down = |r: Result<_, JobError>| matches!(r, Err(JobError::Sql(SqlError::ConnectionFailed { message, .. })) if message == "down");
    assert!(down(run(q.enqueue_raw(job("a", None))).map(|_| ())));
    assert!(down(
      run(q.claim(&Queue::new("a"), "w", 1, Duration::from_secs(1))).map(|_| ())
    ));
    assert!(down(run(q.stats()).map(|_| ())));
    q.fail_with(None);
    assert!(run(q.enqueue_raw(job("a", None))).unwrap().is_new());
    assert_eq!((q.len(), q.is_empty()), (1, false));
    assert!(format!("{q:?}").contains("len: 1"));
  }

  #[test]
  fn cancel_and_retry_now_follow_the_state_machine() {
    let q = store();
    let id = run(q.enqueue_raw(job("a", None))).unwrap().id().unwrap();
    assert_eq!(run(q.retry_now(id)), Ok(false), "only dead or cancelled");
    assert_eq!(run(q.cancel(id)), Ok(true));
    assert_eq!(record(&q, id).state, JobState::Cancelled);
    assert!(record(&q, id).finished_at.is_some());
    assert_eq!(run(q.cancel(id)), Ok(false));
    assert_eq!(run(q.retry_now(id)), Ok(true));
    let r = record(&q, id);
    assert_eq!(
      (r.state, r.attempts, r.finished_at),
      (JobState::Scheduled, 0, None)
    );
    assert_eq!(run(q.cancel(JobId(99))), Ok(false));
    assert_eq!(run(q.retry_now(JobId(99))), Ok(false));
    // A running job is not cancelled.
    run(q.claim(&Queue::new("a"), "w", 1, Duration::from_secs(60))).unwrap();
    assert_eq!(run(q.cancel(id)), Ok(false));
  }

  #[test]
  fn retry_now_reports_a_unique_violation_when_the_key_is_taken() {
    let q = store();
    let key = UniqueKey::while_pending("k");
    let first = run(q.enqueue_raw(job("a", Some(key.clone()))))
      .unwrap()
      .id()
      .unwrap();
    run(q.cancel(first)).unwrap();
    assert!(run(q.enqueue_raw(job("a", Some(key)))).unwrap().is_new());
    let Err(JobError::Sql(e)) = run(q.retry_now(first)) else {
      panic!("expected a unique violation");
    };
    assert!(e.is_unique_violation(), "{e}");
  }

  #[test]
  fn leases_heartbeat_release_and_fencing() {
    let q = store();
    let a = Queue::new("a");
    let id = run(q.enqueue_raw(job("a", None))).unwrap().id().unwrap();
    let claimed = run(q.claim(&a, "w1", 5, Duration::from_secs(10))).unwrap();
    assert_eq!(claimed.len(), 1);
    assert_eq!(claimed[0].attempt, 1);
    let first_lease = record(&q, id).locked_until.unwrap();
    assert_eq!(run(q.heartbeat("w1", Duration::from_secs(100))), Ok(1));
    assert!(record(&q, id).locked_until.unwrap() > first_lease);
    assert_eq!(run(q.heartbeat("w2", Duration::from_secs(100))), Ok(0));
    assert_eq!(
      run(q.complete(id, "w2")),
      Ok(Fenced::Lost),
      "fenced on the worker"
    );
    assert_eq!(run(q.release("w1")), Ok(1));
    let r = record(&q, id);
    assert_eq!(
      (r.state, r.attempts, r.locked_by),
      (JobState::Scheduled, 0, None)
    );
    assert_eq!(run(q.complete(id, "w1")), Ok(Fenced::Lost), "released");
  }

  #[test]
  fn a_discard_or_the_last_attempt_is_dead_and_prune_deletes_finished_rows() {
    let q = store();
    let a = Queue::new("a");
    let id = run(q.enqueue_raw(job("a", None))).unwrap().id().unwrap();
    run(q.claim(&a, "w", 1, Duration::from_secs(10))).unwrap();
    let f = JobFailure::discard("bad payload");
    assert_eq!(
      run(q.fail(id, "w", &f, Some(Duration::from_secs(1)))),
      Ok(Fenced::Applied)
    );
    let r = record(&q, id);
    assert_eq!(r.state, JobState::Dead);
    assert_eq!(r.last_error.as_deref(), Some("bad payload"));
    let later_id = run(q.enqueue_raw(job("a", None))).unwrap().id().unwrap();
    let stats = run(q.stats()).unwrap();
    assert_eq!(stats[0].count(JobState::Dead), 1);
    assert_eq!(stats[0].count(JobState::Scheduled), 1);
    let end = UtcDateTime::unsafe_make(4_000_000_000_000);
    assert_eq!(run(q.prune(end)), Ok(1), "only the finished row");
    assert_eq!(q.len(), 1);
    assert_eq!(record(&q, later_id).state, JobState::Scheduled);
  }

  #[test]
  fn the_layer_provides_the_store() {
    let q = store();
    let program: Effect<Vec<QueueStats>, JobError, JobQueueEnv> =
      need::<JobQueueService>().flat_map(|s| s.stats().narrow());
    let stats = run(program.provide_layer(memory_job_queue_layer(q.clone()))).unwrap();
    assert!(stats.is_empty());
    assert!(q.wake_signal().is_some());
    assert!(format!("{:?}", JobQueueService::new(q)).contains("dyn JobStore"));
  }
}
