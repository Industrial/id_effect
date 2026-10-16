//! Live PostgreSQL tests of `id_effect_sql::migrate` (features `migrate` + `pg`).
//!
//! Every test migrates its own fresh database (created from `DATABASE_URL`, dropped at the end),
//! so tests running in parallel never share a tracking table, an advisory lock or a schema.
//! Without `DATABASE_URL` the tests print `SKIP` and return; with `ID_EFFECT_REQUIRE_PG=1` a
//! missing or unreachable database is a failure (as in `tests/pg.rs`).

use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;

use id_effect::{Cause, Effect, Exit, Layer, hlist, need};
use id_effect_sql::migrate::{
  self, MigrateError, MigratedEnv, MigrationState, MigrationsApplied, Migrator,
};
use id_effect_sql::pg::PgSqlClient;
use id_effect_sql::{SqlClient, SqlClientService, SqlError};
use id_effect_tokio::TokioRuntime;
use sqlx::Connection;
use sqlx::postgres::{PgConnectOptions, PgConnection, PgPoolOptions};

const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/migrations");
const FAILING: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/migrations_failing");

async fn exit<A: Send + 'static, E: Send + 'static>(eff: Effect<A, E>) -> Exit<A, E> {
  TokioRuntime::current().expect("tokio").run(eff).await
}

async fn run<A: Send + 'static>(eff: Effect<A, MigrateError>) -> Result<A, MigrateError> {
  match exit(eff).await {
    Exit::Success(a) => Ok(a),
    Exit::Failure(Cause::Fail(e)) => Err(e),
    Exit::Failure(c) => panic!("unexpected cause: {c:?}"),
  }
}

fn pg_required() -> bool {
  std::env::var("ID_EFFECT_REQUIRE_PG").is_ok_and(|v| v == "1")
}

/// Fail (when `ID_EFFECT_REQUIRE_PG=1`) or loudly skip a live test that has no database.
fn unavailable<T>(test: &str, why: &str) -> Option<T> {
  assert!(
    !pg_required(),
    "{test}: ID_EFFECT_REQUIRE_PG=1 but Postgres is unavailable: {why}"
  );
  eprintln!("SKIP {test}: {why} (set DATABASE_URL, and ID_EFFECT_REQUIRE_PG=1 to require it)");
  None
}

/// A fresh database for one test; dropped by [`LiveDb::drop_db`].
struct LiveDb {
  admin: PgConnectOptions,
  options: PgConnectOptions,
  name: String,
}

impl LiveDb {
  async fn create(test: &str) -> Option<Self> {
    let url = match std::env::var("DATABASE_URL") {
      Ok(url) => url,
      Err(_) => return unavailable(test, "DATABASE_URL is not set"),
    };
    let admin = match PgConnectOptions::from_str(&url) {
      Ok(o) => o,
      Err(e) => return unavailable(test, &format!("bad DATABASE_URL: {e}")),
    };
    let mut conn = match PgConnection::connect_with(&admin).await {
      Ok(c) => c,
      Err(e) => return unavailable(test, &format!("cannot connect: {e}")),
    };
    let name = format!("id_effect_migrate_{test}");
    for sql in [
      format!("DROP DATABASE IF EXISTS {name} WITH (FORCE)"),
      format!("CREATE DATABASE {name}"),
    ] {
      sqlx::raw_sql(&sql).execute(&mut conn).await.expect(&sql);
    }
    let options = admin.clone().database(&name);
    Some(Self {
      admin,
      options,
      name,
    })
  }

  /// A new client (its own pool) on the test database.
  async fn client(&self) -> Arc<dyn SqlClient> {
    let pool = PgPoolOptions::new()
      .max_connections(2)
      .connect_with(self.options.clone())
      .await
      .expect("pool");
    Arc::new(PgSqlClient::new(pool))
  }

  /// `sql`'s single `BIGINT`/`INT8` value.
  async fn scalar(&self, sql: &str) -> i64 {
    let mut conn = PgConnection::connect_with(&self.options)
      .await
      .expect("connect");
    sqlx::query_scalar::<_, i64>(sql)
      .fetch_one(&mut conn)
      .await
      .expect(sql)
  }

  async fn drop_db(self) {
    let mut conn = PgConnection::connect_with(&self.admin)
      .await
      .expect("connect");
    let sql = format!("DROP DATABASE IF EXISTS {} WITH (FORCE)", self.name);
    sqlx::raw_sql(&sql).execute(&mut conn).await.expect(&sql);
  }
}

fn fixtures() -> Migrator {
  Migrator::from_dir(FIXTURES).expect("fixtures")
}

async fn applied_versions(db: &LiveDb) -> Vec<i64> {
  let mut conn = PgConnection::connect_with(&db.options)
    .await
    .expect("connect");
  sqlx::query_scalar::<_, i64>("SELECT version FROM _id_effect_migrations ORDER BY version")
    .fetch_all(&mut conn)
    .await
    .expect("tracking table")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_runs_apply_once() {
  let Some(db) = LiveDb::create("concurrent_runs_apply_once").await else {
    return;
  };
  let m = fixtures();
  let mut clients = Vec::new();
  for _ in 0..4 {
    clients.push(db.client().await);
  }
  let runs: Vec<_> = clients
    .iter()
    .map(|c| tokio::spawn(run(m.run_on(Arc::clone(c)))))
    .collect();
  let mut applied = Vec::new();
  for handle in runs {
    applied.extend(handle.await.expect("join").expect("run"));
  }
  applied.sort_unstable();
  assert_eq!(
    applied,
    [1, 2, 3],
    "each migration applied by exactly one runner"
  );
  assert_eq!(applied_versions(&db).await, [1, 2, 3]);
  assert_eq!(
    db.scalar("SELECT COUNT(*) FROM widgets").await,
    2,
    "seeded once"
  );
  assert_eq!(
    db.scalar("SELECT COUNT(*) FROM widgets WHERE label = 'widget: b; c'")
      .await,
    1
  );
  // Up to date: a further run applies nothing and the check passes.
  assert_eq!(run(m.run_on(Arc::clone(&clients[0]))).await, Ok(vec![]));
  assert_eq!(run(m.check_on(Arc::clone(&clients[0]))).await, Ok(()));
  db.drop_db().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn failed_migration_rolls_back_and_stops() {
  let Some(db) = LiveDb::create("failed_migration_rolls_back_and_stops").await else {
    return;
  };
  let client = db.client().await;
  let m = Migrator::from_dir(FAILING).expect("fixtures");
  let err = run(m.run_on(Arc::clone(&client))).await.unwrap_err();
  match &err {
    MigrateError::Failed {
      version: 2,
      file,
      error: SqlError::QueryFailed { message, .. },
    } => {
      assert_eq!(file, "0002_broken.sql");
      assert!(message.contains("no_such_table"), "{message}");
    }
    other => panic!("expected Failed for 0002_broken.sql, got {other:?}"),
  }
  assert!(err.to_string().contains("0002_broken.sql"));
  assert_eq!(applied_versions(&db).await, [1], "0001 stays applied");
  assert_eq!(
    db.scalar("SELECT COUNT(*) FROM gadgets").await,
    0,
    "0002's first statement rolled back"
  );
  assert_eq!(
    db.scalar("SELECT COUNT(*) FROM pg_tables WHERE tablename = 'never_applied'")
      .await,
    0,
    "the run stopped before 0003"
  );
  let states: Vec<MigrationState> = run(m.status_on(client))
    .await
    .unwrap()
    .iter()
    .map(|s| s.state)
    .collect();
  assert_eq!(
    states,
    [
      MigrationState::Applied,
      MigrationState::Pending,
      MigrationState::Pending
    ]
  );
  db.drop_db().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn down_reverts_last() {
  let Some(db) = LiveDb::create("down_reverts_last").await else {
    return;
  };
  let client = db.client().await;
  let m = fixtures();
  assert_eq!(run(m.run_on(Arc::clone(&client))).await, Ok(vec![1, 2, 3]));
  assert_eq!(run(m.revert_on(Arc::clone(&client), 1)).await, Ok(vec![3]));
  assert_eq!(applied_versions(&db).await, [1, 2]);
  assert_eq!(
    db.scalar("SELECT COUNT(*) FROM widgets").await,
    0,
    "seed reverted"
  );
  assert_eq!(
    run(m.check_on(Arc::clone(&client))).await,
    Err(MigrateError::Pending { versions: vec![3] })
  );
  // Re-applied, then everything reverted, newest first.
  assert_eq!(run(m.run_on(Arc::clone(&client))).await, Ok(vec![3]));
  assert_eq!(
    run(m.revert_on(Arc::clone(&client), 10)).await,
    Ok(vec![3, 2, 1])
  );
  assert_eq!(applied_versions(&db).await, Vec::<i64>::new());
  assert_eq!(
    db.scalar("SELECT COUNT(*) FROM pg_tables WHERE tablename = 'widgets'")
      .await,
    0
  );
  db.drop_db().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn status_and_check_report_drift() {
  let Some(db) = LiveDb::create("status_and_check_report_drift").await else {
    return;
  };
  let client = db.client().await;
  // Embedded variant: the same scripts as the directory, as `(name, script)` pairs.
  let embedded = Migrator::from_files([
    (
      "0001_create_widgets.up.sql",
      include_str!("migrations/0001_create_widgets.up.sql"),
    ),
    (
      "0001_create_widgets.down.sql",
      include_str!("migrations/0001_create_widgets.down.sql"),
    ),
  ])
  .unwrap();
  assert_eq!(run(embedded.run_on(Arc::clone(&client))).await, Ok(vec![1]));
  let status = run(fixtures().status_on(Arc::clone(&client)))
    .await
    .unwrap();
  assert_eq!(status[0].state, MigrationState::Applied, "same checksum");
  assert!(status[0].applied_at.is_some());
  assert_eq!(
    status[0].applied_checksum.as_deref(),
    Some(status[0].checksum.as_slice())
  );

  let changed =
    Migrator::from_files([("0001_create_widgets.up.sql", "CREATE TABLE widgets ()")]).unwrap();
  let drift = MigrateError::ChecksumMismatch {
    version: 1,
    file: "0001_create_widgets.up.sql".into(),
  };
  assert_eq!(
    run(changed.check_on(Arc::clone(&client))).await,
    Err(drift.clone())
  );
  assert_eq!(run(changed.run_on(Arc::clone(&client))).await, Err(drift));
  let empty = Migrator::from_files(Vec::<(String, String)>::new()).unwrap();
  assert_eq!(
    run(empty.check_on(Arc::clone(&client))).await,
    Err(MigrateError::Missing { version: 1 })
  );
  let states: Vec<MigrationState> = run(empty.status_on(client))
    .await
    .unwrap()
    .iter()
    .map(|s| s.state)
    .collect();
  assert_eq!(states, [MigrationState::Missing]);
  db.drop_db().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_held_lock_times_out() {
  let Some(db) = LiveDb::create("a_held_lock_times_out").await else {
    return;
  };
  let client = db.client().await;
  let m = fixtures().with_lock_timeout(Duration::from_millis(300));
  let mut holder = PgConnection::connect_with(&db.options)
    .await
    .expect("connect");
  let mut tx = holder.begin().await.expect("begin");
  sqlx::query("SELECT pg_advisory_xact_lock($1)")
    .bind(m.lock_key())
    .execute(&mut *tx)
    .await
    .expect("lock");
  assert_eq!(
    run(m.run_on(Arc::clone(&client))).await,
    Err(MigrateError::LockTimeout {
      key: m.lock_key(),
      timeout: Duration::from_millis(300)
    })
  );
  tx.rollback().await.expect("release");
  assert_eq!(run(m.run_on(client)).await, Ok(vec![1, 2, 3]));
  db.drop_db().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_layer_migrates_at_startup() {
  let Some(db) = LiveDb::create("the_layer_migrates_at_startup").await else {
    return;
  };
  let client = db.client().await;
  let program: Effect<Vec<i64>, MigrateError, MigratedEnv> =
    need::<MigrationsApplied>().map(|applied| applied.0.clone());
  let started = program
    .provide_layer(migrate::layer(FIXTURES))
    .provide_layer(Layer::succeed(hlist![SqlClientService(client)]));
  assert_eq!(run(started).await, Ok(vec![1, 2, 3]));
  assert_eq!(applied_versions(&db).await, [1, 2, 3]);
  db.drop_db().await;
}
