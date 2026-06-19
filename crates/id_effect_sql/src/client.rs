//! Driver-agnostic SQL client trait and in-memory test double.

#![allow(clippy::new_ret_no_self, clippy::unused_unit)]

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use id_effect::kernel::Effect;

use crate::error::SqlError;
use crate::transaction::{SqlTransaction, TestSqlTransaction, with_transaction};

/// One bound parameter for a SQL statement (MVP: textual cells).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SqlParam {
  /// NULL.
  Null,
  /// Boolean.
  Bool(bool),
  /// Signed integer.
  Int(i64),
  /// UTF-8 text.
  Text(String),
  /// Raw bytes (e.g. BYTEA).
  Bytes(Vec<u8>),
}

/// One result row as ordered textual cells (MVP decoding).
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct SqlRow {
  /// Column values left-to-right.
  pub cells: Vec<String>,
}

impl SqlRow {
  /// Build a row from string cells.
  #[inline]
  pub fn new(cells: Vec<String>) -> Self {
    Self { cells }
  }
}

/// Capability: portable SQL operations as [`Effect`] values.
#[::id_effect::capability(Arc<dyn SqlClient>)]
pub trait SqlClient: Send + Sync + 'static {
  /// Establish connectivity (pool warm-up / health check).
  fn connect(&self) -> Effect<(), SqlError, ()>;

  /// Run a read query and collect all rows.
  fn query(&self, sql: &str, params: &[SqlParam]) -> Effect<Vec<SqlRow>, SqlError, ()>;

  /// Run a write/DDL statement; returns affected row count when known.
  fn execute(&self, sql: &str, params: &[SqlParam]) -> Effect<u64, SqlError, ()>;

  /// Begin a logical transaction handle (full `Scope` wiring in a later leaf).
  fn begin(&self) -> Effect<Arc<dyn SqlTransaction>, SqlError, ()>;
}

/// Run `f` inside a transaction scope: commit on success, rollback on failure.
#[inline]
pub fn transaction_scope<A, F>(client: Arc<dyn SqlClient>, f: F) -> Effect<A, SqlError, ()>
where
  A: 'static,
  F: FnOnce(Arc<dyn SqlTransaction>) -> Effect<A, SqlError, ()> + Send + 'static,
{
  with_transaction(client, f)
}

/// In-memory scriptable client for unit tests.
#[derive(Clone, Default)]
pub struct TestSqlClient {
  inner: Arc<Mutex<TestSqlClientState>>,
}

#[derive(Default)]
pub(crate) struct TestSqlClientState {
  pub(crate) connected: bool,
  pub(crate) queries: Vec<(String, Vec<SqlParam>)>,
  pub(crate) executes: Vec<(String, Vec<SqlParam>)>,
  pub(crate) query_results: HashMap<String, Vec<SqlRow>>,
  pub(crate) execute_rows: HashMap<String, u64>,
  pub(crate) fail_connect: bool,
  pub(crate) fail_query: Option<String>,
  pub(crate) open_transactions: usize,
}

impl TestSqlClient {
  /// Empty test client (disconnected until [`Self::connect`]).
  #[inline]
  pub fn new() -> Self {
    Self::default()
  }

  /// Script rows returned for an exact SQL key.
  #[inline]
  pub fn script_query(&self, sql: impl Into<String>, rows: Vec<SqlRow>) {
    self
      .inner
      .lock()
      .expect("test sql mutex poisoned")
      .query_results
      .insert(sql.into(), rows);
  }

  /// Script affected row count for an exact SQL key.
  #[inline]
  pub fn script_execute(&self, sql: impl Into<String>, rows: u64) {
    self
      .inner
      .lock()
      .expect("test sql mutex poisoned")
      .execute_rows
      .insert(sql.into(), rows);
  }

  /// Force the next connect attempt to fail.
  #[inline]
  pub fn fail_on_connect(&self) {
    self
      .inner
      .lock()
      .expect("test sql mutex poisoned")
      .fail_connect = true;
  }

  /// Force all queries to fail with `message`.
  #[inline]
  pub fn fail_queries(&self, message: impl Into<String>) {
    self
      .inner
      .lock()
      .expect("test sql mutex poisoned")
      .fail_query = Some(message.into());
  }

  /// Recorded query calls (sql, params).
  #[inline]
  pub fn recorded_queries(&self) -> Vec<(String, Vec<SqlParam>)> {
    self
      .inner
      .lock()
      .expect("test sql mutex poisoned")
      .queries
      .clone()
  }

  /// Number of transactions currently open (not yet committed/rolled back).
  #[inline]
  pub fn open_transactions(&self) -> usize {
    self
      .inner
      .lock()
      .expect("test sql mutex poisoned")
      .open_transactions
  }
}

impl SqlClient for TestSqlClient {
  fn connect(&self) -> Effect<(), SqlError, ()> {
    let inner = Arc::clone(&self.inner);
    Effect::new(move |_r: &mut ()| {
      let mut state = inner.lock().expect("test sql mutex poisoned");
      if state.fail_connect {
        return Err(SqlError::NotConnected);
      }
      state.connected = true;
      Ok(())
    })
  }

  fn query(&self, sql: &str, params: &[SqlParam]) -> Effect<Vec<SqlRow>, SqlError, ()> {
    let inner = Arc::clone(&self.inner);
    let sql = sql.to_string();
    let params = params.to_vec();
    Effect::new(move |_r: &mut ()| {
      let mut state = inner.lock().expect("test sql mutex poisoned");
      if !state.connected {
        return Err(SqlError::NotConnected);
      }
      if let Some(msg) = state.fail_query.clone() {
        return Err(SqlError::QueryFailed {
          sql: sql.clone(),
          message: msg,
        });
      }
      state.queries.push((sql.clone(), params));
      Ok(state.query_results.get(&sql).cloned().unwrap_or_default())
    })
  }

  fn execute(&self, sql: &str, params: &[SqlParam]) -> Effect<u64, SqlError, ()> {
    let inner = Arc::clone(&self.inner);
    let sql = sql.to_string();
    let params = params.to_vec();
    Effect::new(move |_r: &mut ()| {
      let mut state = inner.lock().expect("test sql mutex poisoned");
      if !state.connected {
        return Err(SqlError::NotConnected);
      }
      state.executes.push((sql.clone(), params));
      Ok(*state.execute_rows.get(&sql).unwrap_or(&0))
    })
  }

  fn begin(&self) -> Effect<Arc<dyn SqlTransaction>, SqlError, ()> {
    let inner = Arc::clone(&self.inner);
    Effect::new(move |_r: &mut ()| {
      let mut state = inner.lock().expect("test sql mutex poisoned");
      if !state.connected {
        return Err(SqlError::NotConnected);
      }
      state.open_transactions += 1;
      Ok(Arc::new(TestSqlTransaction::new(Arc::clone(&inner))) as Arc<dyn SqlTransaction>)
    })
  }
}

#[cfg(test)]
mod tests {
  use std::sync::Arc;

  use super::{SqlClient, SqlParam, SqlRow, TestSqlClient, transaction_scope};
  use crate::error::SqlError;
  use id_effect::{kernel::Effect, run_blocking};

  #[test]
  fn connect_then_query_returns_scripted_rows() {
    let test = TestSqlClient::new();
    test.script_query("SELECT id FROM users", vec![SqlRow::new(vec!["1".into()])]);
    let client: Arc<dyn SqlClient> = Arc::new(test.clone());
    run_blocking(client.connect(), ()).unwrap();
    let rows = run_blocking(
      client.query("SELECT id FROM users", &[SqlParam::Int(1)]),
      (),
    )
    .unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(test.recorded_queries().len(), 1);
  }

  #[test]
  fn query_before_connect_fails() {
    let client = TestSqlClient::new();
    let err = run_blocking(client.query("SELECT 1", &[]), ()).unwrap_err();
    assert_eq!(err, SqlError::NotConnected);
  }

  #[test]
  fn transaction_scope_commits_on_success() {
    let test = TestSqlClient::new();
    run_blocking(test.connect(), ()).unwrap();
    let client: Arc<dyn SqlClient> = Arc::new(test.clone());
    run_blocking(
      transaction_scope(client, |_tx| {
        Effect::new(|_r: &mut ()| Ok::<u32, SqlError>(42))
      }),
      (),
    )
    .unwrap();
    assert_eq!(test.open_transactions(), 0);
  }

  #[test]
  fn transaction_scope_rolls_back_on_failure() {
    let test = TestSqlClient::new();
    run_blocking(test.connect(), ()).unwrap();
    let client: Arc<dyn SqlClient> = Arc::new(test.clone());
    let err = run_blocking(
      transaction_scope(client, |_tx| {
        Effect::new(|_r: &mut ()| {
          Err::<(), SqlError>(SqlError::QueryFailed {
            sql: "X".into(),
            message: "fail".into(),
          })
        })
      }),
      (),
    )
    .unwrap_err();
    assert!(matches!(err, SqlError::QueryFailed { .. }));
    assert_eq!(test.open_transactions(), 0);
  }

  #[test]
  fn execute_returns_scripted_count() {
    let test = TestSqlClient::new();
    test.script_execute("DELETE FROM t", 3);
    run_blocking(test.connect(), ()).unwrap();
    let n = run_blocking(test.execute("DELETE FROM t", &[]), ()).unwrap();
    assert_eq!(n, 3);
  }

  #[test]
  fn begin_opens_transaction() {
    let test = TestSqlClient::new();
    run_blocking(test.connect(), ()).unwrap();
    let tx = run_blocking(test.begin(), ()).unwrap();
    run_blocking(tx.commit(), ()).unwrap();
    assert_eq!(test.open_transactions(), 0);
  }

  #[test]
  fn sql_param_variants_round_trip_in_query() {
    let test = TestSqlClient::new();
    run_blocking(test.connect(), ()).unwrap();
    run_blocking(
      test.query(
        "Q",
        &[
          SqlParam::Null,
          SqlParam::Bool(true),
          SqlParam::Int(1),
          SqlParam::Text("t".into()),
          SqlParam::Bytes(vec![1, 2]),
        ],
      ),
      (),
    )
    .unwrap();
    assert_eq!(test.recorded_queries().len(), 1);
  }
}
