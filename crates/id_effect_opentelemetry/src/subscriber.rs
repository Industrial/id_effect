//! Tracer provider helpers and [`tracing_subscriber`] wiring for OpenTelemetry.

use opentelemetry::global;
use opentelemetry::trace::TracerProvider as _;
use opentelemetry_sdk::trace::{InMemorySpanExporter, SdkTracerProvider};
use tracing::Subscriber;
use tracing_subscriber::EnvFilter;
use tracing_subscriber::Registry;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

/// Builds an [`SdkTracerProvider`] with a simple in-memory span exporter.
pub fn sdk_tracer_provider_with_in_memory_exporter(
  exporter: &InMemorySpanExporter,
) -> SdkTracerProvider {
  SdkTracerProvider::builder()
    .with_simple_exporter(exporter.clone())
    .build()
}

/// Registers the given tracer provider as the global OTEL tracer provider.
pub fn register_global_tracer_provider(provider: &SdkTracerProvider) {
  global::set_tracer_provider(provider.clone());
}

/// Builds a boxed `tracing_subscriber` stack with an OTEL trace layer.
pub fn trace_subscriber_for_provider(
  provider: &SdkTracerProvider,
  with_fmt_layer: bool,
  env_filter: Option<&str>,
) -> Box<dyn Subscriber + Send + Sync + 'static> {
  let tracer = provider.tracer("id_effect_opentelemetry");
  let filter = env_filter.and_then(|s| EnvFilter::try_new(s).ok());
  match (filter, with_fmt_layer) {
    (Some(filter), true) => Box::new(
      Registry::default()
        .with(filter)
        .with(tracing_opentelemetry::layer().with_tracer(tracer.clone()))
        .with(tracing_subscriber::fmt::layer()),
    ),
    (Some(filter), false) => Box::new(
      Registry::default()
        .with(filter)
        .with(tracing_opentelemetry::layer().with_tracer(tracer)),
    ),
    (None, true) => Box::new(
      Registry::default()
        .with(tracing_opentelemetry::layer().with_tracer(tracer.clone()))
        .with(tracing_subscriber::fmt::layer()),
    ),
    (None, false) => {
      Box::new(Registry::default().with(tracing_opentelemetry::layer().with_tracer(tracer)))
    }
  }
}

/// Installs a global subscriber with OTEL trace export (and optional `fmt` / `EnvFilter`).
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
    drop(tracer.start("hello"));
    let _ = provider.force_flush();
    assert!(
      exporter
        .get_finished_spans()
        .expect("spans")
        .iter()
        .any(|s| s.name == "hello")
    );
    let _ = provider.shutdown();
  }

  /// Emit one `info` span through the subscriber stack and report whether OTEL exported it.
  fn exports_tracing_span(with_fmt_layer: bool, env_filter: Option<&str>) -> bool {
    let exporter = InMemorySpanExporter::default();
    let provider = sdk_tracer_provider_with_in_memory_exporter(&exporter);
    let subscriber = trace_subscriber_for_provider(&provider, with_fmt_layer, env_filter);
    tracing::subscriber::with_default(subscriber, || {
      tracing::info_span!("bridged").in_scope(|| tracing::info!("inside"));
    });
    let _ = provider.force_flush();
    let found = exporter
      .get_finished_spans()
      .expect("spans")
      .iter()
      .any(|s| s.name == "bridged");
    let _ = provider.shutdown();
    found
  }

  #[test]
  fn subscriber_without_filter_or_fmt_exports_spans() {
    assert!(exports_tracing_span(false, None));
  }

  #[test]
  fn subscriber_with_fmt_layer_exports_spans() {
    assert!(exports_tracing_span(true, None));
  }

  #[test]
  fn subscriber_with_filter_and_fmt_exports_enabled_spans() {
    assert!(exports_tracing_span(true, Some("info")));
  }

  #[test]
  fn subscriber_filter_suppresses_disabled_spans() {
    assert!(!exports_tracing_span(false, Some("off")));
  }

  #[test]
  fn invalid_filter_is_ignored() {
    assert!(exports_tracing_span(false, Some("[[[not a filter")));
  }

  #[test]
  fn global_registration_is_one_shot() {
    let exporter = InMemorySpanExporter::default();
    let provider = sdk_tracer_provider_with_in_memory_exporter(&exporter);
    register_global_tracer_provider(&provider);
    let _ = try_init_global_tracing_with_otel(&provider, false, None);
    assert!(
      try_init_global_tracing_with_otel(&provider, false, None).is_err(),
      "a global subscriber can only be installed once"
    );
    let _ = provider.shutdown();
  }
}
