//! Integration test for global [`install_otel_starter`] (runs in isolation).

use std::sync::Arc;

use id_effect::Exit;
use id_effect::{Boundaries, Concurrency, Counter, Effect, Histogram, fork_all, need};
use id_effect_opentelemetry::{
  OtelInMemoryExporters, OtelProviders, OtelRuntime, OtelRuntimeEnv, OtelStarterConfig,
  install_otel_starter, provide_otel_runtime,
};
use id_effect_tokio::TokioRuntime;
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

  // Program on tokio worker fibers: spans through the guard's span hook (a FiberRef),
  // metrics through the global registry bridge.
  let rt = TokioRuntime::new_multi_thread().expect("tokio");
  let requests = Counter::new("starter_install.requests");
  let latency = Histogram::new("starter_install.latency_ms", Boundaries::new([1.0, 10.0]));
  let children: Vec<Effect<(), ()>> = (0..3)
    .map(|i| {
      requests
        .increment()
        .then(latency.observe(f64::from(i) * 4.0))
        .with_span("starter_install.request")
    })
    .collect();
  let program: Effect<usize, ()> = fork_all(children, Concurrency::Unbounded)
    .map(|v| v.len())
    .with_span("starter_install.batch")
    .with_span_hook(guard.span_hook());
  assert!(matches!(rt.block_on(rt.run(program)), Exit::Success(3)));

  // The guard as a capability.
  let runtime: Effect<String, ()> = need::<OtelRuntime>()
    .map::<_, (), OtelRuntimeEnv, _>(|otel| otel.service_name().to_owned())
    .provide_layer(provide_otel_runtime(Arc::new(guard.clone())));
  assert!(
    matches!(rt.block_on(rt.run(runtime)), Exit::Success(name) if name == "integration_test")
  );

  guard.force_flush();

  let spans = exporters.spans.get_finished_spans().expect("spans");
  let batch = spans
    .iter()
    .find(|s| s.name == "starter_install.batch")
    .expect("batch span");
  let requests_spans: Vec<_> = spans
    .iter()
    .filter(|s| s.name == "starter_install.request")
    .collect();
  assert_eq!(requests_spans.len(), 3);
  assert!(
    requests_spans
      .iter()
      .all(|s| s.parent_span_id == batch.span_context.span_id())
  );

  let metrics = exporters.metrics.get_finished_metrics().expect("metrics");
  let names: Vec<&str> = metrics
    .iter()
    .flat_map(|rm| rm.scope_metrics())
    .flat_map(|sm| sm.metrics())
    .map(|m| m.name())
    .collect();
  assert!(names.contains(&"starter_install.requests"), "{names:?}");
  assert!(names.contains(&"starter_install.latency_ms"), "{names:?}");

  guard.shutdown();
}
