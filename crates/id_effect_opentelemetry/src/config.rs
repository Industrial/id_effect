//! OpenTelemetry configuration — standard `OTEL_*` env vars and optional `id_effect_config` keys.

use std::time::Duration;

use crate::error::OtelError;

/// OTLP transport protocol.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum OtelProtocol {
  /// gRPC on port 4317 (default).
  #[default]
  Grpc,
  /// HTTP/protobuf on port 4318.
  Http,
}

/// Production-oriented OpenTelemetry settings.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OtelConfig {
  /// Collector endpoint (`OTEL_EXPORTER_OTLP_ENDPOINT`).
  pub endpoint: String,
  /// Transport protocol (`OTEL_EXPORTER_OTLP_PROTOCOL`).
  pub protocol: OtelProtocol,
  /// `service.name` resource attribute (`OTEL_SERVICE_NAME`).
  pub service_name: String,
  /// Optional `service.version` resource attribute.
  pub service_version: Option<String>,
  /// Extra OTLP request headers (`OTEL_EXPORTER_OTLP_HEADERS`, `key=value` pairs).
  pub headers: Vec<(String, String)>,
  /// Metric export interval.
  pub export_interval: Duration,
  /// Per-export RPC/HTTP timeout.
  pub export_timeout: Duration,
  /// Graceful shutdown timeout for provider flush.
  pub shutdown_timeout: Duration,
  /// Include `tracing_subscriber::fmt` on stdout when installing globals.
  pub with_fmt_layer: bool,
  /// Optional `EnvFilter` directive (e.g. `info,id_effect=debug`).
  pub env_filter: Option<String>,
}

impl OtelConfig {
  /// Reasonable defaults pointing at a local OTLP collector (gRPC).
  pub fn localhost(service_name: impl Into<String>) -> Self {
    Self {
      endpoint: "http://localhost:4317".into(),
      protocol: OtelProtocol::Grpc,
      service_name: service_name.into(),
      service_version: None,
      headers: Vec::new(),
      export_interval: Duration::from_secs(5),
      export_timeout: Duration::from_secs(10),
      shutdown_timeout: Duration::from_secs(3),
      with_fmt_layer: false,
      env_filter: None,
    }
  }

  /// Load from standard OpenTelemetry environment variables.
  pub fn from_env() -> Result<Self, OtelError> {
    let mut cfg =
      Self::localhost(std::env::var("OTEL_SERVICE_NAME").unwrap_or_else(|_| "id_effect".into()));

    if let Ok(endpoint) = std::env::var("OTEL_EXPORTER_OTLP_ENDPOINT") {
      if endpoint.trim().is_empty() {
        return Err(OtelError::Config(
          "OTEL_EXPORTER_OTLP_ENDPOINT must not be empty".into(),
        ));
      }
      cfg.endpoint = endpoint;
    }

    if let Ok(protocol) = std::env::var("OTEL_EXPORTER_OTLP_PROTOCOL") {
      cfg.protocol = parse_protocol(&protocol)?;
    }

    if let Ok(version) = std::env::var("OTEL_SERVICE_VERSION")
      && !version.trim().is_empty()
    {
      cfg.service_version = Some(version);
    }

    if let Ok(headers) = std::env::var("OTEL_EXPORTER_OTLP_HEADERS") {
      cfg.headers = parse_header_list(&headers)?;
    }

    if let Ok(ms) = std::env::var("OTEL_METRIC_EXPORT_INTERVAL") {
      let millis: u64 = ms
        .parse()
        .map_err(|_| OtelError::Config(format!("invalid OTEL_METRIC_EXPORT_INTERVAL: {ms}")))?;
      cfg.export_interval = Duration::from_millis(millis);
    }

    if let Ok(ms) = std::env::var("OTEL_EXPORTER_OTLP_TIMEOUT") {
      let millis: u64 = ms
        .parse()
        .map_err(|_| OtelError::Config(format!("invalid OTEL_EXPORTER_OTLP_TIMEOUT: {ms}")))?;
      cfg.export_timeout = Duration::from_millis(millis);
    }

    if let Ok(filter) = std::env::var("RUST_LOG")
      && !filter.trim().is_empty()
    {
      cfg.env_filter = Some(filter);
    }

    Ok(cfg)
  }

  /// Toggle stdout `fmt` layer for [`crate::install_otel_starter`].
  #[inline]
  pub fn with_fmt_layer(mut self, enabled: bool) -> Self {
    self.with_fmt_layer = enabled;
    self
  }

  /// Override the OTLP endpoint.
  #[inline]
  pub fn with_endpoint(mut self, endpoint: impl Into<String>) -> Self {
    self.endpoint = endpoint.into();
    self
  }

  /// Override the transport protocol.
  #[inline]
  pub fn with_protocol(mut self, protocol: OtelProtocol) -> Self {
    self.protocol = protocol;
    self
  }
}

/// Keys read when the `config` feature is enabled.
pub mod config_keys {
  /// OTLP endpoint URL.
  pub const ENDPOINT: &str = "otel.endpoint";
  /// `grpc` or `http`.
  pub const PROTOCOL: &str = "otel.protocol";
  /// `service.name` resource attribute.
  pub const SERVICE_NAME: &str = "otel.service_name";
  /// Optional `service.version`.
  pub const SERVICE_VERSION: &str = "otel.service_version";
  /// Comma-separated `key=value` OTLP headers.
  pub const HEADERS: &str = "otel.headers";
}

#[cfg(feature = "config")]
/// Load [`OtelConfig`] via [`id_effect_config::ConfigProvider`], falling back to [`OtelConfig::from_env`].
pub fn load_otel_config(
  provider: &dyn id_effect_config::ConfigProvider,
) -> Result<OtelConfig, OtelError> {
  use id_effect_config::config;

  let mut cfg = OtelConfig::from_env()?;

  if let Ok(endpoint) = config::string(provider, config_keys::ENDPOINT) {
    if endpoint.trim().is_empty() {
      return Err(OtelError::Config(format!(
        "{} must not be empty",
        config_keys::ENDPOINT
      )));
    }
    cfg.endpoint = endpoint;
  }

  if let Ok(protocol) = config::string(provider, config_keys::PROTOCOL) {
    cfg.protocol = parse_protocol(&protocol)?;
  }

  if let Ok(name) = config::string(provider, config_keys::SERVICE_NAME) {
    if name.trim().is_empty() {
      return Err(OtelError::Config(format!(
        "{} must not be empty",
        config_keys::SERVICE_NAME
      )));
    }
    cfg.service_name = name;
  }

  if let Ok(version) = config::string(provider, config_keys::SERVICE_VERSION)
    && !version.trim().is_empty()
  {
    cfg.service_version = Some(version);
  }

  if let Ok(headers) = config::string(provider, config_keys::HEADERS) {
    cfg.headers = parse_header_list(&headers)?;
  }

  Ok(cfg)
}

fn parse_protocol(raw: &str) -> Result<OtelProtocol, OtelError> {
  match raw.trim().to_ascii_lowercase().as_str() {
    "grpc" | "grpc/protobuf" => Ok(OtelProtocol::Grpc),
    "http" | "http/protobuf" | "http-proto" => Ok(OtelProtocol::Http),
    other => Err(OtelError::Config(format!(
      "unsupported OTLP protocol {other:?} (expected grpc or http)"
    ))),
  }
}

fn parse_header_list(raw: &str) -> Result<Vec<(String, String)>, OtelError> {
  if raw.trim().is_empty() {
    return Ok(Vec::new());
  }
  raw
    .split(',')
    .map(|pair| {
      let (key, value) = pair.split_once('=').ok_or_else(|| {
        OtelError::Config(format!(
          "invalid OTLP header pair {pair:?} (expected key=value)"
        ))
      })?;
      Ok((key.trim().to_string(), value.trim().to_string()))
    })
    .collect()
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn parse_headers_accepts_comma_separated_pairs() {
    let headers = parse_header_list("authorization=Bearer x, x-tenant=acme").unwrap();
    assert_eq!(
      headers,
      vec![
        ("authorization".into(), "Bearer x".into()),
        ("x-tenant".into(), "acme".into()),
      ]
    );
  }

  #[test]
  fn parse_protocol_rejects_unknown_values() {
    assert!(parse_protocol("nats").is_err());
  }
}
