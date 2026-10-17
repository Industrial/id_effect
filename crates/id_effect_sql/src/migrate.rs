//! **Migrations**: versioned SQL scripts, applied in order and recorded in a tracking table, as
//! [`Effect`]s and as a startup [`Layer`] (feature `migrate`).
//!
//! A [`Migrator`] holds the migrations of a directory ([`Migrator::from_dir`]), of
//! `(file name, script)` pairs ([`Migrator::from_files`], e.g. with `include_str!`), or of a
//! sqlx `Migrator` built at compile time by `sqlx::migrate!` ([`Migrator::from_sqlx`]), so a
//! binary does not need the directory at run time. File names follow sqlx-cli:
//! `<VERSION>_<DESCRIPTION>.sql`, or a reversible pair `<VERSION>_<DESCRIPTION>.up.sql` /
//! `.down.sql` (`0001_init.sql`, `20260101120000_add_users.up.sql`).
//!
//! | Operation | Effect |
//! |---|---|
//! | [`run`](Migrator::run_on) | applies every pending migration, ascending; returns the versions it applied |
//! | [`status`](Migrator::status_on) | every migration with its [`MigrationState`] and checksums |
//! | [`revert(n)`](Migrator::revert_on) | runs the down scripts of the last `n` applied migrations |
//! | [`check`](Migrator::check_on) | fails when a migration is pending, changed or missing |
//!
//! Each operation exists on an explicit client (`*_on(Arc<dyn SqlClient>)`) and on the
//! [`SqlClientService`] of the environment ([`Migrator::run`], …). [`run`](fn@run) and
//! [`layer`](fn@layer) read a directory when they run; [`Migrator::layer`] runs the migrations
//! when the layer is built and provides [`MigrationsApplied`].
//!
//! # Guarantees (PostgreSQL)
//!
//! - **Tracked**: applied versions are rows of the tracking table ([`DEFAULT_TABLE`]: version,
//!   description, SHA-384 checksum of the up script, `installed_on`, `execution_ms`), created on
//!   first use.
//! - **One transaction per migration**: a migration's statements and its tracking row commit
//!   together; a failing statement rolls the migration back and stops the run
//!   ([`MigrateError::Failed`] names the file and carries the database error). Migrations applied
//!   before it stay applied.
//! - **One migrator at a time**: every migration transaction first takes the transaction-scoped
//!   advisory lock [`DEFAULT_LOCK_KEY`] (`pg_try_advisory_xact_lock`, polled until
//!   [`DEFAULT_LOCK_TIMEOUT`], then [`MigrateError::LockTimeout`]), then re-reads whether its
//!   version is applied. Replicas starting together each apply a migration at most once.
//! - **Drift is an error**: an applied migration whose script changed
//!   ([`MigrateError::ChecksumMismatch`]) or that is no longer in the source
//!   ([`MigrateError::Missing`]) stops `run`, `revert` and `check` before anything is applied.
//!
//! Scripts are split into statements (strings, quoted identifiers, dollar-quoted bodies and
//! comments are respected), because drivers prepare one statement at a time. `-- no-transaction`
//! scripts are rejected.
//!
//! The engine runs on [`SqlClient`], so it is unit-testable on
//! [`TestSqlClient`](crate::TestSqlClient); its SQL (advisory lock, `BYTEA`, `TIMESTAMPTZ`,
//! `$n` placeholders) is PostgreSQL's. sqlx (feature `migrate` = sqlx `migrate`) supplies the
//! source model: file naming, checksums and `sqlx::migrate!` embedding, so migration directories
//! are interchangeable with sqlx-cli's.
//!
//! ```rust,no_run
//! use std::sync::Arc;
//!
//! use id_effect::{Effect, fail};
//! use id_effect_sql::SqlClient;
//! use id_effect_sql::migrate::{MigrateError, Migrator};
//!
//! /// Applies `./migrations`, then lists what is applied.
//! fn migrate(db: Arc<dyn SqlClient>) -> Effect<usize, MigrateError> {
//!   let migrator = match Migrator::from_dir("migrations") {
//!     Ok(m) => m,
//!     Err(e) => return fail(e),
//!   };
//!   let status = migrator.clone();
//!   migrator
//!     .run_on(Arc::clone(&db))
//!     .flat_map(move |_applied| status.status_on(db))
//!     .map(|all| all.iter().filter(|s| s.state.is_applied()).count())
//! }
//! ```
//!
//! At startup, as a layer over the client layer (`pg::PgSqlClient::layer(config)` in production):
//!
//! ```rust,no_run
//! use id_effect::{Effect, Layer};
//! use id_effect_sql::migrate::{self, MigrateError, MigratedEnv};
//! use id_effect_sql::{SqlClientEnv, SqlError};
//!
//! #[derive(Debug)]
//! enum StartupError {
//!   Sql(SqlError),
//!   Migrate(MigrateError),
//! }
//! impl From<SqlError> for StartupError {
//!   fn from(e: SqlError) -> Self {
//!     StartupError::Sql(e)
//!   }
//! }
//! impl From<MigrateError> for StartupError {
//!   fn from(e: MigrateError) -> Self {
//!     StartupError::Migrate(e)
//!   }
//! }
//!
//! fn app(
//!   client: Layer<SqlClientEnv, StartupError>,
//! ) -> Layer<id_effect::HCons<id_effect_sql::SqlClientService, MigratedEnv>, StartupError> {
//!   // Connect, then migrate; the app sees the client and `MigrationsApplied`.
//!   migrate::layer("migrations").provide_merge(client)
//! }
//! # let _ = app;
//! ```
//!
//! Embedded at compile time: `static MIGRATIONS: sqlx::migrate::Migrator =
//! sqlx::migrate!("./migrations");` (sqlx with features `macros` and `migrate`), then
//! `Migrator::from_sqlx(&MIGRATIONS)`.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use id_effect::{
  Effect, HCons, HList, HNil, Layer, Selector, fail, hlist, need, sleep, succeed, suspend,
};

use crate::client::{SqlClient, SqlClientEnv, SqlClientService};
use crate::error::SqlError;
use crate::row::SqlRow;
use crate::transaction::{SqlTransaction, with_transaction};
use crate::value::SqlValue;

mod source;
mod split;
#[cfg(test)]
mod tests;

/// The default tracking table.
pub const DEFAULT_TABLE: &str = "_id_effect_migrations";

/// The default advisory lock key (`"idefmigr"` as big-endian ASCII).
pub const DEFAULT_LOCK_KEY: i64 = 0x6964_6566_6d69_6772;

/// How long a migration transaction waits for the advisory lock by default.
pub const DEFAULT_LOCK_TIMEOUT: Duration = Duration::from_secs(60);

/// Interval between two attempts at the advisory lock.
const LOCK_POLL: Duration = Duration::from_millis(100);

/// Takes the transaction-scoped advisory lock if it is free.
const LOCK_SQL: &str = "SELECT pg_try_advisory_xact_lock($1) AS locked";

/// One migration: an up script and, when reversible, a down script.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Migration {
  version: i64,
  description: String,
  file: String,
  sql: String,
  checksum: Vec<u8>,
  down: Option<(String, String)>,
}

impl Migration {
  /// The version (the file name's numeric prefix).
  pub fn version(&self) -> i64 {
    self.version
  }

  /// The description (the file name after the version, `_` read as a space).
  pub fn description(&self) -> &str {
    &self.description
  }

  /// The up script's file name (`0001_init.sql`, `0002_users.up.sql`).
  pub fn file(&self) -> &str {
    &self.file
  }

  /// The up script.
  pub fn sql(&self) -> &str {
    &self.sql
  }

  /// SHA-384 of the up script (sqlx's checksum), recorded when the migration is applied.
  pub fn checksum(&self) -> &[u8] {
    &self.checksum
  }

  /// [`Self::checksum`] as lowercase hex.
  pub fn checksum_hex(&self) -> String {
    hex(&self.checksum)
  }

  /// The down script's file name, when the migration is reversible.
  pub fn down_file(&self) -> Option<&str> {
    self.down.as_ref().map(|(file, _)| file.as_str())
  }

  /// The down script, when the migration is reversible.
  pub fn down_sql(&self) -> Option<&str> {
    self.down.as_ref().map(|(_, sql)| sql.as_str())
  }

  /// Whether a down script exists ([`Migrator::revert_on`] can revert it).
  pub fn is_reversible(&self) -> bool {
    self.down.is_some()
  }
}

/// Migration failures. Non-exhaustive: match it with a wildcard arm.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum MigrateError {
  /// The migration source is unreadable or invalid (directory, file name, duplicate version,
  /// down script without an up script, `-- no-transaction`).
  Source(String),
  /// An applied migration's script changed since it was applied.
  ChecksumMismatch {
    /// The migration's version.
    version: i64,
    /// Its up script's file name.
    file: String,
  },
  /// A migration recorded as applied is not in the source.
  Missing {
    /// The recorded version.
    version: i64,
  },
  /// [`Migrator::check_on`]: migrations are not applied yet.
  Pending {
    /// Their versions, ascending.
    versions: Vec<i64>,
  },
  /// A statement of a migration script failed; that migration was rolled back and the run
  /// stopped.
  Failed {
    /// The migration's version.
    version: i64,
    /// The script's file name (the down script's when reverting).
    file: String,
    /// The database error.
    error: SqlError,
  },
  /// The advisory lock stayed taken (another migrator is running) for the whole timeout.
  LockTimeout {
    /// The advisory lock key.
    key: i64,
    /// How long it was waited for.
    timeout: Duration,
  },
  /// [`Migrator::revert_on`]: a migration to revert has no down script.
  MissingDown {
    /// The migration's version.
    version: i64,
    /// Its up script's file name.
    file: String,
  },
  /// Bookkeeping failed (connection, tracking table, lock query).
  Sql(SqlError),
}

impl fmt::Display for MigrateError {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    match self {
      MigrateError::Source(msg) => write!(f, "migration source: {msg}"),
      MigrateError::ChecksumMismatch { version, file } => write!(
        f,
        "migration {version} ({file}) changed after it was applied: checksum mismatch"
      ),
      MigrateError::Missing { version } => write!(
        f,
        "migration {version} is applied in the database but missing from the source"
      ),
      MigrateError::Pending { versions } => {
        let list: Vec<String> = versions.iter().map(i64::to_string).collect();
        write!(
          f,
          "{} pending migration(s): {}",
          versions.len(),
          list.join(", ")
        )
      }
      MigrateError::Failed { file, error, .. } => write!(f, "migration {file} failed: {error}"),
      MigrateError::LockTimeout { key, timeout } => write!(
        f,
        "timed out after {timeout:?} waiting for the migration lock (advisory lock {key})"
      ),
      MigrateError::MissingDown { file, .. } => write!(
        f,
        "migration {file} cannot be reverted: it has no down script (.down.sql)"
      ),
      MigrateError::Sql(e) => write!(f, "migration bookkeeping failed: {e}"),
    }
  }
}

impl std::error::Error for MigrateError {
  fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
    match self {
      MigrateError::Failed { error, .. } | MigrateError::Sql(error) => Some(error),
      _ => None,
    }
  }
}

impl From<SqlError> for MigrateError {
  fn from(e: SqlError) -> Self {
    MigrateError::Sql(e)
  }
}

/// Where a migration stands, from [`Migrator::status_on`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MigrationState {
  /// In the source, not applied.
  Pending,
  /// Applied with the source's checksum.
  Applied,
  /// Applied, but the script changed since (checksum mismatch).
  Drifted,
  /// Applied, but not in the source.
  Missing,
}

impl MigrationState {
  /// Whether the migration is recorded as applied (`Applied`, `Drifted` or `Missing`).
  pub fn is_applied(self) -> bool {
    self != MigrationState::Pending
  }
}

/// One line of [`Migrator::status_on`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MigrationStatus {
  /// The version.
  pub version: i64,
  /// The description (the recorded one for a `Missing` migration).
  pub description: String,
  /// Where it stands.
  pub state: MigrationState,
  /// The source's checksum (the recorded one for a `Missing` migration).
  pub checksum: Vec<u8>,
  /// The recorded checksum, when applied.
  pub applied_checksum: Option<Vec<u8>>,
  /// When it was applied.
  pub applied_at: Option<jiff::Timestamp>,
  /// Whether the source has a down script.
  pub reversible: bool,
}

impl MigrationStatus {
  /// [`Self::checksum`] as lowercase hex.
  pub fn checksum_hex(&self) -> String {
    hex(&self.checksum)
  }
}

/// Capability provided by [`Migrator::layer`] / [`layer`](fn@layer): the versions the startup
/// run applied (empty when the schema was up to date).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MigrationsApplied(pub Vec<i64>);

/// The environment of the migration layers.
pub type MigratedEnv = HCons<MigrationsApplied, HNil>;

/// A row of the tracking table.
#[derive(Clone, Debug)]
struct AppliedRow {
  version: i64,
  description: String,
  checksum: Vec<u8>,
  applied_at: jiff::Timestamp,
}

/// A set of migrations and how to apply them (tracking table, advisory lock, lock timeout).
/// Cloning is cheap.
#[derive(Clone, Debug)]
pub struct Migrator {
  migrations: Arc<[Migration]>,
  table: Arc<str>,
  lock_key: i64,
  lock_timeout: Duration,
}

impl Migrator {
  fn new(migrations: Vec<Migration>) -> Self {
    Self {
      migrations: migrations.into(),
      table: Arc::from(DEFAULT_TABLE),
      lock_key: DEFAULT_LOCK_KEY,
      lock_timeout: DEFAULT_LOCK_TIMEOUT,
    }
  }

  /// The migrations in directory `dir` (read now; files that are not migrations are ignored).
  pub fn from_dir(dir: impl AsRef<Path>) -> Result<Self, MigrateError> {
    source::from_dir(dir.as_ref()).map(Self::new)
  }

  /// The migrations among `(file name, script)` pairs, named as in a directory (embed them with
  /// `include_str!`).
  pub fn from_files<I, N, S>(files: I) -> Result<Self, MigrateError>
  where
    I: IntoIterator<Item = (N, S)>,
    N: Into<String>,
    S: Into<String>,
  {
    source::from_files(files.into_iter().map(|(n, s)| (n.into(), s.into()))).map(Self::new)
  }

  /// The migrations of a sqlx `Migrator`, e.g. `static M: Migrator = sqlx::migrate!()` embedded
  /// at compile time. Checksums are the same as from the directory.
  pub fn from_sqlx(migrator: &sqlx::migrate::Migrator) -> Result<Self, MigrateError> {
    source::from_sqlx(migrator).map(Self::new)
  }

  /// Track applied migrations in `table` (`name` or `schema.name`) instead of [`DEFAULT_TABLE`].
  ///
  /// # Panics
  ///
  /// When `table` is not one or two `.`-separated identifiers of ASCII letters, digits and `_`
  /// (not starting with a digit).
  pub fn with_table(mut self, table: impl Into<String>) -> Self {
    let table = table.into();
    assert!(
      valid_table(&table),
      "Migrator::with_table: `{table}` is not a [schema.]table name of ASCII letters, digits and _"
    );
    self.table = table.into();
    self
  }

  /// Use advisory lock `key` instead of [`DEFAULT_LOCK_KEY`] (migrators sharing a database and a
  /// tracking table must use the same key).
  pub fn with_lock_key(mut self, key: i64) -> Self {
    self.lock_key = key;
    self
  }

  /// Wait up to `timeout` for the advisory lock instead of [`DEFAULT_LOCK_TIMEOUT`].
  pub fn with_lock_timeout(mut self, timeout: Duration) -> Self {
    self.lock_timeout = timeout;
    self
  }

  /// The migrations, ascending by version.
  pub fn migrations(&self) -> &[Migration] {
    &self.migrations
  }

  /// The tracking table.
  pub fn table(&self) -> &str {
    &self.table
  }

  /// The advisory lock key.
  pub fn lock_key(&self) -> i64 {
    self.lock_key
  }

  /// How long a migration transaction waits for the advisory lock.
  pub fn lock_timeout(&self) -> Duration {
    self.lock_timeout
  }

  /// Applies every pending migration on `client`, ascending, each in its own transaction under
  /// the advisory lock; succeeds with the versions this run applied. Fails before applying
  /// anything when an applied migration drifted or is missing, and stops at the first failing
  /// migration (rolled back; the earlier ones stay applied).
  pub fn run_on(&self, client: Arc<dyn SqlClient>) -> Effect<Vec<i64>, MigrateError> {
    let this = self.clone();
    self.applied(Arc::clone(&client)).flat_map(move |applied| {
      if let Err(e) = this.validate(&applied) {
        return fail(e);
      }
      let pending = this.pending(&applied);
      this.each(pending, move |this, m| {
        this.apply_one(Arc::clone(&client), m)
      })
    })
  }

  /// Every migration of the source and every applied one, ascending, with its state and
  /// checksums.
  pub fn status_on(
    &self,
    client: Arc<dyn SqlClient>,
  ) -> Effect<Vec<MigrationStatus>, MigrateError> {
    let this = self.clone();
    self
      .applied(client)
      .map(move |applied| this.statuses(&applied))
  }

  /// Reverts the last `n` applied migrations (fewer when fewer are applied), newest first, each
  /// in its own transaction under the advisory lock; succeeds with the reverted versions. Fails
  /// before reverting anything when one of them has no down script
  /// ([`MigrateError::MissingDown`]) or an applied migration drifted or is missing.
  pub fn revert_on(&self, client: Arc<dyn SqlClient>, n: usize) -> Effect<Vec<i64>, MigrateError> {
    let this = self.clone();
    self.applied(Arc::clone(&client)).flat_map(move |applied| {
      let targets = match this
        .validate(&applied)
        .and_then(|()| this.revert_targets(&applied, n))
      {
        Ok(targets) => targets,
        Err(e) => return fail(e),
      };
      this.each(targets, move |this, t| {
        this.revert_one(Arc::clone(&client), t)
      })
    })
  }

  /// Succeeds when the database is up to date with the source; fails with
  /// [`MigrateError::Pending`], [`MigrateError::ChecksumMismatch`] or [`MigrateError::Missing`]
  /// otherwise (for CI: "migrations applied and unchanged").
  pub fn check_on(&self, client: Arc<dyn SqlClient>) -> Effect<(), MigrateError> {
    let this = self.clone();
    self.applied(client).flat_map(move |applied| {
      ready(this.validate(&applied).and_then(|()| {
        let versions: Vec<i64> = this
          .pending(&applied)
          .iter()
          .map(Migration::version)
          .collect();
        if versions.is_empty() {
          Ok(())
        } else {
          Err(MigrateError::Pending { versions })
        }
      }))
    })
  }

  /// [`Self::run_on`] on the [`SqlClientService`] in `R`.
  pub fn run<R, I>(&self) -> Effect<Vec<i64>, MigrateError, R>
  where
    R: Selector<SqlClientService, I> + HList,
  {
    let this = self.clone();
    need::<SqlClientService>().flat_map(move |c| this.run_on(Arc::clone(&c.0)).narrow())
  }

  /// [`Self::status_on`] on the [`SqlClientService`] in `R`.
  pub fn status<R, I>(&self) -> Effect<Vec<MigrationStatus>, MigrateError, R>
  where
    R: Selector<SqlClientService, I> + HList,
  {
    let this = self.clone();
    need::<SqlClientService>().flat_map(move |c| this.status_on(Arc::clone(&c.0)).narrow())
  }

  /// [`Self::revert_on`] on the [`SqlClientService`] in `R`.
  pub fn revert<R, I>(&self, n: usize) -> Effect<Vec<i64>, MigrateError, R>
  where
    R: Selector<SqlClientService, I> + HList,
  {
    let this = self.clone();
    need::<SqlClientService>().flat_map(move |c| this.revert_on(Arc::clone(&c.0), n).narrow())
  }

  /// [`Self::check_on`] on the [`SqlClientService`] in `R`.
  pub fn check<R, I>(&self) -> Effect<(), MigrateError, R>
  where
    R: Selector<SqlClientService, I> + HList,
  {
    let this = self.clone();
    need::<SqlClientService>().flat_map(move |c| this.check_on(Arc::clone(&c.0)).narrow())
  }

  /// The startup [`Layer`]: building it runs [`Self::run`] on the [`SqlClientService`] it needs
  /// (a failure fails the build with `E::from(err)`) and provides [`MigrationsApplied`]. Put it
  /// over the client layer with `provide_merge` (keeps the client) or `provide_to`.
  pub fn layer<E>(&self) -> Layer<MigratedEnv, E, SqlClientEnv>
  where
    E: From<MigrateError> + Send + 'static,
  {
    let this = self.clone();
    Layer::effect(move || {
      this
        .run::<SqlClientEnv, _>()
        .map(|applied| hlist![MigrationsApplied(applied)])
        .map_err(E::from)
    })
  }

  // ---- engine ------------------------------------------------------------------------------

  fn create_table_sql(&self) -> String {
    format!(
      "CREATE TABLE IF NOT EXISTS {} (version BIGINT PRIMARY KEY, description TEXT NOT NULL, \
       checksum BYTEA NOT NULL, installed_on TIMESTAMPTZ NOT NULL DEFAULT now(), \
       execution_ms BIGINT NOT NULL)",
      self.table
    )
  }

  fn select_applied_sql(&self) -> String {
    format!(
      "SELECT version, description, checksum, installed_on FROM {} ORDER BY version",
      self.table
    )
  }

  fn recorded_sql(&self) -> String {
    format!("SELECT checksum FROM {} WHERE version = $1", self.table)
  }

  fn insert_sql(&self) -> String {
    format!(
      "INSERT INTO {} (version, description, checksum, execution_ms) VALUES ($1, $2, $3, $4)",
      self.table
    )
  }

  fn delete_sql(&self) -> String {
    format!("DELETE FROM {} WHERE version = $1", self.table)
  }

  /// Creates the tracking table (under the lock) and reads it.
  fn applied(&self, client: Arc<dyn SqlClient>) -> Effect<Vec<AppliedRow>, MigrateError> {
    let (this, create, select) = (
      self.clone(),
      self.create_table_sql(),
      self.select_applied_sql(),
    );
    with_transaction(Arc::clone(&client), move |tx| {
      this.lock(&tx).flat_map(move |()| {
        tx.execute(&create, &[])
          .map(drop)
          .map_err(MigrateError::from)
      })
    })
    .flat_map(move |()| {
      client
        .query(&select, &[])
        .map_err(MigrateError::from)
        .flat_map(|rows| ready(rows.iter().map(applied_row).collect()))
    })
  }

  /// Takes the advisory lock in `tx`, polling until the lock timeout.
  fn lock(&self, tx: &Arc<dyn SqlTransaction>) -> Effect<(), MigrateError> {
    let retries =
      u32::try_from(self.lock_timeout.as_millis() / LOCK_POLL.as_millis()).unwrap_or(u32::MAX);
    try_lock(Arc::clone(tx), self.lock_key, self.lock_timeout, retries)
  }

  /// The checksum recorded for `version`, read in `tx` (after the lock).
  fn recorded(
    &self,
    tx: &Arc<dyn SqlTransaction>,
    version: i64,
  ) -> Effect<Option<Vec<u8>>, MigrateError> {
    tx.query(&self.recorded_sql(), &[SqlValue::I64(version)])
      .map_err(MigrateError::from)
      .flat_map(|rows| {
        ready(
          rows
            .first()
            .map(|r| r.get::<Vec<u8>>("checksum"))
            .transpose()
            .map_err(MigrateError::from),
        )
      })
  }

  /// Applies `m` in one transaction, unless another migrator applied it meanwhile (`None`).
  fn apply_one(
    &self,
    client: Arc<dyn SqlClient>,
    m: Migration,
  ) -> Effect<Option<i64>, MigrateError> {
    let this = self.clone();
    with_transaction(client, move |tx| {
      this.lock(&tx).flat_map(move |()| {
        let insert = this.insert_sql();
        this
          .recorded(&tx, m.version)
          .flat_map(move |recorded| match recorded {
            Some(sum) if sum == m.checksum => succeed(None),
            Some(_) => fail(MigrateError::ChecksumMismatch {
              version: m.version,
              file: m.file,
            }),
            None => {
              let started = Instant::now();
              script(&tx, &m.sql, m.version, &m.file).flat_map(move |()| {
                let ms = i64::try_from(started.elapsed().as_millis()).unwrap_or(i64::MAX);
                let row = [
                  SqlValue::I64(m.version),
                  SqlValue::Text(m.description),
                  SqlValue::Bytes(m.checksum),
                  SqlValue::I64(ms),
                ];
                tx.execute(&insert, &row)
                  .map_err(MigrateError::from)
                  .map(move |_| Some(m.version))
              })
            }
          })
      })
    })
  }

  /// Reverts `(version, down file, down script)` in one transaction, unless another migrator
  /// reverted it meanwhile (`None`).
  fn revert_one(
    &self,
    client: Arc<dyn SqlClient>,
    (version, file, sql): (i64, String, String),
  ) -> Effect<Option<i64>, MigrateError> {
    let this = self.clone();
    with_transaction(client, move |tx| {
      this.lock(&tx).flat_map(move |()| {
        let delete = this.delete_sql();
        this
          .recorded(&tx, version)
          .flat_map(move |recorded| match recorded {
            None => succeed(None),
            Some(_) => script(&tx, &sql, version, &file).flat_map(move |()| {
              tx.execute(&delete, &[SqlValue::I64(version)])
                .map_err(MigrateError::from)
                .map(move |_| Some(version))
            }),
          })
      })
    })
  }

  /// Runs `step` on each item in order, stopping at the first failure; collects the versions.
  fn each<T, F>(&self, items: Vec<T>, step: F) -> Effect<Vec<i64>, MigrateError>
  where
    T: Send + 'static,
    F: Fn(&Migrator, T) -> Effect<Option<i64>, MigrateError> + Send + Sync + 'static,
  {
    let (this, step) = (self.clone(), Arc::new(step));
    items
      .into_iter()
      .fold(succeed(Vec::new()), move |done, item| {
        let (this, step) = (this.clone(), Arc::clone(&step));
        done.flat_map(move |mut out: Vec<i64>| {
          step(&this, item).map(move |version| {
            out.extend(version);
            out
          })
        })
      })
  }

  fn find(&self, version: i64) -> Option<&Migration> {
    self.migrations.iter().find(|m| m.version == version)
  }

  /// Every applied migration is in the source with the recorded checksum.
  fn validate(&self, applied: &[AppliedRow]) -> Result<(), MigrateError> {
    for row in applied {
      match self.find(row.version) {
        None => {
          return Err(MigrateError::Missing {
            version: row.version,
          });
        }
        Some(m) if m.checksum != row.checksum => {
          return Err(MigrateError::ChecksumMismatch {
            version: m.version,
            file: m.file.clone(),
          });
        }
        Some(_) => {}
      }
    }
    Ok(())
  }

  /// The source migrations not applied, ascending.
  fn pending(&self, applied: &[AppliedRow]) -> Vec<Migration> {
    let done: BTreeSet<i64> = applied.iter().map(|row| row.version).collect();
    self
      .migrations
      .iter()
      .filter(|m| !done.contains(&m.version))
      .cloned()
      .collect()
  }

  /// The down scripts of the last `n` applied migrations, newest first.
  fn revert_targets(
    &self,
    applied: &[AppliedRow],
    n: usize,
  ) -> Result<Vec<(i64, String, String)>, MigrateError> {
    applied
      .iter()
      .rev()
      .take(n)
      .map(|row| {
        let m = self.find(row.version).ok_or(MigrateError::Missing {
          version: row.version,
        })?;
        let (file, sql) = m.down.clone().ok_or_else(|| MigrateError::MissingDown {
          version: m.version,
          file: m.file.clone(),
        })?;
        Ok((m.version, file, sql))
      })
      .collect()
  }

  fn statuses(&self, applied: &[AppliedRow]) -> Vec<MigrationStatus> {
    let mut all: BTreeMap<i64, MigrationStatus> = self
      .migrations
      .iter()
      .map(|m| {
        let status = MigrationStatus {
          version: m.version,
          description: m.description.clone(),
          state: MigrationState::Pending,
          checksum: m.checksum.clone(),
          applied_checksum: None,
          applied_at: None,
          reversible: m.is_reversible(),
        };
        (m.version, status)
      })
      .collect();
    for row in applied {
      let status = all.entry(row.version).or_insert_with(|| MigrationStatus {
        version: row.version,
        description: row.description.clone(),
        state: MigrationState::Missing,
        checksum: row.checksum.clone(),
        applied_checksum: None,
        applied_at: None,
        reversible: false,
      });
      if status.state == MigrationState::Pending {
        status.state = if status.checksum == row.checksum {
          MigrationState::Applied
        } else {
          MigrationState::Drifted
        };
      }
      status.applied_checksum = Some(row.checksum.clone());
      status.applied_at = Some(row.applied_at);
    }
    all.into_values().collect()
  }
}

/// Applies every pending migration of directory `dir` (read when the effect runs) on the
/// [`SqlClientService`] in `R`: [`Migrator::from_dir`] then [`Migrator::run`].
pub fn run<R, I>(dir: impl Into<PathBuf>) -> Effect<Vec<i64>, MigrateError, R>
where
  R: Selector<SqlClientService, I> + HList,
{
  let dir = dir.into();
  suspend(move || match Migrator::from_dir(&dir) {
    Ok(migrator) => migrator.run(),
    Err(e) => fail(e),
  })
}

/// The startup [`Layer`] of directory `dir` (read when the layer is built): [`run`](fn@run) on
/// the [`SqlClientService`] it needs, providing [`MigrationsApplied`]. See [`Migrator::layer`].
pub fn layer<E>(dir: impl Into<PathBuf>) -> Layer<MigratedEnv, E, SqlClientEnv>
where
  E: From<MigrateError> + Send + 'static,
{
  let dir = dir.into();
  Layer::effect(move || {
    run::<SqlClientEnv, _>(dir.clone())
      .map(|applied| hlist![MigrationsApplied(applied)])
      .map_err(E::from)
  })
}

fn ready<A: Send + 'static>(r: Result<A, MigrateError>) -> Effect<A, MigrateError> {
  match r {
    Ok(a) => succeed(a),
    Err(e) => fail(e),
  }
}

fn applied_row(row: &SqlRow) -> Result<AppliedRow, MigrateError> {
  Ok(AppliedRow {
    version: row.get("version")?,
    description: row.get("description")?,
    checksum: row.get("checksum")?,
    applied_at: row.get("installed_on")?,
  })
}

/// One attempt at the advisory lock, then `retries` more, [`LOCK_POLL`] apart.
fn try_lock(
  tx: Arc<dyn SqlTransaction>,
  key: i64,
  timeout: Duration,
  retries: u32,
) -> Effect<(), MigrateError> {
  tx.query(LOCK_SQL, &[SqlValue::I64(key)])
    .map_err(MigrateError::from)
    .flat_map(
      move |rows| match rows.first().map(|r| r.get::<bool>("locked")).transpose() {
        Err(e) => fail(e.into()),
        Ok(Some(true)) => succeed(()),
        Ok(_) if retries == 0 => fail(MigrateError::LockTimeout { key, timeout }),
        Ok(_) => sleep(LOCK_POLL).flat_map(move |()| try_lock(tx, key, timeout, retries - 1)),
      },
    )
}

/// Runs the statements of `sql` in `tx`; a failure names the migration file.
fn script(
  tx: &Arc<dyn SqlTransaction>,
  sql: &str,
  version: i64,
  file: &str,
) -> Effect<(), MigrateError> {
  split::statements(sql)
    .into_iter()
    .fold(succeed(()), |done, stmt| {
      let (tx, file) = (Arc::clone(tx), file.to_owned());
      done.flat_map(move |()| {
        tx.execute(&stmt, &[])
          .map(drop)
          .map_err(move |error| MigrateError::Failed {
            version,
            file,
            error,
          })
      })
    })
}

fn valid_table(name: &str) -> bool {
  let parts: Vec<&str> = name.split('.').collect();
  parts.len() <= 2
    && parts.iter().all(|part| {
      part
        .chars()
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && part.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
    })
}

fn hex(bytes: &[u8]) -> String {
  use std::fmt::Write;
  bytes
    .iter()
    .fold(String::with_capacity(bytes.len() * 2), |mut s, b| {
      let _ = write!(s, "{b:02x}");
      s
    })
}
