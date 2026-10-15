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

use std::sync::{Arc, Mutex, PoisonError};

use id_effect::next::{
  Effect, ExitKind, HCons, HList, HNil, Layer, Selector, acquire_use_release, die, fail, hlist,
  need, succeed, suspend,
};
use id_effect::{Exit, Never};

use crate::client::{SqlClient, SqlClientEnv, SqlClientService, SqlParam, SqlRow, TestState, lock};
use crate::error::SqlError;

/// Logical SQL transaction: statements on it, then commit or rollback, as effects.
///
/// After `commit` or `rollback` the handle is finished: every further call fails with
/// [`SqlError::TransactionFailed`].
pub trait SqlTransaction: Send + Sync + 'static {
  /// Run a read query inside the transaction and collect all rows.
  fn query(&self, sql: &str, params: &[SqlParam]) -> Effect<Vec<SqlRow>, SqlError>;
  /// Run a write/DDL statement inside the transaction; returns the affected row count.
  fn execute(&self, sql: &str, params: &[SqlParam]) -> Effect<u64, SqlError>;
  /// Persist work.
  fn commit(&self) -> Effect<(), SqlError>;
  /// Discard work.
  fn rollback(&self) -> Effect<(), SqlError>;
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
/// use id_effect::next::{Effect, need};
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
  queries: Vec<(String, Vec<SqlParam>)>,
  executes: Vec<(String, Vec<SqlParam>)>,
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
  fn query(&self, sql: &str, params: &[SqlParam]) -> Effect<Vec<SqlRow>, SqlError> {
    let (sql, params) = (sql.to_string(), params.to_vec());
    self.statement(move |p, state| {
      let rows = state.run_query(&sql)?;
      p.queries.push((sql, params));
      Ok(rows)
    })
  }

  fn execute(&self, sql: &str, params: &[SqlParam]) -> Effect<u64, SqlError> {
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
}

#[cfg(test)]
mod tests {
  use std::sync::Arc;
  use std::sync::atomic::{AtomicBool, Ordering};
  use std::time::Duration;

  use id_effect::next::{
    Effect, Executor, HCons, HNil, TestExecutor, die, fail, need, sleep, succeed, suspend, sync,
  };
  use id_effect::{Cause, Exit};

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
