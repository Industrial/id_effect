//! Driver-agnostic SQL client trait, its capability and layers, and an in-memory test double.
//!
//! The capability is [`SqlClientService`] (environment [`SqlClientEnv`]). [`sql_client_layer`]
//! is the scoped [`Layer`] that connects a client when it is built and closes it when the
//! build's scope closes; [`provide_sql_client`] provides a client that someone else owns.

use std::collections::HashMap;
use std::fmt;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use id_effect::Never;
use id_effect::{
  Effect, HCons, HList, HNil, Layer, Selector, Stream, fail, hlist, need, sleep, succeed, suspend,
  sync,
};

use crate::error::SqlError;
use crate::raw::Sql;
pub use crate::row::SqlRow;
use crate::transaction::{
  SqlTransaction, TestSqlTransaction, TransactionOptions, begin_with_options,
};
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

  /// Begin a transaction with `options` (isolation level, read-only, deferrable). The default
  /// runs [`TransactionOptions::set_transaction_sql`] as the transaction's first statement
  /// (rolling back if it fails). Prefer
  /// [`with_transaction_options`](crate::with_transaction_options) /
  /// [`transaction_with`](crate::transaction_with).
  fn begin_with(&self, options: TransactionOptions) -> Effect<Arc<dyn SqlTransaction>, SqlError> {
    begin_with_options(self.begin(), options)
  }

  /// Run a read query and stream its rows (see [`crate::stream`]). Nothing runs until the
  /// stream is pulled; dropping or interrupting it stops the query. The default runs
  /// [`query`](Self::query) on the first pull and serves the collected rows (no bounded
  /// buffering); drivers override it (`PgSqlClient` streams from a server-side fetch with
  /// bounded buffering).
  fn query_stream(&self, sql: &str, params: &[SqlValue]) -> Stream<SqlRow, SqlError> {
    Stream::from_effect(self.query(sql, params))
  }

  /// Release the client's resources (close the pool). [`sql_client_layer`] runs it when its
  /// scope closes. The default does nothing.
  fn close(&self) -> Effect<(), Never> {
    succeed(())
  }
}

/// A shared or boxed client is a client: every method forwards to the pointee, its overrides
/// (`begin_with`, `query_stream`, `close`) included, so a wrapper generic over `C: SqlClient`
/// (such as [`InstrumentedSqlClient`](crate::metrics::InstrumentedSqlClient)) wraps an
/// `Arc<dyn SqlClient>` directly.
macro_rules! forward_sql_client {
  ($($pointer:ident),+) => {$(
    impl<T: SqlClient + ?Sized> SqlClient for $pointer<T> {
      fn connect(&self) -> Effect<(), SqlError> {
        (**self).connect()
      }

      fn query(&self, sql: &str, params: &[SqlValue]) -> Effect<Vec<SqlRow>, SqlError> {
        (**self).query(sql, params)
      }

      fn execute(&self, sql: &str, params: &[SqlValue]) -> Effect<u64, SqlError> {
        (**self).execute(sql, params)
      }

      fn begin(&self) -> Effect<Arc<dyn SqlTransaction>, SqlError> {
        (**self).begin()
      }

      fn begin_with(
        &self,
        options: TransactionOptions,
      ) -> Effect<Arc<dyn SqlTransaction>, SqlError> {
        (**self).begin_with(options)
      }

      fn query_stream(&self, sql: &str, params: &[SqlValue]) -> Stream<SqlRow, SqlError> {
        (**self).query_stream(sql, params)
      }

      fn close(&self) -> Effect<(), Never> {
        (**self).close()
      }
    }
  )+};
}

forward_sql_client!(Arc, Box);

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

/// Run `sql` on the [`SqlClientService`] in `R`. `sql` is a literal or a `const` ([`Sql`]); SQL
/// built at run time goes through [`query_raw`](crate::query_raw).
pub fn query<R, I>(sql: impl Into<Sql>, params: Vec<SqlValue>) -> Effect<Vec<SqlRow>, SqlError, R>
where
  R: Selector<SqlClientService, I> + HList,
{
  let sql = sql.into();
  need::<SqlClientService>().flat_map(move |c| c.query(sql.as_str(), &params).narrow())
}

/// Execute `sql` on the [`SqlClientService`] in `R`. `sql` is a literal or a `const` ([`Sql`]);
/// SQL built at run time goes through [`execute_raw`](crate::execute_raw).
pub fn execute<R, I>(sql: impl Into<Sql>, params: Vec<SqlValue>) -> Effect<u64, SqlError, R>
where
  R: Selector<SqlClientService, I> + HList,
{
  let sql = sql.into();
  need::<SqlClientService>().flat_map(move |c| c.execute(sql.as_str(), &params).narrow())
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
  /// Latency of every [`SqlClient::query`] (a sleep on the executor clock).
  pub(crate) query_delay: Option<Duration>,
  pub(crate) fail_commit: bool,
  pub(crate) open_transactions: usize,
  pub(crate) commits: usize,
  pub(crate) rollbacks: usize,
  /// Commits still to fail with a serialization failure.
  pub(crate) serialization_failures: usize,
  /// [`SqlClient::query_stream`] streams started and not yet finished or dropped.
  pub(crate) open_streams: usize,
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

  /// Delay every query by `delay` before it runs (latency injection): a [`sleep`] on the
  /// fiber's executor clock, so virtual time on a
  /// [`TestExecutor`](id_effect::TestExecutor). `Duration::ZERO` removes the delay.
  #[inline]
  pub fn delay_queries(&self, delay: Duration) {
    self.state().query_delay = (!delay.is_zero()).then_some(delay);
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

  /// Make the next `n` commits fail with a serialization failure (SQLSTATE `40001`, see
  /// [`is_serialization_failure`](crate::is_serialization_failure)); the transaction is then
  /// rolled back.
  #[inline]
  pub fn fail_commits_with_serialization_failure(&self, n: usize) {
    self.state().serialization_failures = n;
  }

  /// Number of [`SqlClient::query_stream`] streams started and neither drained nor dropped (`0`
  /// once a consumer stops early, fails or is interrupted).
  #[inline]
  pub fn open_streams(&self) -> usize {
    self.state().open_streams
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
      let delay = lock(&inner).query_delay;
      let run = suspend(move || {
        let mut state = lock(&inner);
        let out = state.run_query(&sql);
        if out.is_ok() {
          state.queries.push((sql, params));
        }
        ready(out)
      });
      match delay {
        Some(d) => sleep(d).then(run),
        None => run,
      }
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

  /// The scripted rows, served lazily in the requested chunk sizes; the query is recorded (and
  /// [`TestSqlClient::open_streams`] counts it) from the first pull.
  fn query_stream(&self, sql: &str, params: &[SqlValue]) -> Stream<SqlRow, SqlError> {
    crate::stream::test_query_stream(Arc::clone(&self.inner), sql.to_string(), params.to_vec())
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
  use std::sync::{Arc, Mutex};

  use id_effect::{Cause, Exit, Never, Stream};
  use id_effect::{Effect, fail, need, sync};

  use super::{
    SqlClient, SqlClientEnv, SqlClientService, SqlRow, TestSqlClient, execute, provide_sql_client,
    query, sql_client_env, sql_client_layer,
  };
  use crate::error::SqlError;
  use crate::params;
  use crate::transaction::{SqlTransaction, TransactionOptions};
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
  fn delay_queries_sleeps_on_the_executor_clock() {
    use std::time::Duration;

    use id_effect::TestExecutor;

    let test = TestSqlClient::new();
    test.script_query("SELECT 1", vec![SqlRow::default()]);
    run(test.connect()).unwrap();
    test.delay_queries(Duration::from_secs(30));
    let exec = TestExecutor::new();
    let h = test
      .query("SELECT 1", &[])
      .fork_root(Arc::new(exec.clone()));
    exec.advance(Duration::from_secs(30) - Duration::from_nanos(1));
    assert!(!h.is_done());
    assert!(test.recorded_queries().is_empty(), "runs after the delay");
    exec.advance(Duration::from_nanos(1));
    assert!(h.is_done());
    assert_eq!(test.recorded_queries().len(), 1);
    test.delay_queries(Duration::ZERO);
    assert_eq!(run(test.query("SELECT 1", &[])).unwrap().len(), 1);
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

  /// A client recording which [`SqlClient`] method ran, the defaulted ones (`begin_with`,
  /// `query_stream`, `close`) included, over a [`TestSqlClient`]. `begin_with` skips the
  /// default's `SET TRANSACTION`, as a driver's own BEGIN does.
  #[derive(Clone, Default)]
  pub(crate) struct Recording {
    pub(crate) inner: TestSqlClient,
    calls: Arc<Mutex<Vec<&'static str>>>,
  }

  impl Recording {
    /// Over a connected [`TestSqlClient`] with two rows scripted for `SELECT n`.
    pub(crate) fn connected() -> Self {
      let r = Self::default();
      run(r.inner.connect()).unwrap();
      let row = |n: i64| SqlRow::from_pairs([("n", n)]);
      r.inner.script_query("SELECT n", vec![row(1), row(2)]);
      r
    }

    pub(crate) fn calls(&self) -> Vec<&'static str> {
      self.calls.lock().unwrap().clone()
    }

    fn record(&self, method: &'static str) {
      self.calls.lock().unwrap().push(method);
    }
  }

  impl SqlClient for Recording {
    fn connect(&self) -> Effect<(), SqlError> {
      self.record("connect");
      self.inner.connect()
    }

    fn query(&self, sql: &str, params: &[SqlValue]) -> Effect<Vec<SqlRow>, SqlError> {
      self.record("query");
      self.inner.query(sql, params)
    }

    fn execute(&self, sql: &str, params: &[SqlValue]) -> Effect<u64, SqlError> {
      self.record("execute");
      self.inner.execute(sql, params)
    }

    fn begin(&self) -> Effect<Arc<dyn SqlTransaction>, SqlError> {
      self.record("begin");
      self.inner.begin()
    }

    fn begin_with(&self, _: TransactionOptions) -> Effect<Arc<dyn SqlTransaction>, SqlError> {
      self.record("begin_with");
      self.inner.begin()
    }

    fn query_stream(&self, sql: &str, params: &[SqlValue]) -> Stream<SqlRow, SqlError> {
      self.record("query_stream");
      self.inner.query_stream(sql, params)
    }

    fn close(&self) -> Effect<(), Never> {
      self.record("close");
      self.inner.close()
    }
  }

  /// Runs every [`SqlClient`] method of `client` once.
  fn every_method(client: impl SqlClient) {
    run(client.connect()).unwrap();
    run(client.query("SELECT 1", &[])).unwrap();
    run(client.execute("DELETE FROM t", &[])).unwrap();
    run(run(client.begin()).unwrap().commit()).unwrap();
    let options = TransactionOptions::default();
    run(run(client.begin_with(options)).unwrap().commit()).unwrap();
    let rows = run(client.query_stream("SELECT n", &[]).run_collect()).unwrap();
    assert_eq!(rows.len(), 2);
    assert!(matches!(client.close().block_on(), Exit::Success(())));
  }

  #[test]
  fn shared_and_boxed_clients_forward_every_method() {
    const ALL: [&str; 7] = [
      "connect",
      "query",
      "execute",
      "begin",
      "begin_with",
      "query_stream",
      "close",
    ];
    let shared = Recording::connected();
    every_method(Arc::new(shared.clone()) as Arc<dyn SqlClient>);
    assert_eq!(shared.calls(), ALL);
    let arc = Recording::connected();
    every_method(Arc::new(arc.clone()));
    assert_eq!(arc.calls(), ALL);
    let boxed = Recording::connected();
    every_method(Box::new(boxed.clone()) as Box<dyn SqlClient>);
    assert_eq!(boxed.calls(), ALL);
    // A decorator generic over its client wraps the shared one directly.
    let service = SqlClientService::new(Arc::new(Recording::connected()) as Arc<dyn SqlClient>);
    assert_eq!(run(service.query("SELECT n", &[])).unwrap().len(), 2);
  }
}
