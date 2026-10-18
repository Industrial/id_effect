//! OpenTelemetry configuration — standard `OTEL_*` env vars and optional `id_effect_config` keys.

use std::time::Duration;

use opentelemetry_sdk::trace::Sampler;

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

/// Head sampler deciding which new traces are recorded (`OTEL_TRACES_SAMPLER`, with the ratio in
/// `OTEL_TRACES_SAMPLER_ARG`).
///
/// The `ParentBased*` variants follow the sampling decision of a remote or local parent and
/// apply their root sampler only to traces that start here, so one service's ratio does not cut
/// traces short that an upstream service decided to keep. Ratios are fractions in `[0, 1]`.
#[derive(Clone, Copy, Debug, Default)]
pub enum OtelSampler {
  /// Record every trace (`always_on`).
  AlwaysOn,
  /// Record no trace (`always_off`).
  AlwaysOff,
  /// Record this fraction of traces, by trace id (`traceidratio`).
  TraceIdRatio(f64),
  /// Follow the parent; record every root trace (`parentbased_always_on`, the default).
  #[default]
  ParentBasedAlwaysOn,
  /// Follow the parent; record no root trace (`parentbased_always_off`).
  ParentBasedAlwaysOff,
  /// Follow the parent; record this fraction of root traces (`parentbased_traceidratio`).
  ParentBasedTraceIdRatio(f64),
}

impl OtelSampler {
  /// The SDK [`Sampler`] for this setting (for tracer providers built by hand).
  pub fn sdk_sampler(&self) -> Sampler {
    match *self {
      Self::AlwaysOn => Sampler::AlwaysOn,
      Self::AlwaysOff => Sampler::AlwaysOff,
      Self::TraceIdRatio(ratio) => Sampler::TraceIdRatioBased(ratio),
      Self::ParentBasedAlwaysOn => Sampler::ParentBased(Box::new(Sampler::AlwaysOn)),
      Self::ParentBasedAlwaysOff => Sampler::ParentBased(Box::new(Sampler::AlwaysOff)),
      Self::ParentBasedTraceIdRatio(ratio) => {
        Sampler::ParentBased(Box::new(Sampler::TraceIdRatioBased(ratio)))
      }
    }
  }
}

/// Ratios compare bit for bit, so equality is reflexive (and `Eq` holds) even for a `NaN`.
impl PartialEq for OtelSampler {
  fn eq(&self, other: &Self) -> bool {
    match (self, other) {
      (Self::TraceIdRatio(a), Self::TraceIdRatio(b))
      | (Self::ParentBasedTraceIdRatio(a), Self::ParentBasedTraceIdRatio(b)) => {
        a.to_bits() == b.to_bits()
      }
      _ => std::mem::discriminant(self) == std::mem::discriminant(other),
    }
  }
}

impl Eq for OtelSampler {}

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
  /// Trace head sampler (`OTEL_TRACES_SAMPLER`, `OTEL_TRACES_SAMPLER_ARG`).
  pub sampler: OtelSampler,
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
      sampler: OtelSampler::default(),
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

    if let Some(sampler) = sampler_from_vars(|key| std::env::var(key).ok())? {
      cfg.sampler = sampler;
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

  /// Override the trace head sampler.
  #[inline]
  pub fn with_sampler(mut self, sampler: OtelSampler) -> Self {
    self.sampler = sampler;
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
  /// Trace sampler name, as `OTEL_TRACES_SAMPLER` (`always_on`, `parentbased_traceidratio`, ...).
  pub const TRACES_SAMPLER: &str = "otel.traces_sampler";
  /// Sampler ratio, as `OTEL_TRACES_SAMPLER_ARG` (read with [`TRACES_SAMPLER`]).
  pub const TRACES_SAMPLER_ARG: &str = "otel.traces_sampler_arg";
}

#[cfg(feature = "config")]
/// Load [`OtelConfig`] via [`id_effect_config::ConfigProvider`], falling back to [`OtelConfig::from_env`].
pub fn load_otel_config(
  provider: &dyn id_effect_config::ConfigProvider,
) -> Result<OtelConfig, OtelError> {
  use id_effect_config::Config;

  // A key that is absent (or unreadable) keeps the env-derived value.
  let string = |key: &str| Config::string(key).load(provider).ok();
  let mut cfg = OtelConfig::from_env()?;

  if let Some(endpoint) = string(config_keys::ENDPOINT) {
    if endpoint.trim().is_empty() {
      return Err(OtelError::Config(format!(
        "{} must not be empty",
        config_keys::ENDPOINT
      )));
    }
    cfg.endpoint = endpoint;
  }

  if let Some(protocol) = string(config_keys::PROTOCOL) {
    cfg.protocol = parse_protocol(&protocol)?;
  }

  if let Some(name) = string(config_keys::SERVICE_NAME) {
    if name.trim().is_empty() {
      return Err(OtelError::Config(format!(
        "{} must not be empty",
        config_keys::SERVICE_NAME
      )));
    }
    cfg.service_name = name;
  }

  if let Some(version) = string(config_keys::SERVICE_VERSION)
    && !version.trim().is_empty()
  {
    cfg.service_version = Some(version);
  }

  if let Some(headers) = string(config_keys::HEADERS) {
    cfg.headers = parse_header_list(&headers)?;
  }

  if let Some(name) = string(config_keys::TRACES_SAMPLER) {
    let arg = string(config_keys::TRACES_SAMPLER_ARG)
      .or_else(|| std::env::var("OTEL_TRACES_SAMPLER_ARG").ok());
    cfg.sampler = parse_sampler(&name, arg.as_deref())?;
  }

  Ok(cfg)
}

/// The sampler named by `OTEL_TRACES_SAMPLER` (ratio from `OTEL_TRACES_SAMPLER_ARG`), read
/// through `var`; `None` when the variable is unset.
fn sampler_from_vars(
  var: impl Fn(&str) -> Option<String>,
) -> Result<Option<OtelSampler>, OtelError> {
  let Some(name) = var("OTEL_TRACES_SAMPLER") else {
    return Ok(None);
  };
  parse_sampler(&name, var("OTEL_TRACES_SAMPLER_ARG").as_deref()).map(Some)
}

/// The OpenTelemetry sampler names; ratio samplers read `arg` (default `1.0`).
fn parse_sampler(name: &str, arg: Option<&str>) -> Result<OtelSampler, OtelError> {
  let ratio = || match arg.map(str::trim).filter(|a| !a.is_empty()) {
    None => Ok(1.0),
    Some(raw) => raw
      .parse::<f64>()
      .ok()
      .filter(|r| (0.0..=1.0).contains(r))
      .ok_or_else(|| {
        OtelError::Config(format!(
          "invalid OTEL_TRACES_SAMPLER_ARG {raw:?} (expected a ratio in [0, 1])"
        ))
      }),
  };
  match name.trim().to_ascii_lowercase().as_str() {
    "always_on" => Ok(OtelSampler::AlwaysOn),
    "always_off" => Ok(OtelSampler::AlwaysOff),
    "traceidratio" => Ok(OtelSampler::TraceIdRatio(ratio()?)),
    "parentbased_always_on" => Ok(OtelSampler::ParentBasedAlwaysOn),
    "parentbased_always_off" => Ok(OtelSampler::ParentBasedAlwaysOff),
    "parentbased_traceidratio" => Ok(OtelSampler::ParentBasedTraceIdRatio(ratio()?)),
    other => Err(OtelError::Config(format!(
      "unsupported OTEL_TRACES_SAMPLER {other:?} (expected always_on, always_off, traceidratio \
       or their parentbased_ forms)"
    ))),
  }
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

  /// `var` over a fixed set of `OTEL_*` variables (the process environment is shared).
  fn vars(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
    let pairs: Vec<(String, String)> = pairs
      .iter()
      .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
      .collect();
    move |key| pairs.iter().find(|(k, _)| k == key).map(|(_, v)| v.clone())
  }

  /// Spans exported by a provider sampling with `sampler`: `roots` root spans, then one child of
  /// a sampled remote parent.
  fn exported(sampler: OtelSampler, roots: usize) -> (usize, bool) {
    use opentelemetry::trace::{
      SpanContext, SpanId, TraceContextExt, TraceFlags, TraceId, TraceState, Tracer,
      TracerProvider as _,
    };
    use opentelemetry_sdk::trace::{InMemorySpanExporter, SdkTracerProvider};

    let exporter = InMemorySpanExporter::default();
    let provider = SdkTracerProvider::builder()
      .with_sampler(sampler.sdk_sampler())
      .with_simple_exporter(exporter.clone())
      .build();
    let tracer = provider.tracer("sampler_test");
    for _ in 0..roots {
      drop(tracer.start("root"));
    }
    let remote = opentelemetry::Context::new().with_remote_span_context(SpanContext::new(
      TraceId::from(7_u128),
      SpanId::from(9_u64),
      TraceFlags::SAMPLED,
      true,
      TraceState::default(),
    ));
    drop(tracer.start_with_context("child", &remote));
    let spans = exporter.get_finished_spans().expect("spans");
    let _ = provider.shutdown();
    (
      spans.iter().filter(|s| s.name == "root").count(),
      spans.iter().any(|s| s.name == "child"),
    )
  }

  #[test]
  fn ratio_sampler_from_env() {
    let ratio = vars(&[
      ("OTEL_TRACES_SAMPLER", "traceidratio"),
      ("OTEL_TRACES_SAMPLER_ARG", "0.25"),
    ]);
    assert_eq!(
      sampler_from_vars(ratio).unwrap(),
      Some(OtelSampler::TraceIdRatio(0.25))
    );
    let parent = vars(&[
      ("OTEL_TRACES_SAMPLER", "parentbased_traceidratio"),
      ("OTEL_TRACES_SAMPLER_ARG", "0"),
    ]);
    let sampler = sampler_from_vars(parent).unwrap().expect("sampler set");
    assert_eq!(sampler, OtelSampler::ParentBasedTraceIdRatio(0.0));
    // Ratio 0 drops every root, but a sampled parent keeps its child.
    assert_eq!(exported(sampler, 20), (0, true));
    // A plain ratio sampler ignores the parent; ratio 1 keeps everything.
    assert_eq!(exported(OtelSampler::TraceIdRatio(0.0), 20), (0, false));
    assert_eq!(exported(OtelSampler::TraceIdRatio(1.0), 20), (20, true));
    let half = exported(OtelSampler::TraceIdRatio(0.5), 200).0;
    assert!(half > 0 && half < 200, "ratio 0.5 kept {half} of 200");
    // Without an argument a ratio sampler keeps everything; unset means the default.
    assert_eq!(
      sampler_from_vars(vars(&[("OTEL_TRACES_SAMPLER", "traceidratio")])).unwrap(),
      Some(OtelSampler::TraceIdRatio(1.0))
    );
    assert_eq!(sampler_from_vars(vars(&[])).unwrap(), None);
    assert_eq!(
      OtelConfig::localhost("svc").sampler,
      OtelSampler::ParentBasedAlwaysOn
    );
  }

  #[test]
  fn sampler_rejects_unknown_names_and_bad_ratios() {
    assert!(parse_sampler("jaeger_remote", None).is_err());
    assert!(parse_sampler("traceidratio", Some("1.5")).is_err());
    assert!(parse_sampler("traceidratio", Some("NaN")).is_err());
    assert!(parse_sampler("parentbased_traceidratio", Some("half")).is_err());
    assert_eq!(
      parse_sampler(" Always_Off ", Some("ignored")).unwrap(),
      OtelSampler::AlwaysOff
    );
    assert_eq!(
      parse_sampler("parentbased_always_off", None).unwrap(),
      OtelSampler::ParentBasedAlwaysOff
    );
    assert_eq!(
      parse_sampler("always_on", None).unwrap(),
      OtelSampler::AlwaysOn
    );
    assert_ne!(
      OtelSampler::TraceIdRatio(0.5),
      OtelSampler::ParentBasedTraceIdRatio(0.5)
    );
  }
}
