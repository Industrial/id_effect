//! Savepoints, transaction options, serialization-failure retry and row streaming on
//! `TestSqlClient` and on minimal hand-written clients (no database needed). The PostgreSQL
//! versions of these tests are the `live_postgres_*` ones in `tests/pg.rs`.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use id_effect::{Cause, Exit};
use id_effect::{Effect, Schedule, Stream, die, fail, sleep, succeed, suspend, sync};
use id_effect_sql::{
  FromRow, Isolation, SQLSTATE_DEADLOCK_DETECTED, SqlClient, SqlClientEnv, SqlError, SqlRow,
  SqlTransaction, SqlValue, TestSqlClient, TransactionOptions, is_serialization_failure,
  query_stream, query_stream_as, retry_serialization_failures, savepoint, sql_client_env,
  transaction_with, with_transaction, with_transaction_options,
};
use id_effect_tokio::TokioRuntime;

fn run<A: Send + 'static>(eff: Effect<A, SqlError>) -> Result<A, SqlError> {
  match eff.block_on() {
    Exit::Success(a) => Ok(a),
    Exit::Failure(Cause::Fail(e)) => Err(e),
    Exit::Failure(c) => panic!("unexpected cause: {c:?}"),
  }
}

fn connected() -> TestSqlClient {
  let db = TestSqlClient::new();
  run(db.connect()).unwrap();
  db
}

fn shared(db: &TestSqlClient) -> Arc<dyn SqlClient> {
  Arc::new(db.clone())
}

fn executed(db: &TestSqlClient) -> Vec<String> {
  db.recorded_executes()
    .into_iter()
    .map(|(sql, _)| sql)
    .collect()
}

fn insert(tx: &Arc<dyn SqlTransaction>, n: u32) -> Effect<u64, SqlError> {
  tx.execute(&format!("INSERT {n}"), &[])
}

fn boom<A: Send + 'static>() -> Effect<A, SqlError> {
  fail(SqlError::Unsupported("boom".into()))
}

// ---- savepoints ---------------------------------------------------------------------------------

#[test]
fn savepoint_rolls_back_inner_only() {
  let db = connected();
  let program = with_transaction(shared(&db), |tx| {
    let inner = savepoint(&tx, "line", |sp| insert(&sp, 2).then(boom::<u64>()));
    insert(&tx, 1)
      .then(inner.catch(|e| {
        assert_eq!(e, SqlError::Unsupported("boom".into()));
        succeed(0)
      }))
      .then(insert(&tx, 3))
  });
  run(program).unwrap();
  assert_eq!(executed(&db), ["INSERT 1", "INSERT 3"]);
  assert_eq!((db.commits(), db.rollbacks()), (1, 0));
}

#[test]
fn nested_savepoints_keep_successes_and_undo_failures() {
  let db = connected();
  let program = with_transaction(shared(&db), |tx| {
    savepoint(&tx, "outer", |sp| {
      let kept = savepoint(&sp, "kept", |k| insert(&k, 2));
      let lost = savepoint(&sp, "lost", |l| insert(&l, 3).then(boom::<u64>()));
      insert(&sp, 1)
        .then(kept)
        .then(lost.catch(|_| succeed(0)))
        .map(|_| "done")
    })
  });
  assert_eq!(run(program), Ok("done"));
  assert_eq!(executed(&db), ["INSERT 1", "INSERT 2"]);
}

#[test]
fn a_defect_in_a_savepoint_rolls_back_only_the_savepoint() {
  let db = connected();
  let tx = run(db.begin()).unwrap();
  run(insert(&tx, 1)).unwrap();
  let exit =
    savepoint::<(), SqlError, _, _>(&tx, "d", |sp| insert(&sp, 2).then(die("defect"))).block_on();
  assert!(matches!(exit, Exit::Failure(Cause::Die(_))), "{exit:?}");
  run(tx.commit()).unwrap();
  assert_eq!(executed(&db), ["INSERT 1"]);
}

/// Forks `eff` on a tokio runtime, waits until `started` is set, interrupts it and returns its
/// exit (the setup and checks around it run outside the runtime, with `block_on`).
fn interrupt_once_started<A: Send + 'static>(
  eff: Effect<A, SqlError>,
  started: &AtomicUsize,
) -> Exit<A, SqlError> {
  let rt = tokio::runtime::Runtime::new().expect("tokio");
  rt.block_on(async {
    let h = TokioRuntime::current().expect("tokio").fork(eff);
    while started.load(Ordering::SeqCst) == 0 {
      tokio::time::sleep(Duration::from_millis(1)).await;
    }
    h.interrupt_now();
    h.join_future().await
  })
}

#[test]
fn an_interrupt_in_a_savepoint_rolls_back_only_the_savepoint() {
  let db = connected();
  let tx = run(db.begin()).unwrap();
  run(insert(&tx, 1)).unwrap();
  let entered = Arc::new(AtomicUsize::new(0));
  let flag = Arc::clone(&entered);
  let hung = savepoint::<(), SqlError, _, _>(&tx, "hung", move |sp| {
    insert(&sp, 2)
      .then(sync(move || flag.store(1, Ordering::SeqCst)))
      .then(sleep(Duration::from_secs(3600)))
  });
  let exit = interrupt_once_started(hung, &entered);
  assert!(
    matches!(exit, Exit::Failure(Cause::Interrupt(_))),
    "{exit:?}"
  );
  run(insert(&tx, 3)).unwrap();
  run(tx.commit()).unwrap();
  assert_eq!(executed(&db), ["INSERT 1", "INSERT 3"]);
}

#[test]
fn test_transaction_checks_savepoint_names() {
  let db = connected();
  let tx = run(db.begin()).unwrap();
  for bad in ["", "1st", "a b", "x;DROP TABLE t", &"a".repeat(64)] {
    assert!(
      matches!(run(tx.create_savepoint(bad)), Err(SqlError::Unsupported(_))),
      "{bad:?}"
    );
  }
  run(tx.create_savepoint(&"_".repeat(63))).unwrap();
  for missing in [
    tx.release_savepoint("nope"),
    tx.rollback_to_savepoint("nope"),
  ] {
    assert!(matches!(run(missing), Err(SqlError::TransactionFailed(_))));
  }
  // ROLLBACK TO keeps the savepoint: it can be rolled back to again.
  run(tx.create_savepoint("s")).unwrap();
  run(insert(&tx, 1)).unwrap();
  run(tx.rollback_to_savepoint("s")).unwrap();
  run(insert(&tx, 2)).unwrap();
  run(tx.rollback_to_savepoint("s")).unwrap();
  run(tx.release_savepoint("s")).unwrap();
  assert!(run(tx.release_savepoint("s")).is_err());
  run(tx.commit()).unwrap();
  assert!(executed(&db).is_empty());
  assert!(run(tx.create_savepoint("late")).is_err());
}

/// A transaction that records the SQL it is given, failing statements that start with one of
/// `fail_on` (exercises the trait's default savepoint methods and `begin_with`).
#[derive(Default)]
struct Recorder {
  sql: Arc<Mutex<Vec<String>>>,
  fail_on: Vec<&'static str>,
}

impl Recorder {
  fn failing(fail_on: Vec<&'static str>) -> Arc<Self> {
    Arc::new(Self {
      fail_on,
      ..Self::default()
    })
  }

  fn sql(&self) -> Vec<String> {
    self.sql.lock().unwrap().clone()
  }

  /// Records `sql` when the effect runs (not when it is built).
  fn record(&self, sql: &str) -> Effect<u64, SqlError> {
    let refuse = self.fail_on.iter().any(|p| sql.starts_with(p));
    let (log, sql) = (Arc::clone(&self.sql), sql.to_owned());
    suspend(move || {
      log.lock().unwrap().push(sql.clone());
      if refuse {
        fail(SqlError::QueryFailed {
          sql,
          message: "refused".into(),
        })
      } else {
        succeed(0)
      }
    })
  }
}

impl SqlTransaction for Recorder {
  fn query(&self, sql: &str, _: &[SqlValue]) -> Effect<Vec<SqlRow>, SqlError> {
    self.record(sql).map(|_| Vec::new())
  }

  fn execute(&self, sql: &str, _: &[SqlValue]) -> Effect<u64, SqlError> {
    self.record(sql)
  }

  fn commit(&self) -> Effect<(), SqlError> {
    self.record("COMMIT").map(|_| ())
  }

  fn rollback(&self) -> Effect<(), SqlError> {
    self.record("ROLLBACK").map(|_| ())
  }
}

/// `sp_<label>_<n>` → `sp_<label>_N`.
fn generic(sql: &[String]) -> Vec<String> {
  sql
    .iter()
    .map(|s| match s.rsplit_once('_') {
      Some((head, n)) if n.chars().all(|c| c.is_ascii_digit()) => format!("{head}_N"),
      _ => s.clone(),
    })
    .collect()
}

#[test]
fn default_savepoint_statements_use_a_sanitised_generated_name() {
  let rec = Recorder::failing(vec![]);
  let tx: Arc<dyn SqlTransaction> = rec.clone();
  run(savepoint(&tx, "Line #1; DROP TABLE x", |sp| {
    sp.execute("WORK", &[])
  }))
  .unwrap();
  let _ = run(savepoint(&tx, "", |_| boom::<()>()));
  assert_eq!(
    generic(&rec.sql()),
    [
      "SAVEPOINT sp_line1droptablex_N",
      "WORK",
      "RELEASE SAVEPOINT sp_line1droptablex_N",
      "SAVEPOINT sp__N",
      "ROLLBACK TO SAVEPOINT sp__N",
      "RELEASE SAVEPOINT sp__N",
    ]
  );
  let long = "x".repeat(200);
  run(savepoint(&tx, &long, |_| succeed::<(), SqlError, _>(()))).unwrap();
  let name = rec.sql().last().unwrap().clone();
  assert!(name.len() <= "RELEASE SAVEPOINT ".len() + 63, "{name}");
  for bad in [
    tx.create_savepoint("a-b"),
    tx.release_savepoint("a-b"),
    tx.rollback_to_savepoint("a-b"),
  ] {
    assert!(matches!(run(bad), Err(SqlError::Unsupported(_))));
  }
}

#[test]
fn failed_savepoint_create_and_release_are_typed_errors() {
  let rec = Recorder::failing(vec!["SAVEPOINT"]);
  let tx: Arc<dyn SqlTransaction> = rec.clone();
  let ran = Arc::new(AtomicUsize::new(0));
  let seen = Arc::clone(&ran);
  let err = run(savepoint(&tx, "a", move |_| {
    sync(move || {
      seen.fetch_add(1, Ordering::SeqCst);
    })
  }))
  .unwrap_err();
  assert!(matches!(err, SqlError::QueryFailed { .. }));
  assert_eq!(ran.load(Ordering::SeqCst), 0);

  // A failed RELEASE fails the scope, which then rolls back to the savepoint.
  let rec = Recorder::failing(vec!["RELEASE"]);
  let tx: Arc<dyn SqlTransaction> = rec.clone();
  assert!(run(savepoint(&tx, "b", |sp| sp.execute("WORK", &[]))).is_err());
  assert_eq!(
    generic(&rec.sql()),
    [
      "SAVEPOINT sp_b_N",
      "WORK",
      "RELEASE SAVEPOINT sp_b_N",
      "ROLLBACK TO SAVEPOINT sp_b_N",
      "RELEASE SAVEPOINT sp_b_N",
    ]
  );
}

// ---- isolation levels and options ----------------------------------------------------------------

#[test]
fn transaction_options_render_set_transaction() {
  assert_eq!(TransactionOptions::new().set_transaction_sql(), None);
  assert_eq!(TransactionOptions::default(), TransactionOptions::new());
  for (level, sql) in [
    (Isolation::ReadUncommitted, "READ UNCOMMITTED"),
    (Isolation::ReadCommitted, "READ COMMITTED"),
    (Isolation::RepeatableRead, "REPEATABLE READ"),
    (Isolation::Serializable, "SERIALIZABLE"),
  ] {
    assert_eq!(level.as_sql(), sql);
    let opts = TransactionOptions::from(level);
    assert_eq!(opts.isolation(), Some(level));
    assert_eq!(
      opts.set_transaction_sql(),
      Some(format!("SET TRANSACTION ISOLATION LEVEL {sql}"))
    );
  }
  let ro = TransactionOptions::new().with_read_only();
  assert!(ro.read_only() && !ro.deferrable() && ro.isolation().is_none());
  assert_eq!(
    ro.set_transaction_sql().as_deref(),
    Some("SET TRANSACTION READ ONLY")
  );
  let all = ro.with_deferrable().with_isolation(Isolation::Serializable);
  assert!(all.deferrable());
  assert_eq!(
    all.set_transaction_sql().as_deref(),
    Some("SET TRANSACTION ISOLATION LEVEL SERIALIZABLE, READ ONLY, DEFERRABLE")
  );
}

#[test]
fn isolation_level_is_applied() {
  let db = connected();
  let n = run(with_transaction_options(
    shared(&db),
    Isolation::Serializable,
    |tx| insert(&tx, 1),
  ))
  .unwrap();
  assert_eq!(n, 0);
  assert_eq!(
    executed(&db),
    ["SET TRANSACTION ISOLATION LEVEL SERIALIZABLE", "INSERT 1"]
  );

  // The environment form, and defaults that add no statement.
  let db = connected();
  let eff: Effect<u64, SqlError, SqlClientEnv> =
    transaction_with(TransactionOptions::new().with_read_only(), |tx| {
      tx.execute("SELECT 1", &[]).narrow()
    });
  run(eff.provide_all(sql_client_env(db.clone()))).unwrap();
  let eff: Effect<u64, SqlError, SqlClientEnv> =
    transaction_with(TransactionOptions::new(), |tx| {
      tx.execute("SELECT 2", &[]).narrow()
    });
  run(eff.provide_all(sql_client_env(db.clone()))).unwrap();
  assert_eq!(
    executed(&db),
    ["SET TRANSACTION READ ONLY", "SELECT 1", "SELECT 2"]
  );
  assert_eq!(db.commits(), 2);
}

/// A client whose transactions are [`Recorder`]s refusing `SET TRANSACTION`.
struct RefusesOptions(Arc<Recorder>);

impl SqlClient for RefusesOptions {
  fn connect(&self) -> Effect<(), SqlError> {
    succeed(())
  }

  fn query(&self, _: &str, _: &[SqlValue]) -> Effect<Vec<SqlRow>, SqlError> {
    succeed(vec![SqlRow::from_pairs([("n", 1i64)])])
  }

  fn execute(&self, _: &str, _: &[SqlValue]) -> Effect<u64, SqlError> {
    succeed(0)
  }

  fn begin(&self) -> Effect<Arc<dyn SqlTransaction>, SqlError> {
    succeed(Arc::clone(&self.0) as Arc<dyn SqlTransaction>)
  }
}

#[test]
fn a_refused_set_transaction_rolls_back_and_fails_the_begin() {
  let rec = Recorder::failing(vec!["SET TRANSACTION"]);
  let client = RefusesOptions(Arc::clone(&rec));
  let err = run(client.begin_with(Isolation::RepeatableRead.into()))
    .err()
    .unwrap();
  assert!(matches!(err, SqlError::QueryFailed { .. }));
  assert_eq!(
    rec.sql(),
    [
      "SET TRANSACTION ISOLATION LEVEL REPEATABLE READ",
      "ROLLBACK"
    ]
  );
  assert!(run(client.begin_with(TransactionOptions::new())).is_ok());
  // The trait's default `query_stream` collects `query` on the first pull.
  let rows = run(client.query_stream("ANY", &[]).run_collect()).unwrap();
  assert_eq!(rows[0].get::<i64>("n"), Ok(1));
}

// ---- serialization failures ----------------------------------------------------------------------

#[test]
fn serialization_failures_are_recognised_by_sqlstate() {
  let tagged = |code: &str| SqlError::QueryFailed {
    sql: "COMMIT".into(),
    message: format!("could not serialize (SQLSTATE {code})"),
  };
  assert!(is_serialization_failure(&tagged("40001")));
  assert!(is_serialization_failure(&tagged(
    SQLSTATE_DEADLOCK_DETECTED
  )));
  assert!(!is_serialization_failure(&tagged("23505")));
  assert!(is_serialization_failure(&SqlError::TransactionFailed(
    "x (SQLSTATE 40001)".into()
  )));
  for other in [
    SqlError::NotConnected,
    SqlError::Decode("(SQLSTATE 40001)".into()),
    SqlError::TransactionFailed("no state".into()),
    SqlError::TransactionFailed("(SQLSTATE 40001".into()),
  ] {
    assert!(!is_serialization_failure(&other), "{other:?}");
  }
}

fn post(client: Arc<dyn SqlClient>, attempts: Arc<AtomicUsize>) -> Effect<u64, SqlError> {
  with_transaction_options(client, Isolation::Serializable, move |tx| {
    attempts.fetch_add(1, Ordering::SeqCst);
    insert(&tx, 1)
  })
}

#[test]
fn serialization_failures_are_retried_as_the_schedule_allows() {
  let db = connected();
  db.fail_commits_with_serialization_failure(2);
  let (client, attempts) = (shared(&db), Arc::new(AtomicUsize::new(0)));
  let tries = Arc::clone(&attempts);
  let eff = retry_serialization_failures(Schedule::recurs(3), move || {
    post(Arc::clone(&client), Arc::clone(&tries))
  });
  assert_eq!(run(eff), Ok(0));
  assert_eq!(attempts.load(Ordering::SeqCst), 3);
  assert_eq!((db.commits(), db.rollbacks()), (1, 2));

  // The schedule gives up: the last serialization failure is returned.
  db.fail_commits_with_serialization_failure(5);
  let (client, attempts) = (shared(&db), Arc::new(AtomicUsize::new(0)));
  let tries = Arc::clone(&attempts);
  let err = run(retry_serialization_failures(
    Schedule::recurs(1),
    move || post(Arc::clone(&client), Arc::clone(&tries)),
  ))
  .unwrap_err();
  assert!(is_serialization_failure(&err), "{err:?}");
  assert_eq!(attempts.load(Ordering::SeqCst), 2);
}

#[test]
fn other_failures_are_not_retried() {
  let attempts = Arc::new(AtomicUsize::new(0));
  let tries = Arc::clone(&attempts);
  let err = run(retry_serialization_failures(
    Schedule::recurs(5),
    move || {
      tries.fetch_add(1, Ordering::SeqCst);
      boom::<()>()
    },
  ))
  .unwrap_err();
  assert_eq!(err, SqlError::Unsupported("boom".into()));
  assert_eq!(attempts.load(Ordering::SeqCst), 1);
}

// ---- streaming ----------------------------------------------------------------------------------

fn numbered(n: i64) -> Vec<SqlRow> {
  (1..=n)
    .map(|i| SqlRow::from_pairs([("id", SqlValue::I64(i)), ("name", format!("r{i}").into())]))
    .collect()
}

#[test]
fn query_stream_serves_rows_in_requested_chunks_and_records_the_query() {
  let db = connected();
  db.script_query("SELECT", numbered(5));
  let stream = db.query_stream("SELECT", &[SqlValue::I64(9)]);
  assert!(
    db.recorded_queries().is_empty(),
    "nothing runs before a pull"
  );
  let (chunk, rest) = run(stream.next_chunk_with_size(2)).unwrap().unwrap();
  assert_eq!(chunk.len(), 2);
  assert_eq!(db.open_streams(), 1);
  assert_eq!(
    db.recorded_queries(),
    [("SELECT".to_owned(), vec![SqlValue::I64(9)])]
  );
  let ids: Vec<i64> = run(rest.run_collect())
    .unwrap()
    .iter()
    .map(|r| r.get::<i64>("id").unwrap())
    .collect();
  assert_eq!(ids, [3, 4, 5]);
  assert_eq!(db.open_streams(), 0);
  // No rows: an empty stream.
  assert!(
    run(db.query_stream("NONE", &[]).run_collect())
      .unwrap()
      .is_empty()
  );
  assert_eq!(db.open_streams(), 0);
}

#[test]
fn dropping_a_query_stream_closes_it() {
  let db = connected();
  db.script_query("SELECT", numbered(100));
  let eff: Stream<SqlRow, SqlError, SqlClientEnv> = query_stream("SELECT", vec![]);
  let first = run(
    eff
      .take(3)
      .run_collect()
      .provide_all(sql_client_env(db.clone())),
  )
  .unwrap();
  assert_eq!(first.len(), 3);
  assert_eq!(db.open_streams(), 0);
  let (_, rest) = run(db.query_stream("SELECT", &[]).next_chunk())
    .unwrap()
    .unwrap();
  assert_eq!(db.open_streams(), 1);
  drop(rest);
  assert_eq!(db.open_streams(), 0);
}

#[test]
fn query_stream_failures_fail_the_stream() {
  let db = TestSqlClient::new();
  assert_eq!(
    run(db.query_stream("SELECT", &[]).run_collect()).unwrap_err(),
    SqlError::NotConnected
  );
  run(db.connect()).unwrap();
  db.fail_queries("down");
  let eff: Stream<SqlRow, SqlError, SqlClientEnv> = query_stream("SELECT", vec![]);
  let err = run(eff.run_collect().provide_all(sql_client_env(db.clone()))).unwrap_err();
  assert!(matches!(err, SqlError::QueryFailed { .. }));
  assert_eq!(db.open_streams(), 0);
}

#[derive(Debug, PartialEq, FromRow)]
struct Named {
  id: i64,
  name: String,
}

#[test]
fn query_stream_as_decodes_each_row() {
  let db = connected();
  db.script_query("SELECT", numbered(3));
  let eff: Stream<Named, SqlError, SqlClientEnv> = query_stream_as("SELECT", vec![]);
  let rows = run(eff.run_collect().provide_all(sql_client_env(db.clone()))).unwrap();
  assert_eq!(
    rows[2],
    Named {
      id: 3,
      name: "r3".into()
    }
  );
  let eff: Stream<(bool,), SqlError, SqlClientEnv> = query_stream_as("SELECT", vec![]);
  let err = run(eff.run_collect().provide_all(sql_client_env(db.clone()))).unwrap_err();
  assert!(matches!(err, SqlError::TypeMismatch { .. }), "{err:?}");
  assert_eq!(db.open_streams(), 0);
}

#[test]
fn interrupting_a_stream_consumer_closes_the_stream() {
  let db = connected();
  db.script_query("SELECT", numbered(1000));
  let seen = Arc::new(AtomicUsize::new(0));
  let (count, probe) = (Arc::clone(&seen), db.clone());
  let consumer = db.query_stream("SELECT", &[]).run_for_each(move |_| {
    count.fetch_add(1, Ordering::SeqCst);
    assert_eq!(probe.open_streams(), 1);
    sleep(Duration::from_secs(3600))
  });
  let exit = interrupt_once_started(consumer, &seen);
  assert!(
    matches!(exit, Exit::Failure(Cause::Interrupt(_))),
    "{exit:?}"
  );
  assert_eq!(db.open_streams(), 0);
  assert_eq!(seen.load(Ordering::SeqCst), 1);
}
