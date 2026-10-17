//! [`PgJobQueue`]: the queue in the PostgreSQL table `id_effect_jobs` over any [`SqlClient`];
//! [`enqueue_in`] on the caller's transaction; the crate's [`migrator`].

use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use id_effect::{Effect, Layer, UtcDateTime, WallClock, fail, hlist, need, succeed};
use id_effect_sql::migrate::Migrator;
use id_effect_sql::{
  SqlClient, SqlClientEnv, SqlClientService, SqlError, SqlRow, SqlTransaction, SqlValue, params,
};

use crate::JobState;
use crate::job::{Job, JobFailure, JobId, Priority, Queue, UniqueKey, UniqueScope};
use crate::memory::LEASE_EXPIRED;
use crate::queue::{
  Claimed, Enqueued, Fenced, JobError, JobFilter, JobOptions, JobQueue, JobQueueEnv,
  JobQueueService, JobRecord, JobStore, NewJob, QueueStats, WakeSignal, fold_stats, later,
  truncate_error,
};

/// The crate's migration `migrations/0001_id_effect_jobs.up.sql`.
const SCHEMA: &str = include_str!("../migrations/0001_id_effect_jobs.up.sql");
/// Its down script.
const SCHEMA_DOWN: &str = include_str!("../migrations/0001_id_effect_jobs.down.sql");

/// The tracking table of [`migrator`]: separate from the application's, so the crate's version
/// numbers never collide with the app's.
pub(crate) const MIGRATIONS_TABLE: &str = "_id_effect_jobs_migrations";

/// The predicate of `id_effect_jobs_unique_idx`: the job holds its unique key.
macro_rules! holds_key {
  () => {
    "(state IN ('scheduled', 'running', 'failed') OR unique_scope = 'pruned')"
  };
}

/// Insert unless the key is held; a conflict inserts nothing and never aborts the transaction.
/// One row `(id, inserted)`, or none when the holder committed after this statement's snapshot.
pub(crate) const ENQUEUE: &str = concat!(
  "WITH ins AS (INSERT INTO id_effect_jobs (name, queue, payload, priority, max_attempts, ",
  "unique_key, unique_scope, run_at, inserted_at) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9) ",
  "ON CONFLICT (queue, unique_key) WHERE unique_key IS NOT NULL AND ",
  holds_key!(),
  " DO NOTHING RETURNING id) ",
  "SELECT id, true AS inserted FROM ins UNION ALL ",
  "SELECT id, false AS inserted FROM id_effect_jobs WHERE queue = $2 AND unique_key = $6 AND ",
  holds_key!(),
  " AND NOT EXISTS (SELECT 1 FROM ins) LIMIT 1"
);

/// Claim: the attempt is counted here, so a crash mid-run still consumes it.
pub(crate) const CLAIM: &str = "WITH next AS (SELECT id FROM id_effect_jobs \
  WHERE queue = $1 AND state IN ('scheduled', 'failed') AND run_at <= $2 \
  ORDER BY priority DESC, run_at, id LIMIT $3 FOR UPDATE SKIP LOCKED) \
  UPDATE id_effect_jobs j SET state = 'running', attempts = j.attempts + 1, attempted_at = $2, \
  locked_by = $4, locked_until = $5 FROM next WHERE j.id = next.id \
  RETURNING j.id, j.name, j.payload, j.attempts, j.max_attempts, j.priority, j.run_at, \
  j.inserted_at";

pub(crate) const COMPLETE: &str = "UPDATE id_effect_jobs SET state = 'succeeded', \
  finished_at = $3, locked_by = NULL, locked_until = NULL \
  WHERE id = $1 AND locked_by = $2 AND state = 'running'";

pub(crate) const FAIL: &str = "UPDATE id_effect_jobs SET \
  state = CASE WHEN $3 OR attempts >= max_attempts THEN 'dead' ELSE 'failed' END, \
  finished_at = CASE WHEN $3 OR attempts >= max_attempts THEN $4 ELSE NULL END, \
  run_at = CASE WHEN $3 OR attempts >= max_attempts THEN run_at ELSE $5 END, \
  last_error = $6, locked_by = NULL, locked_until = NULL \
  WHERE id = $1 AND locked_by = $2 AND state = 'running'";

pub(crate) const HEARTBEAT: &str = "UPDATE id_effect_jobs SET locked_until = $2 \
  WHERE locked_by = $1 AND state = 'running'";

pub(crate) const RELEASE: &str = "UPDATE id_effect_jobs SET state = 'scheduled', \
  attempts = GREATEST(attempts - 1, 0), locked_by = NULL, locked_until = NULL \
  WHERE locked_by = $1 AND state = 'running'";

pub(crate) const RESCUE: &str = "UPDATE id_effect_jobs SET \
  state = CASE WHEN attempts >= max_attempts THEN 'dead' ELSE 'failed' END, \
  finished_at = CASE WHEN attempts >= max_attempts THEN $1 ELSE NULL END, \
  run_at = $1, locked_by = NULL, locked_until = NULL, last_error = $2 \
  WHERE state = 'running' AND locked_until < $1";

pub(crate) const PRUNE: &str = "DELETE FROM id_effect_jobs \
  WHERE state IN ('succeeded', 'dead', 'cancelled') AND finished_at < $1";

pub(crate) const CANCEL: &str = "UPDATE id_effect_jobs SET state = 'cancelled', finished_at = $2 \
  WHERE id = $1 AND state IN ('scheduled', 'failed')";

pub(crate) const RETRY_NOW: &str = "UPDATE id_effect_jobs SET state = 'scheduled', attempts = 0, \
  run_at = $2, finished_at = NULL WHERE id = $1 AND state IN ('dead', 'cancelled')";

pub(crate) const STATS: &str = "SELECT queue, state, count(*) AS jobs, min(run_at) AS oldest \
  FROM id_effect_jobs GROUP BY queue, state ORDER BY queue, state";

pub(crate) const LIST: &str = "SELECT id, name, queue, payload, state, priority, attempts, \
  max_attempts, unique_key, unique_scope, run_at, inserted_at, attempted_at, finished_at, \
  locked_by, locked_until, last_error FROM id_effect_jobs \
  WHERE ($1::text IS NULL OR queue = $1) AND ($2::text IS NULL OR state = $2) \
  AND ($3::bigint IS NULL OR id < $3) ORDER BY id DESC LIMIT $4";

/// The crate's schema as an `id_effect_sql` [`Migrator`] tracked in its own table
/// `_id_effect_jobs_migrations`, so the version numbers never collide with the application's
/// migrations. Run it at startup (`migrator().run_on(client)` or `migrator().layer()`); it takes
/// the migration advisory lock, so replicas starting together apply it once. The alternative is
/// to copy `migrations/0001_id_effect_jobs.{up,down}.sql` into the application's own migrations.
pub fn migrator() -> Migrator {
  Migrator::from_files([
    ("0001_id_effect_jobs.up.sql", SCHEMA),
    ("0001_id_effect_jobs.down.sql", SCHEMA_DOWN),
  ])
  .expect("the embedded id_effect_jobs migrations are valid")
  .with_table(MIGRATIONS_TABLE)
}

/// The SQL statements of a migration file: `--` comment lines dropped, split at `;`.
fn statements(script: &str) -> Vec<String> {
  let code: String = script
    .lines()
    .filter(|line| !line.trim_start().starts_with("--"))
    .map(|line| format!("{line}\n"))
    .collect();
  code
    .split(';')
    .map(str::trim)
    .filter(|s| !s.is_empty())
    .map(str::to_owned)
    .collect()
}

fn ready<A: Send + 'static>(r: Result<A, JobError>) -> Effect<A, JobError> {
  match r {
    Ok(a) => succeed(a),
    Err(e) => fail(e),
  }
}

fn at(row: &SqlRow, column: &str) -> Result<UtcDateTime, SqlError> {
  row.get::<jiff::Timestamp>(column).map(UtcDateTime)
}

fn at_opt(row: &SqlRow, column: &str) -> Result<Option<UtcDateTime>, SqlError> {
  Ok(row.get::<Option<jiff::Timestamp>>(column)?.map(UtcDateTime))
}

fn state(row: &SqlRow) -> Result<JobState, SqlError> {
  let s = row.get::<String>("state")?;
  s.parse()
    .map_err(|e| SqlError::Decode(format!("column `state`: {e}")))
}

fn decode_record(row: &SqlRow) -> Result<JobRecord, SqlError> {
  let unique = match row.get::<Option<String>>("unique_key")? {
    None => None,
    Some(key) => {
      let scope = row.get::<String>("unique_scope")?;
      let scope = UniqueScope::parse(&scope).ok_or_else(|| {
        SqlError::Decode(format!("column `unique_scope`: unknown scope `{scope}`"))
      })?;
      Some(UniqueKey { key, scope })
    }
  };
  Ok(JobRecord {
    id: JobId(row.get("id")?),
    name: row.get("name")?,
    queue: Queue::new(row.get::<String>("queue")?),
    payload: row.get("payload")?,
    state: state(row)?,
    priority: Priority(row.get("priority")?),
    attempts: row.get("attempts")?,
    max_attempts: row.get("max_attempts")?,
    unique,
    run_at: at(row, "run_at")?,
    inserted_at: at(row, "inserted_at")?,
    attempted_at: at_opt(row, "attempted_at")?,
    finished_at: at_opt(row, "finished_at")?,
    locked_by: row.get("locked_by")?,
    locked_until: at_opt(row, "locked_until")?,
    last_error: row.get("last_error")?,
  })
}

fn decode_claimed(row: &SqlRow, queue: &Queue) -> Result<Claimed, SqlError> {
  Ok(Claimed {
    id: JobId(row.get("id")?),
    name: row.get("name")?,
    queue: queue.clone(),
    payload: row.get("payload")?,
    attempt: row.get("attempts")?,
    max_attempts: row.get("max_attempts")?,
    priority: Priority(row.get("priority")?),
    run_at: at(row, "run_at")?,
    inserted_at: at(row, "inserted_at")?,
  })
}

/// The `ENQUEUE` parameters of `job` inserted at `now`.
fn enqueue_params(job: &NewJob, now: UtcDateTime) -> Vec<SqlValue> {
  let (key, scope) = match &job.unique {
    Some(u) => (Some(u.key.clone()), u.scope),
    None => (None, UniqueScope::WhilePending),
  };
  params![
    job.name,
    job.queue.as_str(),
    job.payload,
    job.priority.0,
    i32::from(job.max_attempts),
    key,
    scope.as_str(),
    job.run_at.resolve(now).0,
    now.0,
  ]
}

/// The [`Enqueued`] of an `ENQUEUE` result.
fn enqueued(rows: &[SqlRow]) -> Result<Enqueued, JobError> {
  let Some(row) = rows.first() else {
    return Ok(Enqueued::Duplicate { existing: None });
  };
  let id = JobId(row.get("id")?);
  Ok(if row.get::<bool>("inserted")? {
    Enqueued::New(id)
  } else {
    Enqueued::Duplicate { existing: Some(id) }
  })
}

fn fenced(rows: u64) -> Fenced {
  if rows == 0 {
    Fenced::Lost
  } else {
    Fenced::Applied
  }
}

/// [`JobStore`] in the PostgreSQL table `id_effect_jobs`, over any [`SqlClient`]
/// (`id_effect_sql::pg::PgSqlClient` in production). Clones share the client.
///
/// - Workers claim with `FOR UPDATE SKIP LOCKED` on a partial index of the runnable rows, highest
///   `priority` first, then earliest `run_at`; two workers never claim the same row.
/// - Uniqueness is the partial unique index on `(queue, unique_key)`; an enqueue that conflicts
///   inserts nothing (`ON CONFLICT DO NOTHING`), so it never aborts the caller's transaction.
/// - Every time is bound from the queue's [`WallClock`] (default [`WallClock::system`]), never
///   the database's `now()`: an anchored clock makes it deterministic in tests, and the nodes'
///   clocks must agree to well within the workers' lease.
///
/// Create the table with [`migrator`] or [`ensure_schema`](Self::ensure_schema), which runs the
/// same statements idempotently. Driver and decode failures are [`JobError::Sql`].
#[derive(Clone)]
pub struct PgJobQueue {
  client: SqlClientService,
  clock: WallClock,
  wake: WakeSignal,
}

impl fmt::Debug for PgJobQueue {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_struct("PgJobQueue")
      .field("table", &"id_effect_jobs")
      .field("clock", &self.clock)
      .finish_non_exhaustive()
  }
}

impl PgJobQueue {
  /// A queue over `client`, on the system clock.
  pub fn new(client: impl SqlClient) -> Self {
    Self::from_service(SqlClientService::new(client))
  }

  /// A queue over the client capability `client` (shares the client and its pool).
  pub fn from_service(client: SqlClientService) -> Self {
    Self {
      client,
      clock: WallClock::system(),
      wake: WakeSignal::new(),
    }
  }

  /// Reads every time from `clock` instead of the system clock.
  pub fn with_clock(mut self, clock: WallClock) -> Self {
    self.clock = clock;
    self
  }

  /// The clock it reads.
  pub fn clock(&self) -> &WallClock {
    &self.clock
  }

  /// Create `id_effect_jobs` and its indexes unless they exist: the statements of
  /// `migrations/0001_id_effect_jobs.up.sql`, one at a time. Safe to run at every start; two
  /// instances starting at the same moment may race on `CREATE TABLE` (use [`migrator`], which
  /// takes a lock, when that matters).
  pub fn ensure_schema(&self) -> Effect<(), JobError> {
    let client = self.client.clone();
    statements(SCHEMA)
      .into_iter()
      .fold(succeed(()), move |done, sql| {
        let client = client.clone();
        done.flat_map(move |()| client.execute(&sql, &[]).map(|_| ()).map_err(JobError::Sql))
      })
  }

  /// `f` with the client and the clock's now.
  fn at_now<A: Send + 'static>(
    &self,
    f: impl FnOnce(SqlClientService, UtcDateTime) -> Effect<A, SqlError> + Send + 'static,
  ) -> Effect<A, JobError> {
    let client = self.client.clone();
    self
      .clock
      .now()
      .flat_map(move |now| f(client, now).map_err(JobError::Sql))
  }
}

impl JobQueue for PgJobQueue {
  fn enqueue_raw(&self, job: NewJob) -> Effect<Enqueued, JobError> {
    if let Err(e) = job.validate() {
      return fail(e);
    }
    let wake = self.wake.clone();
    self
      .at_now(move |client, now| client.query(ENQUEUE, &enqueue_params(&job, now)))
      .flat_map(|rows| ready(enqueued(&rows)))
      .map(move |done| {
        if done.is_new() {
          wake.notify();
        }
        done
      })
  }

  fn cancel(&self, id: JobId) -> Effect<bool, JobError> {
    self.at_now(move |c, now| c.execute(CANCEL, &params![id.0, now.0]).map(|n| n > 0))
  }

  fn retry_now(&self, id: JobId) -> Effect<bool, JobError> {
    self.at_now(move |c, now| c.execute(RETRY_NOW, &params![id.0, now.0]).map(|n| n > 0))
  }

  fn stats(&self) -> Effect<Vec<QueueStats>, JobError> {
    self
      .client
      .query(STATS, &[])
      .map_err(JobError::Sql)
      .flat_map(|rows| {
        let groups = rows
          .iter()
          .map(|row| {
            Ok((
              Queue::new(row.get::<String>("queue")?),
              state(row)?,
              row.get::<u64>("jobs")?,
              at_opt(row, "oldest")?,
            ))
          })
          .collect::<Result<Vec<_>, SqlError>>();
        ready(groups.map(fold_stats).map_err(JobError::Sql))
      })
  }

  fn list(&self, filter: JobFilter) -> Effect<Vec<JobRecord>, JobError> {
    let params = params![
      filter.queue.as_ref().map(Queue::as_str),
      filter.state.map(JobState::as_str),
      filter.before.map(|id| id.0),
      i64::from(filter.limit),
    ];
    self
      .client
      .query(LIST, &params)
      .map_err(JobError::Sql)
      .flat_map(|rows| {
        let records = rows.iter().map(decode_record).collect::<Result<_, _>>();
        ready(records.map_err(JobError::Sql))
      })
  }
}

impl JobStore for PgJobQueue {
  fn claim(
    &self,
    queue: &Queue,
    worker: &str,
    limit: u16,
    lease: Duration,
  ) -> Effect<Vec<Claimed>, JobError> {
    let (queue, worker) = (queue.clone(), worker.to_owned());
    let name = queue.clone();
    self
      .at_now(move |c, now| {
        let params = params![
          name.as_str(),
          now.0,
          i64::from(limit),
          worker,
          later(now, lease).0
        ];
        c.query(CLAIM, &params)
      })
      .flat_map(move |rows| {
        let claimed = rows
          .iter()
          .map(|row| decode_claimed(row, &queue))
          .collect::<Result<Vec<_>, _>>()
          .map(|mut claimed| {
            claimed.sort_by(|a, b| {
              b.priority
                .cmp(&a.priority)
                .then(a.run_at.cmp(&b.run_at))
                .then(a.id.cmp(&b.id))
            });
            claimed
          });
        ready(claimed.map_err(JobError::Sql))
      })
  }

  fn complete(&self, id: JobId, worker: &str) -> Effect<Fenced, JobError> {
    let worker = worker.to_owned();
    self.at_now(move |c, now| {
      c.execute(COMPLETE, &params![id.0, worker, now.0])
        .map(fenced)
    })
  }

  fn fail(
    &self,
    id: JobId,
    worker: &str,
    failure: &JobFailure,
    retry_in: Option<Duration>,
  ) -> Effect<Fenced, JobError> {
    let worker = worker.to_owned();
    let dead = failure.is_discard() || retry_in.is_none();
    let error = truncate_error(failure.message());
    self.at_now(move |c, now| {
      let retry_at = later(now, retry_in.unwrap_or_default());
      let params = params![id.0, worker, dead, now.0, retry_at.0, error];
      c.execute(FAIL, &params).map(fenced)
    })
  }

  fn heartbeat(&self, worker: &str, lease: Duration) -> Effect<u64, JobError> {
    let worker = worker.to_owned();
    self.at_now(move |c, now| c.execute(HEARTBEAT, &params![worker, later(now, lease).0]))
  }

  fn release(&self, worker: &str) -> Effect<u64, JobError> {
    self
      .client
      .execute(RELEASE, &params![worker])
      .map_err(JobError::Sql)
  }

  fn rescue_expired(&self) -> Effect<u64, JobError> {
    self.at_now(|c, now| c.execute(RESCUE, &params![now.0, LEASE_EXPIRED]))
  }

  fn prune(&self, finished_before: UtcDateTime) -> Effect<u64, JobError> {
    self
      .client
      .execute(PRUNE, &params![finished_before.0])
      .map_err(JobError::Sql)
  }

  fn wake_signal(&self) -> Option<WakeSignal> {
    Some(self.wake.clone())
  }
}

/// Enqueues `job` with the caller's transaction `tx`: the row commits or rolls back with the
/// caller's work (the transactional outbox for jobs). A unique-key conflict inserts nothing
/// ([`Enqueued::Duplicate`]) and leaves `tx` usable. `clock` is the queue's clock (bind the
/// same one the [`PgJobQueue`] reads). Needs only the `id_effect_jobs` table, no queue
/// capability; it does not wake an in-process worker (it polls).
pub fn enqueue_in<J: Job>(
  tx: &Arc<dyn SqlTransaction>,
  job: J,
  opts: JobOptions,
  clock: &WallClock,
) -> Effect<Enqueued, JobError> {
  let job = NewJob::of(job, opts);
  if let Err(e) = job.validate() {
    return fail(e);
  }
  let tx = Arc::clone(tx);
  clock.now().flat_map(move |now| {
    tx.query(ENQUEUE, &enqueue_params(&job, now))
      .map_err(JobError::Sql)
      .flat_map(|rows| ready(enqueued(&rows)))
  })
}

/// A [`Layer`] providing a [`PgJobQueue`] (system clock) as the [`JobQueueService`], over the
/// [`SqlClientService`] it needs (feed it `PgSqlClient::layer(config)` with `provide_to`). It
/// does not create the table: run [`migrator`] or [`PgJobQueue::ensure_schema`] at startup.
pub fn pg_job_queue_layer<E: Send + 'static>() -> Layer<JobQueueEnv, E, SqlClientEnv> {
  Layer::effect(|| {
    need::<SqlClientService>().map(|client| {
      hlist![JobQueueService::new(PgJobQueue::from_service(
        (*client).clone()
      ))]
    })
  })
}

#[cfg(test)]
mod tests {
  use id_effect::{Cause, Exit, Schema};
  use id_effect_sql::{SqlClient, TestSqlClient, provide_sql_client};

  use super::*;

  fn run<A: Send + 'static>(eff: Effect<A, JobError>) -> Result<A, JobError> {
    match eff.block_on() {
      Exit::Success(a) => Ok(a),
      Exit::Failure(Cause::Fail(e)) => Err(e),
      Exit::Failure(c) => panic!("unexpected cause: {c:?}"),
    }
  }

  fn t(secs: i64) -> UtcDateTime {
    UtcDateTime::unsafe_make(secs * 1000)
  }

  fn connected() -> (PgJobQueue, TestSqlClient) {
    let client = TestSqlClient::new();
    run(client.connect().map_err(JobError::Sql)).unwrap();
    (PgJobQueue::new(client.clone()), client)
  }

  #[derive(Clone, Schema)]
  struct Ping {
    n: i64,
  }

  impl Job for Ping {
    const NAME: &'static str = "test.ping";

    fn unique_key(&self) -> Option<UniqueKey> {
      Some(UniqueKey::until_pruned(format!("ping@{}", self.n)))
    }
  }

  fn record_row(state: &str) -> SqlRow {
    SqlRow::from_pairs([
      ("id", SqlValue::I64(3)),
      ("name", SqlValue::Text("test.ping".into())),
      ("queue", SqlValue::Text("default".into())),
      ("payload", SqlValue::Json(serde_json::json!({"n": 1}))),
      ("state", SqlValue::Text(state.into())),
      ("priority", SqlValue::I16(10)),
      ("attempts", SqlValue::I32(1)),
      ("max_attempts", SqlValue::I32(20)),
      ("unique_key", SqlValue::Text("ping@1".into())),
      ("unique_scope", SqlValue::Text("pruned".into())),
      ("run_at", SqlValue::Timestamp(t(5).0)),
      ("inserted_at", SqlValue::Timestamp(t(4).0)),
      ("attempted_at", SqlValue::Timestamp(t(5).0)),
      ("finished_at", SqlValue::Null),
      ("locked_by", SqlValue::Text("w".into())),
      ("locked_until", SqlValue::Timestamp(t(65).0)),
      ("last_error", SqlValue::Null),
    ])
  }

  #[test]
  fn ensure_schema_runs_the_migration_statements() {
    let (queue, client) = connected();
    run(queue.ensure_schema()).unwrap();
    let sql: Vec<String> = client
      .recorded_executes()
      .into_iter()
      .map(|(sql, _)| sql)
      .collect();
    assert_eq!(sql.len(), 5, "{sql:?}");
    assert!(sql[0].starts_with("CREATE TABLE IF NOT EXISTS id_effect_jobs"));
    assert!(sql[1].contains("id_effect_jobs_dequeue_idx"));
    assert!(sql[2].starts_with("CREATE UNIQUE INDEX IF NOT EXISTS id_effect_jobs_unique_idx"));
    assert!(sql[3].contains("(locked_until)") && sql[4].contains("(finished_at)"));
    assert!(sql.iter().all(|s| !s.contains("--") && !s.ends_with(';')));
    assert!(ENQUEUE.contains(holds_key!()) && SCHEMA.contains(holds_key!()));
  }

  #[test]
  fn the_migrator_has_its_own_tracking_table() {
    let m = migrator();
    assert_eq!(m.table(), MIGRATIONS_TABLE);
    assert_eq!(m.migrations().len(), 1);
    let first = &m.migrations()[0];
    assert_eq!((first.version(), first.is_reversible()), (1, true));
    assert_eq!(first.sql(), SCHEMA);
  }

  #[test]
  fn enqueue_in_inserts_with_the_callers_transaction() {
    let (_, client) = connected();
    client.script_query(
      ENQUEUE,
      vec![SqlRow::from_pairs([
        ("id", SqlValue::I64(5)),
        ("inserted", SqlValue::Bool(true)),
      ])],
    );
    let clock = WallClock::anchored(std::time::Instant::now(), t(100));
    let tx = run(client.begin().map_err(JobError::Sql)).unwrap();
    let done = run(enqueue_in(&tx, Ping { n: 1 }, JobOptions::new(), &clock)).unwrap();
    assert_eq!(done, Enqueued::New(JobId(5)));
    assert!(
      client.recorded_queries().is_empty(),
      "not before the commit"
    );
    run(tx.commit().map_err(JobError::Sql)).unwrap();
    let (sql, params) = &client.recorded_queries()[0];
    assert_eq!(sql, ENQUEUE);
    assert_eq!(params[0], SqlValue::Text("test.ping".into()));
    assert_eq!(params[2], SqlValue::Json(serde_json::json!({"n": 1})));
    assert_eq!(params[5], SqlValue::Text("ping@1".into()));
    assert_eq!(params[6], SqlValue::Text("pruned".into()));
    assert_eq!(params[7], params[8], "runs now");

    // Rolled back: nothing recorded.
    let tx = run(client.begin().map_err(JobError::Sql)).unwrap();
    run(enqueue_in(&tx, Ping { n: 2 }, JobOptions::new(), &clock)).unwrap();
    run(tx.rollback().map_err(JobError::Sql)).unwrap();
    assert_eq!(client.recorded_queries().len(), 1);

    // A held key: the holder's id, or none when it is not visible yet.
    let dup = SqlRow::from_pairs([
      ("id", SqlValue::I64(5)),
      ("inserted", SqlValue::Bool(false)),
    ]);
    assert_eq!(
      enqueued(&[dup]).unwrap(),
      Enqueued::Duplicate {
        existing: Some(JobId(5))
      }
    );
    assert_eq!(
      enqueued(&[]).unwrap(),
      Enqueued::Duplicate { existing: None }
    );
  }

  #[test]
  fn claim_binds_the_lease_and_orders_the_rows() {
    let (queue, client) = connected();
    let row = |id: i64, priority: i16| {
      SqlRow::from_pairs([
        ("id", SqlValue::I64(id)),
        ("name", SqlValue::Text("test.ping".into())),
        ("payload", SqlValue::Json(serde_json::json!({"n": id}))),
        ("attempts", SqlValue::I32(1)),
        ("max_attempts", SqlValue::I32(3)),
        ("priority", SqlValue::I16(priority)),
        ("run_at", SqlValue::Timestamp(t(1).0)),
        ("inserted_at", SqlValue::Timestamp(t(1).0)),
      ])
    };
    client.script_query(CLAIM, vec![row(1, 0), row(2, 10)]);
    let q = Queue::new("mail");
    let claimed = run(queue.claim(&q, "w1", 8, Duration::from_secs(60))).unwrap();
    assert_eq!(
      claimed.iter().map(|c| c.id.0).collect::<Vec<_>>(),
      [2, 1],
      "higher priority first"
    );
    assert_eq!((claimed[0].queue.as_str(), claimed[0].attempt), ("mail", 1));
    let (_, params) = &client.recorded_queries()[0];
    assert_eq!(params[0], SqlValue::Text("mail".into()));
    assert_eq!(params[2], SqlValue::I64(8));
    assert_eq!(params[3], SqlValue::Text("w1".into()));
    let (SqlValue::Timestamp(now), SqlValue::Timestamp(until)) = (&params[1], &params[4]) else {
      panic!("timestamps: {params:?}");
    };
    assert_eq!(until.duration_since(*now).as_secs(), 60);
  }

  #[test]
  fn outcomes_are_fenced_on_the_affected_rows() {
    let (queue, client) = connected();
    assert_eq!(run(queue.complete(JobId(1), "w")), Ok(Fenced::Lost));
    client.script_execute(COMPLETE, 1);
    assert_eq!(run(queue.complete(JobId(1), "w")), Ok(Fenced::Applied));
    client.script_execute(FAIL, 1);
    let f = JobFailure::retry("x".repeat(5000));
    assert_eq!(
      run(queue.fail(JobId(1), "w", &f, Some(Duration::from_secs(2)))),
      Ok(Fenced::Applied)
    );
    let (_, params) = client.recorded_executes().pop().unwrap();
    assert_eq!(params[2], SqlValue::Bool(false), "not dead: a delay");
    assert!(matches!(&params[5], SqlValue::Text(e) if e.len() == 4096));
    run(queue.fail(JobId(1), "w", &JobFailure::discard("no"), None)).unwrap();
    let (_, params) = client.recorded_executes().pop().unwrap();
    assert_eq!(params[2], SqlValue::Bool(true));
    client.script_execute(RESCUE, 2);
    assert_eq!(run(queue.rescue_expired()), Ok(2));
    let (_, params) = client.recorded_executes().pop().unwrap();
    assert_eq!(params[1], SqlValue::Text(LEASE_EXPIRED.into()));
    for (sql, eff) in [
      (HEARTBEAT, queue.heartbeat("w", Duration::from_secs(9))),
      (RELEASE, queue.release("w")),
      (PRUNE, queue.prune(t(1))),
    ] {
      client.script_execute(sql, 4);
      assert_eq!(run(eff), Ok(4), "{sql}");
    }
    client.script_execute(CANCEL, 1);
    assert_eq!(run(queue.cancel(JobId(1))), Ok(true));
    assert_eq!(run(queue.retry_now(JobId(1))), Ok(false));
  }

  #[test]
  fn list_and_stats_decode_their_rows() {
    let (queue, client) = connected();
    client.script_query(LIST, vec![record_row("running")]);
    let filter = JobFilter {
      state: Some(JobState::Running),
      before: Some(JobId(10)),
      ..JobFilter::default()
    };
    let records = run(queue.list(filter)).unwrap();
    assert_eq!(records[0].state, JobState::Running);
    assert_eq!(records[0].unique, Some(UniqueKey::until_pruned("ping@1")));
    assert_eq!(records[0].locked_until, Some(t(65)));
    assert_eq!(records[0].finished_at, None);
    let (_, params) = &client.recorded_queries()[0];
    assert_eq!(params[0], SqlValue::Null);
    assert_eq!(params[1], SqlValue::Text("running".into()));
    assert_eq!(params[2], SqlValue::I64(10));
    assert_eq!(params[3], SqlValue::I64(100));
    client.script_query(LIST, vec![record_row("lost")]);
    let Err(JobError::Sql(SqlError::Decode(m))) = run(queue.list(JobFilter::default())) else {
      panic!("expected a decode error");
    };
    assert!(m.contains("lost"), "{m}");
    client.script_query(
      STATS,
      vec![SqlRow::from_pairs([
        ("queue", SqlValue::Text("default".into())),
        ("state", SqlValue::Text("failed".into())),
        ("jobs", SqlValue::I64(3)),
        ("oldest", SqlValue::Timestamp(t(2).0)),
      ])],
    );
    let stats = run(queue.stats()).unwrap();
    assert_eq!(stats[0].count(JobState::Failed), 3);
    assert_eq!(stats[0].oldest_runnable, Some(t(2)));
  }

  #[test]
  fn driver_failures_are_sql_errors_and_bad_options_fail_early() {
    let queue = PgJobQueue::new(TestSqlClient::new()); // never connected
    let down = |r: Result<(), JobError>| matches!(r, Err(JobError::Sql(SqlError::NotConnected)));
    assert!(down(run(queue.ensure_schema())));
    assert!(down(run(queue.stats()).map(|_| ())));
    assert!(down(run(queue.rescue_expired()).map(|_| ())));
    let mut job = NewJob::of(Ping { n: 1 }, JobOptions::new());
    job.max_attempts = 0;
    assert!(matches!(
      run(queue.enqueue_raw(job)),
      Err(JobError::InvalidOptions(_))
    ));
    assert!(format!("{queue:?}").contains("id_effect_jobs"));
    assert_eq!(
      statements("-- a; b\nSELECT 1;\n\n  -- c\nSELECT 2;\n;"),
      ["SELECT 1", "SELECT 2"]
    );
  }

  #[test]
  fn the_layer_provides_the_queue_over_the_client() {
    let (_, client) = connected();
    let program: Effect<Vec<QueueStats>, JobError, JobQueueEnv> =
      need::<JobQueueService>().flat_map(|q| q.stats().narrow());
    let layer = provide_sql_client::<JobError>(client).provide_to(pg_job_queue_layer());
    assert_eq!(run(program.provide_layer(layer)), Ok(vec![]));
  }
}
