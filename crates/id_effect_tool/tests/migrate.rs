//! `id-effect migrate` end to end: `new` on disk, and `run` / `status` / `revert` / `check` on a
//! live PostgreSQL.
//!
//! Every live test migrates its own fresh database (created from `DATABASE_URL`, dropped at the
//! end). Without `DATABASE_URL` the live tests print `SKIP` and return; with
//! `ID_EFFECT_REQUIRE_PG=1` a missing or unreachable database is a failure (as in
//! `crates/id_effect_sql/tests/migrate.rs`).

use std::fs;
use std::path::Path;
use std::process::{Command, ExitCode, Output};
use std::sync::Arc;

use id_effect::{Effect, Exit};
use id_effect_sql::migrate::DEFAULT_LOCK_KEY;
use id_effect_sql::pg::{PgPoolConfig, PgSqlClient, pg_pool_from_config};
use id_effect_sql::{SqlClient, SqlError, SqlValue};
use id_effect_tokio::TokioRuntime;
use id_effect_tool::cli;
use serde_json::Value;

/// Runs `id-effect migrate <args>` in-process: (exit code, stdout, stderr).
fn migrate(args: &[&str]) -> (ExitCode, String, String) {
  let (mut out, mut err) = (Vec::new(), Vec::new());
  let argv = ["id-effect", "migrate"].iter().chain(args).copied();
  let code = cli::run(argv, &mut out, &mut err);
  let text = |b: Vec<u8>| String::from_utf8(b).expect("utf-8");
  (code, text(out), text(err))
}

/// The installed binary, in `dir`, without the caller's `DATABASE_URL` unless `url` is given.
fn binary(dir: &Path, url: Option<&str>, args: &[&str]) -> Output {
  let mut cmd = Command::new(env!("CARGO_BIN_EXE_id-effect"));
  cmd.current_dir(dir).arg("migrate").args(args);
  match url {
    Some(url) => cmd.env("DATABASE_URL", url),
    None => cmd.env_remove("DATABASE_URL"),
  };
  cmd.output().expect("spawn id-effect")
}

fn json(text: &str) -> Value {
  serde_json::from_str(text.trim_end()).unwrap_or_else(|e| panic!("{e}: {text}"))
}

fn s(path: &Path) -> &str {
  path.to_str().expect("utf-8 path")
}

// ---- without a database ------------------------------------------------------------------------

#[test]
fn new_numbers_after_the_directory() {
  let tmp = tempfile::tempdir().unwrap();
  let fresh = tmp.path().join("fresh");
  let (code, out, err) = migrate(&["new", "create users", "--dir", s(&fresh)]);
  assert_eq!(code, ExitCode::SUCCESS, "{err}");
  let names: Vec<String> = fs::read_dir(&fresh)
    .unwrap()
    .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
    .collect();
  assert_eq!(names.len(), 1);
  let (version, rest) = names[0].split_once('_').unwrap();
  assert_eq!(version.len(), 14, "a UTC timestamp: {version}");
  assert!(version.bytes().all(|b| b.is_ascii_digit()));
  assert_eq!(rest, "create_users.sql");
  assert_eq!(
    out,
    format!("Created {}\n", fresh.join(&names[0]).display())
  );

  // The service template's `0001_init.sql`: the sequence continues.
  let template = tmp.path().join("service");
  fs::create_dir(&template).unwrap();
  fs::write(template.join("0001_init.sql"), "CREATE TABLE t (id INT)").unwrap();
  let (code, out, _) = migrate(&["new", "add-tags", "--reversible", "--dir", s(&template)]);
  assert_eq!(code, ExitCode::SUCCESS);
  for file in ["0002_add_tags.up.sql", "0002_add_tags.down.sql"] {
    assert!(template.join(file).is_file(), "{file}: {out}");
  }
  let (code, _, _) = migrate(&["new", "later", "--timestamp", "--dir", s(&template)]);
  assert_eq!(code, ExitCode::SUCCESS);
  let (code, _, _) = migrate(&["new", "next", "--sequential", "--dir", s(&template)]);
  assert_eq!(code, ExitCode::SUCCESS);
  let stamped = fs::read_dir(&template)
    .unwrap()
    .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
    .find(|n| n.ends_with("_next.sql"))
    .unwrap();
  assert_eq!(stamped.split_once('_').unwrap().0.len(), 14, "{stamped}");
}

#[test]
fn new_reports_an_unwritable_directory() {
  let tmp = tempfile::tempdir().unwrap();
  let file = tmp.path().join("migrations");
  fs::write(&file, "").unwrap();
  let (code, out, err) = migrate(&["new", "x", "--dir", s(&file)]);
  assert_eq!(code, ExitCode::from(1));
  assert!(out.is_empty());
  assert!(
    err.starts_with("error: ") && err.contains("migrations"),
    "{err}"
  );
}

#[test]
fn usage_errors_exit_2() {
  for args in [
    &["new", "bad.name"][..],
    &["new", "x", "--timestamp", "--sequential"],
    &["revert", "--count", "0"],
    &["run", "--table", "a.b.c"],
    &["status", "--database-url", "mysql://u:hunter2@db/app"],
    &[],
  ] {
    let (code, out, err) = migrate(args);
    assert_eq!(code, ExitCode::from(2), "{args:?}: {out}{err}");
    assert!(!err.contains("hunter2"), "{err}");
  }
  let (_, out, _) = migrate(&["check", "--json", "--database-url", "redis://db"]);
  assert_eq!(json(&out)["error"]["kind"], "usage");
}

#[test]
fn without_a_database_url_the_binary_exits_2_and_reads_dotenv() {
  let tmp = tempfile::tempdir().unwrap();
  let out = binary(tmp.path(), None, &["status"]);
  assert_eq!(out.status.code(), Some(2), "{out:?}");
  let stderr = String::from_utf8_lossy(&out.stderr);
  assert!(
    stderr.starts_with("error: no database: pass --database-url"),
    "{stderr}"
  );

  // `.env` supplies it (an unreachable one here: exit 3 names where it came from).
  fs::create_dir(tmp.path().join("migrations")).unwrap();
  fs::write(
    tmp.path().join(".env"),
    "DATABASE_URL=postgres://u:hunter2@127.0.0.1:1/app\n",
  )
  .unwrap();
  let out = binary(tmp.path(), None, &["status"]);
  assert_eq!(out.status.code(), Some(3), "{out:?}");
  let stderr = String::from_utf8_lossy(&out.stderr);
  assert!(
    stderr.starts_with("error: cannot connect to the database (DATABASE_URL (.env)): "),
    "{stderr}"
  );
  assert!(!stderr.contains("hunter2"), "{stderr}");
}

#[test]
fn an_unreachable_database_exits_3_without_the_password() {
  let tmp = tempfile::tempdir().unwrap();
  let url = "postgres://u:hunter2@127.0.0.1:1/app";
  let (code, out, err) = migrate(&["check", "--dir", s(tmp.path()), "--database-url", url]);
  assert_eq!(code, ExitCode::from(3));
  assert!(out.is_empty());
  assert!(
    err.starts_with("error: cannot connect to the database (--database-url): "),
    "{err}"
  );
  assert!(!err.contains("hunter2"), "{err}");
  let (code, out, err) = migrate(&[
    "run",
    "--json",
    "--dir",
    s(tmp.path()),
    "--database-url",
    url,
  ]);
  assert_eq!(code, ExitCode::from(3));
  assert!(err.is_empty(), "{err}");
  let value = json(&out);
  assert_eq!(value["ok"], false);
  assert_eq!(value["error"]["kind"], "database");
}

#[test]
fn an_unreadable_directory_exits_1_before_connecting() {
  let tmp = tempfile::tempdir().unwrap();
  let missing = tmp.path().join("nope");
  let url = "postgres://127.0.0.1:1/app";
  let (code, _, err) = migrate(&["run", "--dir", s(&missing), "--database-url", url]);
  assert_eq!(code, ExitCode::from(1));
  assert!(err.contains("cannot read migration directory"), "{err}");
}

#[test]
fn revert_json_with_table_and_lock_options_reports_an_unreachable_database() {
  let tmp = tempfile::tempdir().unwrap();
  let url = "postgres://u:hunter2@127.0.0.1:1/app";
  let (code, out, err) = migrate(&[
    "revert",
    "--count",
    "2",
    "--json",
    "--table",
    "audit.schema_versions",
    "--lock-timeout",
    "5",
    "--dir",
    s(tmp.path()),
    "--database-url",
    url,
  ]);
  assert_eq!(code, ExitCode::from(3));
  assert!(err.is_empty(), "{err}");
  assert!(!out.contains("hunter2"), "{out}");
  let value = json(&out);
  assert_eq!(value["ok"], false);
  assert_eq!(value["error"]["kind"], "database");
}

// ---- live PostgreSQL ---------------------------------------------------------------------------

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

/// `base` with its database name replaced by `name` (query string kept).
fn with_database(base: &str, name: &str) -> String {
  let (scheme, rest) = base.split_once("://").expect("scheme://");
  let (authority, tail) = rest.split_at(rest.find(['/', '?']).unwrap_or(rest.len()));
  let query = tail.find('?').map_or("", |i| &tail[i..]);
  format!("{scheme}://{authority}/{name}{query}")
}

/// A fresh database for one test, and a migrations directory; dropped by [`LiveDb::drop_db`].
struct LiveDb {
  rt: TokioRuntime,
  admin: Arc<dyn SqlClient>,
  name: String,
  url: String,
  dir: tempfile::TempDir,
}

impl LiveDb {
  fn create(test: &str) -> Option<Self> {
    let base = match std::env::var("DATABASE_URL") {
      Ok(url) if !url.is_empty() => url,
      _ => return unavailable(test, "DATABASE_URL is not set"),
    };
    let rt = TokioRuntime::new_multi_thread().expect("tokio");
    let admin = match connect(&rt, &base) {
      Ok(client) => client,
      Err(e) => return unavailable(test, &format!("cannot connect: {e}")),
    };
    let name = format!("id_effect_tool_{test}");
    let db = Self {
      url: with_database(&base, &name),
      rt,
      admin,
      name,
      dir: tempfile::tempdir().unwrap(),
    };
    db.admin_exec(&format!("DROP DATABASE IF EXISTS {} WITH (FORCE)", db.name));
    db.admin_exec(&format!("CREATE DATABASE {}", db.name));
    Some(db)
  }

  fn run<A: Send + 'static>(&self, eff: Effect<A, SqlError>) -> A {
    match self.rt.block_on(self.rt.run(eff)) {
      Exit::Success(a) => a,
      Exit::Failure(cause) => panic!("{cause:?}"),
    }
  }

  fn admin_exec(&self, sql: &str) {
    self.run(self.admin.execute(sql, &[]));
  }

  /// A client on the test database.
  fn client(&self) -> Arc<dyn SqlClient> {
    connect(&self.rt, &self.url).expect("connect")
  }

  /// `sql`'s single `BIGINT` column `n`.
  fn scalar(&self, sql: &str) -> i64 {
    let rows = self.run(self.client().query(sql, &[]));
    rows[0].get::<i64>("n").expect("n")
  }

  fn path(&self) -> &Path {
    self.dir.path()
  }

  fn write(&self, file: &str, sql: &str) {
    fs::write(self.path().join(file), sql).unwrap();
  }

  /// `id-effect migrate <command> --dir <dir> --database-url <url> <args>`.
  fn migrate(&self, command: &str, args: &[&str]) -> (ExitCode, String, String) {
    let mut all = vec![
      command,
      "--dir",
      s(self.path()),
      "--database-url",
      &self.url,
    ];
    all.extend_from_slice(args);
    migrate(&all)
  }

  fn drop_db(self) {
    self.admin_exec(&format!(
      "DROP DATABASE IF EXISTS {} WITH (FORCE)",
      self.name
    ));
  }
}

fn connect(rt: &TokioRuntime, url: &str) -> Result<Arc<dyn SqlClient>, String> {
  let config = PgPoolConfig::from_url(url).with_max_size(2);
  rt.block_on(pg_pool_from_config(config))
    .map(|pool| Arc::new(PgSqlClient::new(pool)) as Arc<dyn SqlClient>)
    .map_err(|e| e.0.to_string())
}

const OK: ExitCode = ExitCode::SUCCESS;

#[test]
fn run_status_check_and_revert_round_trip() {
  let Some(db) = LiveDb::create("round_trip") else {
    return;
  };
  db.write(
    "0001_init.sql",
    "CREATE TABLE widgets (id BIGINT PRIMARY KEY, label TEXT NOT NULL)",
  );
  let (code, out, _) = migrate(&["new", "add tags", "--reversible", "--dir", s(db.path())]);
  assert_eq!(code, OK);
  assert!(out.contains("0002_add_tags.up.sql"), "{out}");
  db.write(
    "0002_add_tags.up.sql",
    "ALTER TABLE widgets ADD COLUMN tags TEXT",
  );
  db.write(
    "0002_add_tags.down.sql",
    "ALTER TABLE widgets DROP COLUMN tags",
  );

  // data.migrations#8: pending migrations fail the check.
  let (code, out, err) = db.migrate("check", &[]);
  assert_eq!(code, ExitCode::from(1));
  assert!(out.is_empty());
  assert_eq!(
    err,
    "error: 2 pending migration(s): 1, 2\nhint: apply them with `id-effect migrate run`\n"
  );
  let (code, out, err) = db.migrate("check", &["--json"]);
  assert_eq!(code, ExitCode::from(1));
  assert!(err.is_empty());
  let value = json(&out);
  assert_eq!(value["error"]["kind"], "pending");
  assert_eq!(value["error"]["versions"], serde_json::json!([1, 2]));

  // data.migrations#3: status, then run.
  let (code, out, _) = db.migrate("status", &[]);
  assert_eq!(code, OK, "status exits 0 whatever the states");
  assert!(out.contains("  pending  1  "), "{out}");
  assert!(out.contains("add tags  (reversible)"), "{out}");
  assert!(
    out.ends_with("\n0 applied, 2 pending, 0 drifted, 0 missing\n"),
    "{out}"
  );
  let (code, out, err) = db.migrate("run", &[]);
  assert_eq!(code, OK, "{err}");
  assert_eq!(
    out,
    "applied  0001_init.sql\napplied  0002_add_tags.up.sql\n\nApplied 2 migrations.\n"
  );
  assert_eq!(
    db.migrate("run", &[]).1,
    "Up to date: no pending migrations.\n"
  );
  let (code, out, _) = db.migrate("check", &[]);
  assert_eq!(code, OK);
  assert_eq!(
    out,
    "Up to date: 2 migrations applied, none pending, changed or missing.\n"
  );
  let (code, out, _) = db.migrate("status", &["--json"]);
  assert_eq!(code, OK);
  let value = json(&out);
  assert_eq!(value["ok"], true);
  assert_eq!(value["table"], "_id_effect_migrations");
  assert_eq!(value["counts"]["applied"], 2);
  assert_eq!(value["migrations"][1]["reversible"], true);
  assert!(value["migrations"][0]["applied_at"].is_string());
  assert_eq!(
    value["migrations"][0]["checksum"],
    value["migrations"][0]["applied_checksum"]
  );

  // Revert the last one, then hit the migration without a down script.
  let (code, out, _) = db.migrate("revert", &[]);
  assert_eq!(code, OK);
  assert_eq!(
    out,
    "reverted  0002_add_tags.down.sql\n\nReverted 1 migration.\n"
  );
  assert_eq!(
    db.scalar(
      "SELECT COUNT(*) AS n FROM information_schema.columns \
       WHERE table_name = 'widgets' AND column_name = 'tags'"
    ),
    0
  );
  let (code, out, _) = db.migrate("check", &["--json"]);
  assert_eq!(code, ExitCode::from(1));
  assert_eq!(json(&out)["error"]["versions"], serde_json::json!([2]));
  let (code, _, err) = db.migrate("revert", &["--count", "5"]);
  assert_eq!(code, ExitCode::from(1));
  assert!(
    err.starts_with("error: migration 0001_init.sql cannot be reverted"),
    "{err}"
  );
  assert!(err.contains("hint: add its .down.sql script"), "{err}");
  db.drop_db();
}

#[test]
fn check_fails_on_drift_and_on_a_missing_migration() {
  let Some(db) = LiveDb::create("drift") else {
    return;
  };
  db.write("0001_init.sql", "CREATE TABLE gadgets (id BIGINT)");
  let (code, out, _) = db.migrate("run", &["--json"]);
  assert_eq!(code, OK);
  assert_eq!(json(&out), serde_json::json!({"ok": true, "applied": [1]}));

  // data.migrations#8: an applied script that changed fails the check (and blocks `run`).
  db.write(
    "0001_init.sql",
    "CREATE TABLE gadgets (id BIGINT, name TEXT)",
  );
  let (code, _, err) = db.migrate("check", &[]);
  assert_eq!(code, ExitCode::from(1));
  assert!(
    err.starts_with(
      "error: migration 1 (0001_init.sql) changed after it was applied: checksum mismatch\nhint: "
    ),
    "{err}"
  );
  let (code, out, _) = db.migrate("check", &["--json"]);
  assert_eq!(code, ExitCode::from(1));
  let value = json(&out);
  assert_eq!(value["error"]["kind"], "checksum_mismatch");
  assert_eq!(value["error"]["version"], 1);
  assert_eq!(value["error"]["file"], "0001_init.sql");
  assert_eq!(db.migrate("run", &[]).0, ExitCode::from(1));
  let (code, out, _) = db.migrate("status", &[]);
  assert_eq!(code, OK);
  assert!(out.contains("  drifted  1  "), "{out}");

  // So does an applied migration whose file is gone.
  fs::remove_file(db.path().join("0001_init.sql")).unwrap();
  let (code, _, err) = db.migrate("check", &[]);
  assert_eq!(code, ExitCode::from(1));
  assert!(
    err.starts_with("error: migration 1 is applied in the database but missing from the source"),
    "{err}"
  );
  let (_, out, _) = db.migrate("status", &["--json"]);
  assert_eq!(json(&out)["migrations"][0]["state"], "missing");
  db.drop_db();
}

#[test]
fn a_failing_migration_exits_1_and_names_its_file() {
  let Some(db) = LiveDb::create("failing") else {
    return;
  };
  db.write("0001_init.sql", "CREATE TABLE parts (id BIGINT)");
  db.write(
    "0002_broken.sql",
    "INSERT INTO parts VALUES (1);\nINSERT INTO no_such_table VALUES (1);",
  );
  let (code, out, err) = db.migrate("run", &[]);
  assert_eq!(code, ExitCode::from(1));
  assert!(out.is_empty());
  assert!(
    err.starts_with("error: migration 0002_broken.sql failed: "),
    "{err}"
  );
  assert!(err.contains("no_such_table"), "{err}");
  assert_eq!(
    db.scalar("SELECT COUNT(*) AS n FROM parts"),
    0,
    "rolled back"
  );
  let (_, out, _) = db.migrate("status", &["--json"]);
  let value = json(&out);
  assert_eq!(value["counts"]["applied"], 1);
  assert_eq!(value["counts"]["pending"], 1);
  db.drop_db();
}

#[test]
fn a_held_lock_times_out_with_exit_3() {
  let Some(db) = LiveDb::create("lock") else {
    return;
  };
  db.write("0001_init.sql", "CREATE TABLE locks (id BIGINT)");
  let holder = db.client();
  let tx = db.run(holder.begin());
  db.run(tx.execute(
    "SELECT pg_advisory_xact_lock($1)",
    &[SqlValue::I64(DEFAULT_LOCK_KEY)],
  ));
  let (code, _, err) = db.migrate("run", &["--lock-timeout", "1"]);
  assert_eq!(code, ExitCode::from(3));
  assert!(
    err.starts_with("error: timed out after 1s waiting for the migration lock"),
    "{err}"
  );
  assert!(
    err.contains("hint: another migrator holds the lock"),
    "{err}"
  );
  db.run(tx.rollback());
  assert_eq!(db.migrate("run", &["--lock-timeout", "1"]).0, OK);
  db.drop_db();
}

#[test]
fn the_binary_reads_database_url_and_a_custom_table() {
  let Some(db) = LiveDb::create("binary") else {
    return;
  };
  db.write("0001_init.sql", "CREATE TABLE things (id BIGINT)");
  let dir = s(db.path());
  let out = binary(
    db.path(),
    Some(&db.url),
    &["run", "--dir", dir, "--table", "app_migrations"],
  );
  assert!(out.status.success(), "{out:?}");
  assert_eq!(
    String::from_utf8_lossy(&out.stdout),
    "applied  0001_init.sql\n\nApplied 1 migration.\n"
  );
  assert_eq!(db.scalar("SELECT COUNT(*) AS n FROM app_migrations"), 1);
  let out = binary(
    db.path(),
    Some(&db.url),
    &["check", "--dir", dir, "--table", "app_migrations"],
  );
  assert_eq!(out.status.code(), Some(0), "{out:?}");
  // The default table knows nothing of it: everything is pending there.
  let out = binary(db.path(), Some(&db.url), &["check", "--dir", dir]);
  assert_eq!(out.status.code(), Some(1), "{out:?}");
  db.drop_db();
}
