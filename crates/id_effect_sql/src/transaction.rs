//! Transaction handle and scope (commit/rollback as `Effect`).

use std::sync::{Arc, Mutex, Weak};

use id_effect::kernel::Effect;

use crate::client::SqlClient;
use crate::error::SqlError;

/// Logical SQL transaction: commit or rollback as effects.
pub trait SqlTransaction: Send + Sync + 'static {
  /// Persist work.
  fn commit(&self) -> Effect<(), SqlError, ()>;
  /// Discard work.
  fn rollback(&self) -> Effect<(), SqlError, ()>;
}

struct TestSqlTransactionInner {
  client: Weak<Mutex<crate::client::TestSqlClientState>>,
  finished: Mutex<bool>,
}

/// Test double transaction tied to [`crate::client::TestSqlClient`] state.
pub(crate) struct TestSqlTransaction {
  inner: Arc<TestSqlTransactionInner>,
}

impl TestSqlTransaction {
  pub(crate) fn new(client: Arc<Mutex<crate::client::TestSqlClientState>>) -> Self {
    Self {
      inner: Arc::new(TestSqlTransactionInner {
        client: Arc::downgrade(&client),
        finished: Mutex::new(false),
      }),
    }
  }

  fn finish(inner: &TestSqlTransactionInner, commit: bool) -> Result<(), SqlError> {
    let mut finished = inner.finished.lock().expect("tx finished mutex poisoned");
    if *finished {
      return Err(SqlError::TransactionFailed(
        "transaction already finished".into(),
      ));
    }
    *finished = true;
    let Some(state) = inner.client.upgrade() else {
      return Err(SqlError::TransactionFailed("client dropped".into()));
    };
    let mut state = state.lock().expect("test sql mutex poisoned");
    if state.open_transactions == 0 {
      return Err(SqlError::TransactionFailed("no open transaction".into()));
    }
    state.open_transactions -= 1;
    if !commit {
      state.queries.clear();
      state.executes.clear();
    }
    Ok(())
  }
}

impl SqlTransaction for TestSqlTransaction {
  fn commit(&self) -> Effect<(), SqlError, ()> {
    let inner = Arc::clone(&self.inner);
    Effect::new(move |_r: &mut ()| Self::finish(&inner, true))
  }

  fn rollback(&self) -> Effect<(), SqlError, ()> {
    let inner = Arc::clone(&self.inner);
    Effect::new(move |_r: &mut ()| Self::finish(&inner, false))
  }
}

/// Begin a transaction, run `f`, then commit on success or rollback on failure.
///
/// Runs `f` inside a transaction; commits on success and rolls back on failure.
pub fn with_transaction<A, F>(client: Arc<dyn SqlClient>, f: F) -> Effect<A, SqlError, ()>
where
  A: 'static,
  F: FnOnce(Arc<dyn SqlTransaction>) -> Effect<A, SqlError, ()> + Send + 'static,
{
  Effect::new_async(move |r| {
    id_effect::box_future(async move {
      let tx = client.begin().run(r).await?;
      match f(tx.clone()).run(r).await {
        Ok(value) => {
          tx.commit().run(r).await?;
          Ok(value)
        }
        Err(err) => {
          let _ = tx.rollback().run(r).await;
          Err(err)
        }
      }
    })
  })
}

#[cfg(test)]
mod tests {
  use super::with_transaction;
  use crate::client::{SqlClient, TestSqlClient};
  use crate::error::SqlError;
  use id_effect::{kernel::Effect, run_blocking};
  use std::sync::Arc;

  #[test]
  fn explicit_commit_closes_transaction() {
    let test = TestSqlClient::new();
    run_blocking(test.connect(), ()).unwrap();
    let client: Arc<dyn SqlClient> = Arc::new(test.clone());
    let tx = run_blocking(client.begin(), ()).unwrap();
    run_blocking(tx.commit(), ()).unwrap();
    assert_eq!(test.open_transactions(), 0);
  }

  #[test]
  fn explicit_rollback_clears_recorded_queries() {
    let test = TestSqlClient::new();
    run_blocking(test.connect(), ()).unwrap();
    run_blocking(test.query("SELECT 1", &[]), ()).unwrap();
    assert_eq!(test.recorded_queries().len(), 1);
    let client: Arc<dyn SqlClient> = Arc::new(test.clone());
    let tx = run_blocking(client.begin(), ()).unwrap();
    run_blocking(test.query("SELECT 2", &[]), ()).unwrap();
    run_blocking(tx.rollback(), ()).unwrap();
    assert_eq!(test.recorded_queries().len(), 0);
  }

  #[test]
  fn with_transaction_propagates_inner_error() {
    let test = TestSqlClient::new();
    run_blocking(test.connect(), ()).unwrap();
    let client: Arc<dyn SqlClient> = Arc::new(test.clone());
    let err = run_blocking(
      with_transaction(client, |_tx| {
        Effect::new(|_r: &mut ()| Err::<(), SqlError>(SqlError::Unsupported("nope".into())))
      }),
      (),
    )
    .unwrap_err();
    assert!(matches!(err, SqlError::Unsupported(_)));
    assert_eq!(test.open_transactions(), 0);
  }
}
