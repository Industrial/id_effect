//! Minimal example: in-memory OTEL trace and metric export from an effect.
//!
//! Run: `cargo run -p id_effect_opentelemetry --example otel_minimal`

use id_effect::Exit;
use id_effect::{Effect, MetricRegistry, succeed};
use id_effect_opentelemetry::{
  OtelSpanHook, bridge_metrics, sdk_meter_provider_with_in_memory_exporter,
  sdk_tracer_provider_with_in_memory_exporter,
};
use opentelemetry::metrics::MeterProvider as _;
use opentelemetry_sdk::metrics::InMemoryMetricExporter;
use opentelemetry_sdk::trace::InMemorySpanExporter;

fn main() {
  let span_exporter = InMemorySpanExporter::default();
  let tracer_provider = sdk_tracer_provider_with_in_memory_exporter(&span_exporter);
  let metric_exporter = InMemoryMetricExporter::default();
  let meter_provider = sdk_meter_provider_with_in_memory_exporter(&metric_exporter);

  let registry = MetricRegistry::new();
  let _bridge = bridge_metrics(&registry, meter_provider.meter("example"));
  let work = registry.counter("example.work");

  let hook = OtelSpanHook::for_provider(&tracer_provider, "example").into_hook();
  let eff: Effect<u32, ()> = work
    .increment()
    .then(succeed(42))
    .with_span("example.work")
    .with_span_hook(hook);
  assert!(matches!(eff.block_on(), Exit::Success(42)));

  let _ = tracer_provider.force_flush();
  let _ = meter_provider.force_flush();
  let spans = span_exporter.get_finished_spans().expect("read spans");
  let metrics = metric_exporter
    .get_finished_metrics()
    .expect("read metrics");
  println!(
    "exported spans: {}, metric exports: {}",
    spans.len(),
    metrics.len()
  );
  let _ = tracer_provider.shutdown();
  let _ = meter_provider.shutdown();
}
