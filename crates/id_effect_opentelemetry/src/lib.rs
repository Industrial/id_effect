//! OpenTelemetry integration for `id_effect`: tracing spans, W3C propagation, and metric bridges.
//!
//! Opt-in at the dependency boundary: core `id_effect` has no OpenTelemetry dependency.
//!
//! Built on the 0.7 kernel:
//!
//! - **Metrics** go through the [`MetricRegistry`](id_effect::MetricRegistry):
//!   [`bridge_metrics`] / [`bridge_global_metrics`] export every series (counters and gauges
//!   pulled from registry snapshots, histograms, summaries and frequencies pushed by a registry
//!   listener).
//! - **Spans** go through the span-hook `FiberRef`: install [`OtelSpanHook`] around the root
//!   effect with `Effect::with_span_hook`, and every `Effect::with_span` becomes an
//!   OpenTelemetry span, parented across forked fibers on any executor thread.
//!   `TracedHttpClient` (feature `platform`) opens a client span per outbound request and sends
//!   its W3C `traceparent`; `id_effect_sql`'s `InstrumentedSqlClient` opens a `db.query` span
//!   per statement. Where OpenTelemetry is set up, `TracedHttpClient::layer` and
//!   `instrumented_sql_layer` (feature `sql`) install them around the `HttpClientService` /
//!   `SqlClientService` layers, so every call through the capability is traced.
//!   [`OtelConfig::sampler`] picks the head sampler (`OTEL_TRACES_SAMPLER`).
//! - **Logs**: [`OtelLogBackend`] is an `id_effect_logger` backend; `tracing` events reach OTEL
//!   logs through the subscriber layers.

#![forbid(unsafe_code)]
#![deny(missing_docs)]

mod axum;
#[cfg(feature = "platform")]
mod client;
mod config;
mod error;
mod logs_bridge;
mod metrics_bridge;
#[cfg(feature = "otlp")]
mod otlp;
mod propagation;
mod providers;
mod shutdown;
mod span_bridge;
#[cfg(feature = "sql")]
mod sql;
mod starter;
mod subscriber;
mod testing;

pub use axum::trace_request;
#[cfg(feature = "platform")]
pub use client::TracedHttpClient;
#[cfg(feature = "config")]
pub use config::load_otel_config;
pub use config::{OtelConfig, OtelProtocol, OtelSampler, config_keys};
pub use error::OtelError;
pub use logs_bridge::{
  OtelLogBackend, otel_log_backend_for_provider, sdk_logger_provider_with_in_memory_exporter,
  trace_and_log_subscriber_for_providers, tracing_logs_layer_for_provider,
  try_init_global_tracing_with_otel_logs,
};
pub use metrics_bridge::{
  MetricsBridge, bridge_global_metrics, bridge_metrics, sdk_meter_provider_with_in_memory_exporter,
};
#[cfg(feature = "otlp")]
pub use otlp::build_otlp_providers;
#[cfg(feature = "platform")]
pub use propagation::{extract_from_http_request, inject_into_http_request};
pub use propagation::{
  extract_trace_context_from_headers, inject_current_trace_context,
  inject_trace_context_into_headers, install_w3c_propagators, install_w3c_trace_context_propagator,
};
pub use providers::{OtelRuntime, OtelRuntimeEnv, provide_otel_runtime};
pub use shutdown::{
  graceful_otel_shutdown, run_until_shutdown, shutdown_otel_on_signal,
  shutdown_otel_on_signal_with_timeout,
};
pub use span_bridge::{OtelSpanHook, current_otel_context, span_otel_context};
#[cfg(feature = "sql")]
pub use sql::{instrumented_sql_client, instrumented_sql_layer};
pub use starter::{
  OtelInMemoryExporters, OtelProviders, OtelStarterConfig, OtelStarterGuard, install_from_config,
  install_otel_starter,
};
pub use subscriber::{
  EnvFilterHandle, register_global_tracer_provider, reloadable_env_filter,
  sdk_tracer_provider_with_in_memory_exporter, trace_subscriber_for_provider,
  try_init_global_tracing_with_otel,
};
pub use testing::with_otel_test_harness;
