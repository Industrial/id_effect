//! Bridge [`id_effect_logger`] records and `tracing` events to OpenTelemetry logs export.

use id_effect_logger::{EffectLoggerError, LogBackend, LogLevel, LogRecord};
use opentelemetry::logs::{
  AnyValue, LogRecord as OtelLogRecord, Logger, LoggerProvider as _, Severity,
};
use opentelemetry::trace::TracerProvider as _;
use opentelemetry_appender_tracing::layer::OpenTelemetryTracingBridge;
use opentelemetry_sdk::logs::{InMemoryLogExporter, SdkLogger, SdkLoggerProvider};
use opentelemetry_sdk::trace::SdkTracerProvider;
use tracing::Subscriber;
use tracing_subscriber::Registry;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

fn log_level_to_severity(level: LogLevel) -> Severity {
  match level {
    LogLevel::Trace => Severity::Trace,
    LogLevel::Debug => Severity::Debug,
    LogLevel::Info => Severity::Info,
    LogLevel::Warn => Severity::Warn,
    LogLevel::Error | LogLevel::Fatal => Severity::Error,
    LogLevel::None => Severity::Info,
  }
}

fn format_log_body(rec: &LogRecord<'_>) -> String {
  let mut body = String::new();
  if !rec.spans.is_empty() {
    body.push('[');
    body.push_str(&rec.spans.join(" > "));
    body.push_str("] ");
  }
  body.push_str(rec.message.as_ref());
  for (k, v) in rec.annotations.iter() {
    body.push(' ');
    body.push_str(k);
    body.push('=');
    body.push_str(v);
  }
  body
}

/// Builds an [`SdkLoggerProvider`] that exports logs to an in-memory buffer (tests and spikes).
pub fn sdk_logger_provider_with_in_memory_exporter(
  exporter: &InMemoryLogExporter,
) -> SdkLoggerProvider {
  SdkLoggerProvider::builder()
    .with_simple_exporter(exporter.clone())
    .build()
}

/// Returns a [`LogBackend`] that emits [`LogRecord`] values as OTEL log records.
pub fn otel_log_backend_for_provider(provider: &SdkLoggerProvider) -> OtelLogBackend {
  OtelLogBackend {
    logger: provider.logger("id_effect_opentelemetry"),
  }
}

/// [`LogBackend`] that forwards [`LogRecord`] values to an OpenTelemetry [`SdkLogger`].
#[derive(Clone)]
pub struct OtelLogBackend {
  logger: SdkLogger,
}

impl LogBackend for OtelLogBackend {
  fn emit(&self, rec: &LogRecord<'_>) -> Result<(), EffectLoggerError> {
    if rec.level == LogLevel::None {
      return Ok(());
    }
    let mut record = self.logger.create_log_record();
    record.set_body(AnyValue::String(format_log_body(rec).into()));
    record.set_severity_number(log_level_to_severity(rec.level));
    record.set_severity_text(rec.level.as_str());
    if !rec.annotations.is_empty() {
      record.add_attributes(rec.annotations.iter().map(|(k, v)| (k.clone(), v.clone())));
    }
    self.logger.emit(record);
    Ok(())
  }
}

/// Returns a [`tracing_subscriber::Layer`] that forwards `tracing` events to OTEL logs.
pub fn tracing_logs_layer_for_provider(
  provider: &SdkLoggerProvider,
) -> OpenTelemetryTracingBridge<SdkLoggerProvider, SdkLogger> {
  OpenTelemetryTracingBridge::new(provider)
}

/// Combined trace + log subscriber for tests and apps (optional `fmt` layer).
pub fn trace_and_log_subscriber_for_providers(
  tracer_provider: &SdkTracerProvider,
  logger_provider: &SdkLoggerProvider,
  with_fmt_layer: bool,
) -> Box<dyn Subscriber + Send + Sync + 'static> {
  let tracer = tracer_provider.tracer("id_effect_opentelemetry");
  let otel_trace_layer = tracing_opentelemetry::layer().with_tracer(tracer);
  let otel_log_layer = tracing_logs_layer_for_provider(logger_provider);
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
}

/// Installs a global subscriber with both OTEL trace and log export layers.
pub fn try_init_global_tracing_with_otel_logs(
  tracer_provider: &SdkTracerProvider,
  logger_provider: &SdkLoggerProvider,
  with_fmt_layer: bool,
) -> Result<(), tracing_subscriber::util::TryInitError> {
  trace_and_log_subscriber_for_providers(tracer_provider, logger_provider, with_fmt_layer)
    .try_init()
}

#[cfg(test)]
mod tests {
  use std::sync::Arc;

  use super::*;
  use crate::subscriber::sdk_tracer_provider_with_in_memory_exporter;
  use id_effect_logger::{CompositeLogBackend, Logger};
  use opentelemetry_sdk::trace::InMemorySpanExporter;

  mod otel_log_backend_for_provider {
    use super::*;

    #[test]
    fn exports_effect_logger_record_to_otel() {
      let exporter = InMemoryLogExporter::default();
      let provider = sdk_logger_provider_with_in_memory_exporter(&exporter);
      let backend = otel_log_backend_for_provider(&provider);
      let mut rec = LogRecord {
        level: LogLevel::Info,
        message: std::borrow::Cow::Borrowed("hello otel"),
        annotations: Default::default(),
        spans: vec!["outer".into()],
      };
      rec.annotations.insert("request_id".into(), "abc".into());
      backend.emit(&rec).unwrap();
      let _ = provider.force_flush();
      let logs = exporter.get_emitted_logs().expect("logs");
      assert_eq!(logs.len(), 1, "expected one log record, got {logs:?}");
      assert!(matches!(
        logs[0].record.body(),
        Some(AnyValue::String(body)) if body.as_str() == "[outer] hello otel request_id=abc"
      ));
      assert_eq!(logs[0].record.severity_number(), Some(Severity::Info));
      let _ = provider.shutdown();
    }
  }

  mod tracing_logs_layer_for_provider {
    use super::*;

    #[test]
    fn exports_tracing_event_to_otel_logs() {
      let exporter = InMemoryLogExporter::default();
      let provider = sdk_logger_provider_with_in_memory_exporter(&exporter);
      let layer = tracing_logs_layer_for_provider(&provider);
      let subscriber = Registry::default().with(layer);
      tracing::subscriber::with_default(subscriber, || {
        tracing::info!(target: "id_effect_opentelemetry", "tracing log line");
      });
      let _ = provider.force_flush();
      let logs = exporter.get_emitted_logs().expect("logs");
      assert!(
        logs.iter().any(|l| {
          matches!(
            &l.record.body(),
            Some(AnyValue::String(body)) if body.as_str() == "tracing log line"
          )
        }),
        "expected tracing log line in OTEL export, got {logs:?}"
      );
      let _ = provider.shutdown();
    }
  }

  mod trace_and_log_subscriber_for_providers {
    use super::*;

    #[test]
    fn exports_both_spans_and_logs() {
      let span_exporter = InMemorySpanExporter::default();
      let log_exporter = InMemoryLogExporter::default();
      let tracer_provider = sdk_tracer_provider_with_in_memory_exporter(&span_exporter);
      let logger_provider = sdk_logger_provider_with_in_memory_exporter(&log_exporter);
      let subscriber =
        trace_and_log_subscriber_for_providers(&tracer_provider, &logger_provider, false);
      tracing::subscriber::with_default(subscriber, || {
        let span = tracing::info_span!("work");
        let _g = span.enter();
        tracing::info!(target: "id_effect_opentelemetry", "inside span");
      });
      let _ = tracer_provider.force_flush();
      let _ = logger_provider.force_flush();
      let spans = span_exporter.get_finished_spans().expect("spans");
      assert!(
        spans.iter().any(|s| s.name == "work"),
        "expected work span, got {spans:?}"
      );
      let logs = log_exporter.get_emitted_logs().expect("logs");
      assert!(
        logs.iter().any(|l| {
          matches!(
            &l.record.body(),
            Some(AnyValue::String(body)) if body.as_str() == "inside span"
          )
        }),
        "expected log inside span, got {logs:?}"
      );
      let _ = tracer_provider.shutdown();
      let _ = logger_provider.shutdown();
    }
  }

  mod composite_logger_with_otel_backend {
    use super::*;

    #[test]
    fn fan_out_effect_logger_to_otel() {
      let exporter = InMemoryLogExporter::default();
      let provider = sdk_logger_provider_with_in_memory_exporter(&exporter);
      let composite = CompositeLogBackend::new();
      composite
        .add(Arc::new(otel_log_backend_for_provider(&provider)))
        .unwrap();
      let rec = LogRecord {
        level: LogLevel::Warn,
        message: std::borrow::Cow::Borrowed("composite path"),
        annotations: Default::default(),
        spans: vec![],
      };
      composite.emit(&rec).unwrap();
      let _ = provider.force_flush();
      let logs = exporter.get_emitted_logs().expect("logs");
      assert_eq!(logs.len(), 1);
      assert!(matches!(
        logs[0].record.body(),
        Some(AnyValue::String(body)) if body.as_str() == "composite path"
      ));
      let _ = provider.shutdown();
    }
  }
}
