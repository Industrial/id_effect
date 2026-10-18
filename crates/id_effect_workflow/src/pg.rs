//! [`PgWorkflowStore`]: instances, history, completions and signals in the PostgreSQL tables
//! `workflow_instances`, `workflow_history`, `workflow_completions` and `workflow_signals` over
//! any [`SqlClient`]; the crate's [`migrator`].

use std::fmt;

use id_effect::{Effect, UtcDateTime, fail, succeed};
use id_effect_sql::migrate::Migrator;
use id_effect_sql::{SqlClient, SqlClientService, SqlError, SqlRow, params};

use crate::error::WorkflowError;
use crate::history::{
  Claimed, Completion, EventKind, Finish, HistoryEvent, InstanceRecord, NewInstance, SignalRecord,
  WorkflowStatus, WorkflowStore,
};
use crate::lease::{EngineId, Fenced, Lease};
use crate::workflow::WorkflowId;

/// The crate's migration `migrations/0001_id_effect_workflow.up.sql`.
const SCHEMA: &str = include_str!("../migrations/0001_id_effect_workflow.up.sql");
/// Its down script.
const SCHEMA_DOWN: &str = include_str!("../migrations/0001_id_effect_workflow.down.sql");
/// The migration `migrations/0002_activities_timers_signals.up.sql`.
const SCHEMA_2: &str = include_str!("../migrations/0002_activities_timers_signals.up.sql");
/// Its down script.
const SCHEMA_2_DOWN: &str = include_str!("../migrations/0002_activities_timers_signals.down.sql");

/// The tracking table of [`migrator`]: separate from the application's, so the crate's version
/// numbers never collide with the app's.
pub(crate) const MIGRATIONS_TABLE: &str = "_id_effect_workflow_migrations";

pub(crate) const CREATE: &str = "INSERT INTO workflow_instances \
  (id, name, version, status, input, fencing_token, created_at, updated_at) \
  VALUES ($1, $2, $3, 'running', $4, 0, $5, $5) ON CONFLICT (id) DO NOTHING";

pub(crate) const GET: &str = "SELECT id, name, version, status, input, output, error, \
  lease_owner, lease_expires_at, fencing_token, waiting, wakeups, created_at, updated_at \
  FROM workflow_instances WHERE id = $1";

/// Takes one instance's lease when free or expired and the instance is not parked. The `UPDATE`
/// waits on a concurrent writer's row lock and re-checks the predicate on the row it committed,
/// so two engines never both win.
pub(crate) const ACQUIRE: &str = "UPDATE workflow_instances SET lease_owner = $2, \
  lease_expires_at = $4, fencing_token = fencing_token + 1, updated_at = $3 \
  WHERE id = $1 AND status = 'running' AND NOT waiting \
  AND (lease_expires_at IS NULL OR lease_expires_at < $3) \
  RETURNING id, name, version, input, fencing_token, wakeups";

/// Takes up to `$3` leases of workflow `$1`, skipping parked rows and rows another engine has
/// locked.
pub(crate) const CLAIM: &str = "WITH next AS (SELECT id FROM workflow_instances \
  WHERE name = $1 AND status = 'running' AND NOT waiting \
  AND (lease_expires_at IS NULL OR lease_expires_at < $2) \
  ORDER BY created_at, id LIMIT $3 FOR UPDATE SKIP LOCKED) \
  UPDATE workflow_instances w SET lease_owner = $4, lease_expires_at = $5, \
  fencing_token = w.fencing_token + 1, updated_at = $2 FROM next WHERE w.id = next.id \
  RETURNING w.id, w.name, w.version, w.input, w.fencing_token, w.wakeups, w.created_at";

pub(crate) const HISTORY: &str = "SELECT seq, kind, name, payload, recorded_at \
  FROM workflow_history WHERE instance_id = $1 ORDER BY seq";

/// The fenced append: the `UPDATE` checks owner, token, status and expiry and extends the lease
/// while holding the instance row's lock, and the insert only happens when it matched, in one
/// statement. A takeover (`ACQUIRE`/`CLAIM`) serialises on the same row lock, so a stale writer's
/// append is either before the takeover (its lease still held) or rejected (token changed). A
/// recorded `seq` inserts nothing.
pub(crate) const APPEND: &str = "WITH lease AS (UPDATE workflow_instances \
  SET lease_expires_at = $8, updated_at = $7 \
  WHERE id = $1 AND lease_owner = $5 AND fencing_token = $6 AND status = 'running' \
  AND lease_expires_at > $7 RETURNING id) \
  INSERT INTO workflow_history (instance_id, seq, kind, name, payload, recorded_at) \
  SELECT id, $2::bigint, $3::text, $4::text, $9::jsonb, $7::timestamptz FROM lease \
  ON CONFLICT (instance_id, seq) DO NOTHING";

pub(crate) const RENEW: &str = "UPDATE workflow_instances SET lease_expires_at = $5, \
  updated_at = $4 WHERE id = $1 AND lease_owner = $2 AND fencing_token = $3 \
  AND status = 'running'";

pub(crate) const FINISH: &str = "UPDATE workflow_instances SET status = $4, output = $5, \
  error = $6, lease_owner = NULL, lease_expires_at = NULL, updated_at = $7 \
  WHERE id = $1 AND lease_owner = $2 AND fencing_token = $3 AND status = 'running' \
  AND lease_expires_at > $7";

pub(crate) const RELEASE: &str = "UPDATE workflow_instances SET lease_owner = NULL, \
  lease_expires_at = NULL, updated_at = $4 \
  WHERE id = $1 AND lease_owner = $2 AND fencing_token = $3 AND status = 'running'";

/// Parks the instance (fenced) only while `wakeups` is the value the run claimed it with. A
/// completion or a signal increments `wakeups` under the same row lock, so it either lands
/// before this statement (which then matches nothing: the engine releases instead and the
/// instance runs again) or after it (and clears `waiting`): no wake-up is lost.
pub(crate) const WAIT: &str = "UPDATE workflow_instances SET waiting = true, lease_owner = NULL, \
  lease_expires_at = NULL, updated_at = $5 \
  WHERE id = $1 AND lease_owner = $2 AND fencing_token = $3 AND status = 'running' \
  AND wakeups = $4";

pub(crate) const COMPLETION: &str = "SELECT seq, kind, name, payload, completed_at \
  FROM workflow_completions WHERE instance_id = $1 AND seq = $2";

/// Records a completion once per `(instance, seq)` (the idempotency key) and wakes the instance
/// in the same statement; a duplicate inserts nothing and wakes nothing.
pub(crate) const COMPLETE: &str = "WITH ins AS (INSERT INTO workflow_completions \
  (instance_id, seq, kind, name, payload, completed_at) \
  SELECT id, $2::bigint, $3::text, $4::text, $5::jsonb, $6::timestamptz \
  FROM workflow_instances WHERE id = $1 \
  ON CONFLICT (instance_id, seq) DO NOTHING RETURNING instance_id) \
  UPDATE workflow_instances w SET wakeups = w.wakeups + 1, waiting = false, updated_at = $6 \
  FROM ins WHERE w.id = ins.instance_id";

/// Stores a signal numbered by the instance's incremented `wakeups`: the number is taken under
/// the instance row lock, so numbers grow in commit order.
pub(crate) const SIGNAL: &str = "WITH bump AS (UPDATE workflow_instances \
  SET wakeups = wakeups + 1, waiting = false, updated_at = $4 \
  WHERE id = $1 AND status = 'running' RETURNING id, wakeups) \
  INSERT INTO workflow_signals (instance_id, number, name, payload, received_at) \
  SELECT id, wakeups, $2::text, $3::jsonb, $4::timestamptz FROM bump RETURNING number";

pub(crate) const NEXT_SIGNAL: &str = "SELECT number, name, payload, received_at \
  FROM workflow_signals WHERE instance_id = $1 AND name = $2 AND number > $3 \
  ORDER BY number LIMIT 1";

/// The crate's schema as an `id_effect_sql` [`Migrator`] tracked in its own table
/// `_id_effect_workflow_migrations`, so the version numbers never collide with the
/// application's migrations. Run it at startup (`migrator().run_on(client)` or
/// `migrator().layer()`); it takes the migration advisory lock, so replicas starting together
/// apply it once. The alternative is to copy `migrations/0001_id_effect_workflow.{up,down}.sql`
/// and `migrations/0002_activities_timers_signals.{up,down}.sql` into the application's own
/// migrations.
pub fn migrator() -> Migrator {
  Migrator::from_files([
    ("0001_id_effect_workflow.up.sql", SCHEMA),
    ("0001_id_effect_workflow.down.sql", SCHEMA_DOWN),
    ("0002_activities_timers_signals.up.sql", SCHEMA_2),
    ("0002_activities_timers_signals.down.sql", SCHEMA_2_DOWN),
  ])
  .expect("the embedded id_effect_workflow migrations are valid")
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

fn ready<A: Send + 'static>(r: Result<A, WorkflowError>) -> Effect<A, WorkflowError> {
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

fn decode_record(row: &SqlRow) -> Result<InstanceRecord, WorkflowError> {
  Ok(InstanceRecord {
    id: WorkflowId::stored(row.get("id")?),
    name: row.get("name")?,
    version: row.get("version")?,
    status: row.get::<String>("status")?.parse()?,
    input: row.get("input")?,
    output: row.get("output")?,
    error: row.get("error")?,
    lease_owner: row.get::<Option<String>>("lease_owner")?.map(EngineId::new),
    lease_expires_at: at_opt(row, "lease_expires_at")?,
    fencing_token: row.get("fencing_token")?,
    waiting: row.get("waiting")?,
    wakeups: row.get("wakeups")?,
    created_at: at(row, "created_at")?,
    updated_at: at(row, "updated_at")?,
  })
}

fn decode_claimed(
  row: &SqlRow,
  owner: &EngineId,
  expires_at: UtcDateTime,
) -> Result<Claimed, WorkflowError> {
  Ok(Claimed {
    lease: Lease {
      instance: WorkflowId::stored(row.get("id")?),
      owner: owner.clone(),
      token: row.get("fencing_token")?,
      expires_at,
    },
    name: row.get("name")?,
    version: row.get("version")?,
    input: row.get("input")?,
    wakeups: row.get("wakeups")?,
  })
}

fn decode_event(row: &SqlRow) -> Result<HistoryEvent, WorkflowError> {
  Ok(HistoryEvent {
    seq: row.get("seq")?,
    kind: row.get::<String>("kind")?.parse::<EventKind>()?,
    name: row.get("name")?,
    payload: row.get("payload")?,
    recorded_at: at(row, "recorded_at")?,
  })
}

fn decode_completion(row: &SqlRow) -> Result<Completion, WorkflowError> {
  Ok(Completion {
    seq: row.get("seq")?,
    kind: row.get::<String>("kind")?.parse::<EventKind>()?,
    name: row.get("name")?,
    payload: row.get("payload")?,
    completed_at: at(row, "completed_at")?,
  })
}

fn decode_signal(row: &SqlRow) -> Result<SignalRecord, WorkflowError> {
  Ok(SignalRecord {
    number: row.get("number")?,
    name: row.get("name")?,
    payload: row.get("payload")?,
    received_at: at(row, "received_at")?,
  })
}

/// [`WorkflowStore`] in the PostgreSQL tables `workflow_instances`, `workflow_history`,
/// `workflow_completions` and `workflow_signals`, over any [`SqlClient`]
/// (`id_effect_sql::pg::PgSqlClient` in production). Clones share the client.
///
/// - Each operation is one statement (autocommit); the fenced ones check the lease's owner,
///   fencing token and status on the instance row, and the append and finish also its expiry, so
///   a stale writer is rejected however long it paused (see [`crate::lease`]).
/// - `claim` uses `FOR UPDATE SKIP LOCKED` on the running instances of one workflow, oldest
///   first; two engines never take the same lease.
/// - Every time is the caller's (the engine's [`WallClock`](id_effect::WallClock)), never the
///   database's `now()`: the engines' clocks must agree to well within the lease.
/// - A completion and a signal each wake the instance in the statement that records them
///   (`waiting = false`, `wakeups + 1` on the instance row), and parking checks `wakeups`, so a
///   wake-up that races a run's decision to park is never lost.
///
/// Create the tables with [`migrator`] or [`ensure_schema`](Self::ensure_schema), which runs the
/// same statements idempotently. Driver and decode failures are [`WorkflowError::Sql`].
#[derive(Clone)]
pub struct PgWorkflowStore {
  client: SqlClientService,
}

impl fmt::Debug for PgWorkflowStore {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_struct("PgWorkflowStore")
      .field(
        "tables",
        &(
          "workflow_instances",
          "workflow_history",
          "workflow_completions",
          "workflow_signals",
        ),
      )
      .finish_non_exhaustive()
  }
}

impl PgWorkflowStore {
  /// A store over `client`.
  pub fn new(client: impl SqlClient) -> Self {
    Self::from_service(SqlClientService::new(client))
  }

  /// A store over the client capability `client` (shares the client and its pool).
  pub fn from_service(client: SqlClientService) -> Self {
    Self { client }
  }

  /// Create the tables and indexes unless they exist: the statements of
  /// `migrations/0001_id_effect_workflow.up.sql` and
  /// `migrations/0002_activities_timers_signals.up.sql`, one at a time. Safe to run at every
  /// start; two instances starting at the same moment may race on `CREATE TABLE` (use
  /// [`migrator`], which takes a lock, when that matters).
  pub fn ensure_schema(&self) -> Effect<(), WorkflowError> {
    let client = self.client.clone();
    statements(SCHEMA)
      .into_iter()
      .chain(statements(SCHEMA_2))
      .fold(succeed(()), move |done, sql| {
        let client = client.clone();
        done.flat_map(move |()| {
          client
            .execute(&sql, &[])
            .map(|_| ())
            .map_err(WorkflowError::Sql)
        })
      })
  }

  fn execute(
    &self,
    sql: &'static str,
    params: Vec<id_effect_sql::SqlValue>,
  ) -> Effect<u64, WorkflowError> {
    self
      .client
      .execute(sql, &params)
      .map_err(WorkflowError::Sql)
  }

  fn query(
    &self,
    sql: &'static str,
    params: Vec<id_effect_sql::SqlValue>,
  ) -> Effect<Vec<SqlRow>, WorkflowError> {
    self.client.query(sql, &params).map_err(WorkflowError::Sql)
  }
}

impl WorkflowStore for PgWorkflowStore {
  fn create(&self, instance: NewInstance, now: UtcDateTime) -> Effect<bool, WorkflowError> {
    let params = params![
      instance.id.as_str(),
      instance.name,
      instance.version,
      instance.input,
      now.0,
    ];
    self.execute(CREATE, params).map(|n| n > 0)
  }

  fn get(&self, id: &WorkflowId) -> Effect<Option<InstanceRecord>, WorkflowError> {
    self
      .query(GET, params![id.as_str()])
      .flat_map(|rows| ready(rows.first().map(decode_record).transpose()))
  }

  fn acquire(
    &self,
    id: &WorkflowId,
    owner: &EngineId,
    now: UtcDateTime,
    expires_at: UtcDateTime,
  ) -> Effect<Option<Claimed>, WorkflowError> {
    let owner = owner.clone();
    let params = params![id.as_str(), owner.as_str(), now.0, expires_at.0];
    self.query(ACQUIRE, params).flat_map(move |rows| {
      ready(
        rows
          .first()
          .map(|row| decode_claimed(row, &owner, expires_at))
          .transpose(),
      )
    })
  }

  fn claim(
    &self,
    name: &str,
    owner: &EngineId,
    limit: u16,
    now: UtcDateTime,
    expires_at: UtcDateTime,
  ) -> Effect<Vec<Claimed>, WorkflowError> {
    let owner = owner.clone();
    let params = params![name, now.0, i64::from(limit), owner.as_str(), expires_at.0];
    self.query(CLAIM, params).flat_map(move |rows| {
      // RETURNING follows no order: sort as claimed, oldest first.
      let mut keyed = Vec::with_capacity(rows.len());
      for row in &rows {
        let created = match at(row, "created_at") {
          Ok(at) => at,
          Err(e) => return fail(WorkflowError::Sql(e)),
        };
        match decode_claimed(row, &owner, expires_at) {
          Ok(claimed) => keyed.push((created, claimed)),
          Err(e) => return fail(e),
        }
      }
      keyed.sort_by(|a, b| {
        a.0
          .cmp(&b.0)
          .then_with(|| a.1.lease.instance.cmp(&b.1.lease.instance))
      });
      succeed(keyed.into_iter().map(|(_, c)| c).collect())
    })
  }

  fn history(&self, id: &WorkflowId) -> Effect<Vec<HistoryEvent>, WorkflowError> {
    self
      .query(HISTORY, params![id.as_str()])
      .flat_map(|rows| ready(rows.iter().map(decode_event).collect()))
  }

  fn append(
    &self,
    lease: &Lease,
    event: HistoryEvent,
    extend_to: UtcDateTime,
  ) -> Effect<Fenced, WorkflowError> {
    let params = params![
      lease.instance.as_str(),
      event.seq,
      event.kind.as_str(),
      event.name,
      lease.owner.as_str(),
      lease.token,
      event.recorded_at.0,
      extend_to.0,
      event.payload,
    ];
    self.execute(APPEND, params).map(Fenced::from_rows)
  }

  fn renew(
    &self,
    lease: &Lease,
    now: UtcDateTime,
    expires_at: UtcDateTime,
  ) -> Effect<Fenced, WorkflowError> {
    let params = params![
      lease.instance.as_str(),
      lease.owner.as_str(),
      lease.token,
      now.0,
      expires_at.0,
    ];
    self.execute(RENEW, params).map(Fenced::from_rows)
  }

  fn finish(
    &self,
    lease: &Lease,
    outcome: Finish,
    now: UtcDateTime,
  ) -> Effect<Fenced, WorkflowError> {
    let (status, output, error) = match outcome {
      Finish::Completed(output) => (WorkflowStatus::Completed, Some(output), None),
      Finish::Failed(error) => (WorkflowStatus::Failed, None, Some(error)),
    };
    let params = params![
      lease.instance.as_str(),
      lease.owner.as_str(),
      lease.token,
      status.as_str(),
      output,
      error,
      now.0,
    ];
    self.execute(FINISH, params).map(Fenced::from_rows)
  }

  fn release(&self, lease: &Lease, now: UtcDateTime) -> Effect<Fenced, WorkflowError> {
    let params = params![
      lease.instance.as_str(),
      lease.owner.as_str(),
      lease.token,
      now.0,
    ];
    self.execute(RELEASE, params).map(Fenced::from_rows)
  }

  fn wait(&self, lease: &Lease, wakeups: i64, now: UtcDateTime) -> Effect<Fenced, WorkflowError> {
    let params = params![
      lease.instance.as_str(),
      lease.owner.as_str(),
      lease.token,
      wakeups,
      now.0,
    ];
    self.execute(WAIT, params).map(Fenced::from_rows)
  }

  fn completion(&self, id: &WorkflowId, seq: i64) -> Effect<Option<Completion>, WorkflowError> {
    self
      .query(COMPLETION, params![id.as_str(), seq])
      .flat_map(|rows| ready(rows.first().map(decode_completion).transpose()))
  }

  fn complete(&self, id: &WorkflowId, completion: Completion) -> Effect<bool, WorkflowError> {
    let params = params![
      id.as_str(),
      completion.seq,
      completion.kind.as_str(),
      completion.name,
      completion.payload,
      completion.completed_at.0,
    ];
    self.execute(COMPLETE, params).map(|n| n > 0)
  }

  fn signal(
    &self,
    id: &WorkflowId,
    name: &str,
    payload: serde_json::Value,
    now: UtcDateTime,
  ) -> Effect<Option<i64>, WorkflowError> {
    let params = params![id.as_str(), name, payload, now.0];
    self.query(SIGNAL, params).flat_map(|rows| {
      ready(
        rows
          .first()
          .map(|row| row.get::<i64>("number").map_err(WorkflowError::Sql))
          .transpose(),
      )
    })
  }

  fn next_signal(
    &self,
    id: &WorkflowId,
    name: &str,
    after: i64,
  ) -> Effect<Option<SignalRecord>, WorkflowError> {
    self
      .query(NEXT_SIGNAL, params![id.as_str(), name, after])
      .flat_map(|rows| ready(rows.first().map(decode_signal).transpose()))
  }
}

#[cfg(test)]
mod tests {
  use id_effect::{Cause, Exit};
  use id_effect_sql::{SqlValue, TestSqlClient};

  use super::*;

  fn run<A: Send + 'static>(eff: Effect<A, WorkflowError>) -> Result<A, WorkflowError> {
    match eff.block_on() {
      Exit::Success(a) => Ok(a),
      Exit::Failure(Cause::Fail(e)) => Err(e),
      Exit::Failure(c) => panic!("unexpected cause: {c:?}"),
    }
  }

  fn t(secs: i64) -> UtcDateTime {
    UtcDateTime::unsafe_make(secs * 1000)
  }

  fn connected() -> (PgWorkflowStore, TestSqlClient) {
    let client = TestSqlClient::new();
    run(client.connect().map_err(WorkflowError::Sql)).unwrap();
    (PgWorkflowStore::new(client.clone()), client)
  }

  fn lease() -> Lease {
    Lease {
      instance: WorkflowId::new("i").unwrap(),
      owner: EngineId::new("e"),
      token: 3,
      expires_at: t(10),
    }
  }

  #[test]
  fn ensure_schema_runs_the_migration_statements() {
    let (store, client) = connected();
    run(store.ensure_schema()).unwrap();
    let sql: Vec<String> = client
      .recorded_executes()
      .into_iter()
      .map(|(sql, _)| sql)
      .collect();
    assert_eq!(sql.len(), 3 + 7, "{sql:?}");
    assert!(sql[0].starts_with("CREATE TABLE IF NOT EXISTS workflow_instances"));
    assert!(sql[1].contains("workflow_instances_claim_idx"));
    assert!(sql[2].starts_with("CREATE TABLE IF NOT EXISTS workflow_history"));
    assert!(sql[2].contains("UNIQUE (instance_id, seq)"));
    // 0002: two columns, the claim index replaced, completions, signals and their index.
    assert!(sql[3].contains("ADD COLUMN IF NOT EXISTS waiting"));
    assert!(sql[4].contains("ADD COLUMN IF NOT EXISTS wakeups"));
    assert_eq!(sql[5], "DROP INDEX IF EXISTS workflow_instances_claim_idx");
    assert!(sql[6].contains("workflow_instances_ready_idx") && sql[6].contains("NOT waiting"));
    assert!(sql[7].starts_with("CREATE TABLE IF NOT EXISTS workflow_completions"));
    assert!(sql[7].contains("PRIMARY KEY (instance_id, seq)"));
    assert!(sql[8].starts_with("CREATE TABLE IF NOT EXISTS workflow_signals"));
    assert!(sql[9].contains("workflow_signals_name_idx"));
    assert!(sql.iter().all(|s| !s.contains("--") && !s.ends_with(';')));
  }

  #[test]
  fn the_migrator_has_its_own_tracking_table() {
    let m = migrator();
    assert_eq!(m.table(), MIGRATIONS_TABLE);
    assert_eq!(m.migrations().len(), 2);
    let first = &m.migrations()[0];
    assert_eq!((first.version(), first.is_reversible()), (1, true));
    assert_eq!(first.sql(), SCHEMA);
    let second = &m.migrations()[1];
    assert_eq!((second.version(), second.is_reversible()), (2, true));
    assert_eq!(second.sql(), SCHEMA_2);
  }

  #[test]
  fn wait_complete_and_signal_bind_their_keys() {
    let (store, client) = connected();
    client.script_execute(WAIT, 1);
    assert_eq!(run(store.wait(&lease(), 7, t(5))), Ok(Fenced::Applied));
    let (sql, params) = &client.recorded_executes()[0];
    assert_eq!(sql, WAIT);
    assert_eq!(params[2], SqlValue::I64(3));
    assert_eq!(params[3], SqlValue::I64(7));
    assert!(WAIT.contains("lease_owner = $2") && WAIT.contains("wakeups = $4"));

    let i = WorkflowId::new("i").unwrap();
    let completion = Completion {
      seq: 2,
      kind: EventKind::Activity,
      name: "charge".into(),
      payload: serde_json::json!({ "ok": { "n": 1 } }),
      completed_at: t(6),
    };
    client.script_execute(COMPLETE, 1);
    assert_eq!(run(store.complete(&i, completion.clone())), Ok(true));
    let (sql, params) = &client.recorded_executes()[1];
    assert_eq!(sql, COMPLETE);
    assert_eq!(params[1], SqlValue::I64(2));
    assert_eq!(params[2], SqlValue::Text("activity".into()));
    assert!(COMPLETE.contains("ON CONFLICT (instance_id, seq) DO NOTHING"));
    client.script_execute(COMPLETE, 0);
    assert_eq!(run(store.complete(&i, completion.clone())), Ok(false));

    client.script_query(
      COMPLETION,
      vec![SqlRow::from_pairs([
        ("seq", SqlValue::I64(2)),
        ("kind", SqlValue::Text("activity".into())),
        ("name", SqlValue::Text("charge".into())),
        ("payload", SqlValue::Json(completion.payload.clone())),
        ("completed_at", SqlValue::Timestamp(t(6).0)),
      ])],
    );
    assert_eq!(run(store.completion(&i, 2)), Ok(Some(completion)));
    client.script_query(COMPLETION, vec![]);
    assert_eq!(run(store.completion(&i, 3)), Ok(None));

    client.script_query(
      SIGNAL,
      vec![SqlRow::from_pairs([("number", SqlValue::I64(4))])],
    );
    let sent = store.signal(&i, "approve", serde_json::json!({ "n": 1 }), t(7));
    assert_eq!(run(sent), Ok(Some(4)));
    client.script_query(SIGNAL, vec![]);
    let sent = store.signal(&i, "approve", serde_json::json!({}), t(7));
    assert_eq!(run(sent), Ok(None), "no running instance");
    client.script_query(
      SIGNAL,
      vec![SqlRow::from_pairs([("number", SqlValue::Text("x".into()))])],
    );
    let sent = store.signal(&i, "approve", serde_json::json!({}), t(7));
    assert!(matches!(run(sent), Err(WorkflowError::Sql(_))));

    client.script_query(
      NEXT_SIGNAL,
      vec![SqlRow::from_pairs([
        ("number", SqlValue::I64(4)),
        ("name", SqlValue::Text("approve".into())),
        ("payload", SqlValue::Json(serde_json::json!({ "n": 1 }))),
        ("received_at", SqlValue::Timestamp(t(7).0)),
      ])],
    );
    let next = run(store.next_signal(&i, "approve", 0)).unwrap().unwrap();
    assert_eq!((next.number, next.name.as_str()), (4, "approve"));
    for sql in [COMPLETE, SIGNAL] {
      assert!(
        sql.contains("wakeups + 1") && sql.contains("waiting = false"),
        "{sql}"
      );
    }
    for sql in [ACQUIRE, CLAIM] {
      assert!(
        sql.contains("NOT waiting") && sql.contains("wakeups"),
        "{sql}"
      );
    }
  }

  #[test]
  fn fenced_writes_bind_owner_and_token() {
    let (store, client) = connected();
    client.script_execute(APPEND, 1);
    let event = HistoryEvent {
      seq: 4,
      kind: EventKind::Step,
      name: "charge".into(),
      payload: serde_json::json!({ "ok": { "n": 1 } }),
      recorded_at: t(5),
    };
    assert_eq!(
      run(store.append(&lease(), event, t(15))),
      Ok(Fenced::Applied)
    );
    let (sql, params) = &client.recorded_executes()[0];
    assert_eq!(sql, APPEND);
    assert_eq!(params[0], SqlValue::Text("i".into()));
    assert_eq!(params[1], SqlValue::I64(4));
    assert_eq!(params[2], SqlValue::Text("step".into()));
    assert_eq!(params[4], SqlValue::Text("e".into()));
    assert_eq!(params[5], SqlValue::I64(3));
    assert_eq!(params[6], SqlValue::Timestamp(t(5).0));
    assert_eq!(params[7], SqlValue::Timestamp(t(15).0));

    // Unscripted statements affect 0 rows: the lease is lost.
    assert_eq!(run(store.renew(&lease(), t(6), t(16))), Ok(Fenced::Lost));
    let done = Finish::Failed("boom".into());
    assert_eq!(run(store.finish(&lease(), done, t(7))), Ok(Fenced::Lost));
    let (sql, params) = &client.recorded_executes()[2];
    assert_eq!(sql, FINISH);
    assert_eq!(params[3], SqlValue::Text("failed".into()));
    assert_eq!(params[4], SqlValue::Null);
    assert_eq!(params[5], SqlValue::Text("boom".into()));
    assert_eq!(run(store.release(&lease(), t(8))), Ok(Fenced::Lost));
    for sql in [APPEND, RENEW, FINISH, RELEASE] {
      assert!(
        sql.contains("lease_owner = $") && sql.contains("fencing_token = $"),
        "{sql}"
      );
    }
    assert!(APPEND.contains("lease_expires_at > $7") && FINISH.contains("lease_expires_at > $7"));
  }

  #[test]
  fn rows_decode_into_records_claims_and_events() {
    let (store, client) = connected();
    let instance = SqlRow::from_pairs([
      ("id", SqlValue::Text("i".into())),
      ("name", SqlValue::Text("w".into())),
      ("version", SqlValue::Text("2".into())),
      ("status", SqlValue::Text("running".into())),
      ("input", SqlValue::Json(serde_json::json!({ "n": 1 }))),
      ("output", SqlValue::Null),
      ("error", SqlValue::Null),
      ("lease_owner", SqlValue::Text("e".into())),
      ("lease_expires_at", SqlValue::Timestamp(t(10).0)),
      ("fencing_token", SqlValue::I64(3)),
      ("waiting", SqlValue::Bool(false)),
      ("wakeups", SqlValue::I64(2)),
      ("created_at", SqlValue::Timestamp(t(1).0)),
      ("updated_at", SqlValue::Timestamp(t(2).0)),
    ]);
    client.script_query(GET, vec![instance.clone()]);
    let record = run(store.get(&WorkflowId::new("i").unwrap()))
      .unwrap()
      .unwrap();
    assert_eq!(record.status, WorkflowStatus::Running);
    assert_eq!(record.lease_owner, Some(EngineId::new("e")));
    assert_eq!((record.fencing_token, record.version.as_str()), (3, "2"));
    assert_eq!((record.waiting, record.wakeups), (false, 2));

    client.script_query(ACQUIRE, vec![instance]);
    let owner = EngineId::new("e");
    let claimed = run(store.acquire(&record.id, &owner, t(3), t(13)))
      .unwrap()
      .unwrap();
    assert_eq!(
      claimed.lease,
      Lease {
        expires_at: t(13),
        ..lease()
      }
    );

    client.script_query(
      CLAIM,
      vec![claimed_row(&record, "j", 5), claimed_row(&record, "h", 0)],
    );
    let ids: Vec<String> = run(store.claim("w", &owner, 2, t(3), t(13)))
      .unwrap()
      .into_iter()
      .map(|c| c.lease.instance.to_string())
      .collect();
    assert_eq!(ids, ["h", "j"], "oldest first");

    client.script_query(
      HISTORY,
      vec![SqlRow::from_pairs([
        ("seq", SqlValue::I64(1)),
        ("kind", SqlValue::Text("now".into())),
        ("name", SqlValue::Text("now".into())),
        ("payload", SqlValue::Json(serde_json::json!({ "at": "x" }))),
        ("recorded_at", SqlValue::Timestamp(t(4).0)),
      ])],
    );
    let events = run(store.history(&record.id)).unwrap();
    assert_eq!((events[0].seq, events[0].kind), (1, EventKind::Now));

    client.script_query(
      HISTORY,
      vec![SqlRow::from_pairs([("seq", SqlValue::I64(1))])],
    );
    assert!(matches!(
      run(store.history(&record.id)),
      Err(WorkflowError::Sql(_))
    ));
  }

  fn claimed_row(record: &InstanceRecord, id: &str, created: i64) -> SqlRow {
    SqlRow::from_pairs([
      ("id", SqlValue::Text(id.into())),
      ("name", SqlValue::Text(record.name.clone())),
      ("version", SqlValue::Text(record.version.clone())),
      ("input", SqlValue::Json(record.input.clone())),
      ("fencing_token", SqlValue::I64(1)),
      ("wakeups", SqlValue::I64(0)),
      ("created_at", SqlValue::Timestamp(t(created).0)),
    ])
  }
}
