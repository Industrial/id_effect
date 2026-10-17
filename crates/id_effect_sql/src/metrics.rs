//! Automatic database client metrics and spans: [`InstrumentedSqlClient`] wraps any
//! [`SqlClient`].
//!
//! Each `connect`, `query`, `execute` and `begin` records, however it ends:
//!
//! - [`DB_CLIENT_OPERATIONS_TOTAL`]`{op,outcome}` — `outcome` is `ok`, or `error` for a typed
//!   failure, a defect or an interrupt;
//! - [`DB_CLIENT_OPERATION_DURATION_SECONDS`]`{op}` — executor clock.
//!
//! Labels never carry SQL text, so the series set is bounded (4 ops × 2 outcomes). Statements
//! run through a transaction handle are not counted (the `begin` that opened it is). With the
//! `pg` feature, `record_pg_pool` samples a pool's connection gauges; call it before each
//! scrape (for example from `id_effect_axum::metrics::metrics_route_with`).
//!
//! Each `query` and `execute` also runs inside a `db.query` span ([`Effect::with_span_attrs`])
//! with the attributes `db.system` ([`InstrumentedSqlClient::with_db_system`], default
//! `other_sql`), `db.operation` (the statement's first keyword, `SELECT`), `db.statement` (the
//! SQL text with every string and numeric literal replaced by `?`, at most 1024 characters) and
//! `otel.kind = client`. Bound parameters never reach a span. Under
//! `id_effect_opentelemetry::OtelSpanHook` the span is exported as an OpenTelemetry client span,
//! a child of the caller's span; this crate has no OpenTelemetry dependency.
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

/// Gauge: pool connections, by `state` (`idle` or `used`), set by `record_pg_pool` (feature
/// `pg`).
pub const DB_CLIENT_CONNECTION_COUNT: &str = "db_client_connection_count";

/// Gauge: the pool's maximum connections, set by `record_pg_pool` (feature `pg`).
pub const DB_CLIENT_CONNECTION_MAX: &str = "db_client_connection_max";

/// The duration buckets of [`DB_CLIENT_OPERATION_DURATION_SECONDS`] (1 ms to 10 s).
pub fn duration_boundaries() -> Boundaries {
  Boundaries::new([
    0.001, 0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0,
  ])
}

/// Span name of `query` / `execute` (see the [module docs](self)).
pub const DB_QUERY_SPAN: &str = "db.query";

/// Longest `db.statement` span attribute, in characters.
const STATEMENT_MAX_CHARS: usize = 1024;

/// A [`SqlClient`] that records [`DB_CLIENT_OPERATIONS_TOTAL`] and
/// [`DB_CLIENT_OPERATION_DURATION_SECONDS`] around every operation of `C` and runs statements in
/// a [`DB_QUERY_SPAN`] span (see the [module docs](self)). `close` is passed through unrecorded.
#[derive(Clone, Debug)]
pub struct InstrumentedSqlClient<C> {
  inner: C,
  operations: Counter,
  duration: Histogram,
  db_system: &'static str,
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
      db_system: "other_sql",
    }
  }

  /// Sets the spans' `db.system` attribute (OpenTelemetry names: `postgresql`, `mysql`,
  /// `sqlite`; default `other_sql`).
  pub fn with_db_system(mut self, system: &'static str) -> Self {
    self.db_system = system;
    self
  }

  /// The wrapped client.
  pub fn inner(&self) -> &C {
    &self.inner
  }

  /// `eff` (statement `sql`, operation `op`) instrumented and run in a [`DB_QUERY_SPAN`] span.
  fn statement<A: Send + 'static>(
    &self,
    op: &'static str,
    sql: &str,
    eff: Effect<A, SqlError>,
  ) -> Effect<A, SqlError> {
    let statement = statement_summary(sql);
    let operation = statement
      .split(|c: char| !c.is_ascii_alphabetic())
      .find(|word| !word.is_empty())
      .unwrap_or_default()
      .to_ascii_uppercase();
    self.instrument(op, eff).with_span_attrs(
      DB_QUERY_SPAN,
      [
        ("otel.kind", "client".to_owned()),
        ("db.system", self.db_system.to_owned()),
        ("db.operation", operation),
        ("db.statement", statement),
      ],
    )
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
    self.statement("query", sql, self.inner.query(sql, params))
  }

  fn execute(&self, sql: &str, params: &[SqlValue]) -> Effect<u64, SqlError> {
    self.statement("execute", sql, self.inner.execute(sql, params))
  }

  fn begin(&self) -> Effect<Arc<dyn SqlTransaction>, SqlError> {
    self.instrument("begin", self.inner.begin())
  }

  fn close(&self) -> Effect<(), Never> {
    self.inner.close()
  }
}

/// `sql` with every string literal (`'...'`, `''` escapes) and numeric literal replaced by `?`,
/// whitespace runs collapsed to one space, truncated to [`STATEMENT_MAX_CHARS`]: the statement's
/// shape without its inline values. Placeholders (`$1`, `?`) and identifiers (`t1`) are kept.
fn statement_summary(sql: &str) -> String {
  let mut out = String::new();
  let mut len = 0;
  let mut prev = ' ';
  let mut chars = sql.chars().peekable();
  while let Some(c) = chars.next() {
    if len >= STATEMENT_MAX_CHARS {
      break;
    }
    let pushed = if c == '\'' {
      while let Some(n) = chars.next() {
        if n == '\'' && chars.next_if_eq(&'\'').is_none() {
          break;
        }
      }
      '?'
    } else if c.is_whitespace() {
      if prev == ' ' {
        continue;
      }
      ' '
    } else if c.is_ascii_digit() && !(prev.is_alphanumeric() || matches!(prev, '_' | '$')) {
      while chars
        .next_if(|n| n.is_ascii_alphanumeric() || *n == '.')
        .is_some()
      {}
      '?'
    } else {
      c
    };
    out.push(pushed);
    len += 1;
    prev = pushed;
  }
  out.truncate(out.trim_end().len());
  out
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

  /// A started span's name and attributes.
  type Started = (String, Vec<(String, String)>);

  /// Records every span that starts.
  #[derive(Default)]
  struct Recorder(std::sync::Mutex<Vec<Started>>);

  impl id_effect::SpanHook for Recorder {
    fn on_start(&self, span: &id_effect::Span) {
      self
        .0
        .lock()
        .unwrap()
        .push((span.name().to_owned(), span.attributes().to_vec()));
    }
  }

  #[test]
  fn statements_run_in_a_db_query_span_without_values() {
    let test = TestSqlClient::new();
    run(test.connect()).unwrap();
    let client = InstrumentedSqlClient::with_registry(test, MetricRegistry::new())
      .with_db_system("postgresql");
    let recorder = Arc::new(Recorder::default());
    let hook: Arc<dyn id_effect::SpanHook> = recorder.clone();
    let secret = SqlValue::Text("hunter2".into());
    let sql = "select *\n  from users where name = 'ada' and id = $1 and age > 42";
    let _ = run(client.query(sql, &[secret]).with_span_hook(hook.clone()));
    run(
      client
        .execute("DELETE FROM t", &[])
        .with_span_hook(hook.clone()),
    )
    .unwrap();
    run(client.begin().with_span_hook(hook)).unwrap();

    let spans = recorder.0.lock().unwrap();
    assert_eq!(spans.len(), 2, "begin opens no span: {spans:?}");
    let (name, attrs) = &spans[0];
    assert_eq!(name, DB_QUERY_SPAN);
    let get = |k: &str| {
      attrs
        .iter()
        .find(|(key, _)| key == k)
        .map(|(_, v)| v.as_str())
    };
    assert_eq!(get("otel.kind"), Some("client"));
    assert_eq!(get("db.system"), Some("postgresql"));
    assert_eq!(get("db.operation"), Some("SELECT"));
    assert_eq!(
      get("db.statement"),
      Some("select * from users where name = ? and id = $1 and age > ?")
    );
    assert!(
      attrs
        .iter()
        .all(|(_, v)| !v.contains("hunter2") && !v.contains("ada"))
    );
    assert_eq!(spans[1].1[2], ("db.operation".into(), "DELETE".into()));
  }

  #[test]
  fn statement_summary_masks_literals_and_keeps_shape() {
    assert_eq!(
      statement_summary("UPDATE t1 SET note = 'it''s', n = -3.5e2 WHERE k = ?"),
      "UPDATE t1 SET note = ?, n = -? WHERE k = ?"
    );
    assert_eq!(statement_summary("  SELECT 'open"), "SELECT ?");
    assert_eq!(
      statement_summary("INSERT INTO col_2 VALUES (0x1F)"),
      "INSERT INTO col_2 VALUES (?)"
    );
    let long = format!("SELECT {}", "abc,".repeat(2000));
    assert_eq!(
      statement_summary(&long).chars().count(),
      STATEMENT_MAX_CHARS
    );
    let client = InstrumentedSqlClient::new(TestSqlClient::new());
    assert_eq!(client.db_system, "other_sql");
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
