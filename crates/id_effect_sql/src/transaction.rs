//! Transactions: the [`SqlTransaction`] handle, the [`with_transaction`] /
//! [`transaction`](fn@transaction) brackets and the scoped [`transaction_layer`].
//!
//! Every way of running work in a transaction finishes it exactly once: it commits when the
//! work succeeds and rolls back when the work fails, dies or is interrupted.
//!
//! - [`with_transaction`] / [`transaction`](fn@transaction) are brackets around a closure. The
//!   commit is part of the bracket's result, so a failed `COMMIT` is a typed error
//!   (`E::from(SqlError)`).
//! - [`transaction_layer`] is a scoped [`Layer`] (Effect.ts `Layer.scoped(acquireRelease(..))`)
//!   providing [`SqlTransactionService`]: `BEGIN` when it is built, and, when its scope closes,
//!   `COMMIT` if the scope's owner succeeded or `ROLLBACK` otherwise (the release reads the
//!   closing [`ExitKind`]). A release cannot fail with a typed error, so a failed `COMMIT` there
//!   is a defect (`Cause::Die`) of the owner.
//!
//! Inside a transaction, [`savepoint`](fn@savepoint) is a nested bracket (`SAVEPOINT`, then
//! `RELEASE SAVEPOINT` on success or `ROLLBACK TO SAVEPOINT` otherwise), so a failure, defect or
//! interrupt of the inner work undoes only the inner work. [`with_transaction_options`] /
//! [`transaction_with`] begin with [`TransactionOptions`] (isolation level, read-only,
//! deferrable), and [`retry_serialization_failures`] reruns a transaction that lost a
//! serialization conflict (SQLSTATE `40001` / `40P01`) as an id_effect [`Schedule`] allows.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use id_effect::{
  Effect, ExitKind, HCons, HList, HNil, Layer, Schedule, Selector, acquire_use_release, die, fail,
  hlist, need, succeed, suspend,
};
use id_effect::{Exit, Never};

use crate::client::{SqlClient, SqlClientEnv, SqlClientService, TestState, lock};
use crate::error::SqlError;
use crate::row::SqlRow;
use crate::value::SqlValue;

/// Logical SQL transaction: statements on it, then commit or rollback, as effects.
///
/// After `commit` or `rollback` the handle is finished: every further call fails with
/// [`SqlError::TransactionFailed`].
pub trait SqlTransaction: Send + Sync + 'static {
  /// Run a read query inside the transaction and collect all rows.
  fn query(&self, sql: &str, params: &[SqlValue]) -> Effect<Vec<SqlRow>, SqlError>;
  /// Run a write/DDL statement inside the transaction; returns the affected row count.
  fn execute(&self, sql: &str, params: &[SqlValue]) -> Effect<u64, SqlError>;
  /// Persist work.
  fn commit(&self) -> Effect<(), SqlError>;
  /// Discard work.
  fn rollback(&self) -> Effect<(), SqlError>;

  /// `SAVEPOINT name`. `name` must be an identifier (`[A-Za-z_][A-Za-z0-9_]*`, at most 63
  /// bytes), else [`SqlError::Unsupported`]. Prefer [`savepoint`](fn@savepoint), which generates
  /// the name and always releases or rolls back to it.
  fn create_savepoint(&self, name: &str) -> Effect<(), SqlError> {
    match savepoint_sql("SAVEPOINT", name) {
      Ok(sql) => self.execute(&sql, &[]).map(|_| ()),
      Err(e) => fail(e),
    }
  }

  /// `RELEASE SAVEPOINT name`: keep the work done since the savepoint (and forget it).
  fn release_savepoint(&self, name: &str) -> Effect<(), SqlError> {
    match savepoint_sql("RELEASE SAVEPOINT", name) {
      Ok(sql) => self.execute(&sql, &[]).map(|_| ()),
      Err(e) => fail(e),
    }
  }

  /// `ROLLBACK TO SAVEPOINT name`: discard the work done since the savepoint (which stays
  /// defined), leaving the transaction usable again after a failed statement.
  fn rollback_to_savepoint(&self, name: &str) -> Effect<(), SqlError> {
    match savepoint_sql("ROLLBACK TO SAVEPOINT", name) {
      Ok(sql) => self.execute(&sql, &[]).map(|_| ()),
      Err(e) => fail(e),
    }
  }
}

/// `"{verb} {name}"` once `name` is checked to be a plain identifier (so it is never quoted or
/// spliced from untrusted text).
pub(crate) fn savepoint_sql(verb: &str, name: &str) -> Result<String, SqlError> {
  let mut chars = name.chars();
  let ident = name.len() <= 63
    && chars
      .next()
      .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
    && chars.all(|c| c.is_ascii_alphanumeric() || c == '_');
  if ident {
    Ok(format!("{verb} {name}"))
  } else {
    Err(SqlError::Unsupported(format!(
      "invalid savepoint name `{name}` (expected [A-Za-z_][A-Za-z0-9_]*, at most 63 bytes)"
    )))
  }
}

/// Capability: the open transaction of a [`transaction_layer`] (derefs to `dyn SqlTransaction`).
#[derive(Clone)]
pub struct SqlTransactionService(pub Arc<dyn SqlTransaction>);

impl std::ops::Deref for SqlTransactionService {
  type Target = dyn SqlTransaction;

  fn deref(&self) -> &Self::Target {
    &*self.0
  }
}

impl std::fmt::Debug for SqlTransactionService {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.debug_tuple("SqlTransactionService")
      .field(&"<dyn SqlTransaction>")
      .finish()
  }
}

/// The minimal typed environment holding only [`SqlTransactionService`].
pub type SqlTransactionEnv = HCons<SqlTransactionService, HNil>;

/// Rolls `tx` back, ignoring a rollback failure (the transaction is already lost, and the
/// original outcome is what the caller sees).
fn rollback_quietly<R: HList>(tx: &Arc<dyn SqlTransaction>) -> Effect<(), Never, R> {
  tx.rollback().exit::<Never>().map(|_| ()).narrow()
}

/// The bracket shared by [`with_transaction`] and [`transaction`](fn@transaction): `begin` runs
/// uninterruptibly, `f` with the caller's interruptibility, then an uninterruptible `COMMIT`;
/// anything but a success rolls back.
fn bracket<A, E, R, F>(begin: Effect<Arc<dyn SqlTransaction>, E, R>, f: F) -> Effect<A, E, R>
where
  A: Send + 'static,
  E: From<SqlError> + Send + 'static,
  R: HList,
  F: FnOnce(Arc<dyn SqlTransaction>) -> Effect<A, E, R> + Send + 'static,
{
  acquire_use_release(
    begin,
    move |tx: Arc<dyn SqlTransaction>| {
      let commit = tx
        .commit()
        .map_err(E::from)
        .narrow::<R, _>()
        .uninterruptible();
      f(tx).flat_map(move |a| commit.map(move |()| a))
    },
    |tx, kind| match kind {
      // A failed commit reports `Failure`; the handle is finished then, so this is a no-op.
      ExitKind::Success => succeed(()),
      _ => rollback_quietly(&tx),
    },
  )
}

/// Begin a transaction on `client`, run `f` with it, then commit on success or roll back when
/// `f` fails, dies or is interrupted. A failed `BEGIN` or `COMMIT` fails with `E::from(err)`.
pub fn with_transaction<A, E, F>(client: Arc<dyn SqlClient>, f: F) -> Effect<A, E>
where
  A: Send + 'static,
  E: From<SqlError> + Send + 'static,
  F: FnOnce(Arc<dyn SqlTransaction>) -> Effect<A, E> + Send + 'static,
{
  bracket(suspend(move || client.begin().map_err(E::from)), f)
}

/// [`with_transaction`] on the [`SqlClientService`] in `R`; `f`'s effect may need the rest of `R`.
pub fn transaction<A, E, R, I, F>(f: F) -> Effect<A, E, R>
where
  A: Send + 'static,
  E: From<SqlError> + Send + 'static,
  R: Selector<SqlClientService, I> + HList,
  F: FnOnce(Arc<dyn SqlTransaction>) -> Effect<A, E, R> + Send + 'static,
{
  let begin = need::<SqlClientService>().flat_map(|c| c.begin().map_err(E::from).narrow());
  bracket(begin, f)
}

/// SQL-standard transaction isolation level ([`TransactionOptions::with_isolation`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Isolation {
  /// `READ UNCOMMITTED` (PostgreSQL runs it as `READ COMMITTED`).
  ReadUncommitted,
  /// `READ COMMITTED` (PostgreSQL's default).
  ReadCommitted,
  /// `REPEATABLE READ`.
  RepeatableRead,
  /// `SERIALIZABLE`: commits may fail with a serialization failure; see
  /// [`retry_serialization_failures`].
  Serializable,
}

impl Isolation {
  /// The level as SQL (`"SERIALIZABLE"`, …).
  pub fn as_sql(self) -> &'static str {
    match self {
      Isolation::ReadUncommitted => "READ UNCOMMITTED",
      Isolation::ReadCommitted => "READ COMMITTED",
      Isolation::RepeatableRead => "REPEATABLE READ",
      Isolation::Serializable => "SERIALIZABLE",
    }
  }
}

/// How [`with_transaction_options`] / [`transaction_with`] / [`SqlClient::begin_with`] begin a
/// transaction. The default is the database's default (no `SET TRANSACTION`).
///
/// ```rust
/// use id_effect_sql::{Isolation, TransactionOptions};
///
/// let opts = TransactionOptions::from(Isolation::Serializable)
///   .with_read_only()
///   .with_deferrable();
/// assert_eq!(
///   opts.set_transaction_sql().as_deref(),
///   Some("SET TRANSACTION ISOLATION LEVEL SERIALIZABLE, READ ONLY, DEFERRABLE")
/// );
/// assert_eq!(TransactionOptions::new().set_transaction_sql(), None);
/// ```
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct TransactionOptions {
  isolation: Option<Isolation>,
  read_only: bool,
  deferrable: bool,
}

impl TransactionOptions {
  /// The database's defaults.
  #[inline]
  pub fn new() -> Self {
    Self::default()
  }

  /// Run at `level`.
  #[inline]
  #[must_use]
  pub fn with_isolation(mut self, level: Isolation) -> Self {
    self.isolation = Some(level);
    self
  }

  /// `READ ONLY`: writes fail.
  #[inline]
  #[must_use]
  pub fn with_read_only(mut self) -> Self {
    self.read_only = true;
    self
  }

  /// `DEFERRABLE` (PostgreSQL; with `SERIALIZABLE READ ONLY` it waits for a safe snapshot and
  /// then never fails with a serialization failure).
  #[inline]
  #[must_use]
  pub fn with_deferrable(mut self) -> Self {
    self.deferrable = true;
    self
  }

  /// The isolation level asked for (`None`: the database's default).
  #[inline]
  pub fn isolation(&self) -> Option<Isolation> {
    self.isolation
  }

  /// Whether the transaction is `READ ONLY`.
  #[inline]
  pub fn read_only(&self) -> bool {
    self.read_only
  }

  /// Whether the transaction is `DEFERRABLE`.
  #[inline]
  pub fn deferrable(&self) -> bool {
    self.deferrable
  }

  /// The `SET TRANSACTION …` statement the default [`SqlClient::begin_with`] runs first in the
  /// transaction, or `None` for the defaults.
  pub fn set_transaction_sql(&self) -> Option<String> {
    let mut modes = Vec::new();
    if let Some(level) = self.isolation {
      modes.push(format!("ISOLATION LEVEL {}", level.as_sql()));
    }
    if self.read_only {
      modes.push("READ ONLY".to_owned());
    }
    if self.deferrable {
      modes.push("DEFERRABLE".to_owned());
    }
    (!modes.is_empty()).then(|| format!("SET TRANSACTION {}", modes.join(", ")))
  }
}

impl From<Isolation> for TransactionOptions {
  fn from(level: Isolation) -> Self {
    Self::new().with_isolation(level)
  }
}

/// The default [`SqlClient::begin_with`]: `begin`, then `options`' `SET TRANSACTION` as the
/// transaction's first statement; when that fails the transaction is rolled back and the error
/// returned.
pub(crate) fn begin_with_options(
  begin: Effect<Arc<dyn SqlTransaction>, SqlError>,
  options: TransactionOptions,
) -> Effect<Arc<dyn SqlTransaction>, SqlError> {
  let Some(sql) = options.set_transaction_sql() else {
    return begin;
  };
  begin.flat_map(move |tx| {
    let (ok, undo) = (Arc::clone(&tx), Arc::clone(&tx));
    tx.execute(&sql, &[]).map(move |_| ok).catch(move |e| {
      rollback_quietly::<HNil>(&undo)
        .map_err(|n: Never| match n {})
        .flat_map(move |()| fail(e))
    })
  })
}

/// [`with_transaction`] beginning with `options` ([`SqlClient::begin_with`]), e.g.
/// `with_transaction_options(client, Isolation::Serializable, f)`.
pub fn with_transaction_options<A, E, F>(
  client: Arc<dyn SqlClient>,
  options: impl Into<TransactionOptions>,
  f: F,
) -> Effect<A, E>
where
  A: Send + 'static,
  E: From<SqlError> + Send + 'static,
  F: FnOnce(Arc<dyn SqlTransaction>) -> Effect<A, E> + Send + 'static,
{
  let options = options.into();
  bracket(
    suspend(move || client.begin_with(options).map_err(E::from)),
    f,
  )
}

/// [`transaction`](fn@transaction) beginning with `options`, e.g.
/// `transaction_with(Isolation::Serializable, |tx| …)`.
///
/// ```rust
/// use id_effect::{Effect, Exit};
/// use id_effect_sql::{
///   Isolation, SqlClient, SqlClientEnv, SqlError, TestSqlClient, sql_client_env, transaction_with,
/// };
///
/// let db = TestSqlClient::new();
/// let _ = db.connect().block_on();
/// let post: Effect<u64, SqlError, SqlClientEnv> =
///   transaction_with(Isolation::Serializable, |tx| tx.execute("INSERT INTO t VALUES (1)", &[]).narrow());
/// assert!(matches!(post.provide_all(sql_client_env(db.clone())).block_on(), Exit::Success(0)));
/// assert_eq!(db.recorded_executes()[0].0, "SET TRANSACTION ISOLATION LEVEL SERIALIZABLE");
/// ```
pub fn transaction_with<A, E, R, I, F>(
  options: impl Into<TransactionOptions>,
  f: F,
) -> Effect<A, E, R>
where
  A: Send + 'static,
  E: From<SqlError> + Send + 'static,
  R: Selector<SqlClientService, I> + HList,
  F: FnOnce(Arc<dyn SqlTransaction>) -> Effect<A, E, R> + Send + 'static,
{
  let options = options.into();
  let begin =
    need::<SqlClientService>().flat_map(move |c| c.begin_with(options).map_err(E::from).narrow());
  bracket(begin, f)
}

/// Counter making every generated savepoint name unique (nesting, concurrent transactions).
static SAVEPOINTS: AtomicU64 = AtomicU64::new(0);

/// `sp_<label>_<n>`: the label's ASCII letters, digits and `_` (lowercased, at most 32), then a
/// process-unique number, so the name is always a valid identifier of at most 63 bytes.
fn savepoint_name(label: &str) -> String {
  let label: String = label
    .chars()
    .filter(|c| c.is_ascii_alphanumeric() || *c == '_')
    .take(32)
    .collect();
  let n = SAVEPOINTS.fetch_add(1, Ordering::Relaxed);
  format!("sp_{}_{n}", label.to_ascii_lowercase())
}

/// Rolls back to `name` and releases it, ignoring failures (the inner work already failed).
fn undo_savepoint<R: HList>(tx: &Arc<dyn SqlTransaction>, name: &str) -> Effect<(), Never, R> {
  tx.rollback_to_savepoint(name)
    .then(tx.release_savepoint(name))
    .exit::<Never>()
    .map(|_| ())
    .narrow()
}

/// A nested scope in the transaction `tx`: `SAVEPOINT` (uninterruptibly), `f`, then `RELEASE
/// SAVEPOINT` when `f` succeeds or `ROLLBACK TO SAVEPOINT` when it fails, dies or is
/// interrupted, so only `f`'s statements are undone and the outer transaction stays usable
/// (catch `f`'s error to carry on). `label` only names the savepoint for logs: the name is
/// generated (`sp_<label>_<n>`), never spliced from untrusted text. A failed `SAVEPOINT` or
/// `RELEASE` fails with `E::from(err)`. Savepoints nest.
///
/// ```rust
/// use std::sync::Arc;
/// use id_effect::{Effect, Exit, fail, succeed};
/// use id_effect_sql::{SqlClient, SqlError, TestSqlClient, savepoint, with_transaction};
///
/// let db = TestSqlClient::new();
/// let _ = db.connect().block_on();
/// let client: Arc<dyn SqlClient> = Arc::new(db.clone());
/// let program = with_transaction(client, |tx| {
///   let outer = tx.execute("INSERT INTO t VALUES (1)", &[]);
///   let inner = savepoint(&tx, "line", |sp| {
///     sp.execute("INSERT INTO t VALUES (2)", &[])
///       .then(fail::<u64, _, _>(SqlError::Unsupported("bad line".into())))
///   })
///   .catch(|_| succeed(0));
///   outer.then(inner)
/// });
/// assert!(matches!(program.block_on(), Exit::Success(0)));
/// let kept: Vec<String> = db.recorded_executes().into_iter().map(|(sql, _)| sql).collect();
/// assert_eq!(kept, ["INSERT INTO t VALUES (1)"]);
/// ```
pub fn savepoint<A, E, R, F>(tx: &Arc<dyn SqlTransaction>, label: &str, f: F) -> Effect<A, E, R>
where
  A: Send + 'static,
  E: From<SqlError> + Send + 'static,
  R: HList,
  F: FnOnce(Arc<dyn SqlTransaction>) -> Effect<A, E, R> + Send + 'static,
{
  let name: Arc<str> = savepoint_name(label).into();
  let (create, release) = (Arc::clone(&name), Arc::clone(&name));
  let tx = Arc::clone(tx);
  acquire_use_release(
    tx.create_savepoint(&create)
      .map_err(E::from)
      .narrow::<R, _>()
      .map(move |()| tx),
    move |tx: Arc<dyn SqlTransaction>| {
      let keep = tx
        .release_savepoint(&release)
        .map_err(E::from)
        .narrow::<R, _>()
        .uninterruptible();
      f(tx).flat_map(move |a| keep.map(move |()| a))
    },
    move |tx, kind| match kind {
      // A failed RELEASE reports `Failure`, so it is rolled back to as well.
      ExitKind::Success => succeed(()),
      _ => undo_savepoint(&tx, &name),
    },
  )
}

/// SQLSTATE of a serialization failure (`SERIALIZABLE` / `REPEATABLE READ` conflict).
pub const SQLSTATE_SERIALIZATION_FAILURE: &str = "40001";
/// SQLSTATE of a detected deadlock.
pub const SQLSTATE_DEADLOCK_DETECTED: &str = "40P01";

/// `message` tagged with its SQLSTATE the way drivers report it in [`SqlError::QueryFailed`]
/// (`"… (SQLSTATE 40001)"`).
pub(crate) fn with_sqlstate(message: impl std::fmt::Display, code: &str) -> String {
  format!("{message} (SQLSTATE {code})")
}

/// Whether `err` is a serialization failure (`40001`) or a deadlock (`40P01`): the transaction
/// lost a conflict and running it again may succeed.
pub fn is_serialization_failure(err: &SqlError) -> bool {
  matches!(
    err.sqlstate(),
    Some(SQLSTATE_SERIALIZATION_FAILURE | SQLSTATE_DEADLOCK_DETECTED)
  )
}

/// Runs `make()` (typically a whole [`with_transaction_options`] / [`transaction_with`]) and runs
/// it again after a [serialization failure](is_serialization_failure) as `schedule` allows (e.g.
/// `Schedule::exponential(10ms)` combined with `Schedule::recurs(5)`). Any other error, defect or
/// interrupt is not retried; once the schedule is done the last error is returned.
///
/// ```rust
/// use std::sync::Arc;
/// use id_effect::{Exit, Schedule};
/// use id_effect_sql::{
///   Isolation, SqlClient, TestSqlClient, retry_serialization_failures, with_transaction_options,
/// };
///
/// let db = TestSqlClient::new();
/// let _ = db.connect().block_on();
/// db.fail_commits_with_serialization_failure(2);
/// let client: Arc<dyn SqlClient> = Arc::new(db.clone());
/// let post = retry_serialization_failures(Schedule::recurs(3), move || {
///   with_transaction_options(Arc::clone(&client), Isolation::Serializable, |tx| {
///     tx.execute("UPDATE accounts SET n = n + 1", &[])
///   })
/// });
/// assert!(matches!(post.block_on(), Exit::Success(0)));
/// assert_eq!((db.commits(), db.rollbacks()), (1, 2));
/// ```
pub fn retry_serialization_failures<A, R, Out, F>(
  schedule: Schedule<Out, SqlError>,
  make: F,
) -> Effect<A, SqlError, R>
where
  A: Send + 'static,
  R: Send + Sync + 'static,
  Out: Send + 'static,
  F: Fn() -> Effect<A, SqlError, R> + Send + Sync + 'static,
{
  Effect::retry(make, schedule.while_input(is_serialization_failure))
}

/// The release of a [`transaction_layer`]: commit after a success, roll back otherwise.
fn finish(tx: Arc<dyn SqlTransaction>, kind: ExitKind) -> Effect<(), Never> {
  match kind {
    ExitKind::Success => tx.commit().exit::<Never>().flat_map(|exit| match exit {
      Exit::Success(()) => succeed(()),
      Exit::Failure(cause) => die(format!(
        "sql transaction_layer: COMMIT failed after a successful scope: {cause:?}"
      )),
    }),
    _ => rollback_quietly(&tx),
  }
}

/// The scoped transaction [`Layer`]: builds [`SqlTransactionService`] from the
/// [`SqlClientService`] it needs (`BEGIN`, uninterruptibly), and when its scope closes commits
/// (the owner succeeded) or rolls back (it failed, died or was interrupted). See the
/// [module docs](self) for how a failed `COMMIT` surfaces.
///
/// ```rust
/// use id_effect::Exit;
/// use id_effect::{Effect, need};
/// use id_effect_sql::{
///   SqlClient, SqlError, SqlTransactionEnv, SqlTransactionService, TestSqlClient,
///   provide_sql_client, transaction_layer,
/// };
///
/// let db = TestSqlClient::new();
/// let _ = db.connect().block_on();
/// let work: Effect<u64, SqlError, SqlTransactionEnv> =
///   need::<SqlTransactionService>().flat_map(|tx| tx.execute("DELETE FROM t", &[]).narrow());
/// let program = work
///   .provide_layer(transaction_layer())
///   .provide_layer(provide_sql_client(db.clone()));
/// assert!(matches!(program.block_on(), Exit::Success(0)));
/// assert_eq!((db.commits(), db.rollbacks()), (1, 0));
/// ```
pub fn transaction_layer<E>() -> Layer<SqlTransactionEnv, E, SqlClientEnv>
where
  E: From<SqlError> + Send + 'static,
{
  Layer::acquire_release(
    || {
      need::<SqlClientService>().flat_map(|c| {
        c.begin()
          .map_err(E::from)
          .map(|tx| hlist![SqlTransactionService(tx)])
          .narrow()
      })
    },
    |env: SqlTransactionEnv, kind| finish(Arc::clone(&env.0.0), kind),
  )
}

/// Statements buffered by a [`TestSqlTransaction`] until it commits.
#[derive(Default)]
struct Pending {
  finished: bool,
  queries: Vec<(String, Vec<SqlValue>)>,
  executes: Vec<(String, Vec<SqlValue>)>,
  /// Open savepoints, innermost last: name and the statement counts when it was created.
  savepoints: Vec<(String, usize, usize)>,
}

impl Pending {
  /// The index of the innermost savepoint called `name`.
  fn savepoint(&self, name: &str) -> Result<usize, SqlError> {
    self
      .savepoints
      .iter()
      .rposition(|(n, _, _)| n == name)
      .ok_or_else(|| SqlError::TransactionFailed(format!("savepoint `{name}` does not exist")))
  }
}

/// Test double transaction tied to [`crate::client::TestSqlClient`] state: its statements are
/// recorded on the client when it commits and discarded when it rolls back.
pub(crate) struct TestSqlTransaction {
  client: TestState,
  pending: Arc<Mutex<Pending>>,
}

impl TestSqlTransaction {
  pub(crate) fn new(client: TestState) -> Self {
    Self {
      client,
      pending: Arc::default(),
    }
  }

  fn statement<A: Send + 'static>(
    &self,
    run: impl FnOnce(&mut Pending, &crate::client::TestSqlClientState) -> Result<A, SqlError>
    + Send
    + 'static,
  ) -> Effect<A, SqlError> {
    let (client, pending) = (Arc::clone(&self.client), Arc::clone(&self.pending));
    suspend(move || {
      let mut p = pending.lock().unwrap_or_else(PoisonError::into_inner);
      if p.finished {
        return fail(already_finished());
      }
      match run(&mut p, &lock(&client)) {
        Ok(a) => succeed(a),
        Err(e) => fail(e),
      }
    })
  }

  fn finish(&self, commit: bool) -> Effect<(), SqlError> {
    let (client, pending) = (Arc::clone(&self.client), Arc::clone(&self.pending));
    suspend(move || {
      let mut p = pending.lock().unwrap_or_else(PoisonError::into_inner);
      if p.finished {
        return fail(already_finished());
      }
      p.finished = true;
      let mut state = lock(&client);
      state.open_transactions = state.open_transactions.saturating_sub(1);
      if commit && state.fail_commit {
        state.rollbacks += 1;
        return fail(SqlError::TransactionFailed("commit failed".into()));
      }
      if commit && state.serialization_failures > 0 {
        state.serialization_failures -= 1;
        state.rollbacks += 1;
        return fail(SqlError::QueryFailed {
          sql: "COMMIT".into(),
          message: with_sqlstate(
            "could not serialize access due to concurrent update",
            SQLSTATE_SERIALIZATION_FAILURE,
          ),
        });
      }
      if commit {
        state.commits += 1;
        state.queries.append(&mut p.queries);
        state.executes.append(&mut p.executes);
      } else {
        state.rollbacks += 1;
      }
      succeed(())
    })
  }
}

fn already_finished() -> SqlError {
  SqlError::TransactionFailed("transaction already finished".into())
}

impl SqlTransaction for TestSqlTransaction {
  fn query(&self, sql: &str, params: &[SqlValue]) -> Effect<Vec<SqlRow>, SqlError> {
    let (sql, params) = (sql.to_string(), params.to_vec());
    self.statement(move |p, state| {
      let rows = state.run_query(&sql)?;
      p.queries.push((sql, params));
      Ok(rows)
    })
  }

  fn execute(&self, sql: &str, params: &[SqlValue]) -> Effect<u64, SqlError> {
    let (sql, params) = (sql.to_string(), params.to_vec());
    self.statement(move |p, state| {
      let n = state.run_execute(&sql)?;
      p.executes.push((sql, params));
      Ok(n)
    })
  }

  fn commit(&self) -> Effect<(), SqlError> {
    self.finish(true)
  }

  fn rollback(&self) -> Effect<(), SqlError> {
    self.finish(false)
  }

  // Savepoints mark the buffered statements instead of recording SQL.

  fn create_savepoint(&self, name: &str) -> Effect<(), SqlError> {
    let name = name.to_owned();
    self.statement(move |p, _| {
      savepoint_sql("SAVEPOINT", &name)?;
      let mark = (name, p.queries.len(), p.executes.len());
      p.savepoints.push(mark);
      Ok(())
    })
  }

  fn release_savepoint(&self, name: &str) -> Effect<(), SqlError> {
    let name = name.to_owned();
    self.statement(move |p, _| {
      let i = p.savepoint(&name)?;
      p.savepoints.truncate(i);
      Ok(())
    })
  }

  fn rollback_to_savepoint(&self, name: &str) -> Effect<(), SqlError> {
    let name = name.to_owned();
    self.statement(move |p, _| {
      let i = p.savepoint(&name)?;
      let (_, queries, executes) = p.savepoints[i];
      p.queries.truncate(queries);
      p.executes.truncate(executes);
      p.savepoints.truncate(i + 1);
      Ok(())
    })
  }
}

#[cfg(test)]
mod tests {
  use std::sync::Arc;
  use std::sync::atomic::{AtomicBool, Ordering};
  use std::time::Duration;

  use id_effect::{Cause, Exit};
  use id_effect::{
    Effect, Executor, HCons, HNil, TestExecutor, die, fail, need, sleep, succeed, suspend, sync,
  };

  use super::{
    SqlTransactionEnv, SqlTransactionService, transaction, transaction_layer, with_transaction,
  };
  use crate::client::tests::run;
  use crate::client::{SqlClient, SqlClientEnv, TestSqlClient, provide_sql_client, sql_client_env};
  use crate::error::SqlError;

  fn connected() -> TestSqlClient {
    let test = TestSqlClient::new();
    run(test.connect()).unwrap();
    test
  }

  fn insert(tx: &Arc<dyn super::SqlTransaction>) -> Effect<u64, SqlError> {
    tx.execute("INSERT INTO t VALUES (1)", &[])
  }

  #[test]
  fn explicit_commit_records_the_statements() {
    let test = connected();
    let tx = run(test.begin()).unwrap();
    run(tx.execute("INSERT", &[])).unwrap();
    run(tx.query("SELECT 1", &[])).unwrap();
    assert!(test.recorded_executes().is_empty());
    run(tx.commit()).unwrap();
    assert_eq!(test.recorded_executes().len(), 1);
    assert_eq!(test.recorded_queries().len(), 1);
    assert_eq!((test.open_transactions(), test.commits()), (0, 1));
  }

  #[test]
  fn explicit_rollback_discards_the_statements() {
    let test = connected();
    run(test.query("SELECT 1", &[])).unwrap();
    let tx = run(test.begin()).unwrap();
    run(tx.query("SELECT 2", &[])).unwrap();
    run(tx.rollback()).unwrap();
    assert_eq!(test.recorded_queries().len(), 1);
    assert_eq!(test.rollbacks(), 1);
  }

  #[test]
  fn a_finished_transaction_rejects_everything() {
    let test = connected();
    let tx = run(test.begin()).unwrap();
    run(tx.commit()).unwrap();
    for eff in [tx.commit(), tx.rollback(), tx.execute("X", &[]).map(|_| ())] {
      assert!(matches!(run(eff), Err(SqlError::TransactionFailed(_))));
    }
    assert!(run(tx.query("X", &[])).is_err());
  }

  #[test]
  fn with_transaction_commits_on_success() {
    let test = connected();
    let client: Arc<dyn SqlClient> = Arc::new(test.clone());
    let n = run(with_transaction(client, |tx| insert(&tx).map(|_| 42u32))).unwrap();
    assert_eq!(n, 42);
    assert_eq!(
      (test.commits(), test.rollbacks(), test.open_transactions()),
      (1, 0, 0)
    );
    assert_eq!(test.recorded_executes().len(), 1);
  }

  #[test]
  fn with_transaction_rolls_back_on_failure() {
    let test = connected();
    let client: Arc<dyn SqlClient> = Arc::new(test.clone());
    let err = run(with_transaction(client, |tx| {
      insert(&tx).then(fail::<(), _, _>(SqlError::Unsupported("nope".into())))
    }))
    .unwrap_err();
    assert!(matches!(err, SqlError::Unsupported(_)));
    assert_eq!(
      (test.commits(), test.rollbacks(), test.open_transactions()),
      (0, 1, 0)
    );
    assert!(test.recorded_executes().is_empty());
  }

  #[test]
  fn with_transaction_rolls_back_on_a_defect_and_a_panic() {
    let test = connected();
    let client: Arc<dyn SqlClient> = Arc::new(test.clone());
    let exit = with_transaction::<(), SqlError, _>(Arc::clone(&client), |_| die("boom")).block_on();
    assert!(matches!(exit, Exit::Failure(Cause::Die(_))), "{exit:?}");
    let exit =
      with_transaction::<(), SqlError, _>(client, |_| -> Effect<(), SqlError> { panic!("p") })
        .block_on();
    assert!(matches!(exit, Exit::Failure(Cause::Die(_))), "{exit:?}");
    assert_eq!((test.rollbacks(), test.open_transactions()), (2, 0));
  }

  #[test]
  fn with_transaction_reports_a_failed_commit() {
    let test = connected();
    test.fail_commits();
    let client: Arc<dyn SqlClient> = Arc::new(test.clone());
    let err = run(with_transaction(client, |tx| insert(&tx))).unwrap_err();
    assert!(matches!(err, SqlError::TransactionFailed(_)));
    assert_eq!((test.commits(), test.open_transactions()), (0, 0));
  }

  #[test]
  fn with_transaction_fails_when_begin_fails() {
    let test = TestSqlClient::new();
    let client: Arc<dyn SqlClient> = Arc::new(test.clone());
    let ran = Arc::new(AtomicBool::new(false));
    let seen = Arc::clone(&ran);
    let err = run(with_transaction(client, move |_| {
      sync(move || seen.store(true, Ordering::SeqCst))
    }))
    .unwrap_err();
    assert_eq!(err, SqlError::NotConnected);
    assert!(!ran.load(Ordering::SeqCst));
  }

  #[test]
  fn with_transaction_lifts_into_the_callers_error() {
    #[derive(Debug, PartialEq)]
    enum AppError {
      Sql(SqlError),
    }
    impl From<SqlError> for AppError {
      fn from(e: SqlError) -> Self {
        AppError::Sql(e)
      }
    }
    let client: Arc<dyn SqlClient> = Arc::new(TestSqlClient::new());
    let exit = with_transaction::<(), AppError, _>(client, |_| succeed(())).block_on();
    assert!(matches!(
      exit,
      Exit::Failure(Cause::Fail(AppError::Sql(SqlError::NotConnected)))
    ));
  }

  #[test]
  fn transaction_reads_the_client_from_the_environment() {
    let test = connected();
    let eff: Effect<u64, SqlError, SqlClientEnv> =
      transaction(|tx| tx.execute("DELETE FROM t", &[]).narrow());
    assert_eq!(
      run(eff.provide_all(sql_client_env(test.clone()))).unwrap(),
      0
    );
    assert_eq!(test.commits(), 1);
  }

  /// Runs `eff` on the deterministic test executor (virtual time) and returns its exit.
  fn on_test_executor<A: Send + 'static>(eff: Effect<A, SqlError>) -> Exit<A, SqlError> {
    let exec = TestExecutor::new();
    let h = eff.fork_root(Arc::new(exec.clone()));
    exec.run_until_stalled();
    while !h.is_done() {
      let deadline = exec.next_timer_deadline().expect("stalled with no timer");
      exec.advance(deadline.saturating_duration_since(exec.now()));
    }
    let mut fut = std::pin::pin!(h.join_future());
    match fut
      .as_mut()
      .poll(&mut std::task::Context::from_waker(std::task::Waker::noop()))
    {
      std::task::Poll::Ready(exit) => exit,
      std::task::Poll::Pending => panic!("root fiber is not done"),
    }
  }

  #[test]
  fn with_transaction_rolls_back_on_interrupt() {
    let test = connected();
    let client: Arc<dyn SqlClient> = Arc::new(test.clone());
    let work = with_transaction(client, |tx| {
      insert(&tx).then(sleep::<SqlError, HNil>(Duration::from_secs(3600)))
    });
    let eff = work.fork().widen().flat_map(|h| {
      sleep(Duration::from_secs(1))
        .then(h.interrupt())
        .map(|ex| matches!(ex, Exit::Failure(Cause::Interrupt(_))))
    });
    let Exit::Success(interrupted) = on_test_executor(eff) else {
      panic!("root failed")
    };
    assert!(interrupted);
    assert_eq!(
      (test.commits(), test.rollbacks(), test.open_transactions()),
      (0, 1, 0)
    );
    assert!(test.recorded_executes().is_empty());
  }

  fn tx_work() -> Effect<u64, SqlError, SqlTransactionEnv> {
    need::<SqlTransactionService>().flat_map(|tx| tx.execute("INSERT", &[]).narrow())
  }

  #[test]
  fn transaction_layer_commits_after_a_success() {
    let test = connected();
    let program = tx_work()
      .provide_layer(transaction_layer())
      .provide_layer(provide_sql_client(test.clone()));
    assert_eq!(run(program).unwrap(), 0);
    assert_eq!(
      (test.commits(), test.rollbacks(), test.open_transactions()),
      (1, 0, 0)
    );
    assert_eq!(test.recorded_executes().len(), 1);
  }

  #[test]
  fn transaction_layer_rolls_back_after_a_failure() {
    let test = connected();
    let work = tx_work().flat_map(|_| fail::<(), _, _>(SqlError::Unsupported("x".into())));
    let program = work
      .provide_layer(transaction_layer())
      .provide_all(sql_client_env(test.clone()));
    assert!(run(program).is_err());
    assert_eq!(
      (test.commits(), test.rollbacks(), test.open_transactions()),
      (0, 1, 0)
    );
    assert!(test.recorded_executes().is_empty());
  }

  #[test]
  fn transaction_layer_rolls_back_after_a_defect() {
    let test = connected();
    let work = tx_work().flat_map(|_| die::<(), SqlError, SqlTransactionEnv>("boom"));
    let exit = work
      .provide_layer(transaction_layer())
      .provide_all(sql_client_env(test.clone()))
      .block_on();
    assert!(matches!(exit, Exit::Failure(Cause::Die(_))));
    assert_eq!((test.rollbacks(), test.open_transactions()), (1, 0));
  }

  #[test]
  fn transaction_layer_rolls_back_on_interrupt() {
    let test = connected();
    let work = tx_work().then(sleep(Duration::from_secs(3600)));
    let provided = work
      .provide_layer(transaction_layer())
      .provide_all(sql_client_env(test.clone()));
    let eff = provided.fork().widen().flat_map(|h| {
      sleep(Duration::from_secs(1))
        .then(h.interrupt())
        .map(|ex| matches!(ex, Exit::Failure(Cause::Interrupt(_))))
    });
    let Exit::Success(interrupted) = on_test_executor(eff) else {
      panic!("root failed")
    };
    assert!(interrupted);
    assert_eq!(
      (test.commits(), test.rollbacks(), test.open_transactions()),
      (0, 1, 0)
    );
    assert!(test.recorded_executes().is_empty());
  }

  #[test]
  fn transaction_layer_failed_commit_is_a_defect() {
    let test = connected();
    test.fail_commits();
    let exit = tx_work()
      .provide_layer(transaction_layer())
      .provide_all(sql_client_env(test.clone()))
      .block_on();
    assert!(matches!(exit, Exit::Failure(Cause::Die(_))), "{exit:?}");
    assert_eq!(test.open_transactions(), 0);
  }

  #[test]
  fn transaction_layer_fails_the_build_when_begin_fails() {
    let test = TestSqlClient::new();
    let ran = Arc::new(AtomicBool::new(false));
    let seen = Arc::clone(&ran);
    let work: Effect<(), SqlError, HCons<SqlTransactionService, HNil>> =
      suspend(move || sync(move || seen.store(true, Ordering::SeqCst)));
    let exit = work
      .provide_layer(transaction_layer())
      .provide_all(sql_client_env(test))
      .block_on();
    assert!(matches!(
      exit,
      Exit::Failure(Cause::Fail(SqlError::NotConnected))
    ));
    assert!(!ran.load(Ordering::SeqCst));
  }
}
