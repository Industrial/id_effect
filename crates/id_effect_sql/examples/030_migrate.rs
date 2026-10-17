//! Versioned migrations end to end: `status`, `check`, `run` and `revert` of
//! `id_effect_sql::migrate`, on the `SqlClientService` of the environment.
//!
//! By default it runs on the in-memory `TestSqlClient`, scripted with what PostgreSQL's tracking
//! table would answer after each step, so no database is needed. With `DATABASE_URL` set, or with
//! `--live`, it runs the same steps on PostgreSQL (tracking table `_id_effect_example_030`) and
//! reverts every migration at the end, so it can be run again.
//!
//! Run: `cargo run -p id_effect_sql --example 030_migrate --features migrate,pg`
//!
//! Live: `DATABASE_URL=postgres://… cargo run -p id_effect_sql --example 030_migrate --features
//! migrate,pg -- --live`

use std::process::ExitCode;

use id_effect::{Cause, Effect, Exit};
use id_effect_sql::jiff::Timestamp;
use id_effect_sql::migrate::{MigrateError, MigrationState, MigrationStatus, Migrator};
use id_effect_sql::pg::{PgPoolConfig, PgSqlClient};
use id_effect_sql::{SqlClient, SqlClientEnv, SqlRow, SqlValue, TestSqlClient, sql_client_env};
use id_effect_tokio::TokioRuntime;

/// The tracking table of this example (kept apart from an application's).
const TABLE: &str = "_id_effect_example_030";

/// The engine's advisory-lock query (scripted as free in memory).
const LOCK: &str = "SELECT pg_try_advisory_xact_lock($1) AS locked";

/// The migrations, embedded as `(file name, script)` pairs: a table, then a reversible change
/// with two statements.
fn migrator() -> Migrator {
  Migrator::from_files([
    (
      "0001_create_notes.up.sql",
      "CREATE TABLE example_030_notes (id BIGSERIAL PRIMARY KEY, body TEXT NOT NULL)",
    ),
    ("0001_create_notes.down.sql", "DROP TABLE example_030_notes"),
    (
      "0002_note_tags.up.sql",
      "ALTER TABLE example_030_notes ADD COLUMN tags TEXT NOT NULL DEFAULT '';\n\
       CREATE INDEX example_030_notes_tags ON example_030_notes (tags);",
    ),
    (
      "0002_note_tags.down.sql",
      "DROP INDEX example_030_notes_tags;\nALTER TABLE example_030_notes DROP COLUMN tags;",
    ),
  ])
  .expect("valid migration files")
  .with_table(TABLE)
}

/// Where the steps run.
enum Target {
  /// `TestSqlClient`, standing in for PostgreSQL.
  Memory(TestSqlClient),
  /// PostgreSQL at `url`, on a tokio runtime.
  Live {
    url: String,
    rt: tokio::runtime::Runtime,
  },
}

impl Target {
  /// Runs one step against the target's client.
  fn exec<A: Send + 'static>(
    &self,
    step: Effect<A, MigrateError, SqlClientEnv>,
  ) -> Result<A, MigrateError> {
    let exit = match self {
      Target::Memory(test) => step.provide_all(sql_client_env(test.clone())).block_on(),
      Target::Live { url, rt } => {
        let pool = PgSqlClient::layer::<MigrateError>(PgPoolConfig::from_url(url.clone()));
        let step = step.provide_layer(pool);
        rt.block_on(async { TokioRuntime::current().expect("tokio").run(step).await })
      }
    };
    match exit {
      Exit::Success(a) => Ok(a),
      Exit::Failure(Cause::Fail(e)) => Err(e),
      Exit::Failure(cause) => panic!("not a typed failure: {cause:?}"),
    }
  }

  /// In memory: answers the tracking-table queries as PostgreSQL would once `applied` are the
  /// applied versions. PostgreSQL keeps that state itself.
  fn record(&self, m: &Migrator, applied: &[i64]) {
    let Target::Memory(test) = self else {
      return;
    };
    let rows: Vec<SqlRow> = m
      .migrations()
      .iter()
      .filter(|x| applied.contains(&x.version()))
      .map(|x| {
        SqlRow::from_pairs([
          ("version", SqlValue::I64(x.version())),
          ("description", SqlValue::Text(x.description().into())),
          ("checksum", SqlValue::Bytes(x.checksum().to_vec())),
          ("installed_on", SqlValue::Timestamp(Timestamp::now())),
        ])
      })
      .collect();
    let checksums = rows
      .iter()
      .take(1)
      .map(|r| SqlRow::from_pairs([("checksum", r.get::<Vec<u8>>("checksum").expect("bytes"))]))
      .collect();
    test.script_query(
      format!("SELECT version, description, checksum, installed_on FROM {TABLE} ORDER BY version"),
      rows,
    );
    test.script_query(
      format!("SELECT checksum FROM {TABLE} WHERE version = $1"),
      checksums,
    );
  }
}

/// `pending`, `applied`, `drifted` or `missing`.
fn label(state: MigrationState) -> &'static str {
  match state {
    MigrationState::Pending => "pending",
    MigrationState::Applied => "applied",
    MigrationState::Drifted => "drifted",
    MigrationState::Missing => "missing",
  }
}

/// Prints the status lines; returns the states.
fn show(title: &str, all: &[MigrationStatus]) -> Vec<MigrationState> {
  println!("{title}");
  for s in all {
    let reversible = if s.reversible { "  (reversible)" } else { "" };
    println!(
      "  {:7}  {}  {}  {}…{reversible}",
      label(s.state),
      s.version,
      s.description,
      &s.checksum_hex()[..12],
    );
  }
  all.iter().map(|s| s.state).collect()
}

fn target() -> Result<Target, String> {
  let live = std::env::args().skip(1).any(|a| a == "--live");
  match std::env::var("DATABASE_URL") {
    Ok(url) if !url.is_empty() => {
      let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|e| format!("tokio runtime: {e}"))?;
      Ok(Target::Live { url, rt })
    }
    _ if live => Err("--live needs DATABASE_URL (postgres://user:pass@host:port/db)".into()),
    _ => {
      let test = TestSqlClient::new();
      // The advisory lock is free.
      test.script_query(LOCK, vec![SqlRow::from_pairs([("locked", true)])]);
      match test.connect().block_on() {
        Exit::Success(()) => Ok(Target::Memory(test)),
        Exit::Failure(cause) => Err(format!("connect: {cause:?}")),
      }
    }
  }
}

fn steps(db: &Target) -> Result<(), MigrateError> {
  use MigrationState::{Applied, Pending};
  let m = migrator();

  let before = show("status (fresh database):", &db.exec(m.status())?);
  assert_eq!(before, [Pending, Pending]);

  // `check` is the CI gate: it fails while migrations are pending.
  match db.exec(m.check()) {
    Err(e @ MigrateError::Pending { .. }) => println!("check: {e}"),
    other => panic!("expected pending migrations, got {other:?}"),
  }

  // One transaction per migration, under the advisory lock.
  let applied = db.exec(m.run())?;
  assert_eq!(applied, [1, 2]);
  println!("run: applied {applied:?}");
  db.record(&m, &applied);

  let after = show("status (after run):", &db.exec(m.status())?);
  assert_eq!(after, [Applied, Applied]);
  db.exec(m.check())?;
  println!("check: up to date");

  // A second run (another replica starting) has nothing to do.
  assert_eq!(db.exec(m.run())?, Vec::<i64>::new());

  // Down scripts of the last two applied migrations, newest first.
  let reverted = db.exec(m.revert(2))?;
  assert_eq!(reverted, [2, 1]);
  println!("revert: reverted {reverted:?}");
  db.record(&m, &[]);

  let reset = show("status (after revert):", &db.exec(m.status())?);
  assert_eq!(reset, [Pending, Pending]);
  Ok(())
}

fn main() -> ExitCode {
  let db = match target() {
    Ok(db) => db,
    Err(why) => {
      eprintln!("030_migrate: {why}");
      return ExitCode::from(2);
    }
  };
  let mode = match &db {
    Target::Memory(_) => "in memory",
    Target::Live { .. } => "on PostgreSQL",
  };
  match steps(&db) {
    Ok(()) => {
      if let Target::Memory(test) = &db {
        println!(
          "({} transactions committed, {} statements executed)",
          test.commits(),
          test.recorded_executes().len()
        );
      }
      println!("030_migrate ok ({mode}: run, status, check, revert)");
      ExitCode::SUCCESS
    }
    Err(e) => {
      eprintln!("030_migrate failed ({mode}): {e}");
      ExitCode::FAILURE
    }
  }
}
