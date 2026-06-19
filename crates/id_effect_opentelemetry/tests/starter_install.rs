//! Integration test for global [`install_otel_starter`] (runs in isolation).

use id_effect_opentelemetry::{
  OtelInMemoryExporters, OtelProviders, OtelStarterConfig, install_otel_starter,
};
use opentelemetry::logs::AnyValue;

#[test]
fn install_otel_starter_exports_span_and_log_globally() {
  let exporters = OtelInMemoryExporters::default();
  let providers = OtelProviders::with_in_memory_exporters(&exporters);
  let config = OtelStarterConfig::new("integration_test");
  let guard = install_otel_starter(&providers, &config).expect("install starter");

  tracing::info_span!("integration_span").in_scope(|| {
    tracing::info!(target: "id_effect_opentelemetry", "integration log");
  });

  guard.force_flush();

  let spans = exporters.spans.get_finished_spans().expect("spans");
  assert!(
    spans.iter().any(|s| s.name == "integration_span"),
    "expected integration_span, got {spans:?}"
  );

  let logs = exporters.logs.get_emitted_logs().expect("logs");
  assert!(
    logs.iter().any(|l| {
      matches!(
        &l.record.body(),
        Some(AnyValue::String(body)) if body.as_str() == "integration log"
      )
    }),
    "expected integration log, got {logs:?}"
  );

  guard.shutdown();
}
