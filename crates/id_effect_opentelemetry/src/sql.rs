//! Database spans where OpenTelemetry is set up: [`instrumented_sql_client`] and
//! [`instrumented_sql_layer`] wrap the [`SqlClientService`] capability in `id_effect_sql`'s
//! [`InstrumentedSqlClient`] (feature `sql`).
//!
//! Every `query`, `execute` and first `query_stream` pull through the wrapped capability then
//! runs in a `db.query` client span (`db.system`, `db.operation`, `db.statement` with literals
//! replaced by `?`; bound parameters never reach a span), exported under
//! [`OtelSpanHook`](crate::OtelSpanHook) as a child of the caller's span, and is counted in the
//! `db_client_operations_total` / `db_client_operation_duration_seconds` metrics.

use id_effect::{Layer, hlist, need};
use id_effect_sql::metrics::InstrumentedSqlClient;
use id_effect_sql::{SqlClientEnv, SqlClientService};

/// `service`, instrumented: its statements run in `db.query` spans whose `db.system` is
/// `db_system` (`postgresql`, `mysql`, `sqlite`; see
/// [`InstrumentedSqlClient::with_db_system`]). Metrics go to the global
/// [`MetricRegistry`](id_effect::MetricRegistry). Wrap a client once: a second wrap opens a
/// second span per statement.
pub fn instrumented_sql_client(
  service: SqlClientService,
  db_system: &'static str,
) -> SqlClientService {
  SqlClientService::new(InstrumentedSqlClient::new(service.0).with_db_system(db_system))
}

/// The client `inner` provides, instrumented with [`instrumented_sql_client`]: put it where the
/// app sets up OpenTelemetry, e.g. `instrumented_sql_layer(sql_client_layer(pg), "postgresql")`,
/// and every program reading the [`SqlClientService`] gets its statements traced.
pub fn instrumented_sql_layer<E: Send + 'static>(
  inner: Layer<SqlClientEnv, E>,
  db_system: &'static str,
) -> Layer<SqlClientEnv, E> {
  let instrumented: Layer<SqlClientEnv, E, SqlClientEnv> = Layer::effect(move || {
    need::<SqlClientService>()
      .map(move |service| hlist![instrumented_sql_client((*service).clone(), db_system)])
  });
  inner.provide_to(instrumented)
}

#[cfg(test)]
mod tests {
  use id_effect::{Exit, Never};
  use id_effect_sql::{SqlClient, SqlRow, TestSqlClient, provide_sql_client};
  use opentelemetry::Value;
  use opentelemetry::trace::SpanKind;
  use opentelemetry_sdk::trace::InMemorySpanExporter;

  use super::*;
  use crate::{OtelSpanHook, sdk_tracer_provider_with_in_memory_exporter};

  #[test]
  fn the_layer_puts_every_statement_of_the_capability_in_a_db_query_span() {
    let exporter = InMemorySpanExporter::default();
    let provider = sdk_tracer_provider_with_in_memory_exporter(&exporter);
    let test = TestSqlClient::new();
    test.script_query("SELECT 1", vec![SqlRow::default()]);
    assert!(matches!(test.connect().block_on(), Exit::Success(())));

    let layer = instrumented_sql_layer::<Never>(provide_sql_client(test), "postgresql");
    let Exit::Success(env) = layer.build().block_on() else {
      panic!("the layer failed")
    };
    let query = env
      .0
      .query("SELECT 1", &[])
      .with_span("handler")
      .with_span_hook(OtelSpanHook::for_provider(&provider, "sql_test").into_hook());
    assert!(matches!(query.block_on(), Exit::Success(rows) if rows.len() == 1));

    let spans = exporter.get_finished_spans().expect("spans");
    let handler = spans.iter().find(|s| s.name == "handler").expect("handler");
    let db = spans
      .iter()
      .find(|s| s.name == "db.query")
      .expect("a db.query span");
    assert_eq!(db.parent_span_id, handler.span_context.span_id());
    assert_eq!(db.span_kind, SpanKind::Client);
    let system = db
      .attributes
      .iter()
      .find(|kv| kv.key.as_str() == "db.system");
    assert_eq!(
      system.map(|kv| kv.value.clone()),
      Some(Value::from("postgresql"))
    );
    let _ = provider.shutdown();
  }
}
