//! OpenTelemetry integration for `id_effect`: tracing spans, W3C propagation, and metric bridges.
//!
//! Phase B (`@effect/opentelemetry` parity) integration layer — opt-in at the dependency boundary.

#![forbid(unsafe_code)]
#![deny(missing_docs)]

mod axum;
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
mod starter;
mod subscriber;
mod testing;

pub use axum::trace_request;
#[cfg(feature = "config")]
pub use config::load_otel_config;
pub use config::{OtelConfig, OtelProtocol, config_keys};
pub use error::OtelError;
pub use logs_bridge::{
  OtelLogBackend, otel_log_backend_for_provider, sdk_logger_provider_with_in_memory_exporter,
  trace_and_log_subscriber_for_providers, tracing_logs_layer_for_provider,
  try_init_global_tracing_with_otel_logs,
};
pub use metrics_bridge::{CounterBridge, DurationHistogramBridge};
#[cfg(feature = "otlp")]
pub use otlp::build_otlp_providers;
#[cfg(feature = "platform")]
pub use propagation::{extract_from_http_request, inject_into_http_request};
pub use propagation::{
  extract_trace_context_from_headers, inject_current_trace_context,
  inject_trace_context_into_headers, install_w3c_propagators, install_w3c_trace_context_propagator,
};
pub use providers::{OtelRuntimeKey, provide_otel_runtime};
pub use shutdown::{
  graceful_otel_shutdown, run_until_shutdown, shutdown_otel_on_signal,
  shutdown_otel_on_signal_with_timeout,
};
pub use span_bridge::with_span_otel;
pub use starter::{
  OtelInMemoryExporters, OtelProviders, OtelStarterConfig, OtelStarterGuard, install_from_config,
  install_otel_starter,
};
pub use subscriber::{
  register_global_tracer_provider, sdk_tracer_provider_with_in_memory_exporter,
  trace_subscriber_for_provider, try_init_global_tracing_with_otel,
};
pub use testing::with_otel_test_harness;
