//! Errors for OpenTelemetry configuration and installation.

use std::fmt;

/// Configuration, export, or subscriber installation failures.
#[derive(Debug)]
pub enum OtelError {
  /// Invalid or missing configuration.
  Config(String),
  /// OTLP exporter or provider construction failed.
  Exporter(String),
  /// Global tracing subscriber could not be installed.
  Install(tracing_subscriber::util::TryInitError),
}

impl fmt::Display for OtelError {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    match self {
      Self::Config(msg) => write!(f, "otel config: {msg}"),
      Self::Exporter(msg) => write!(f, "otel exporter: {msg}"),
      Self::Install(err) => write!(f, "otel install: {err}"),
    }
  }
}

impl std::error::Error for OtelError {}

impl From<tracing_subscriber::util::TryInitError> for OtelError {
  fn from(err: tracing_subscriber::util::TryInitError) -> Self {
    Self::Install(err)
  }
}
