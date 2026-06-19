//! OpenTelemetry integration for [`id_effect`]: tracing spans, W3C propagation, and metric bridges.
//!
//! This crate is the Phase B (`@effect/opentelemetry` parity) integration layer. It stays **opt-in**
//! at the dependency level: the core `id_effect` crate does not pull OpenTelemetry.
//!
//! ## Areas
//!
//! - **Span bridge** — compose [`id_effect::with_span`] with `tracing` spans exported to OTEL.
//! - **Propagation** — W3C Trace Context (`traceparent` / `tracestate`) on header maps.
//! - **Subscriber helpers** — build [`opentelemetry_sdk::trace::SdkTracerProvider`] for tests and apps.
//! - **Logs bridge** — export [`id_effect_logger`] records and `tracing` events to OTEL logs.
//! - **Metric bridges** — dual-write [`id_effect::Metric`] instruments to OTEL.
//! - **Starter** — [`install_otel_starter`] installs traces, metrics, logs, and W3C propagation in one call.
//!
//! ## Testing
//!
//! Prefer [`trace_subscriber_for_provider`] with [`tracing::subscriber::with_default`] in unit tests
//! so the global tracing dispatcher is not permanently claimed. See unit tests in each module.

#![forbid(unsafe_code)]
#![deny(missing_docs)]

mod axum;
mod logs_bridge;
mod metrics_bridge;
mod propagation;
mod span_bridge;
mod starter;
mod subscriber;

pub use axum::trace_request;
pub use logs_bridge::{
  OtelLogBackend, otel_log_backend_for_provider, sdk_logger_provider_with_in_memory_exporter,
  trace_and_log_subscriber_for_providers, tracing_logs_layer_for_provider,
  try_init_global_tracing_with_otel_logs,
};
pub use metrics_bridge::{CounterBridge, DurationHistogramBridge};
pub use propagation::{
  extract_trace_context_from_headers, inject_trace_context_into_headers,
  install_w3c_trace_context_propagator,
};
pub use span_bridge::with_span_otel;
pub use starter::{
  OtelInMemoryExporters, OtelProviders, OtelStarterConfig, OtelStarterGuard, install_otel_starter,
};
pub use subscriber::{
  register_global_tracer_provider, sdk_tracer_provider_with_in_memory_exporter,
  trace_subscriber_for_provider, try_init_global_tracing_with_otel,
};
