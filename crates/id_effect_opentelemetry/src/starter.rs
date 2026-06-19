//! Unified OpenTelemetry starter: one call to install traces, metrics, logs, and propagation.

use opentelemetry::global;
use opentelemetry::metrics::Meter;
use opentelemetry::metrics::MeterProvider as _;
use opentelemetry_sdk::logs::{InMemoryLogExporter, SdkLoggerProvider};
use opentelemetry_sdk::metrics::{InMemoryMetricExporter, SdkMeterProvider};
use opentelemetry_sdk::trace::{InMemorySpanExporter, SdkTracerProvider};

use crate::logs_bridge::{
  sdk_logger_provider_with_in_memory_exporter, try_init_global_tracing_with_otel_logs,
};
use crate::metrics_bridge::sdk_meter_provider_with_in_memory_exporter;
use crate::propagation::install_w3c_trace_context_propagator;
use crate::subscriber::{
  register_global_tracer_provider, sdk_tracer_provider_with_in_memory_exporter,
};

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
}

/// Options for [`install_otel_starter`].
pub struct OtelStarterConfig {
  /// Meter / tracer instrument scope name.
  pub service_name: &'static str,
  /// Include `tracing_subscriber::fmt` on stdout.
  pub with_fmt_layer: bool,
}

impl OtelStarterConfig {
  /// Defaults: no stdout `fmt` layer.
  pub fn new(service_name: &'static str) -> Self {
    Self {
      service_name,
      with_fmt_layer: false,
    }
  }

  /// Toggle the optional `tracing_subscriber::fmt` layer.
  pub fn with_fmt_layer(mut self, enabled: bool) -> Self {
    self.with_fmt_layer = enabled;
    self
  }
}

/// Handle returned after a successful [`install_otel_starter`]; call [`force_flush`](Self::force_flush)
/// before shutdown and [`shutdown`](Self::shutdown) on graceful exit.
pub struct OtelStarterGuard {
  providers: OtelProviders,
  service_name: &'static str,
}

impl OtelStarterGuard {
  /// Installed SDK providers (clone cheaply for bridges and tests).
  pub fn providers(&self) -> &OtelProviders {
    &self.providers
  }

  /// OpenTelemetry [`Meter`] scoped to the configured service name.
  pub fn meter(&self) -> Meter {
    self.providers.meter.meter(self.service_name)
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

/// Installs global tracer + meter providers, W3C propagation, and a tracing subscriber with OTEL trace + log layers.
///
/// Returns a guard that owns provider clones for flush/shutdown. Prefer
/// [`tracing::subscriber::with_default`] in tests when the global dispatcher must not be claimed.
pub fn install_otel_starter(
  providers: &OtelProviders,
  config: &OtelStarterConfig,
) -> Result<OtelStarterGuard, tracing_subscriber::util::TryInitError> {
  register_global_tracer_provider(&providers.tracer);
  global::set_meter_provider(providers.meter.clone());
  install_w3c_trace_context_propagator();
  try_init_global_tracing_with_otel_logs(
    &providers.tracer,
    &providers.logger,
    config.with_fmt_layer,
  )?;
  Ok(OtelStarterGuard {
    providers: providers.clone(),
    service_name: config.service_name,
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

  mod install_otel_starter {
    use super::*;

    #[test]
    fn exports_traces_metrics_and_logs_via_scoped_subscriber() {
      let exporters = OtelInMemoryExporters::default();
      let providers = OtelProviders::with_in_memory_exporters(&exporters);
      let subscriber =
        trace_and_log_subscriber_for_providers(&providers.tracer, &providers.logger, false);
      tracing::subscriber::with_default(subscriber, || {
        register_global_tracer_provider(&providers.tracer);
        global::set_meter_provider(providers.meter.clone());
        install_w3c_trace_context_propagator();

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
      assert!(
        spans.iter().any(|s| s.name == "starter_span"),
        "expected starter_span, got {spans:?}"
      );

      let metrics = exporters.metrics.get_finished_metrics().expect("metrics");
      assert!(
        !metrics.is_empty(),
        "expected metric export after flush, got {metrics:?}"
      );

      let logs = exporters.logs.get_emitted_logs().expect("logs");
      assert!(
        logs.iter().any(|l| {
          matches!(
            &l.record.body(),
            Some(AnyValue::String(body)) if body.as_str() == "starter log"
          )
        }),
        "expected starter log, got {logs:?}"
      );

      let _ = providers.tracer.shutdown();
      let _ = providers.meter.shutdown();
      let _ = providers.logger.shutdown();
    }
  }

  mod otel_starter_guard {
    use super::*;

    #[test]
    fn meter_returns_instruments_for_service_name() {
      let exporters = OtelInMemoryExporters::default();
      let providers = OtelProviders::with_in_memory_exporters(&exporters);
      let guard = OtelStarterGuard {
        providers,
        service_name: "my_service",
      };
      let meter = guard.meter();
      let counter = meter
        .u64_counter("health_checks")
        .with_description("liveness probes")
        .build();
      counter.add(1, &[KeyValue::new("probe", "health")]);
      guard.force_flush();
      guard.shutdown();
    }
  }
}
