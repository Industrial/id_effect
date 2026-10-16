//! Driver-agnostic SQL client trait, its capability and layers, and an in-memory test double.
//!
//! The capability is [`SqlClientService`] (environment [`SqlClientEnv`]). [`sql_client_layer`]
//! is the scoped [`Layer`] that connects a client when it is built and closes it when the
//! build's scope closes; [`provide_sql_client`] provides a client that someone else owns.

use std::collections::HashMap;
use std::fmt;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use id_effect::Never;
use id_effect::{
  Effect, HCons, HList, HNil, Layer, Selector, fail, hlist, need, succeed, suspend, sync,
};

use crate::error::SqlError;
pub use crate::row::SqlRow;
use crate::transaction::{SqlTransaction, TestSqlTransaction};
use crate::value::SqlValue;

/// Capability: an `Arc<dyn SqlClient>` in the typed environment (derefs to `dyn SqlClient`).
#[derive(Clone)]
pub struct SqlClientService(pub Arc<dyn SqlClient>);

impl SqlClientService {
  /// Wrap `client` as the capability.
  #[inline]
  pub fn new(client: impl SqlClient) -> Self {
    Self(Arc::new(client))
  }
}

impl std::ops::Deref for SqlClientService {
  type Target = dyn SqlClient;

  fn deref(&self) -> &Self::Target {
    &*self.0
  }
}

impl fmt::Debug for SqlClientService {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_tuple("SqlClientService")
      .field(&"<dyn SqlClient>")
      .finish()
  }
}

/// The minimal typed environment holding only [`SqlClientService`].
pub type SqlClientEnv = HCons<SqlClientService, HNil>;

/// One bound parameter for a SQL statement (the 0.8 parameter type).
///
/// Deprecated: statements take [`SqlValue`]s. `SqlValue::from(param)` converts (`Int` becomes
/// [`SqlValue::I64`]), and `SqlValue == SqlParam` compares through that conversion. Removed in
/// 0.10.
#[deprecated(since = "0.9.0", note = "use SqlValue (SqlParam::Int → SqlValue::I64)")]
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

/// Capability: portable SQL operations as [`Effect`] values.
pub trait SqlClient: Send + Sync + 'static {
  /// Establish connectivity (pool warm-up / health check).
  fn connect(&self) -> Effect<(), SqlError>;

  /// Run a read query and collect all rows. `params` bind to the placeholders in order
  /// (`&params![a, b]`, `&[x.into()]`).
  fn query(&self, sql: &str, params: &[SqlValue]) -> Effect<Vec<SqlRow>, SqlError>;

  /// Run a write/DDL statement; returns affected row count when known.
  fn execute(&self, sql: &str, params: &[SqlValue]) -> Effect<u64, SqlError>;

  /// Begin a transaction. Statements run through the returned handle belong to it. Prefer
  /// [`with_transaction`](crate::with_transaction), [`transaction`](fn@crate::transaction) or
  /// [`transaction_layer`](crate::transaction_layer), which always finish it.
  fn begin(&self) -> Effect<Arc<dyn SqlTransaction>, SqlError>;

  /// Release the client's resources (close the pool). [`sql_client_layer`] runs it when its
  /// scope closes. The default does nothing.
  fn close(&self) -> Effect<(), Never> {
    succeed(())
  }
}

/// A [`SqlClientEnv`] wrapping `client` (for `.provide_all(..)` in tests and entry points).
#[inline]
pub fn sql_client_env(client: impl SqlClient) -> SqlClientEnv {
  hlist![SqlClientService::new(client)]
}

/// A [`Layer`] providing `client` as it is: nothing is connected or closed (the caller owns it).
#[inline]
pub fn provide_sql_client<E: Send + 'static>(client: impl SqlClient) -> Layer<SqlClientEnv, E> {
  Layer::succeed(sql_client_env(client))
}

/// The scoped client [`Layer`] (Effect.ts `Layer.scoped`): building it runs
/// [`SqlClient::connect`] (a failure fails the build with `E::from(err)`), and the build's scope
/// closing runs [`SqlClient::close`], after the effect a
/// [`provide_layer`](id_effect::Effect::provide_layer) runs, however it ends.
pub fn sql_client_layer<E>(client: impl SqlClient) -> Layer<SqlClientEnv, E>
where
  E: From<SqlError> + Send + 'static,
{
  let service = SqlClientService::new(client);
  Layer::acquire_release(
    move || {
      let service = service.clone();
      service
        .connect()
        .map_err(E::from)
        .map(move |()| hlist![service])
    },
    |env: SqlClientEnv, _kind| env.0.close(),
  )
}

/// Run `sql` on the [`SqlClientService`] in `R`.
pub fn query<R, I>(
  sql: impl Into<String>,
  params: Vec<SqlValue>,
) -> Effect<Vec<SqlRow>, SqlError, R>
where
  R: Selector<SqlClientService, I> + HList,
{
  let sql = sql.into();
  need::<SqlClientService>().flat_map(move |c| c.query(&sql, &params).narrow())
}

/// Execute `sql` on the [`SqlClientService`] in `R`.
pub fn execute<R, I>(sql: impl Into<String>, params: Vec<SqlValue>) -> Effect<u64, SqlError, R>
where
  R: Selector<SqlClientService, I> + HList,
{
  let sql = sql.into();
  need::<SqlClientService>().flat_map(move |c| c.execute(&sql, &params).narrow())
}

/// In-memory scriptable client for unit tests.
#[derive(Clone, Default)]
pub struct TestSqlClient {
  inner: TestState,
}

#[derive(Default)]
pub(crate) struct TestSqlClientState {
  pub(crate) connected: bool,
  pub(crate) closes: usize,
  pub(crate) queries: Vec<(String, Vec<SqlValue>)>,
  pub(crate) executes: Vec<(String, Vec<SqlValue>)>,
  pub(crate) query_results: HashMap<String, Vec<SqlRow>>,
  pub(crate) execute_rows: HashMap<String, u64>,
  pub(crate) fail_connect: bool,
  pub(crate) fail_query: Option<String>,
  pub(crate) fail_commit: bool,
  pub(crate) open_transactions: usize,
  pub(crate) commits: usize,
  pub(crate) rollbacks: usize,
}

pub(crate) type TestState = Arc<Mutex<TestSqlClientState>>;

pub(crate) fn lock(state: &TestState) -> MutexGuard<'_, TestSqlClientState> {
  state.lock().unwrap_or_else(PoisonError::into_inner)
}

impl TestSqlClientState {
  /// The scripted rows of a query (recording is the caller's job).
  pub(crate) fn run_query(&self, sql: &str) -> Result<Vec<SqlRow>, SqlError> {
    if !self.connected {
      return Err(SqlError::NotConnected);
    }
    if let Some(message) = self.fail_query.clone() {
      return Err(SqlError::QueryFailed {
        sql: sql.to_string(),
        message,
      });
    }
    Ok(self.query_results.get(sql).cloned().unwrap_or_default())
  }

  /// The scripted row count of a statement (recording is the caller's job).
  pub(crate) fn run_execute(&self, sql: &str) -> Result<u64, SqlError> {
    if !self.connected {
      return Err(SqlError::NotConnected);
    }
    Ok(*self.execute_rows.get(sql).unwrap_or(&0))
  }
}

fn ready<A: Send + 'static>(r: Result<A, SqlError>) -> Effect<A, SqlError> {
  match r {
    Ok(a) => succeed(a),
    Err(e) => fail(e),
  }
}

impl TestSqlClient {
  /// Empty test client (disconnected until [`SqlClient::connect`]).
  #[inline]
  pub fn new() -> Self {
    Self::default()
  }

  fn state(&self) -> MutexGuard<'_, TestSqlClientState> {
    lock(&self.inner)
  }

  /// Script rows returned for an exact SQL key (build them with [`SqlRow::from_pairs`]).
  #[inline]
  pub fn script_query(&self, sql: impl Into<String>, rows: Vec<SqlRow>) {
    self.state().query_results.insert(sql.into(), rows);
  }

  /// Script affected row count for an exact SQL key.
  #[inline]
  pub fn script_execute(&self, sql: impl Into<String>, rows: u64) {
    self.state().execute_rows.insert(sql.into(), rows);
  }

  /// Force the next connect attempt to fail.
  #[inline]
  pub fn fail_on_connect(&self) {
    self.state().fail_connect = true;
  }

  /// Force all queries to fail with `message`.
  #[inline]
  pub fn fail_queries(&self, message: impl Into<String>) {
    self.state().fail_query = Some(message.into());
  }

  /// Force every commit to fail (the transaction is then rolled back).
  #[inline]
  pub fn fail_commits(&self) {
    self.state().fail_commit = true;
  }

  /// Recorded query calls (sql, params). A transaction's statements are recorded when it
  /// commits; a rolled-back transaction's are discarded.
  #[inline]
  pub fn recorded_queries(&self) -> Vec<(String, Vec<SqlValue>)> {
    self.state().queries.clone()
  }

  /// Recorded execute calls (sql, params), with the same transaction rule as
  /// [`Self::recorded_queries`].
  #[inline]
  pub fn recorded_executes(&self) -> Vec<(String, Vec<SqlValue>)> {
    self.state().executes.clone()
  }

  /// Number of transactions currently open (not yet committed/rolled back).
  #[inline]
  pub fn open_transactions(&self) -> usize {
    self.state().open_transactions
  }

  /// Number of committed transactions.
  #[inline]
  pub fn commits(&self) -> usize {
    self.state().commits
  }

  /// Number of rolled-back transactions.
  #[inline]
  pub fn rollbacks(&self) -> usize {
    self.state().rollbacks
  }

  /// Whether the client is connected (connected and not closed since).
  #[inline]
  pub fn is_connected(&self) -> bool {
    self.state().connected
  }

  /// Number of [`SqlClient::close`] calls.
  #[inline]
  pub fn closes(&self) -> usize {
    self.state().closes
  }
}

impl SqlClient for TestSqlClient {
  fn connect(&self) -> Effect<(), SqlError> {
    let inner = Arc::clone(&self.inner);
    suspend(move || {
      let mut state = lock(&inner);
      if state.fail_connect {
        return fail(SqlError::NotConnected);
      }
      state.connected = true;
      succeed(())
    })
  }

  fn query(&self, sql: &str, params: &[SqlValue]) -> Effect<Vec<SqlRow>, SqlError> {
    let (inner, sql, params) = (Arc::clone(&self.inner), sql.to_string(), params.to_vec());
    suspend(move || {
      let mut state = lock(&inner);
      let out = state.run_query(&sql);
      if out.is_ok() {
        state.queries.push((sql, params));
      }
      ready(out)
    })
  }

  fn execute(&self, sql: &str, params: &[SqlValue]) -> Effect<u64, SqlError> {
    let (inner, sql, params) = (Arc::clone(&self.inner), sql.to_string(), params.to_vec());
    suspend(move || {
      let mut state = lock(&inner);
      let out = state.run_execute(&sql);
      if out.is_ok() {
        state.executes.push((sql, params));
      }
      ready(out)
    })
  }

  fn begin(&self) -> Effect<Arc<dyn SqlTransaction>, SqlError> {
    let inner = Arc::clone(&self.inner);
    suspend(move || {
      let mut state = lock(&inner);
      if !state.connected {
        return fail(SqlError::NotConnected);
      }
      state.open_transactions += 1;
      drop(state);
      succeed(Arc::new(TestSqlTransaction::new(inner)) as Arc<dyn SqlTransaction>)
    })
  }

  fn close(&self) -> Effect<(), Never> {
    let inner = Arc::clone(&self.inner);
    sync(move || {
      let mut state = lock(&inner);
      state.connected = false;
      state.closes += 1;
    })
  }
}

#[cfg(test)]
pub(crate) mod tests {
  use std::sync::Arc;

  use id_effect::{Cause, Exit};
  use id_effect::{Effect, fail, need, sync};

  use super::{
    SqlClient, SqlClientEnv, SqlClientService, SqlRow, TestSqlClient, execute, provide_sql_client,
    query, sql_client_env, sql_client_layer,
  };
  use crate::error::SqlError;
  use crate::params;
  use crate::value::SqlValue;

  /// Runs `eff` with `Effect::block_on`; the typed outcome as a `Result` (panics on any other cause).
  pub(crate) fn run<A: Send + 'static>(eff: Effect<A, SqlError>) -> Result<A, SqlError> {
    match eff.block_on() {
      Exit::Success(a) => Ok(a),
      Exit::Failure(Cause::Fail(e)) => Err(e),
      Exit::Failure(c) => panic!("unexpected cause: {c:?}"),
    }
  }

  #[test]
  fn sql_client_service_debug_formats_dyn() {
    let svc = SqlClientService::new(TestSqlClient::new());
    let rendered = format!("{svc:?}");
    assert!(rendered.contains("SqlClientService"));
    assert!(rendered.contains("dyn SqlClient"));
  }

  #[test]
  fn connect_then_query_returns_scripted_rows() {
    let test = TestSqlClient::new();
    test.script_query(
      "SELECT id FROM users",
      vec![SqlRow::from_pairs([("id", 1i64)])],
    );
    let client: Arc<dyn SqlClient> = Arc::new(test.clone());
    run(client.connect()).unwrap();
    let rows = run(client.query("SELECT id FROM users", &[SqlValue::I64(1)])).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].get::<i64>("id"), Ok(1));
    assert_eq!(test.recorded_queries().len(), 1);
  }

  #[test]
  #[allow(deprecated)]
  fn deprecated_sqlparam_still_converts_and_compares() {
    use super::SqlParam;
    let test = TestSqlClient::new();
    run(test.connect()).unwrap();
    run(test.query("Q", &[SqlParam::Int(1).into()])).unwrap();
    assert_eq!(test.recorded_queries()[0].1, [SqlParam::Int(1)]);
    assert_eq!(
      SqlRow::new(vec!["1".into()]).get::<String>(0usize),
      Ok("1".into())
    );
  }

  #[test]
  fn statements_before_connect_fail() {
    let client = TestSqlClient::new();
    assert_eq!(
      run(client.query("SELECT 1", &[])).unwrap_err(),
      SqlError::NotConnected
    );
    assert_eq!(
      run(client.execute("DELETE FROM t", &[])).unwrap_err(),
      SqlError::NotConnected
    );
    assert!(run(client.begin()).is_err());
  }

  #[test]
  fn failing_connect_and_queries() {
    let test = TestSqlClient::new();
    test.fail_on_connect();
    assert_eq!(run(test.connect()).unwrap_err(), SqlError::NotConnected);
    let test = TestSqlClient::new();
    run(test.connect()).unwrap();
    test.fail_queries("boom");
    assert!(matches!(
      run(test.query("SELECT 1", &[])),
      Err(SqlError::QueryFailed { .. })
    ));
    assert!(test.recorded_queries().is_empty());
  }

  #[test]
  fn execute_returns_scripted_count() {
    let test = TestSqlClient::new();
    test.script_execute("DELETE FROM t", 3);
    run(test.connect()).unwrap();
    assert_eq!(run(test.execute("DELETE FROM t", &[])).unwrap(), 3);
    assert_eq!(test.recorded_executes().len(), 1);
  }

  #[test]
  fn begin_opens_transaction() {
    let test = TestSqlClient::new();
    run(test.connect()).unwrap();
    let tx = run(test.begin()).unwrap();
    assert_eq!(test.open_transactions(), 1);
    run(tx.commit()).unwrap();
    assert_eq!(test.open_transactions(), 0);
  }

  #[test]
  fn sql_values_are_recorded_as_bound() {
    let test = TestSqlClient::new();
    run(test.connect()).unwrap();
    let id = uuid::Uuid::from_u128(7);
    let params = params![None::<i64>, true, 1i64, "t", vec![1u8, 2], id, 2.5f64];
    run(test.query("Q", &params)).unwrap();
    assert_eq!(test.recorded_queries(), [("Q".to_owned(), params)]);
  }

  #[test]
  fn free_functions_read_the_client_from_the_environment() {
    let test = TestSqlClient::new();
    test.script_query("SELECT 1", vec![SqlRow::default()]);
    test.script_execute("DELETE FROM t", 2);
    run(test.connect()).unwrap();
    let rows = run(query("SELECT 1", vec![]).provide_all(sql_client_env(test.clone()))).unwrap();
    assert_eq!(rows.len(), 1);
    let n =
      run(execute("DELETE FROM t", vec![]).provide_all(sql_client_env(test.clone()))).unwrap();
    assert_eq!(n, 2);
  }

  #[test]
  fn client_layer_connects_on_build_and_closes_after_the_effect() {
    let test = TestSqlClient::new();
    let probe = test.clone();
    let body: Effect<bool, SqlError, SqlClientEnv> =
      need::<SqlClientService>().flat_map(move |_| sync(move || probe.is_connected()));
    assert!(run(body.provide_layer(sql_client_layer(test.clone()))).unwrap());
    assert!(!test.is_connected());
    assert_eq!(test.closes(), 1);
  }

  #[test]
  fn client_layer_closes_after_a_failing_effect() {
    let test = TestSqlClient::new();
    let body: Effect<(), SqlError, SqlClientEnv> = query::<SqlClientEnv, _>("SELECT 1", vec![])
      .flat_map(|_| fail(SqlError::Unsupported("x".into())));
    let err = run(body.provide_layer(sql_client_layer(test.clone()))).unwrap_err();
    assert_eq!(err, SqlError::Unsupported("x".into()));
    assert_eq!(test.closes(), 1);
  }

  #[test]
  fn client_layer_failing_connect_fails_the_build_and_closes_nothing() {
    let test = TestSqlClient::new();
    test.fail_on_connect();
    let body: Effect<(), SqlError, SqlClientEnv> = need::<SqlClientService>().map(|_| ());
    let err = run(body.provide_layer(sql_client_layer(test.clone()))).unwrap_err();
    assert_eq!(err, SqlError::NotConnected);
    assert_eq!(test.closes(), 0);
  }

  #[test]
  fn provide_sql_client_neither_connects_nor_closes() {
    let test = TestSqlClient::new();
    let body: Effect<(), SqlError, SqlClientEnv> = need::<SqlClientService>().map(|_| ());
    run(body.provide_layer(provide_sql_client(test.clone()))).unwrap();
    assert!(!test.is_connected());
    assert_eq!(test.closes(), 0);
  }
}
