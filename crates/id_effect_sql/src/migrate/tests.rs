//! The migration engine on [`TestSqlClient`]: statement order, transactions, bookkeeping and
//! errors. Live PostgreSQL behaviour (locking, rollback) is in `tests/migrate.rs`.

use std::error::Error as _;
use std::sync::Arc;
use std::time::Duration;

use id_effect::{Cause, Effect, Exit, need};

use super::{
  DEFAULT_LOCK_KEY, DEFAULT_LOCK_TIMEOUT, DEFAULT_TABLE, LOCK_SQL, MigrateError, MigratedEnv,
  MigrationState, MigrationsApplied, Migrator, layer, run,
};
use crate::client::{SqlClient, TestSqlClient, provide_sql_client};
use crate::error::SqlError;
use crate::row::SqlRow;
use crate::value::SqlValue;

fn exit<A: Send + 'static>(eff: Effect<A, MigrateError>) -> Result<A, MigrateError> {
  match eff.block_on() {
    Exit::Success(a) => Ok(a),
    Exit::Failure(Cause::Fail(e)) => Err(e),
    Exit::Failure(c) => panic!("unexpected cause: {c:?}"),
  }
}

/// A connected client whose advisory lock is free.
fn client() -> TestSqlClient {
  let test = TestSqlClient::new();
  test.script_query(LOCK_SQL, vec![SqlRow::from_pairs([("locked", true)])]);
  exit(test.connect().map_err(MigrateError::from)).unwrap();
  test
}

fn db(test: &TestSqlClient) -> Arc<dyn SqlClient> {
  Arc::new(test.clone())
}

fn migrator() -> Migrator {
  Migrator::from_files([
    (
      "0001_a.up.sql",
      "CREATE TABLE a (id INT); INSERT INTO a VALUES (1)",
    ),
    ("0001_a.down.sql", "DROP TABLE a"),
    ("0002_b.sql", "CREATE TABLE b (id INT)"),
  ])
  .unwrap()
}

fn applied_row(version: i64, checksum: &[u8]) -> SqlRow {
  SqlRow::from_pairs([
    ("version", SqlValue::I64(version)),
    ("description", SqlValue::Text(format!("v{version}"))),
    ("checksum", SqlValue::Bytes(checksum.to_vec())),
    (
      "installed_on",
      SqlValue::Timestamp(jiff::Timestamp::from_second(1_700_000_000).unwrap()),
    ),
  ])
}

/// Scripts the tracking table's content.
fn applied(test: &TestSqlClient, m: &Migrator, rows: Vec<SqlRow>) {
  test.script_query(m.select_applied_sql(), rows);
}

/// Scripts the per-migration re-read (the same answer for every version).
fn recorded(test: &TestSqlClient, m: &Migrator, checksum: Option<&[u8]>) {
  let rows = checksum
    .map(|c| vec![SqlRow::from_pairs([("checksum", c.to_vec())])])
    .unwrap_or_default();
  test.script_query(m.recorded_sql(), rows);
}

fn executed(test: &TestSqlClient) -> Vec<String> {
  test
    .recorded_executes()
    .into_iter()
    .map(|(sql, _)| sql)
    .collect()
}

#[test]
fn defaults_and_builders() {
  let m = migrator();
  assert_eq!(
    (m.table(), m.lock_key(), m.lock_timeout()),
    (DEFAULT_TABLE, DEFAULT_LOCK_KEY, DEFAULT_LOCK_TIMEOUT)
  );
  assert_eq!(m.migrations().len(), 2);
  let m = m
    .with_table("app.schema_migrations")
    .with_lock_key(7)
    .with_lock_timeout(Duration::from_secs(1));
  assert_eq!(
    (m.table(), m.lock_key(), m.lock_timeout()),
    ("app.schema_migrations", 7, Duration::from_secs(1))
  );
  assert!(m.create_table_sql().contains("app.schema_migrations"));
  let _ = Migrator::from_files([("1_x.sql", "")])
    .unwrap()
    .with_table("_t1");
}

#[test]
#[should_panic(expected = "is not a [schema.]table name")]
fn with_table_rejects_an_injection() {
  let _ = migrator().with_table("t; DROP TABLE users");
}

#[test]
fn table_names_are_checked() {
  for bad in ["", "1t", "a.b.c", "a.", "t-1", "\"t\""] {
    assert!(!super::valid_table(bad), "{bad}");
  }
}

#[test]
fn run_applies_pending_migrations_in_order_one_transaction_each() {
  let test = client();
  let m = migrator();
  assert_eq!(exit(m.run_on(db(&test))), Ok(vec![1, 2]));
  let sql = executed(&test);
  assert!(sql[0].starts_with("CREATE TABLE IF NOT EXISTS _id_effect_migrations"));
  assert_eq!(
    sql[1..],
    [
      "CREATE TABLE a (id INT)".to_owned(),
      "INSERT INTO a VALUES (1)".to_owned(),
      m.insert_sql(),
      "CREATE TABLE b (id INT)".to_owned(),
      m.insert_sql(),
    ]
  );
  let (_, row) = &test.recorded_executes()[3];
  assert_eq!(row[0], SqlValue::I64(1));
  assert_eq!(row[1], SqlValue::Text("a".into()));
  assert_eq!(
    row[2],
    SqlValue::Bytes(m.migrations()[0].checksum().to_vec())
  );
  // The table, then one transaction per migration, each under the lock.
  assert_eq!((test.commits(), test.rollbacks()), (3, 0));
  let locks = test
    .recorded_queries()
    .iter()
    .filter(|(sql, p)| sql == LOCK_SQL && p[..] == [SqlValue::I64(DEFAULT_LOCK_KEY)])
    .count();
  assert_eq!(locks, 3);
}

#[test]
fn run_skips_what_is_applied() {
  let test = client();
  let m = migrator();
  applied(
    &test,
    &m,
    vec![applied_row(1, m.migrations()[0].checksum())],
  );
  assert_eq!(exit(m.run_on(db(&test))), Ok(vec![2]));
  assert!(!executed(&test).iter().any(|s| s.contains("TABLE a")));
}

#[test]
fn run_skips_a_migration_another_runner_applied_meanwhile() {
  let test = client();
  let m = Migrator::from_files([("1_only.sql", "CREATE TABLE x (id INT)")]).unwrap();
  recorded(&test, &m, Some(m.migrations()[0].checksum()));
  assert_eq!(exit(m.run_on(db(&test))), Ok(vec![]));
  assert_eq!(executed(&test).len(), 1, "only the tracking table");
  // Recorded meanwhile with another script: drift.
  let test = client();
  recorded(&test, &m, Some(b"other"));
  assert_eq!(
    exit(m.run_on(db(&test))),
    Err(MigrateError::ChecksumMismatch {
      version: 1,
      file: "1_only.sql".into()
    })
  );
  assert_eq!(test.rollbacks(), 1);
}

#[test]
fn drift_and_missing_migrations_stop_everything() {
  let m = migrator();
  let test = client();
  applied(&test, &m, vec![applied_row(1, b"changed")]);
  let drift = MigrateError::ChecksumMismatch {
    version: 1,
    file: "0001_a.up.sql".into(),
  };
  assert_eq!(exit(m.run_on(db(&test))), Err(drift.clone()));
  assert_eq!(exit(m.revert_on(db(&test), 1)), Err(drift.clone()));
  assert_eq!(exit(m.check_on(db(&test))), Err(drift));
  let test = client();
  applied(&test, &m, vec![applied_row(9, b"x")]);
  assert_eq!(
    exit(m.run_on(db(&test))),
    Err(MigrateError::Missing { version: 9 })
  );
  assert_eq!(executed(&test).len(), 1, "nothing applied");
}

#[test]
fn a_taken_lock_times_out() {
  let test = TestSqlClient::new();
  exit(test.connect().map_err(MigrateError::from)).unwrap();
  test.script_query(LOCK_SQL, vec![SqlRow::from_pairs([("locked", false)])]);
  let m = migrator().with_lock_timeout(Duration::ZERO);
  assert_eq!(
    exit(m.run_on(db(&test))),
    Err(MigrateError::LockTimeout {
      key: DEFAULT_LOCK_KEY,
      timeout: Duration::ZERO
    })
  );
  // Polled again after LOCK_POLL, then given up.
  let m = m.with_lock_timeout(Duration::from_millis(150));
  assert!(matches!(
    exit(m.run_on(db(&test))),
    Err(MigrateError::LockTimeout { .. })
  ));
  assert_eq!((test.commits(), test.rollbacks()), (0, 2));
}

#[test]
fn bookkeeping_errors_are_sql_errors() {
  let m = migrator();
  assert_eq!(
    exit(m.run_on(db(&TestSqlClient::new()))),
    Err(MigrateError::Sql(SqlError::NotConnected))
  );
  let test = client();
  test.script_query(LOCK_SQL, vec![SqlRow::from_pairs([("locked", 1i64)])]);
  assert!(matches!(
    exit(m.run_on(db(&test))),
    Err(MigrateError::Sql(SqlError::TypeMismatch { .. }))
  ));
  let test = client();
  applied(&test, &m, vec![SqlRow::from_pairs([("version", 1i64)])]);
  assert!(matches!(
    exit(m.status_on(db(&test))),
    Err(MigrateError::Sql(SqlError::ColumnNotFound { .. }))
  ));
  let test = client();
  test.script_query(
    m.recorded_sql(),
    vec![SqlRow::from_pairs([("checksum", 1i64)])],
  );
  assert!(matches!(
    exit(m.run_on(db(&test))),
    Err(MigrateError::Sql(SqlError::TypeMismatch { .. }))
  ));
  let test = client();
  test.fail_queries("down");
  assert!(matches!(
    exit(m.check_on(db(&test))),
    Err(MigrateError::Sql(SqlError::QueryFailed { .. }))
  ));
}

#[test]
fn status_lists_every_state() {
  let m = Migrator::from_files([
    ("1_a.up.sql", "A"),
    ("1_a.down.sql", "a"),
    ("2_b.sql", "B"),
    ("3_c.sql", "C"),
  ])
  .unwrap();
  let test = client();
  applied(
    &test,
    &m,
    vec![
      applied_row(1, m.migrations()[0].checksum()),
      applied_row(2, b"old"),
      applied_row(4, b"gone"),
    ],
  );
  let all = exit(m.status_on(db(&test))).unwrap();
  let states: Vec<(i64, MigrationState)> = all.iter().map(|s| (s.version, s.state)).collect();
  assert_eq!(
    states,
    [
      (1, MigrationState::Applied),
      (2, MigrationState::Drifted),
      (3, MigrationState::Pending),
      (4, MigrationState::Missing),
    ]
  );
  assert!(all[0].reversible && !all[1].reversible);
  assert_eq!(all[0].checksum_hex(), m.migrations()[0].checksum_hex());
  assert_eq!(all[1].applied_checksum.as_deref(), Some(&b"old"[..]));
  assert!(all[2].applied_at.is_none() && all[3].applied_at.is_some());
  assert_eq!(
    (all[3].description.as_str(), &all[3].checksum[..]),
    ("v4", &b"gone"[..])
  );
  let applied: Vec<bool> = all.iter().map(|s| s.state.is_applied()).collect();
  assert_eq!(applied, [true, true, false, true]);
}

#[test]
fn check_reports_pending_migrations() {
  let m = migrator();
  let test = client();
  assert_eq!(
    exit(m.check_on(db(&test))),
    Err(MigrateError::Pending {
      versions: vec![1, 2]
    })
  );
  let test = client();
  let rows = m
    .migrations()
    .iter()
    .map(|x| applied_row(x.version(), x.checksum()))
    .collect();
  applied(&test, &m, rows);
  assert_eq!(exit(m.check_on(db(&test))), Ok(()));
}

#[test]
fn revert_runs_the_down_scripts_of_the_last_applied_newest_first() {
  let m = Migrator::from_files([
    ("1_a.up.sql", "CREATE TABLE a (id INT)"),
    ("1_a.down.sql", "DROP TABLE a"),
    ("2_b.up.sql", "CREATE TABLE b (id INT)"),
    ("2_b.down.sql", "DROP TABLE b; DROP TYPE bt"),
  ])
  .unwrap();
  let test = client();
  applied(
    &test,
    &m,
    vec![
      applied_row(1, m.migrations()[0].checksum()),
      applied_row(2, m.migrations()[1].checksum()),
    ],
  );
  recorded(&test, &m, Some(b"present"));
  assert_eq!(exit(m.revert_on(db(&test), 0)), Ok(vec![]));
  assert_eq!(exit(m.revert_on(db(&test), 5)), Ok(vec![2, 1]));
  let sql = executed(&test);
  let delete = m.delete_sql();
  let tail: Vec<&str> = sql[sql.len() - 5..].iter().map(String::as_str).collect();
  assert_eq!(
    tail,
    [
      "DROP TABLE b",
      "DROP TYPE bt",
      delete.as_str(),
      "DROP TABLE a",
      delete.as_str()
    ]
  );
  // Reverted meanwhile by another migrator: nothing to do.
  let test = client();
  applied(
    &test,
    &m,
    vec![applied_row(1, m.migrations()[0].checksum())],
  );
  assert_eq!(exit(m.revert_on(db(&test), 1)), Ok(vec![]));
}

#[test]
fn revert_needs_a_down_script() {
  let m = migrator();
  let test = client();
  applied(
    &test,
    &m,
    vec![
      applied_row(1, m.migrations()[0].checksum()),
      applied_row(2, m.migrations()[1].checksum()),
    ],
  );
  assert_eq!(
    exit(m.revert_on(db(&test), 1)),
    Err(MigrateError::MissingDown {
      version: 2,
      file: "0002_b.sql".into()
    })
  );
}

#[test]
fn the_environment_variants_and_the_layer_use_the_service() {
  let m = migrator();
  let test = client();
  let program: Effect<Vec<i64>, MigrateError, MigratedEnv> =
    need::<MigrationsApplied>().map(|applied| applied.0.clone());
  let migrated = program
    .provide_layer(m.layer())
    .provide_layer(provide_sql_client(test.clone()));
  assert_eq!(exit(migrated), Ok(vec![1, 2]));

  let env = || crate::client::sql_client_env(test.clone());
  let rows = m
    .migrations()
    .iter()
    .map(|x| applied_row(x.version(), x.checksum()))
    .collect();
  applied(&test, &m, rows);
  assert_eq!(exit(m.run().provide_all(env())), Ok(vec![]));
  assert_eq!(exit(m.check().provide_all(env())), Ok(()));
  assert_eq!(exit(m.status().provide_all(env())).unwrap().len(), 2);
  assert!(matches!(
    exit(m.revert(1).provide_all(env())),
    Err(MigrateError::MissingDown { version: 2, .. })
  ));
}

#[test]
fn run_and_layer_read_a_directory() {
  let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/migrations");
  let test = client();
  let env = crate::client::sql_client_env(test.clone());
  assert_eq!(exit(run(dir).provide_all(env)), Ok(vec![1, 2, 3]));

  let program: Effect<usize, MigrateError, MigratedEnv> =
    need::<MigrationsApplied>().map(|applied| applied.0.len());
  let built = program
    .provide_layer(layer(dir))
    .provide_layer(provide_sql_client(client()));
  assert_eq!(exit(built), Ok(3));

  let missing = run("/no/such/migrations").provide_all(crate::client::sql_client_env(client()));
  assert!(matches!(exit(missing), Err(MigrateError::Source(_))));
}

#[test]
fn errors_display_and_chain() {
  let db_error = SqlError::QueryFailed {
    sql: String::new(),
    message: "relation \"x\" does not exist".into(),
  };
  let cases = [
    (MigrateError::Source("bad".into()), "migration source: bad"),
    (
      MigrateError::ChecksumMismatch {
        version: 3,
        file: "0003_c.sql".into(),
      },
      "migration 3 (0003_c.sql) changed after it was applied: checksum mismatch",
    ),
    (
      MigrateError::Missing { version: 4 },
      "migration 4 is applied in the database but missing from the source",
    ),
    (
      MigrateError::Pending {
        versions: vec![5, 6],
      },
      "2 pending migration(s): 5, 6",
    ),
    (
      MigrateError::Failed {
        version: 2,
        file: "0002_b.sql".into(),
        error: db_error.clone(),
      },
      "migration 0002_b.sql failed: sql query failed (): relation \"x\" does not exist",
    ),
    (
      MigrateError::LockTimeout {
        key: 1,
        timeout: Duration::from_secs(2),
      },
      "timed out after 2s waiting for the migration lock (advisory lock 1)",
    ),
    (
      MigrateError::MissingDown {
        version: 2,
        file: "0002_b.sql".into(),
      },
      "migration 0002_b.sql cannot be reverted: it has no down script (.down.sql)",
    ),
    (
      MigrateError::Sql(SqlError::NotConnected),
      "migration bookkeeping failed: sql client not connected",
    ),
  ];
  for (err, shown) in &cases {
    assert_eq!(err.to_string(), *shown);
  }
  assert!(cases[4].0.source().is_some() && cases[7].0.source().is_some());
  assert!(cases[0].0.source().is_none());
}
