//! `id_effect_sql`'s `db.query` spans exported through [`OtelSpanHook`] (the SQL crate has no
//! OpenTelemetry dependency: the bridge maps its core span and `otel.kind`).

use id_effect::{Effect, Exit};
use id_effect_opentelemetry::{OtelSpanHook, sdk_tracer_provider_with_in_memory_exporter};
use id_effect_sql::metrics::InstrumentedSqlClient;
use id_effect_sql::{SqlClient, SqlError, SqlRow, SqlValue, TestSqlClient};
use opentelemetry::Value;
use opentelemetry::trace::{SpanKind, Status};
use opentelemetry_sdk::trace::InMemorySpanExporter;

#[test]
fn db_query_creates_span() {
  let exporter = InMemorySpanExporter::default();
  let provider = sdk_tracer_provider_with_in_memory_exporter(&exporter);
  let hook = OtelSpanHook::for_provider(&provider, "db_test").into_hook();

  let test = TestSqlClient::new();
  let sql = "SELECT id FROM users WHERE email = $1 AND plan = 'pro'";
  test.script_query(sql, vec![SqlRow::default()]);
  let client = InstrumentedSqlClient::new(test.clone()).with_db_system("postgresql");
  assert!(matches!(test.connect().block_on(), Exit::Success(())));
  let params = [SqlValue::Text("ada@example.com".into())];
  let eff: Effect<usize, SqlError> = client
    .query(sql, &params)
    .map(|rows| rows.len())
    .with_span("handler")
    .with_span_hook(hook);
  assert!(matches!(eff.block_on(), Exit::Success(1)));

  let spans = exporter.get_finished_spans().expect("spans");
  let handler = spans.iter().find(|s| s.name == "handler").expect("handler");
  let db = spans
    .iter()
    .find(|s| s.name == "db.query")
    .expect("db.query");
  assert_eq!(db.parent_span_id, handler.span_context.span_id());
  assert_eq!(db.span_context.trace_id(), handler.span_context.trace_id());
  assert_eq!(db.span_kind, SpanKind::Client);
  assert_eq!(db.status, Status::Ok);
  let attr = |key: &str| {
    db.attributes
      .iter()
      .find(|kv| kv.key.as_str() == key)
      .map(|kv| kv.value.clone())
  };
  assert_eq!(attr("db.system"), Some(Value::from("postgresql")));
  assert_eq!(attr("db.operation"), Some(Value::from("SELECT")));
  assert_eq!(
    attr("db.statement"),
    Some(Value::from(
      "SELECT id FROM users WHERE email = $1 AND plan = ?"
    ))
  );
  assert!(attr("otel.kind").is_none());
  assert!(
    db.attributes
      .iter()
      .all(|kv| !kv.value.as_str().contains("ada@example.com")),
    "parameters never reach a span"
  );

  // A failed statement ends its span with an error status.
  test.fail_queries("boom");
  let failing = client
    .query("SELECT 1", &[])
    .with_span_hook(OtelSpanHook::for_provider(&provider, "db_test").into_hook());
  assert!(matches!(failing.block_on(), Exit::Failure(_)));
  let spans = exporter.get_finished_spans().expect("spans");
  let failed = spans
    .iter()
    .filter(|s| s.name == "db.query")
    .find(|s| matches!(s.status, Status::Error { .. }));
  assert!(failed.is_some(), "the failed query span is an error");
  let _ = provider.shutdown();
}
