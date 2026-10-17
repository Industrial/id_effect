//! Automatic database client metrics: [`InstrumentedSqlClient`] wraps any [`SqlClient`].
//!
//! Each `connect`, `query`, `execute` and `begin` records, however it ends:
//!
//! - [`DB_CLIENT_OPERATIONS_TOTAL`]`{op,outcome}` — `outcome` is `ok`, or `error` for a typed
//!   failure, a defect or an interrupt;
//! - [`DB_CLIENT_OPERATION_DURATION_SECONDS`]`{op}` — executor clock.
//!
//! Labels never carry SQL text, so the series set is bounded (4 ops × 2 outcomes). Statements
//! run through a transaction handle are not counted (the `begin` that opened it is). With the
//! `pg` feature, [`record_pg_pool`] samples a pool's connection gauges; call it before each
//! scrape (for example from `id_effect_axum::metrics::metrics_route_with`).
//!
//! ```rust
//! use id_effect::{Exit, MetricRegistry, MetricState};
//! use id_effect_sql::metrics::InstrumentedSqlClient;
//! use id_effect_sql::{SqlClient, TestSqlClient};
//!
//! let registry = MetricRegistry::new();
//! let client = InstrumentedSqlClient::with_registry(TestSqlClient::new(), registry.clone());
//! assert!(matches!(client.connect().block_on(), Exit::Success(())));
//! let ok = registry
//!   .snapshot()
//!   .into_iter()
//!   .find(|p| p.key.name() == "db_client_operations_total")
//!   .unwrap();
//! assert_eq!(ok.state, MetricState::Counter { count: 1 });
//! ```

use std::sync::Arc;

use id_effect::{Boundaries, Counter, Effect, Histogram, Metric, MetricRegistry, Never};

use crate::client::SqlClient;
use crate::error::SqlError;
use crate::transaction::SqlTransaction;
use crate::{SqlRow, SqlValue};

/// Counter: client operations, by `op` and `outcome`.
pub const DB_CLIENT_OPERATIONS_TOTAL: &str = "db_client_operations_total";

/// Histogram: client operation duration in seconds, by `op`.
pub const DB_CLIENT_OPERATION_DURATION_SECONDS: &str = "db_client_operation_duration_seconds";

/// Gauge: pool connections, by `state` (`idle` or `used`), set by [`record_pg_pool`].
pub const DB_CLIENT_CONNECTION_COUNT: &str = "db_client_connection_count";

/// Gauge: the pool's maximum connections, set by [`record_pg_pool`].
pub const DB_CLIENT_CONNECTION_MAX: &str = "db_client_connection_max";

/// The duration buckets of [`DB_CLIENT_OPERATION_DURATION_SECONDS`] (1 ms to 10 s).
pub fn duration_boundaries() -> Boundaries {
  Boundaries::new([
    0.001, 0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0,
  ])
}

/// A [`SqlClient`] that records [`DB_CLIENT_OPERATIONS_TOTAL`] and
/// [`DB_CLIENT_OPERATION_DURATION_SECONDS`] around every operation of `C` (see the
/// [module docs](self)). `close` is passed through unrecorded.
#[derive(Clone, Debug)]
pub struct InstrumentedSqlClient<C> {
  inner: C,
  operations: Counter,
  duration: Histogram,
}

impl<C: SqlClient> InstrumentedSqlClient<C> {
  /// `inner`, recording into [`MetricRegistry::global`].
  pub fn new(inner: C) -> Self {
    Self::with_registry(inner, MetricRegistry::global().clone())
  }

  /// `inner`, recording into `registry`.
  pub fn with_registry(inner: C, registry: MetricRegistry) -> Self {
    Self {
      inner,
      operations: registry.counter(DB_CLIENT_OPERATIONS_TOTAL),
      duration: registry.histogram(DB_CLIENT_OPERATION_DURATION_SECONDS, duration_boundaries()),
    }
  }

  /// The wrapped client.
  pub fn inner(&self) -> &C {
    &self.inner
  }

  /// `eff` with the operation counter and duration recorded under `op`.
  fn instrument<A: Send + 'static>(
    &self,
    op: &'static str,
    eff: Effect<A, SqlError>,
  ) -> Effect<A, SqlError> {
    let ops = self.operations.tagged("op", op);
    let eff = ops
      .tagged("outcome", "ok")
      .track_exit_with(eff, |k| k.is_success().then_some(1));
    let eff = ops
      .tagged("outcome", "error")
      .track_exit_with(eff, |k| (!k.is_success()).then_some(1));
    self
      .duration
      .tagged("op", op)
      .track_duration_with(eff, |d| d.as_secs_f64())
  }
}

impl<C: SqlClient> SqlClient for InstrumentedSqlClient<C> {
  fn connect(&self) -> Effect<(), SqlError> {
    self.instrument("connect", self.inner.connect())
  }

  fn query(&self, sql: &str, params: &[SqlValue]) -> Effect<Vec<SqlRow>, SqlError> {
    self.instrument("query", self.inner.query(sql, params))
  }

  fn execute(&self, sql: &str, params: &[SqlValue]) -> Effect<u64, SqlError> {
    self.instrument("execute", self.inner.execute(sql, params))
  }

  fn begin(&self) -> Effect<Arc<dyn SqlTransaction>, SqlError> {
    self.instrument("begin", self.inner.begin())
  }

  fn close(&self) -> Effect<(), Never> {
    self.inner.close()
  }
}

/// Sets the connection gauges of `client`'s pool in `registry`:
/// [`DB_CLIENT_CONNECTION_COUNT`]`{state="idle"|"used"}` and [`DB_CLIENT_CONNECTION_MAX`].
/// Reads the pool through the public [`PgSqlClient::pool`](crate::pg::PgSqlClient::pool).
#[cfg(feature = "pg")]
pub fn record_pg_pool(client: &crate::pg::PgSqlClient, registry: &MetricRegistry) {
  let pool = client.pool();
  let size = i64::from(pool.size());
  let idle = i64::try_from(pool.num_idle()).unwrap_or(i64::MAX);
  let count = registry.gauge(DB_CLIENT_CONNECTION_COUNT);
  count.tagged("state", "idle").update_now(idle);
  count
    .tagged("state", "used")
    .update_now(size.saturating_sub(idle).max(0));
  registry
    .gauge(DB_CLIENT_CONNECTION_MAX)
    .update_now(i64::from(pool.options().get_max_connections()));
}

#[cfg(test)]
mod tests {
  use id_effect::{MetricState, Runtime};

  use super::*;
  use crate::TestSqlClient;
  use crate::client::tests::run;

  fn count(registry: &MetricRegistry, op: &str, outcome: &str) -> u64 {
    registry
      .snapshot()
      .into_iter()
      .find(|p| {
        p.key.name() == DB_CLIENT_OPERATIONS_TOTAL
          && p.key.tags()
            == [
              ("op".to_owned(), op.to_owned()),
              ("outcome".to_owned(), outcome.to_owned()),
            ]
      })
      .map_or(0, |p| match p.state {
        MetricState::Counter { count } => count,
        other => panic!("not a counter: {other:?}"),
      })
  }

  fn durations(registry: &MetricRegistry, op: &str) -> u64 {
    registry
      .snapshot()
      .into_iter()
      .find(|p| {
        p.key.name() == DB_CLIENT_OPERATION_DURATION_SECONDS
          && p.key.tags() == [("op".to_owned(), op.to_owned())]
      })
      .map_or(0, |p| match p.state {
        MetricState::Histogram(h) => h.count,
        other => panic!("not a histogram: {other:?}"),
      })
  }

  #[test]
  fn records_operations_by_op_and_outcome_with_durations() {
    let registry = MetricRegistry::new();
    let test = TestSqlClient::new();
    test.script_query("SELECT 1", vec![SqlRow::default()]);
    let client = InstrumentedSqlClient::with_registry(test.clone(), registry.clone());

    assert_eq!(
      run(client.query("SELECT 1", &[])).unwrap_err(),
      SqlError::NotConnected
    );
    run(client.connect()).unwrap();
    assert_eq!(run(client.query("SELECT 1", &[])).unwrap().len(), 1);
    assert_eq!(run(client.query("SELECT 1", &[])).unwrap().len(), 1);
    run(client.execute("DELETE FROM t", &[])).unwrap();
    let tx = run(client.begin()).unwrap();
    run(tx.commit()).unwrap();
    test.fail_queries("boom");
    assert!(run(client.query("SELECT 1", &[])).is_err());
    let closed = Runtime::global().block_on(client.close());
    assert!(matches!(closed, id_effect::Exit::Success(())));

    assert_eq!(count(&registry, "connect", "ok"), 1);
    assert_eq!(count(&registry, "query", "ok"), 2);
    assert_eq!(count(&registry, "query", "error"), 2);
    assert_eq!(count(&registry, "execute", "ok"), 1);
    assert_eq!(count(&registry, "begin", "ok"), 1);
    assert_eq!(count(&registry, "connect", "error"), 0);
    assert_eq!(durations(&registry, "query"), 4);
    assert_eq!(durations(&registry, "connect"), 1);
    assert_eq!(test.recorded_queries().len(), 2, "the inner client ran");
    assert_eq!(test.closes(), 1, "close passes through");
    // Only bounded labels: no SQL text anywhere in the series.
    assert!(
      registry
        .snapshot()
        .iter()
        .all(|p| p.key.tags().iter().all(|(_, v)| !v.contains("SELECT")))
    );
    assert!(registry.snapshot().iter().all(|p| p.key.name() != "op"));
  }

  #[test]
  fn the_wrapper_is_a_client_capability() {
    let test = TestSqlClient::new();
    test.script_execute("DELETE FROM t", 2);
    run(test.connect()).unwrap();
    let client = InstrumentedSqlClient::new(test.clone());
    assert!(client.inner().is_connected());
    let n = run(crate::execute("DELETE FROM t", vec![]).provide_all(crate::sql_client_env(client)))
      .unwrap();
    assert_eq!(n, 2);
  }
}
