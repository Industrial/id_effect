//! Test harness helpers — isolate OTEL globals between tests.

use crate::logs_bridge::trace_and_log_subscriber_for_providers;
use crate::propagation::install_w3c_propagators;
use crate::starter::{OtelInMemoryExporters, OtelProviders};
use crate::subscriber::register_global_tracer_provider;
use opentelemetry::global;

/// Run `f` with in-memory OTEL exporters and a scoped tracing subscriber (no global clashes).
pub fn with_otel_test_harness<F, R>(f: F) -> R
where
  F: FnOnce(&OtelProviders, &OtelInMemoryExporters) -> R,
{
  let exporters = OtelInMemoryExporters::default();
  let providers = OtelProviders::with_in_memory_exporters(&exporters);
  let subscriber =
    trace_and_log_subscriber_for_providers(&providers.tracer, &providers.logger, false, None);
  tracing::subscriber::with_default(subscriber, || {
    register_global_tracer_provider(&providers.tracer);
    global::set_meter_provider(providers.meter.clone());
    install_w3c_propagators();
    f(&providers, &exporters)
  })
}
