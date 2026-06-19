//! OTLP exporter builders for traces, metrics, and logs.

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
fn grpc_metadata(
  headers: &[(String, String)],
) -> opentelemetry_otlp::tonic_types::metadata::MetadataMap {
  use http::HeaderMap;
  use http::header::{HeaderName, HeaderValue};
  use opentelemetry_otlp::tonic_types::metadata::MetadataMap;

  let mut map = HeaderMap::new();
  for (key, value) in headers {
    if let (Ok(name), Ok(val)) = (
      HeaderName::try_from(key.as_str()),
      HeaderValue::try_from(value.as_str()),
    ) {
      map.insert(name, val);
    }
  }
  MetadataMap::from_headers(map)
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
      builder
        .build()
        .map_err(|e| OtelError::Exporter(e.to_string()))
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
      builder
        .build()
        .map_err(|e| OtelError::Exporter(e.to_string()))
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
      builder
        .build()
        .map_err(|e| OtelError::Exporter(e.to_string()))
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
      builder
        .build()
        .map_err(|e| OtelError::Exporter(e.to_string()))
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
      builder
        .build()
        .map_err(|e| OtelError::Exporter(e.to_string()))
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
      builder
        .build()
        .map_err(|e| OtelError::Exporter(e.to_string()))
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
