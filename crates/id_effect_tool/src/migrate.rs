//! `id-effect migrate`: create, apply, list, revert and check a project's SQL migrations on
//! PostgreSQL, with the engine of [`id_effect_sql::migrate`] (the one `migrate::layer` runs at
//! startup: tracking table, advisory lock, one transaction per migration).
//!
//! ```text
//! id-effect migrate new <NAME> [--reversible] [--timestamp | --sequential] [--dir DIR]
//! id-effect migrate run    [DB OPTIONS]
//! id-effect migrate status [DB OPTIONS]
//! id-effect migrate revert [--count N] [DB OPTIONS]
//! id-effect migrate check  [DB OPTIONS]
//!
//! DB OPTIONS: [--dir DIR] [--database-url URL] [--table TABLE] [--lock-timeout SECS] [--json]
//! ```
//!
//! Files are sqlx-cli's: `<VERSION>_<NAME>.sql`, or with `--reversible` the pair
//! `<VERSION>_<NAME>.up.sql` / `.down.sql`, in `--dir` (default `migrations`). `new` numbers
//! after the directory: short sequential versions (the `service` template's `0001_init.sql`)
//! continue at their width (`0002_…`); an empty directory, or one of timestamps, gets the UTC time
//! `YYYYMMDDHHMMSS`. `--timestamp` / `--sequential` force a scheme.
//!
//! The database is `--database-url`, else `DATABASE_URL` from the environment, else from `./.env`;
//! it must be a `postgres://` URL, and is never printed.
//!
//! | Exit | Meaning |
//! |---|---|
//! | `0` | done (`check`: up to date; `status` exits 0 whatever the states) |
//! | `1` | the migrations need attention: pending (`check`), changed after they were applied or missing from DIR (drift), a failing script, no down script to revert, an unreadable or invalid DIR, or `new` could not write |
//! | `2` | usage: bad arguments, no database URL, not a `postgres://` URL |
//! | `3` | the database: cannot connect, the migration lock stayed taken, a bookkeeping query failed |
//!
//! Errors go to stderr as `error: …` (plus a `hint: …`); with `--json`, to stdout as
//! `{"ok": false, "error": {"kind", "message", …}}`.

mod new;
mod report;

use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use clap::{Args, Subcommand};
use id_effect::{Cause, Effect, Exit};
use id_effect_sql::SqlClient;
use id_effect_sql::migrate::{MigrateError, Migrator};
use id_effect_sql::pg::{PgPoolConfig, PgSqlClient, pg_pool_from_config};
use id_effect_tokio::TokioRuntime;
use serde_json::{Value, json};

use new::Numbering;

/// Exit code: the migrations need attention (pending, drift, a failing script, …).
pub const EXIT_ATTENTION: u8 = 1;
/// Exit code: a usage error (arguments, no or a non-PostgreSQL database URL).
pub const EXIT_USAGE: u8 = 2;
/// Exit code: the database (connection, lock timeout, bookkeeping).
pub const EXIT_DATABASE: u8 = 3;

/// Arguments of `id-effect migrate`.
#[derive(Debug, Args)]
pub struct MigrateArgs {
  /// What to do.
  #[command(subcommand)]
  pub command: MigrateCommand,
}

/// The `id-effect migrate` subcommands.
#[derive(Debug, Subcommand)]
pub enum MigrateCommand {
  /// Create the next migration in DIR: `<VERSION>_<NAME>.sql`, or an `.up.sql` / `.down.sql`
  /// pair with --reversible.
  New(NewMigrationArgs),
  /// Apply every pending migration, oldest first, each in its own transaction.
  Run(DbArgs),
  /// List every migration: applied, pending, drifted (changed since applied) or missing
  /// (applied, not in DIR). Exits 0 whatever the states; `check` is the gate.
  Status(DbArgs),
  /// Revert the last applied migrations with their down scripts, newest first.
  Revert(RevertArgs),
  /// Exit 1 when a migration is pending, changed since it was applied, or missing (for CI).
  Check(DbArgs),
}

/// Arguments of `id-effect migrate new`.
#[derive(Debug, Args)]
pub struct NewMigrationArgs {
  /// What the migration does (ASCII letters, digits, `_`, `-`, spaces; written with `_`).
  #[arg(value_parser = new::parse_name)]
  pub name: String,
  /// Write an `.up.sql` / `.down.sql` pair, so `migrate revert` can undo it.
  #[arg(short, long)]
  pub reversible: bool,
  /// Number with the UTC time (`YYYYMMDDHHMMSS`) even after sequential migrations.
  #[arg(long, conflicts_with = "sequential")]
  pub timestamp: bool,
  /// Number with the newest version + 1 (`0001` in an empty directory).
  #[arg(long)]
  pub sequential: bool,
  /// The migrations directory (created when missing).
  #[arg(long, value_name = "DIR", default_value = "migrations")]
  pub dir: PathBuf,
}

impl NewMigrationArgs {
  fn numbering(&self) -> Numbering {
    match (self.timestamp, self.sequential) {
      (true, _) => Numbering::Timestamp,
      (_, true) => Numbering::Sequential,
      _ => Numbering::Infer,
    }
  }
}

/// The options of every command that talks to the database.
#[derive(Debug, Args)]
pub struct DbArgs {
  /// The migrations directory.
  #[arg(long, value_name = "DIR", default_value = "migrations")]
  pub dir: PathBuf,
  /// The PostgreSQL URL (default: DATABASE_URL from the environment, else from ./.env).
  #[arg(long, value_name = "URL")]
  pub database_url: Option<String>,
  /// The tracking table, `name` or `schema.name` (default `_id_effect_migrations`).
  #[arg(long, value_name = "TABLE", value_parser = parse_table)]
  pub table: Option<String>,
  /// Seconds to wait for the migration lock (default 60).
  #[arg(long, value_name = "SECS")]
  pub lock_timeout: Option<u64>,
  /// Print one JSON object (errors too, on stdout).
  #[arg(long)]
  pub json: bool,
}

/// Arguments of `id-effect migrate revert`.
#[derive(Debug, Args)]
pub struct RevertArgs {
  /// How many applied migrations to revert, newest first.
  #[arg(long, value_name = "N", default_value_t = 1, value_parser = clap::value_parser!(u64).range(1..))]
  pub count: u64,
  /// The database options.
  #[command(flatten)]
  pub db: DbArgs,
}

/// Why a `migrate` command failed; decides the exit code.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Failure {
  /// No usable database URL (exit 2).
  Usage(String),
  /// Connecting, or the runtime (exit 3).
  Database(String),
  /// The engine (exit 1 or 3, by kind).
  Migrate(MigrateError),
  /// `new` could not write (exit 1).
  Write(String),
}

impl Failure {
  fn code(&self) -> u8 {
    match self {
      Self::Usage(_) => EXIT_USAGE,
      Self::Database(_)
      | Self::Migrate(MigrateError::LockTimeout { .. } | MigrateError::Sql(_)) => EXIT_DATABASE,
      Self::Migrate(_) | Self::Write(_) => EXIT_ATTENTION,
    }
  }

  fn kind(&self) -> &'static str {
    match self {
      Self::Usage(_) => "usage",
      Self::Database(_) => "database",
      Self::Write(_) => "write",
      Self::Migrate(e) => match e {
        MigrateError::Source(_) => "source",
        MigrateError::ChecksumMismatch { .. } => "checksum_mismatch",
        MigrateError::Missing { .. } => "missing",
        MigrateError::Pending { .. } => "pending",
        MigrateError::Failed { .. } => "failed",
        MigrateError::LockTimeout { .. } => "lock_timeout",
        MigrateError::MissingDown { .. } => "missing_down",
        _ => "sql",
      },
    }
  }

  fn message(&self) -> String {
    match self {
      Self::Usage(m) | Self::Database(m) | Self::Write(m) => m.clone(),
      Self::Migrate(e) => e.to_string(),
    }
  }

  fn hint(&self) -> Option<&'static str> {
    let Self::Migrate(e) = self else {
      return None;
    };
    match e {
      MigrateError::Pending { .. } => Some("apply them with `id-effect migrate run`"),
      MigrateError::ChecksumMismatch { .. } => Some(
        "restore the applied script; change the schema with a new migration (`id-effect migrate new`)",
      ),
      MigrateError::Missing { .. } => {
        Some("restore the migration file, or point --dir / --table at the right place")
      }
      MigrateError::MissingDown { .. } => {
        Some("add its .down.sql script, or undo it with a new migration")
      }
      MigrateError::LockTimeout { .. } => {
        Some("another migrator holds the lock; retry, or raise --lock-timeout")
      }
      _ => None,
    }
  }

  /// `{"ok": false, "error": {"kind", "message", "version"?, "file"?, "versions"?}}`.
  fn to_json(&self) -> Value {
    let mut error = json!({ "kind": self.kind(), "message": self.message() });
    let (version, file, versions) = match self {
      Self::Migrate(
        MigrateError::ChecksumMismatch { version, file }
        | MigrateError::Failed { version, file, .. }
        | MigrateError::MissingDown { version, file },
      ) => (Some(*version), Some(file.clone()), None),
      Self::Migrate(MigrateError::Missing { version }) => (Some(*version), None, None),
      Self::Migrate(MigrateError::Pending { versions }) => (None, None, Some(versions.clone())),
      _ => (None, None, None),
    };
    if let Some(version) = version {
      error["version"] = json!(version);
    }
    if let Some(file) = file {
      error["file"] = json!(file);
    }
    if let Some(versions) = versions {
      error["versions"] = json!(versions);
    }
    json!({ "ok": false, "error": error })
  }
}

/// Runs `id-effect migrate`, writing to `out` and `err`; see the [module docs](self) for the exit
/// codes.
pub fn run(args: &MigrateArgs, out: &mut dyn Write, err: &mut dyn Write) -> ExitCode {
  let json = match &args.command {
    MigrateCommand::New(_) => false,
    MigrateCommand::Run(db) | MigrateCommand::Status(db) | MigrateCommand::Check(db) => db.json,
    MigrateCommand::Revert(revert) => revert.db.json,
  };
  match execute(&args.command) {
    Ok(text) => {
      let _ = write!(out, "{text}");
      ExitCode::SUCCESS
    }
    Err(failure) => {
      if json {
        let _ = writeln!(out, "{}", failure.to_json());
      } else {
        let _ = writeln!(err, "error: {}", failure.message());
        if let Some(hint) = failure.hint() {
          let _ = writeln!(err, "hint: {hint}");
        }
      }
      ExitCode::from(failure.code())
    }
  }
}

fn execute(command: &MigrateCommand) -> Result<String, Failure> {
  match command {
    MigrateCommand::New(args) => new::create(
      &args.dir,
      &args.name,
      args.reversible,
      args.numbering(),
      jiff::Timestamp::now(),
    )
    .map(|paths| report::created(&paths)),
    MigrateCommand::Run(db) => {
      let (session, m) = db.open()?;
      let applied = session.exec(m.run_on(session.client()))?;
      Ok(report::applied(&m, &applied, db.json))
    }
    MigrateCommand::Status(db) => {
      let (session, m) = db.open()?;
      let statuses = session.exec(m.status_on(session.client()))?;
      Ok(report::status(&db.dir, &m, &statuses, db.json))
    }
    MigrateCommand::Revert(args) => {
      let (session, m) = args.db.open()?;
      let n = usize::try_from(args.count).unwrap_or(usize::MAX);
      let reverted = session.exec(m.revert_on(session.client(), n))?;
      Ok(report::reverted(&m, &reverted, args.db.json))
    }
    MigrateCommand::Check(db) => {
      let (session, m) = db.open()?;
      session.exec(m.check_on(session.client()))?;
      Ok(report::checked(&m, db.json))
    }
  }
}

impl DbArgs {
  /// The database URL, the migrations of `--dir`, and a connection.
  fn open(&self) -> Result<(Session, Migrator), Failure> {
    let dotenv = fs::read_to_string(".env").ok();
    let url = database_url(
      self.database_url.as_deref(),
      std::env::var("DATABASE_URL").ok(),
      dotenv.as_deref(),
    )?;
    let mut migrator = Migrator::from_dir(&self.dir).map_err(Failure::Migrate)?;
    if let Some(table) = &self.table {
      migrator = migrator.with_table(table);
    }
    if let Some(secs) = self.lock_timeout {
      migrator = migrator.with_lock_timeout(Duration::from_secs(secs));
    }
    Ok((Session::connect(&url)?, migrator))
  }
}

/// A database URL and where it came from (printed instead of the URL, which may hold a
/// password).
#[derive(Debug, PartialEq, Eq)]
struct DatabaseUrl {
  value: String,
  source: &'static str,
}

/// `--database-url`, else a non-empty `DATABASE_URL` from the environment, else from `.env`; it
/// must be `postgres://` or `postgresql://`.
fn database_url(
  flag: Option<&str>,
  env: Option<String>,
  dotenv: Option<&str>,
) -> Result<DatabaseUrl, Failure> {
  let found = flag
    .map(|v| (v.to_string(), "--database-url"))
    .or_else(|| {
      env
        .filter(|v| !v.trim().is_empty())
        .map(|v| (v, "DATABASE_URL (environment)"))
    })
    .or_else(|| {
      dotenv
        .and_then(|text| dotenv_value(text, "DATABASE_URL"))
        .filter(|v| !v.is_empty())
        .map(|v| (v, "DATABASE_URL (.env)"))
    });
  let Some((value, source)) = found else {
    return Err(Failure::Usage(
      "no database: pass --database-url, or set DATABASE_URL in the environment or ./.env".into(),
    ));
  };
  let scheme = value.split_once("://").map(|(s, _)| s.to_ascii_lowercase());
  if !matches!(scheme.as_deref(), Some("postgres" | "postgresql")) {
    return Err(Failure::Usage(format!(
      "{source} is not a postgres:// URL (id-effect migrate runs on PostgreSQL)"
    )));
  }
  Ok(DatabaseUrl { value, source })
}

/// `key`'s value in a dotenv file: the first `KEY=value` line (an `export ` prefix, matching
/// quotes and an unquoted ` #` comment dropped).
fn dotenv_value(text: &str, key: &str) -> Option<String> {
  text.lines().find_map(|line| {
    let line = line.trim();
    let line = line.strip_prefix("export ").unwrap_or(line);
    let (k, value) = line.split_once('=')?;
    if k.trim() != key {
      return None;
    }
    let value = value.trim();
    let quoted = ['"', '\''].into_iter().find_map(|q| {
      value
        .strip_prefix(q)
        .and_then(|rest| rest.split_once(q))
        .map(|(inner, _)| inner)
    });
    let value =
      quoted.unwrap_or_else(|| value.split_once(" #").map_or(value, |(v, _)| v.trim_end()));
    Some(value.to_string())
  })
}

/// `--table`: one or two `.`-separated identifiers of ASCII letters, digits and `_`, not starting
/// with a digit (what `Migrator::with_table` accepts).
fn parse_table(table: &str) -> Result<String, String> {
  let ident = |part: &str| {
    part
      .chars()
      .next()
      .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
      && part.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
  };
  let parts: Vec<&str> = table.split('.').collect();
  if parts.len() <= 2 && parts.iter().all(|p| ident(p)) {
    Ok(table.to_string())
  } else {
    Err("expected `name` or `schema.name` of ASCII letters, digits and `_`".into())
  }
}

/// A tokio runtime and a PostgreSQL client on it; closes the pool when dropped.
struct Session {
  rt: TokioRuntime,
  client: Arc<dyn SqlClient>,
}

impl Session {
  fn connect(url: &DatabaseUrl) -> Result<Self, Failure> {
    let rt = TokioRuntime::new_multi_thread()
      .map_err(|e| Failure::Database(format!("cannot start the tokio runtime: {e}")))?;
    let config = PgPoolConfig::from_url(url.value.clone()).with_max_size(2);
    let pool = rt.block_on(pg_pool_from_config(config)).map_err(|e| {
      Failure::Database(format!(
        "cannot connect to the database ({}): {}",
        url.source, e.0
      ))
    })?;
    let client: Arc<dyn SqlClient> = Arc::new(PgSqlClient::new(pool));
    Ok(Self { rt, client })
  }

  fn client(&self) -> Arc<dyn SqlClient> {
    Arc::clone(&self.client)
  }

  /// Runs `eff` to its end.
  fn exec<A: Send + 'static>(&self, eff: Effect<A, MigrateError>) -> Result<A, Failure> {
    exit_result(self.rt.block_on(self.rt.run(eff)))
  }
}

impl Drop for Session {
  fn drop(&mut self) {
    let _ = self.rt.block_on(self.rt.run(self.client.close()));
  }
}

/// A typed failure is a [`Failure::Migrate`]; a defect or an interruption, a database failure.
fn exit_result<A>(exit: Exit<A, MigrateError>) -> Result<A, Failure> {
  match exit {
    Exit::Success(a) => Ok(a),
    Exit::Failure(Cause::Fail(e)) => Err(Failure::Migrate(e)),
    Exit::Failure(cause) => Err(Failure::Database(format!(
      "the migration did not finish: {cause:?}"
    ))),
  }
}

#[cfg(test)]
mod tests {
  use id_effect_sql::SqlError;

  use super::*;

  fn url(
    flag: Option<&str>,
    env: Option<&str>,
    dotenv: Option<&str>,
  ) -> Result<DatabaseUrl, Failure> {
    database_url(flag, env.map(str::to_string), dotenv)
  }

  #[test]
  fn the_url_comes_from_the_flag_the_environment_then_dotenv() {
    let pg = "postgres://u:secret@db/app";
    let got = |r: Result<DatabaseUrl, Failure>| r.map(|u| (u.value, u.source));
    assert_eq!(
      got(url(Some(pg), Some("postgres://env"), None)),
      Ok((pg.to_string(), "--database-url"))
    );
    assert_eq!(
      got(url(None, Some(pg), Some("DATABASE_URL=postgres://file"))),
      Ok((pg.to_string(), "DATABASE_URL (environment)"))
    );
    assert_eq!(
      got(url(
        None,
        Some(" "),
        Some("export DATABASE_URL=\"postgresql://f\" # db")
      )),
      Ok(("postgresql://f".to_string(), "DATABASE_URL (.env)"))
    );
    let Err(Failure::Usage(none)) = url(None, None, Some("DATABASE_URL=\n")) else {
      panic!("no URL is a usage error");
    };
    assert!(
      none.starts_with("no database: pass --database-url"),
      "{none}"
    );
    let Err(Failure::Usage(mysql)) = url(Some("mysql://u:secret@db/app"), None, None) else {
      panic!("not PostgreSQL is a usage error");
    };
    assert_eq!(
      mysql,
      "--database-url is not a postgres:// URL (id-effect migrate runs on PostgreSQL)"
    );
    assert!(matches!(
      url(Some("db"), None, None),
      Err(Failure::Usage(_))
    ));
  }

  #[test]
  fn dotenv_values_read_like_a_shell() {
    let text = "# db\nPORT=1\nexport DATABASE_URL = 'postgres://a' \nDATABASE_URL=postgres://b\n";
    assert_eq!(
      dotenv_value(text, "DATABASE_URL").as_deref(),
      Some("postgres://a")
    );
    assert_eq!(dotenv_value("K=v # c\n", "K").as_deref(), Some("v"));
    assert_eq!(
      dotenv_value("K=\"unclosed\n", "K").as_deref(),
      Some("\"unclosed")
    );
    assert_eq!(dotenv_value("not an assignment\n", "K"), None);
  }

  #[test]
  fn tables_are_one_or_two_identifiers() {
    assert_eq!(parse_table("_migrations").as_deref(), Ok("_migrations"));
    assert_eq!(parse_table("app.schema_v2").as_deref(), Ok("app.schema_v2"));
    for bad in ["", "1x", "a.b.c", "a-b", "a.", "x;drop"] {
      assert!(parse_table(bad).is_err(), "{bad}");
    }
  }

  #[test]
  fn every_failure_has_its_exit_code_kind_and_hint() {
    let sql = SqlError::NotConnected;
    let cases = [
      (Failure::Usage("u".into()), 2, "usage", false),
      (Failure::Database("d".into()), 3, "database", false),
      (Failure::Write("w".into()), 1, "write", false),
      (
        Failure::Migrate(MigrateError::Source("s".into())),
        1,
        "source",
        false,
      ),
      (
        Failure::Migrate(MigrateError::ChecksumMismatch {
          version: 1,
          file: "1_a.sql".into(),
        }),
        1,
        "checksum_mismatch",
        true,
      ),
      (
        Failure::Migrate(MigrateError::Missing { version: 1 }),
        1,
        "missing",
        true,
      ),
      (
        Failure::Migrate(MigrateError::Pending { versions: vec![2] }),
        1,
        "pending",
        true,
      ),
      (
        Failure::Migrate(MigrateError::Failed {
          version: 1,
          file: "1_a.sql".into(),
          error: sql.clone(),
        }),
        1,
        "failed",
        false,
      ),
      (
        Failure::Migrate(MigrateError::MissingDown {
          version: 1,
          file: "1_a.sql".into(),
        }),
        1,
        "missing_down",
        true,
      ),
      (
        Failure::Migrate(MigrateError::LockTimeout {
          key: 7,
          timeout: Duration::from_secs(1),
        }),
        3,
        "lock_timeout",
        true,
      ),
      (Failure::Migrate(MigrateError::Sql(sql)), 3, "sql", false),
    ];
    for (failure, code, kind, hinted) in cases {
      assert_eq!(failure.code(), code, "{failure:?}");
      assert_eq!(failure.kind(), kind, "{failure:?}");
      assert_eq!(failure.hint().is_some(), hinted, "{failure:?}");
      let value = failure.to_json();
      assert_eq!(value["ok"], false);
      assert_eq!(value["error"]["kind"], kind);
      assert_eq!(value["error"]["message"], failure.message());
    }
  }

  #[test]
  fn json_errors_carry_versions_and_files() {
    let drift = Failure::Migrate(MigrateError::ChecksumMismatch {
      version: 3,
      file: "3_a.sql".into(),
    });
    assert_eq!(drift.to_json()["error"]["version"], 3);
    assert_eq!(drift.to_json()["error"]["file"], "3_a.sql");
    let missing = Failure::Migrate(MigrateError::Missing { version: 4 }).to_json();
    assert_eq!(missing["error"]["version"], 4);
    assert_eq!(missing["error"]["file"], Value::Null);
    let pending = Failure::Migrate(MigrateError::Pending {
      versions: vec![5, 6],
    });
    assert_eq!(pending.to_json()["error"]["versions"], json!([5, 6]));
  }

  #[test]
  fn defects_and_interruptions_are_database_failures() {
    assert_eq!(exit_result(Exit::<u8, MigrateError>::Success(1)), Ok(1));
    let fail = Exit::<u8, MigrateError>::Failure(Cause::Fail(MigrateError::Missing { version: 1 }));
    assert_eq!(
      exit_result(fail),
      Err(Failure::Migrate(MigrateError::Missing { version: 1 }))
    );
    let die = Exit::<u8, MigrateError>::Failure(Cause::die("boom"));
    let Err(Failure::Database(msg)) = exit_result(die) else {
      panic!("a defect is a database failure");
    };
    assert!(msg.starts_with("the migration did not finish: "), "{msg}");
  }

  #[test]
  fn numbering_follows_the_flags() {
    let args = |timestamp, sequential| NewMigrationArgs {
      name: "x".into(),
      reversible: false,
      timestamp,
      sequential,
      dir: PathBuf::from("m"),
    };
    assert_eq!(args(false, false).numbering(), Numbering::Infer);
    assert_eq!(args(true, false).numbering(), Numbering::Timestamp);
    assert_eq!(args(false, true).numbering(), Numbering::Sequential);
  }
}
