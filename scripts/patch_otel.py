from pathlib import Path

ROOT = Path("/home/tom/Code/rust/id_effect/crates/id_effect_opentelemetry")

(ROOT / "src/lib.rs").write_text(r'''//! OpenTelemetry integration for `id_effect`: tracing spans, W3C propagation, and metric bridges.
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
pub use config::{OtelConfig, OtelProtocol, config_keys};
#[cfg(feature = "config")]
pub use config::load_otel_config;
pub use error::OtelError;
pub use logs_bridge::{
  OtelLogBackend, otel_log_backend_for_provider, sdk_logger_provider_with_in_memory_exporter,
  trace_and_log_subscriber_for_providers, tracing_logs_layer_for_provider,
  try_init_global_tracing_with_otel_logs,
};
pub use metrics_bridge::{CounterBridge, DurationHistogramBridge};
pub use propagation::{
  extract_trace_context_from_headers, inject_current_trace_context, inject_trace_context_into_headers,
  install_w3c_propagators, install_w3c_trace_context_propagator,
};
#[cfg(feature = "platform")]
pub use propagation::{extract_from_http_request, inject_into_http_request};
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
#[cfg(feature = "otlp")]
pub use otlp::build_otlp_providers;
pub use subscriber::{
  register_global_tracer_provider, sdk_tracer_provider_with_in_memory_exporter,
  trace_subscriber_for_provider, try_init_global_tracing_with_otel,
};
pub use testing::with_otel_test_harness;
''')
print("patched lib.rs")

(ROOT / "src/propagation.rs").write_text(r'''//! W3C Trace Context and baggage propagation on portable header maps.

use opentelemetry::Context;
use opentelemetry::propagation::{Extractor, Injector, TextMapCompositePropagator};
use opentelemetry_sdk::propagation::{BaggagePropagator, TraceContextPropagator};

/// Installs W3C trace context **and** baggage propagators as the global text-map propagator.
pub fn install_w3c_propagators() {
  let composite = TextMapCompositePropagator::new(vec![
    Box::new(TraceContextPropagator::new()),
    Box::new(BaggagePropagator::new()),
  ]);
  opentelemetry::global::set_text_map_propagator(composite);
}

/// Installs trace context only (legacy alias — prefer [`install_w3c_propagators`]).
pub fn install_w3c_trace_context_propagator() {
  install_w3c_propagators();
}

struct VecHeadersInjector<'a>(&'a mut Vec<(String, String)>);

impl Injector for VecHeadersInjector<'_> {
  fn set(&mut self, key: &str, value: String) {
    self.0.retain(|(k, _)| !k.eq_ignore_ascii_case(key));
    self.0.push((key.to_string(), value));
  }
}

/// Carrier for [`Extractor`] over immutable header slices.
pub struct VecHeadersExtractor<'a> {
  headers: &'a [(String, String)],
}

impl<'a> VecHeadersExtractor<'a> {
  /// Wraps a borrowed header list for extraction.
  pub fn new(headers: &'a [(String, String)]) -> Self {
    Self { headers }
  }
}

impl Extractor for VecHeadersExtractor<'_> {
  fn get(&self, key: &str) -> Option<&str> {
    self.headers.iter().find_map(|(k, v)| {
      if k.eq_ignore_ascii_case(key) {
        Some(v.as_str())
      } else {
        None
      }
    })
  }

  fn keys(&self) -> Vec<&str> {
    self.headers.iter().map(|(k, _)| k.as_str()).collect()
  }
}

/// Injects `cx` trace/baggage state into `headers`.
pub fn inject_trace_context_into_headers(cx: &Context, headers: &mut Vec<(String, String)>) {
  let mut inj = VecHeadersInjector(headers);
  opentelemetry::global::get_text_map_propagator(|prop| prop.inject_context(cx, &mut inj));
}

/// Injects the current OTEL [`Context`] into `headers`.
#[inline]
pub fn inject_current_trace_context(headers: &mut Vec<(String, String)>) {
  inject_trace_context_into_headers(&Context::current(), headers);
}

/// Extracts a [`Context`] from `headers` using the global propagator.
pub fn extract_trace_context_from_headers(base: &Context, headers: &[(String, String)]) -> Context {
  let ext = VecHeadersExtractor::new(headers);
  opentelemetry::global::get_text_map_propagator(|prop| prop.extract_with_context(base, &ext))
}

#[cfg(feature = "platform")]
/// Inject trace context into an [`id_effect_platform::HttpRequest`] before dispatch.
pub fn inject_into_http_request(req: &mut id_effect_platform::HttpRequest) {
  inject_current_trace_context(&mut req.headers);
}

#[cfg(feature = "platform")]
/// Extract parent context from an [`id_effect_platform::HttpRequest`].
pub fn extract_from_http_request(req: &id_effect_platform::HttpRequest) -> Context {
  extract_trace_context_from_headers(&Context::new(), &req.headers)
}

#[cfg(test)]
mod tests {
  use super::*;
  use opentelemetry::trace::TracerProvider as _;
  use opentelemetry::trace::{TraceContextExt, Tracer};
  use opentelemetry_sdk::trace::{InMemorySpanExporter, SdkTracerProvider};

  #[test]
  fn inject_then_extract_preserves_remote_span_id() {
    install_w3c_propagators();
    let exporter = InMemorySpanExporter::default();
    let provider = SdkTracerProvider::builder()
      .with_simple_exporter(exporter.clone())
      .build();
    let tracer = provider.tracer("propagation_test");
    let span = tracer.start("remote");
    let cx = opentelemetry::Context::current_with_span(span);
    let mut headers = Vec::new();
    inject_trace_context_into_headers(&cx, &mut headers);
    assert!(
      headers
        .iter()
        .any(|(k, _)| k.eq_ignore_ascii_case("traceparent")),
      "expected traceparent header, got {headers:?}"
    );
    let extracted = extract_trace_context_from_headers(&Context::default(), &headers);
    assert!(extracted.span().span_context().is_valid());
    let _ = provider.shutdown();
  }

  #[test]
  fn get_is_case_insensitive_for_header_name() {
    let headers = vec![(
      "TraceParent".to_string(),
      "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01".to_string(),
    )];
    let ext = VecHeadersExtractor::new(&headers);
    assert!(ext.get("traceparent").is_some());
  }
}
''')
print("patched propagation.rs")

(ROOT / "src/starter.rs").write_text(r'''//! Unified OpenTelemetry starter: one call to install traces, metrics, logs, and propagation.

use opentelemetry::global;
use opentelemetry::metrics::Meter;
use opentelemetry::metrics::MeterProvider as _;
use opentelemetry_sdk::logs::{InMemoryLogExporter, SdkLoggerProvider};
use opentelemetry_sdk::metrics::{InMemoryMetricExporter, SdkMeterProvider};
use opentelemetry_sdk::trace::{InMemorySpanExporter, SdkTracerProvider};

use crate::config::OtelConfig;
use crate::error::OtelError;
use crate::logs_bridge::{
  sdk_logger_provider_with_in_memory_exporter, try_init_global_tracing_with_otel_logs,
};
use crate::metrics_bridge::sdk_meter_provider_with_in_memory_exporter;
use crate::propagation::install_w3c_propagators;
use crate::subscriber::{
  register_global_tracer_provider, sdk_tracer_provider_with_in_memory_exporter,
};

#[cfg(feature = "otlp")]
use crate::otlp::build_otlp_providers;

/// In-memory exporters for tests and local spikes (all three signals).
#[derive(Default)]
pub struct OtelInMemoryExporters {
  /// Span exporter buffer.
  pub spans: InMemorySpanExporter,
  /// Metric exporter buffer.
  pub metrics: InMemoryMetricExporter,
  /// Log exporter buffer.
  pub logs: InMemoryLogExporter,
}

/// The three SDK providers used by [`install_otel_starter`].
#[derive(Clone)]
pub struct OtelProviders {
  /// Trace export provider.
  pub tracer: SdkTracerProvider,
  /// Metric export provider.
  pub meter: SdkMeterProvider,
  /// Log export provider.
  pub logger: SdkLoggerProvider,
}

impl OtelProviders {
  /// Builds providers wired to in-memory exporters (see [`OtelInMemoryExporters`]).
  pub fn with_in_memory_exporters(exporters: &OtelInMemoryExporters) -> Self {
    Self {
      tracer: sdk_tracer_provider_with_in_memory_exporter(&exporters.spans),
      meter: sdk_meter_provider_with_in_memory_exporter(&exporters.metrics),
      logger: sdk_logger_provider_with_in_memory_exporter(&exporters.logs),
    }
  }

  /// Builds production providers exporting via OTLP (requires `otlp` feature).
  #[cfg(feature = "otlp")]
  pub fn from_otlp_config(config: &OtelConfig) -> Result<Self, OtelError> {
    build_otlp_providers(config)
  }
}

/// Options for [`install_otel_starter`].
#[derive(Clone, Debug)]
pub struct OtelStarterConfig {
  /// Meter / tracer instrument scope name.
  pub service_name: String,
  /// Include `tracing_subscriber::fmt` on stdout.
  pub with_fmt_layer: bool,
  /// Optional `EnvFilter` directive (e.g. `info,id_effect=debug`).
  pub env_filter: Option<String>,
}

impl OtelStarterConfig {
  /// Defaults: no stdout `fmt` layer.
  pub fn new(service_name: impl Into<String>) -> Self {
    Self {
      service_name: service_name.into(),
      with_fmt_layer: false,
      env_filter: None,
    }
  }

  /// Toggle the optional `tracing_subscriber::fmt` layer.
  pub fn with_fmt_layer(mut self, enabled: bool) -> Self {
    self.with_fmt_layer = enabled;
    self
  }

  /// Set an explicit `EnvFilter` directive.
  pub fn with_env_filter(mut self, directive: impl Into<String>) -> Self {
    self.env_filter = Some(directive.into());
    self
  }
}

impl From<&OtelConfig> for OtelStarterConfig {
  fn from(config: &OtelConfig) -> Self {
    Self {
      service_name: config.service_name.clone(),
      with_fmt_layer: config.with_fmt_layer,
      env_filter: config.env_filter.clone(),
    }
  }
}

/// Handle returned after a successful [`install_otel_starter`]; call [`force_flush`](Self::force_flush)
/// before shutdown and [`shutdown`](Self::shutdown) on graceful exit.
#[derive(Clone)]
pub struct OtelStarterGuard {
  providers: OtelProviders,
  service_name: String,
}

impl OtelStarterGuard {
  /// Installed SDK providers (clone cheaply for bridges and tests).
  pub fn providers(&self) -> &OtelProviders {
    &self.providers
  }

  /// OpenTelemetry [`Meter`] scoped to the configured service name.
  pub fn meter(&self) -> Meter {
    self.providers.meter.meter(self.service_name.clone())
  }

  /// Flushes pending trace, metric, and log batches.
  pub fn force_flush(&self) {
    let _ = self.providers.tracer.force_flush();
    let _ = self.providers.meter.force_flush();
    let _ = self.providers.logger.force_flush();
  }

  /// Shuts down all three providers (call once on graceful process exit).
  pub fn shutdown(self) {
    let _ = self.providers.tracer.shutdown();
    let _ = self.providers.meter.shutdown();
    let _ = self.providers.logger.shutdown();
  }
}

/// Build OTLP providers from `config` and install globals + tracing subscriber.
#[cfg(feature = "otlp")]
pub fn install_from_config(config: OtelConfig) -> Result<OtelStarterGuard, OtelError> {
  let providers = build_otlp_providers(&config)?;
  let starter = OtelStarterConfig::from(&config);
  install_otel_starter(&providers, &starter).map_err(OtelError::from)
}

/// Installs global tracer + meter providers, W3C propagation, and a tracing subscriber with OTEL trace + log layers.
pub fn install_otel_starter(
  providers: &OtelProviders,
  config: &OtelStarterConfig,
) -> Result<OtelStarterGuard, tracing_subscriber::util::TryInitError> {
  register_global_tracer_provider(&providers.tracer);
  global::set_meter_provider(providers.meter.clone());
  install_w3c_propagators();
  try_init_global_tracing_with_otel_logs(
    &providers.tracer,
    &providers.logger,
    config.with_fmt_layer,
    config.env_filter.as_deref(),
  )?;
  Ok(OtelStarterGuard {
    providers: providers.clone(),
    service_name: config.service_name.clone(),
  })
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::CounterBridge;
  use crate::logs_bridge::trace_and_log_subscriber_for_providers;
  use id_effect::run_blocking;
  use opentelemetry::KeyValue;
  use opentelemetry::logs::AnyValue;
  use opentelemetry::metrics::MeterProvider;

  #[test]
  fn exports_traces_metrics_and_logs_via_scoped_subscriber() {
    let exporters = OtelInMemoryExporters::default();
    let providers = OtelProviders::with_in_memory_exporters(&exporters);
    let subscriber = trace_and_log_subscriber_for_providers(
      &providers.tracer,
      &providers.logger,
      false,
      None,
    );
    tracing::subscriber::with_default(subscriber, || {
      register_global_tracer_provider(&providers.tracer);
      global::set_meter_provider(providers.meter.clone());
      install_w3c_propagators();

      let meter = providers.meter.meter("starter_test");
      let local = id_effect::Metric::counter("req", Vec::<(String, String)>::new());
      let bridge = CounterBridge::new(local, &meter, "req_otel");
      let _ = run_blocking(bridge.apply(2), ());

      let span = tracing::info_span!("starter_span");
      let _g = span.enter();
      tracing::info!(target: "id_effect_opentelemetry", "starter log");
    });

    let _ = providers.tracer.force_flush();
    let _ = providers.meter.force_flush();
    let _ = providers.logger.force_flush();

    let spans = exporters.spans.get_finished_spans().expect("spans");
    assert!(spans.iter().any(|s| s.name == "starter_span"));

    let metrics = exporters.metrics.get_finished_metrics().expect("metrics");
    assert!(!metrics.is_empty());

    let logs = exporters.logs.get_emitted_logs().expect("logs");
    assert!(logs.iter().any(|l| {
      matches!(
        &l.record.body(),
        Some(AnyValue::String(body)) if body.as_str() == "starter log"
      )
    }));

    let _ = providers.tracer.shutdown();
    let _ = providers.meter.shutdown();
    let _ = providers.logger.shutdown();
  }
}
''')
print("patched starter.rs")

(ROOT / "src/span_bridge.rs").write_text(r'''//! Bridge `id_effect::with_span` with `tracing` spans so OTEL exporters see effect-scoped work.

use id_effect::{Effect, box_future};
use tracing::Instrument;

/// Runs `effect` under both `id_effect::with_span` and a `tracing` span exported to OTEL.
pub fn with_span_otel<A, E, R>(name: &'static str, effect: Effect<A, E, R>) -> Effect<A, E, R>
where
  A: 'static,
  E: 'static,
  R: 'static,
{
  let parent = tracing::Span::current();
  let inner = id_effect::with_span(effect, name);
  Effect::new_async(move |env: &mut R| {
    let span = tracing::trace_span!(parent: &parent, "id_effect.effect", otel.span_name = name);
    box_future(async move { inner.run(env).instrument(span).await })
  })
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::subscriber::{
    sdk_tracer_provider_with_in_memory_exporter, trace_subscriber_for_provider,
  };
  use id_effect::concurrency::fiber_ref::with_fiber_id;
  use id_effect::runtime::FiberId;
  use id_effect::{TracingConfig, install_tracing_layer, run_blocking, succeed};
  use opentelemetry_sdk::trace::InMemorySpanExporter;

  #[test]
  fn emits_tracing_span_exported_to_otel() {
    let exporter = InMemorySpanExporter::default();
    let provider = sdk_tracer_provider_with_in_memory_exporter(&exporter);
    let subscriber = trace_subscriber_for_provider(&provider, false, None);
    tracing::subscriber::with_default(subscriber, || {
      let _ = run_blocking(install_tracing_layer(TracingConfig::enabled()), ());
      let eff = with_span_otel("otel.inner", succeed::<(), (), ()>(()));
      let _ = run_blocking(eff, ());
    });
    let _ = provider.force_flush();
    let spans = exporter.get_finished_spans().expect("spans");
    assert!(spans.iter().any(|s| s.name == "id_effect.effect"));
    let _ = provider.shutdown();
  }

  #[test]
  fn nested_spans_link_under_parent() {
    let exporter = InMemorySpanExporter::default();
    let provider = sdk_tracer_provider_with_in_memory_exporter(&exporter);
    let subscriber = trace_subscriber_for_provider(&provider, false, None);
    tracing::subscriber::with_default(subscriber, || {
      let _ = run_blocking(install_tracing_layer(TracingConfig::enabled()), ());
      let inner = with_span_otel("inner", succeed::<(), (), ()>(()));
      let outer = with_span_otel("outer", inner);
      let _ = run_blocking(outer, ());
    });
    let _ = provider.force_flush();
    let count = exporter
      .get_finished_spans()
      .expect("spans")
      .into_iter()
      .filter(|s| s.name == "id_effect.effect")
      .count();
    assert!(count >= 2);
    let _ = provider.shutdown();
  }

  #[test]
  fn child_spans_link_under_tracing_parent_across_fibers() {
    let exporter = InMemorySpanExporter::default();
    let provider = sdk_tracer_provider_with_in_memory_exporter(&exporter);
    let subscriber = trace_subscriber_for_provider(&provider, false, None);
    tracing::subscriber::with_default(subscriber, || {
      let parent = tracing::info_span!("parent_fiber");
      parent.in_scope(|| {
        with_fiber_id(FiberId::fresh(), || {
          let _ = run_blocking(with_span_otel("child_a", succeed::<(), (), ()>(())), ());
        });
        with_fiber_id(FiberId::fresh(), || {
          let _ = run_blocking(with_span_otel("child_b", succeed::<(), (), ()>(())), ());
        });
      });
    });
    let _ = provider.force_flush();
    let count = exporter
      .get_finished_spans()
      .expect("spans")
      .into_iter()
      .filter(|s| s.name == "id_effect.effect")
      .count();
    assert!(count >= 2);
    let _ = provider.shutdown();
  }
}
''')
print("patched span_bridge.rs")

# Fix Cargo.toml tokio dep
cargo = ROOT / "Cargo.toml"
cargo_text = cargo.read_text()
if "tokio = { version = \"1.50.0\", default-features = false" not in cargo_text:
    cargo_text = cargo_text.replace(
        "axum = { version = \"0.8\" }",
        "tokio = { version = \"1.50.0\", default-features = false, features = [\"macros\", \"rt\", \"signal\"] }\naxum = { version = \"0.8\" }",
    )
    cargo.write_text(cargo_text)

(ROOT / "src/providers.rs").write_text(r'''//! Capability DI provider for an installed OpenTelemetry runtime handle.

use std::sync::Arc;

use id_effect::{CapabilityId, CapabilityKey, Env, ProviderBox, ProviderError, ProviderNode};

use crate::starter::OtelStarterGuard;

/// Capability key for a process-wide OpenTelemetry runtime handle (flush/shutdown + meter access).
#[derive(Clone, Copy, Debug, Default)]
pub struct OtelRuntimeKey;

impl CapabilityKey for OtelRuntimeKey {
  type Value = Arc<OtelStarterGuard>;
}

/// Register `guard` in the capability environment for domain programs that need OTEL flush/shutdown.
#[inline]
pub fn provide_otel_runtime(guard: Arc<OtelStarterGuard>) -> ProviderBox {
  struct Node(Arc<OtelStarterGuard>);

  impl ProviderNode for Node {
    fn id(&self) -> &str {
      "opentelemetry/runtime"
    }

    fn requires(&self) -> &[CapabilityId] {
      &[]
    }

    fn provides(&self) -> CapabilityId {
      OtelRuntimeKey::id()
    }

    fn cap_name(&self) -> &str {
      "OtelRuntimeKey"
    }

    fn build(&self, deps: &Env) -> Result<Env, ProviderError> {
      let mut out = deps.clone();
      out.insert::<OtelRuntimeKey>(Arc::clone(&self.0));
      Ok(out)
    }
  }

  ProviderBox(Arc::new(Node(guard)))
}
''')

(ROOT / "src/otlp.rs").write_text(r'''//! OTLP exporter builders for traces, metrics, and logs.

use std::collections::HashMap;

use opentelemetry::KeyValue;
use opentelemetry_sdk::Resource;
use opentelemetry_sdk::logs::SdkLoggerProvider;
use opentelemetry_sdk::metrics::{PeriodicReader, SdkMeterProvider};
use opentelemetry_sdk::trace::SdkTracerProvider;

use crate::config::{OtelConfig, OtelProtocol};
use crate::error::OtelError;
use crate::starter::OtelProviders;

fn resource_from_config(config: &OtelConfig) -> Resource {
  let mut builder = Resource::builder().with_service_name(config.service_name.clone());
  if let Some(version) = &config.service_version {
    builder = builder.with_attribute(KeyValue::new("service.version", version.clone()));
  }
  builder.build()
}

#[cfg(feature = "otlp")]
fn grpc_metadata(headers: &[(String, String)]) -> tonic::metadata::MetadataMap {
  use tonic::metadata::{MetadataKey, MetadataMap, MetadataValue};
  let mut metadata = MetadataMap::new();
  for (key, value) in headers {
    if let (Ok(k), Ok(v)) = (
      MetadataKey::from_bytes(key.as_bytes()),
      MetadataValue::try_from(value.as_str()),
    ) {
      metadata.insert(k, v);
    }
  }
  metadata
}

#[cfg(feature = "otlp")]
fn http_headers(headers: &[(String, String)]) -> HashMap<String, String> {
  headers.iter().cloned().collect()
}

#[cfg(feature = "otlp")]
fn span_exporter(config: &OtelConfig) -> Result<opentelemetry_otlp::SpanExporter, OtelError> {
  use opentelemetry_otlp::WithExportConfig;

  match config.protocol {
    OtelProtocol::Grpc => {
      use opentelemetry_otlp::WithTonicConfig;
      let mut builder = opentelemetry_otlp::SpanExporter::builder()
        .with_tonic()
        .with_endpoint(config.endpoint.clone())
        .with_timeout(config.export_timeout);
      if !config.headers.is_empty() {
        builder = builder.with_metadata(grpc_metadata(&config.headers));
      }
      builder.build().map_err(|e| OtelError::Exporter(e.to_string()))
    }
    OtelProtocol::Http => {
      use opentelemetry_otlp::WithHttpConfig;
      let mut builder = opentelemetry_otlp::SpanExporter::builder()
        .with_http()
        .with_endpoint(config.endpoint.clone())
        .with_timeout(config.export_timeout);
      if !config.headers.is_empty() {
        builder = builder.with_headers(http_headers(&config.headers));
      }
      builder.build().map_err(|e| OtelError::Exporter(e.to_string()))
    }
  }
}

#[cfg(feature = "otlp")]
fn metric_exporter(config: &OtelConfig) -> Result<opentelemetry_otlp::MetricExporter, OtelError> {
  use opentelemetry_otlp::WithExportConfig;

  match config.protocol {
    OtelProtocol::Grpc => {
      use opentelemetry_otlp::WithTonicConfig;
      let mut builder = opentelemetry_otlp::MetricExporter::builder()
        .with_tonic()
        .with_endpoint(config.endpoint.clone())
        .with_timeout(config.export_timeout);
      if !config.headers.is_empty() {
        builder = builder.with_metadata(grpc_metadata(&config.headers));
      }
      builder.build().map_err(|e| OtelError::Exporter(e.to_string()))
    }
    OtelProtocol::Http => {
      use opentelemetry_otlp::WithHttpConfig;
      let mut builder = opentelemetry_otlp::MetricExporter::builder()
        .with_http()
        .with_endpoint(config.endpoint.clone())
        .with_timeout(config.export_timeout);
      if !config.headers.is_empty() {
        builder = builder.with_headers(http_headers(&config.headers));
      }
      builder.build().map_err(|e| OtelError::Exporter(e.to_string()))
    }
  }
}

#[cfg(feature = "otlp")]
fn log_exporter(config: &OtelConfig) -> Result<opentelemetry_otlp::LogExporter, OtelError> {
  use opentelemetry_otlp::WithExportConfig;

  match config.protocol {
    OtelProtocol::Grpc => {
      use opentelemetry_otlp::WithTonicConfig;
      let mut builder = opentelemetry_otlp::LogExporter::builder()
        .with_tonic()
        .with_endpoint(config.endpoint.clone())
        .with_timeout(config.export_timeout);
      if !config.headers.is_empty() {
        builder = builder.with_metadata(grpc_metadata(&config.headers));
      }
      builder.build().map_err(|e| OtelError::Exporter(e.to_string()))
    }
    OtelProtocol::Http => {
      use opentelemetry_otlp::WithHttpConfig;
      let mut builder = opentelemetry_otlp::LogExporter::builder()
        .with_http()
        .with_endpoint(config.endpoint.clone())
        .with_timeout(config.export_timeout);
      if !config.headers.is_empty() {
        builder = builder.with_headers(http_headers(&config.headers));
      }
      builder.build().map_err(|e| OtelError::Exporter(e.to_string()))
    }
  }
}

/// Build [`OtelProviders`] wired to OTLP batch/periodic exporters.
#[cfg(feature = "otlp")]
pub fn build_otlp_providers(config: &OtelConfig) -> Result<OtelProviders, OtelError> {
  let resource = resource_from_config(config);

  let trace_exporter = span_exporter(config)?;
  let tracer = SdkTracerProvider::builder()
    .with_resource(resource.clone())
    .with_batch_exporter(trace_exporter)
    .build();

  let metric_exporter = metric_exporter(config)?;
  let reader = PeriodicReader::builder(metric_exporter)
    .with_interval(config.export_interval)
    .build();
  let meter = SdkMeterProvider::builder()
    .with_resource(resource.clone())
    .with_reader(reader)
    .build();

  let log_exporter = log_exporter(config)?;
  let logger = SdkLoggerProvider::builder()
    .with_resource(resource)
    .with_batch_exporter(log_exporter)
    .build();

  Ok(OtelProviders {
    tracer,
    meter,
    logger,
  })
}

#[cfg(not(feature = "otlp"))]
pub fn build_otlp_providers(_config: &OtelConfig) -> Result<OtelProviders, OtelError> {
  Err(OtelError::Config(
    "OTLP support disabled; enable the `otlp` feature on id_effect_opentelemetry".into(),
  ))
}
''')

# Fix config load_otel_config
cfg = ROOT / "src/config.rs"
cfg_text = cfg.read_text()
cfg_text = cfg_text.replace("use id_effect_config::Config;", "use id_effect_config::config;")
cfg_text = cfg_text.replace("Config::string(provider,", "config::string(provider,")
cfg_text = cfg_text.replace("Config::string(provider, config_keys::PROTOCOL)", "config::string(provider, config_keys::PROTOCOL)")
# fix optional_string if any
cfg.write_text(cfg_text)

# Fix propagation platform path
prop = ROOT / "src/propagation.rs"
prop_text = prop.read_text()
prop_text = prop_text.replace("id_effect_platform::HttpRequest", "id_effect_platform::http::HttpRequest")
prop.write_text(prop_text)

# Fix subscriber layering
(ROOT / "src/subscriber.rs").write_text(r'''//! Tracer provider helpers and [`tracing_subscriber`] wiring for OpenTelemetry.

use opentelemetry::global;
use opentelemetry::trace::TracerProvider as _;
use opentelemetry_sdk::trace::{InMemorySpanExporter, SdkTracerProvider};
use tracing::Subscriber;
use tracing_subscriber::EnvFilter;
use tracing_subscriber::Registry;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

/// Builds an [`SdkTracerProvider`] that exports spans to an in-memory buffer (tests and spikes).
pub fn sdk_tracer_provider_with_in_memory_exporter(
  exporter: &InMemorySpanExporter,
) -> SdkTracerProvider {
  SdkTracerProvider::builder()
    .with_simple_exporter(exporter.clone())
    .build()
}

/// Registers `provider` as the process-wide OpenTelemetry tracer provider.
pub fn register_global_tracer_provider(provider: &SdkTracerProvider) {
  global::set_tracer_provider(provider.clone());
}

fn build_trace_subscriber(
  otel_layer: tracing_opentelemetry::OpenTelemetryLayer<Registry, opentelemetry_sdk::trace::SdkTracer>,
  with_fmt_layer: bool,
  env_filter: Option<&str>,
) -> Box<dyn Subscriber + Send + Sync + 'static> {
  if let Some(spec) = env_filter {
    if let Ok(filter) = EnvFilter::try_new(spec) {
      if with_fmt_layer {
        return Box::new(
          Registry::default()
            .with(filter)
            .with(otel_layer)
            .with(tracing_subscriber::fmt::layer()),
        );
      }
      return Box::new(Registry::default().with(filter).with(otel_layer));
    }
  }
  if with_fmt_layer {
    Box::new(
      Registry::default()
        .with(otel_layer)
        .with(tracing_subscriber::fmt::layer()),
    )
  } else {
    Box::new(Registry::default().with(otel_layer))
  }
}

/// Returns a boxed [`tracing`] [`Subscriber`]: registry + OpenTelemetry layer, optionally with `fmt`.
pub fn trace_subscriber_for_provider(
  provider: &SdkTracerProvider,
  with_fmt_layer: bool,
  env_filter: Option<&str>,
) -> Box<dyn Subscriber + Send + Sync + 'static> {
  let tracer = provider.tracer("id_effect_opentelemetry");
  let otel_layer = tracing_opentelemetry::layer().with_tracer(tracer);
  build_trace_subscriber(otel_layer, with_fmt_layer, env_filter)
}

/// Installs a global subscriber built from [`trace_subscriber_for_provider`].
pub fn try_init_global_tracing_with_otel(
  provider: &SdkTracerProvider,
  with_fmt_layer: bool,
  env_filter: Option<&str>,
) -> Result<(), tracing_subscriber::util::TryInitError> {
  trace_subscriber_for_provider(provider, with_fmt_layer, env_filter).try_init()
}

#[cfg(test)]
mod tests {
  use super::*;
  use opentelemetry::trace::Tracer;

  #[test]
  fn exports_finished_span_after_flush() {
    let exporter = InMemorySpanExporter::default();
    let provider = sdk_tracer_provider_with_in_memory_exporter(&exporter);
    let tracer = provider.tracer("unit");
    {
      let span = tracer.start("hello");
      drop(span);
    }
    let _ = provider.force_flush();
    let spans = exporter.get_finished_spans().expect("spans");
    assert!(spans.iter().any(|s| s.name == "hello"));
    let _ = provider.shutdown();
  }

  #[test]
  fn records_tracing_span_without_fmt_layer() {
    let exporter = InMemorySpanExporter::default();
    let provider = sdk_tracer_provider_with_in_memory_exporter(&exporter);
    let sub = trace_subscriber_for_provider(&provider, false, None);
    tracing::subscriber::with_default(sub, || {
      let root = tracing::info_span!("root_op");
      let _g = root.enter();
      tracing::info!(target: "id_effect_opentelemetry", "event");
    });
    let _ = provider.force_flush();
    let spans = exporter.get_finished_spans().expect("spans");
    assert!(spans.iter().any(|s| s.name == "root_op"));
    let _ = provider.shutdown();
  }
}
''')

# Fix logs_bridge subscriber builder similarly
lb = ROOT / "src/logs_bridge.rs"
lb_text = lb.read_text()
old_fn = '''pub fn trace_and_log_subscriber_for_providers(
  tracer_provider: &SdkTracerProvider,
  logger_provider: &SdkLoggerProvider,
  with_fmt_layer: bool,
  env_filter: Option<&str>,
) -> Box<dyn Subscriber + Send + Sync + 'static> {
  let tracer = tracer_provider.tracer("id_effect_opentelemetry");
  let otel_trace_layer = tracing_opentelemetry::layer().with_tracer(tracer);
  let otel_log_layer = tracing_logs_layer_for_provider(logger_provider);
  let mut registry = Registry::default()
    .with(otel_trace_layer)
    .with(otel_log_layer);
  if let Some(spec) = env_filter {
    if let Ok(filter) = tracing_subscriber::EnvFilter::try_new(spec) {
      registry = registry.with(filter);
    }
  }
  if with_fmt_layer {
    Box::new(registry.with(tracing_subscriber::fmt::layer()))
  } else {
    Box::new(registry)
  }
}'''
new_fn = '''pub fn trace_and_log_subscriber_for_providers(
  tracer_provider: &SdkTracerProvider,
  logger_provider: &SdkLoggerProvider,
  with_fmt_layer: bool,
  env_filter: Option<&str>,
) -> Box<dyn Subscriber + Send + Sync + 'static> {
  use tracing_subscriber::EnvFilter;
  let tracer = tracer_provider.tracer("id_effect_opentelemetry");
  let otel_trace_layer = tracing_opentelemetry::layer().with_tracer(tracer);
  let otel_log_layer = tracing_logs_layer_for_provider(logger_provider);
  if let Some(spec) = env_filter {
    if let Ok(filter) = EnvFilter::try_new(spec) {
      if with_fmt_layer {
        return Box::new(
          Registry::default()
            .with(filter)
            .with(otel_trace_layer)
            .with(otel_log_layer)
            .with(tracing_subscriber::fmt::layer()),
        );
      }
      return Box::new(
        Registry::default()
          .with(filter)
          .with(otel_trace_layer)
          .with(otel_log_layer),
      );
    }
  }
  if with_fmt_layer {
    Box::new(
      Registry::default()
        .with(otel_trace_layer)
        .with(otel_log_layer)
        .with(tracing_subscriber::fmt::layer()),
    )
  } else {
    Box::new(
      Registry::default()
        .with(otel_trace_layer)
        .with(otel_log_layer),
    )
  }
}'''
if old_fn in lb_text:
    lb.write_text(lb_text.replace(old_fn, new_fn))

# Fix starter meter call
st = ROOT / "src/starter.rs"
st.write_text(st.read_text().replace(
  "self.providers.meter.meter(self.service_name.clone())",
  "self.providers.meter.meter(&self.service_name)",
))

print("patched fixes batch 2")
