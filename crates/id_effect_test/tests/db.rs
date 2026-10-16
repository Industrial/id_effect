//! Per-test database isolation (feature `sql`).
//!
//! - Rollback mode (`isolated_db()`): on the in-memory `TestSqlClient` and on SQLite files (no
//!   Docker), and on PostgreSQL — every exit rolls back, two tests writing the same row never
//!   see each other, a failing statement leaves the transaction usable, nested transactions are
//!   savepoints.
//! - Clone mode (`TemplateDb`): PostgreSQL only — committed data in a database of its own,
//!   dropped afterwards, the template set up once.
//!
//! Without Docker the PostgreSQL tests print `SKIP` and pass, unless `ID_EFFECT_REQUIRE_DOCKER=1`.

#![cfg(feature = "sql")]

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use id_effect::{Cause, Effect, Exit, caps, die, fail, from_async, need, sleep, succeed};
use id_effect_sql::pg::{PgPoolConfig, PgSqlClient};
use id_effect_sql::sqlite::{SqlitePoolConfig, SqliteSqlClient};
use id_effect_sql::{
  Isolation, SqlClient, SqlClientService, SqlError, SqlTransactionService, TestSqlClient, execute,
  params, provide_sql_client, query_one_as, savepoint, transaction, transaction_with,
};
use id_effect_test::{
  ContainerError, IsolatedDatabase, Postgres, Shared, TemplateDb, TransactionSqlClient,
  isolated_db, require_docker,
};
use id_effect_tokio::TokioRuntime;

#[derive(Clone, Debug, PartialEq)]
enum TestError {
  Container(ContainerError),
  Sql(SqlError),
}

impl From<ContainerError> for TestError {
  fn from(e: ContainerError) -> Self {
    Self::Container(e)
  }
}

impl From<SqlError> for TestError {
  fn from(e: SqlError) -> Self {
    Self::Sql(e)
  }
}

async fn exit<A: Send + 'static, E: Send + 'static>(eff: Effect<A, E>) -> Exit<A, E> {
  TokioRuntime::current().expect("tokio").run(eff).await
}

async fn run<A: Send + 'static>(eff: Effect<A, TestError>) -> Result<A, TestError> {
  match exit(eff).await {
    Exit::Success(a) => Ok(a),
    Exit::Failure(Cause::Fail(e)) => Err(e),
    Exit::Failure(c) => panic!("unexpected cause: {c:?}"),
  }
}

type Db = caps!(SqlClientService);

/// `SELECT COUNT(*) FROM table`.
fn count(table: &str) -> Effect<i64, SqlError, Db> {
  query_one_as::<(i64,), Db, _>(format!("SELECT COUNT(*) AS n FROM {table}"), vec![]).map(|(n,)| n)
}

fn sql<A: Send + 'static>(eff: Effect<A, SqlError, Db>) -> Effect<A, TestError, Db> {
  eff.map_err(TestError::from)
}

/// A connected in-memory test client.
fn test_client() -> TestSqlClient {
  let db = TestSqlClient::new();
  assert!(matches!(db.connect().block_on(), Exit::Success(())));
  db
}

// ---------------------------------------------------------------------------------------------
// Rollback mode, without a server
// ---------------------------------------------------------------------------------------------

#[test]
fn a_successful_test_is_rolled_back() {
  let db = test_client();
  let insert: Effect<u64, SqlError, Db> = execute("INSERT INTO t VALUES (1)", vec![]);
  let exit = insert
    .provide_layer(provide_sql_client(db.clone()).provide_to(isolated_db()))
    .block_on();
  assert!(matches!(exit, Exit::Success(0)), "{exit:?}");
  assert_eq!(
    (db.commits(), db.rollbacks(), db.open_transactions()),
    (0, 1, 0)
  );
  assert!(db.recorded_executes().is_empty(), "nothing was committed");
}

#[test]
fn a_failed_or_dead_test_is_rolled_back() {
  let db = test_client();
  let failing: Effect<u64, SqlError, Db> =
    execute("INSERT INTO t VALUES (1)", vec![]).flat_map(|_| fail(SqlError::RowNotFound));
  let exit = failing
    .provide_layer(provide_sql_client(db.clone()).provide_to(isolated_db()))
    .block_on();
  assert!(matches!(
    exit,
    Exit::Failure(Cause::Fail(SqlError::RowNotFound))
  ));

  let dying: Effect<u64, SqlError, Db> =
    execute("INSERT INTO t VALUES (1)", vec![]).flat_map(|_| die("boom"));
  let exit = dying
    .provide_layer(isolated_db())
    .provide_layer(provide_sql_client(db.clone()))
    .block_on();
  assert!(matches!(exit, Exit::Failure(Cause::Die(_))), "{exit:?}");
  assert_eq!(
    (db.commits(), db.rollbacks(), db.open_transactions()),
    (0, 2, 0)
  );
}

#[test]
fn a_failed_begin_fails_the_build() {
  let db = TestSqlClient::new(); // never connected: BEGIN fails
  let program: Effect<u64, SqlError, Db> = execute("SELECT 1", vec![]);
  let exit = program
    .provide_layer(provide_sql_client(db).provide_to(isolated_db()))
    .block_on();
  assert!(matches!(
    exit,
    Exit::Failure(Cause::Fail(SqlError::NotConnected))
  ));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_interrupted_test_is_rolled_back() {
  let db = TestSqlClient::new();
  assert!(matches!(exit(db.connect()).await, Exit::Success(())));
  let started = Arc::new(AtomicBool::new(false));
  let flag = Arc::clone(&started);
  let forever: Effect<u64, SqlError, Db> =
    execute("INSERT INTO t VALUES (1)", vec![]).flat_map(move |_| {
      flag.store(true, Ordering::SeqCst);
      sleep(Duration::from_secs(3600)).map(|()| 0)
    });
  let watch = Arc::clone(&started);
  let inserted = from_async(move || async move {
    while !watch.load(Ordering::SeqCst) {
      tokio::time::sleep(Duration::from_millis(5)).await;
    }
    Ok(1)
  });
  let raced = forever
    .provide_layer(provide_sql_client(db.clone()).provide_to(isolated_db()))
    .race_first(inserted);
  assert!(matches!(exit(raced).await, Exit::Success(1)));
  assert_eq!(
    (db.commits(), db.rollbacks(), db.open_transactions()),
    (0, 1, 0)
  );
}

#[test]
fn the_transaction_client_runs_everything_in_its_transaction() {
  let db = test_client();
  let Exit::Success(tx) = db.begin().block_on() else {
    panic!("begin")
  };
  let client = TransactionSqlClient::new(Arc::clone(&tx));
  assert!(Arc::ptr_eq(client.transaction(), &tx));
  assert!(format!("{client:?}").contains("TransactionSqlClient"));
  assert!(matches!(client.connect().block_on(), Exit::Success(())));
  assert!(matches!(
    client.execute("UPDATE t SET n = 1", &[]).block_on(),
    Exit::Success(0)
  ));
  assert!(
    matches!(client.query("SELECT n FROM t", &[]).block_on(), Exit::Success(rows) if rows.is_empty())
  );
  assert!(matches!(client.close().block_on(), Exit::Success(())));
  assert!(matches!(tx.commit().block_on(), Exit::Success(())));
  // The statements committed with the outer transaction (each under its own savepoint).
  let executed: Vec<String> = db.recorded_executes().into_iter().map(|(s, _)| s).collect();
  assert!(
    executed.contains(&"UPDATE t SET n = 1".to_owned()),
    "{executed:?}"
  );
  assert_eq!(db.recorded_queries().len(), 1);
}

/// A SQLite database file under the temp dir, deleted (with its WAL files) on drop.
struct TempDb(PathBuf);

impl TempDb {
  fn new(name: &str) -> Self {
    let db = Self(std::env::temp_dir().join(format!(
      "id_effect_test_db_{name}_{}.sqlite",
      std::process::id()
    )));
    db.remove();
    db
  }

  fn path(&self) -> &Path {
    &self.0
  }

  fn remove(&self) {
    for suffix in ["", "-wal", "-shm"] {
      let mut path = self.0.clone().into_os_string();
      path.push(suffix);
      let _ = std::fs::remove_file(path);
    }
  }
}

impl Drop for TempDb {
  fn drop(&mut self) {
    self.remove();
  }
}

/// A SQLite file with a committed `accounts` table holding one seed row.
async fn sqlite_accounts(name: &str) -> (TempDb, SqlitePoolConfig) {
  let file = TempDb::new(name);
  let mut config = SqlitePoolConfig::file(file.path());
  // Opening a WAL database file syncs it several times: generous on a loaded CI machine.
  config.acquire_timeout = Duration::from_secs(120);
  config.busy_timeout = Duration::from_secs(60);
  let schema: Effect<u64, SqlError, Db> = execute(
    "CREATE TABLE accounts (id INTEGER PRIMARY KEY, name TEXT NOT NULL UNIQUE)",
    vec![],
  )
  .flat_map(|_| execute("INSERT INTO accounts VALUES (0, 'seed')", vec![]));
  let schema = sql(schema).provide_layer(SqliteSqlClient::layer(config.clone()));
  run(schema).await.expect("schema");
  (file, config)
}

/// One test: writes account 1 (`alice`), returns what it sees.
fn write_alice() -> Effect<i64, TestError, Db> {
  sql(execute("INSERT INTO accounts VALUES (1, 'alice')", vec![]).flat_map(|_| count("accounts")))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sqlite_tests_writing_the_same_row_do_not_see_each_other() {
  let (_file, config) = sqlite_accounts("same_row").await;
  for _test in 0..2 {
    let isolated = SqliteSqlClient::layer(config.clone()).provide_to(isolated_db());
    assert_eq!(run(write_alice().provide_layer(isolated)).await, Ok(2));
  }
  let committed = sql(count("accounts")).provide_layer(SqliteSqlClient::layer(config));
  assert_eq!(
    run(committed).await,
    Ok(1),
    "only the seed row is committed"
  );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sqlite_a_failing_statement_keeps_the_test_transaction_usable() {
  let (_file, config) = sqlite_accounts("failing").await;
  let program = write_alice()
    .flat_map(|_| {
      // A duplicate: the test expects the constraint violation.
      sql(execute("INSERT INTO accounts VALUES (2, 'alice')", vec![]))
        .map(|_| false)
        .catch(|_| succeed(true))
    })
    .flat_map(|violated| {
      sql(execute("INSERT INTO accounts VALUES (3, 'bob')", vec![]).flat_map(|_| count("accounts")))
        .map(move |n| (violated, n))
    });
  let isolated = SqliteSqlClient::layer(config).provide_to(isolated_db());
  assert_eq!(run(program.provide_layer(isolated)).await, Ok((true, 3)));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sqlite_nested_transactions_are_savepoints() {
  let (_file, config) = sqlite_accounts("nested").await;
  type Env = caps!(SqlClientService, SqlTransactionService);
  // Committed nested transaction: kept (in the test transaction).
  let kept: Effect<u64, SqlError, Env> = transaction(|tx| {
    tx.execute("INSERT INTO accounts VALUES (1, 'kept')", &[])
      .narrow()
  });
  // Failed nested transaction: rolled back to its savepoint.
  let undone: Effect<u64, SqlError, Env> = transaction(|tx| {
    tx.execute("INSERT INTO accounts VALUES (2, 'undone')", &[])
      .flat_map(|_| fail(SqlError::RowNotFound))
      .narrow()
  });
  // Options are ignored inside the test transaction.
  let serializable: Effect<u64, SqlError, Env> = transaction_with(Isolation::Serializable, |tx| {
    tx.execute("INSERT INTO accounts VALUES (3, 'serializable')", &[])
      .narrow()
  });
  // A finished nested transaction refuses further work.
  let finished: Effect<Vec<bool>, SqlError, Env> = need::<SqlClientService>().flat_map(|client| {
    client
      .begin()
      .flat_map(|tx| {
        let inner = Arc::clone(&tx);
        let part = savepoint(&tx, "part", move |sp| {
          sp.execute("INSERT INTO accounts VALUES (4, 'part')", &[])
            .flat_map(|_| fail::<u64, SqlError, _>(SqlError::RowNotFound))
        })
        .map(|_| ())
        .catch(|_| succeed(()));
        part.flat_map(move |()| {
          let after = Arc::clone(&inner);
          inner.commit().flat_map(move |()| {
            let checks = [
              after.commit(),
              after.rollback(),
              after.execute("SELECT 1", &[]).map(|_| ()),
              after.query("SELECT 1", &[]).map(|_| ()),
              after.create_savepoint("s"),
              after.release_savepoint("s"),
              after.rollback_to_savepoint("s"),
            ];
            checks.into_iter().fold(succeed(Vec::new()), |acc, check| {
              acc.flat_map(move |mut v| {
                check.exit::<SqlError>().map(move |e| {
                  v.push(matches!(
                    e,
                    Exit::Failure(Cause::Fail(SqlError::TransactionFailed(_)))
                  ));
                  v
                })
              })
            })
          })
        })
      })
      .narrow()
  });
  // The capability is the test transaction itself.
  let through_tx: Effect<u64, SqlError, Env> = need::<SqlTransactionService>().flat_map(|tx| {
    tx.execute("INSERT INTO accounts VALUES (5, 'tx')", &[])
      .narrow()
  });
  let rolled_back: Effect<u64, SqlError, Env> = transaction(|tx| {
    let undo = Arc::clone(&tx);
    tx.execute("INSERT INTO accounts VALUES (6, 'manual')", &[])
      .flat_map(move |_| undo.rollback())
      .map(|()| 0)
      .narrow()
  });
  let program = kept
    .flat_map(|_| undone.catch(|_| succeed(0)))
    .flat_map(|_| serializable)
    .flat_map(|_| finished)
    .flat_map(|checks| through_tx.map(move |_| checks))
    .flat_map(|mut checks| {
      // The bracket's commit then finds the transaction finished.
      rolled_back.exit::<SqlError>().map(move |exit| {
        checks.push(matches!(
          exit,
          Exit::Failure(Cause::Fail(SqlError::TransactionFailed(_)))
        ));
        checks
      })
    })
    .flat_map(|checks| {
      query_one_as::<(i64,), Env, _>("SELECT COUNT(*) FROM accounts", vec![])
        .map(move |(n,)| (checks, n))
    })
    .map_err(TestError::from);
  let isolated = SqliteSqlClient::layer(config.clone()).provide_to(isolated_db());
  let (checks, seen) = run(program.provide_layer(isolated)).await.expect("program");
  assert_eq!(checks, vec![true; 8], "every call after the end fails");
  // seed + kept + serializable + tx; `undone`, `part` and `manual` were rolled back.
  assert_eq!(seen, 4);
  let committed = sql(count("accounts")).provide_layer(SqliteSqlClient::layer(config));
  assert_eq!(run(committed).await, Ok(1));
}

// ---------------------------------------------------------------------------------------------
// PostgreSQL: rollback mode
// ---------------------------------------------------------------------------------------------

static PG: Shared<Postgres> = Shared::new(Postgres::new);

/// Creates (and commits) `table` with a seed row on the shared server.
async fn pg_table(table: &'static str) {
  let ddl = format!("CREATE TABLE {table} (id BIGINT PRIMARY KEY, name TEXT NOT NULL UNIQUE)");
  let seed = format!("INSERT INTO {table} VALUES (0, 'seed')");
  let setup: Effect<u64, TestError, Db> =
    sql(execute(ddl, vec![]).flat_map(move |_| execute(seed, vec![])));
  run(setup.provide_layer(PG.sql_layer()))
    .await
    .expect("table");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn pg_concurrent_tests_writing_the_same_table_do_not_see_each_other() {
  if !require_docker() {
    return;
  }
  pg_table("rollback_accounts").await;
  let barrier = Arc::new(tokio::sync::Barrier::new(2));
  let test = |id: i64| {
    let barrier = Arc::clone(&barrier);
    let insert = sql(execute(
      "INSERT INTO rollback_accounts VALUES ($1, $2)",
      params![id, format!("user-{id}")],
    ));
    // Both have written before either counts.
    let wait = from_async(move || async move {
      barrier.wait().await;
      Ok::<_, TestError>(())
    });
    let program = insert
      .flat_map(move |_| wait)
      .flat_map(|()| sql(count("rollback_accounts")));
    run(program.provide_layer(PG.sql_layer().provide_to(isolated_db())))
  };
  let (a, b) = tokio::join!(test(1), test(2));
  assert_eq!((a, b), (Ok(2), Ok(2)), "each sees the seed and its own row");
  // The same row, written by a later test: no conflict, nothing left behind.
  let again = sql(execute(
    "INSERT INTO rollback_accounts VALUES (1, 'user-1')",
    vec![],
  ))
  .flat_map(|_| sql(count("rollback_accounts")));
  let again = run(again.provide_layer(PG.sql_layer().provide_to(isolated_db()))).await;
  assert_eq!(again, Ok(2));
  let committed = run(sql(count("rollback_accounts")).provide_layer(PG.sql_layer())).await;
  assert_eq!(committed, Ok(1));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pg_a_failing_statement_keeps_the_test_transaction_usable() {
  if !require_docker() {
    return;
  }
  pg_table("failing_accounts").await;
  // PostgreSQL aborts a transaction on its first error: the statement savepoint prevents it.
  let program = sql(execute(
    "INSERT INTO failing_accounts VALUES (0, 'duplicate')",
    vec![],
  ))
  .map(|_| false)
  .catch(|e| succeed(matches!(e, TestError::Sql(SqlError::QueryFailed { .. }))))
  .flat_map(|violated| sql(count("failing_accounts")).map(move |n| (violated, n)));
  let result = run(program.provide_layer(PG.sql_layer().provide_to(isolated_db()))).await;
  assert_eq!(result, Ok((true, 1)));
}

// ---------------------------------------------------------------------------------------------
// PostgreSQL: clone mode
// ---------------------------------------------------------------------------------------------

static LEDGER: TemplateDb = TemplateDb::new(|db| {
  let seed = Arc::clone(&db);
  db.execute(
    "CREATE TABLE accounts (id BIGINT PRIMARY KEY, name TEXT NOT NULL UNIQUE)",
    &[],
  )
  .flat_map(move |_| seed.execute("INSERT INTO accounts VALUES (0, 'seed')", &[]))
  .map(|_| ())
});

type Cloned = caps!(SqlClientService, IsolatedDatabase);

/// Commits account 1 and reads it back through a second pool: (database, template, rows seen
/// by another connection).
fn commit_and_look() -> Effect<(String, String, i64), TestError, Cloned> {
  let insert: Effect<u64, SqlError, Cloned> = transaction(|tx| {
    tx.execute("INSERT INTO accounts VALUES (1, 'alice')", &[])
      .narrow()
  });
  insert.map_err(TestError::from).flat_map(|_| {
    need::<IsolatedDatabase>().flat_map(|db| {
      let elsewhere = sql(count("accounts")).provide_layer(PgSqlClient::layer::<TestError>(
        PgPoolConfig::from_url(db.url()),
      ));
      let (name, template) = (db.name().to_owned(), db.template().to_owned());
      elsewhere.narrow().map(move |n| (name, template, n))
    })
  })
}

/// Whether database `name` exists on the shared server.
async fn pg_database_exists(name: String) -> bool {
  let found: Effect<i64, TestError, Db> = sql(
    query_one_as::<(i64,), Db, _>(
      "SELECT COUNT(*) FROM pg_database WHERE datname = $1",
      params![name],
    )
    .map(|(n,)| n),
  );
  run(found.provide_layer(PG.sql_layer()))
    .await
    .expect("pg_database")
    == 1
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pg_clone_mode_commits_into_a_database_of_its_own_and_drops_it() {
  if !require_docker() {
    return;
  }
  let first = run(commit_and_look().provide_layer(PG.layer().provide_to(LEDGER.isolated_db())))
    .await
    .expect("first test");
  let second = run(commit_and_look().provide_layer(PG.layer().provide_to(LEDGER.isolated_db())))
    .await
    .expect("second test");
  assert_eq!(first.2, 2, "the commit is visible to another connection");
  assert_eq!(
    second.2, 2,
    "the second test starts from the template, not the first test"
  );
  assert_ne!(first.0, second.0, "a database per test");
  assert_eq!(first.1, second.1, "one template, set up once");
  assert!(
    !pg_database_exists(first.0).await,
    "dropped when the scope closed"
  );
  assert!(pg_database_exists(first.1).await, "the template stays");

  // A failing test's database is dropped too.
  let seen = Arc::new(std::sync::Mutex::new(String::new()));
  let record = Arc::clone(&seen);
  let failing: Effect<(), TestError, Cloned> = need::<IsolatedDatabase>().flat_map(move |db| {
    *record.lock().unwrap() = db.name().to_owned();
    fail(TestError::Sql(SqlError::RowNotFound))
  });
  let failed = run(failing.provide_layer(PG.layer().provide_to(LEDGER.isolated_db()))).await;
  assert_eq!(failed, Err(TestError::Sql(SqlError::RowNotFound)));
  let name = seen.lock().unwrap().clone();
  assert!(!name.is_empty() && !pg_database_exists(name).await);
}

static BROKEN: TemplateDb = TemplateDb::new(|db| db.execute("THIS IS NOT SQL", &[]).map(|_| ()));

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pg_a_failed_template_setup_is_remembered() {
  if !require_docker() {
    return;
  }
  let program = || -> Effect<i64, TestError, Cloned> { sql(count("accounts")).narrow() };
  let first = run(program().provide_layer(PG.layer().provide_to(BROKEN.isolated_db()))).await;
  let second = run(program().provide_layer(PG.layer().provide_to(BROKEN.isolated_db()))).await;
  assert!(
    matches!(&first, Err(TestError::Sql(SqlError::QueryFailed { .. }))),
    "{first:?}"
  );
  assert_eq!(first, second, "the same error, without a second setup");
  assert!(format!("{BROKEN:?}").contains("Err("));
}

#[cfg(feature = "migrate")]
mod migrations {
  use super::*;
  use id_effect_test::db::migrate;

  static MIGRATED: TemplateDb = TemplateDb::new(|db| {
    migrate(
      db,
      concat!(env!("CARGO_MANIFEST_DIR"), "/tests/db_migrations"),
    )
  });

  #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
  async fn pg_a_template_runs_the_apps_migrations() {
    if !require_docker() {
      return;
    }
    let program: Effect<(i64, i64), TestError, Cloned> = sql(
      count("accounts")
        .flat_map(|accounts| count("_id_effect_migrations").map(move |m| (accounts, m))),
    )
    .narrow();
    let seen = run(program.provide_layer(PG.layer().provide_to(MIGRATED.isolated_db()))).await;
    assert_eq!(seen, Ok((1, 1)), "the seed row, and one applied migration");
  }

  #[test]
  fn migration_errors_become_sql_errors() {
    let db: Arc<dyn SqlClient> = Arc::new(test_client());
    let missing = migrate(Arc::clone(&db), "/nonexistent/id_effect_test/migrations").block_on();
    assert!(
      matches!(&missing, Exit::Failure(Cause::Fail(SqlError::QueryFailed { sql, .. })) if sql.contains("/nonexistent")),
      "{missing:?}"
    );
    let broken = TestSqlClient::new();
    let _ = broken.connect().block_on();
    broken.fail_queries("boom");
    let failed = migrate(
      Arc::new(broken),
      concat!(env!("CARGO_MANIFEST_DIR"), "/tests/db_migrations"),
    )
    .block_on();
    assert!(
      matches!(&failed, Exit::Failure(Cause::Fail(SqlError::QueryFailed { message, .. })) if message == "boom"),
      "{failed:?}"
    );
  }
}
