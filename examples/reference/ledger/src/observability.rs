//! Tracing, metrics and what logs never show.
//!
//! - **Traces**: with `OTEL_EXPORTER_OTLP_ENDPOINT` set, [`install`] exports traces, metrics and
//!   logs over OTLP. The sampler comes from the environment (`OTEL_TRACES_SAMPLER`,
//!   `OTEL_TRACES_SAMPLER_ARG`; default `parentbased_always_on`), see [`sampler`]. Every request
//!   runs in a span (`RequestIdLayer`), every statement in a `db.query` client span
//!   ([`crate::db`]), and every outbound HTTP call (SendGrid) in a client span carrying the W3C
//!   `traceparent` ([`crate::mail::http_client`]).
//! - **Log level**: the OpenTelemetry starter's `EnvFilter` is reloadable; the console changes
//!   it at runtime ([`crate::console`]).
//! - **Redaction**: [`redaction`] masks every log annotation named like a password, a token, a
//!   secret, a cookie or an authorization, and every occurrence of the profile's own secrets,
//!   before any log backend sees the record.

use id_effect_kit::logger::RedactionPolicy;
use id_effect_kit::otel::{
  OtelConfig, OtelError, OtelSampler, OtelStarterGuard, install_from_config,
};

use crate::config::LedgerConfig;

/// The service name traces are exported under (`OTEL_SERVICE_NAME` overrides it).
pub const SERVICE_NAME: &str = "ledger";

/// The trace sampler the environment selects.
pub fn sampler() -> Result<OtelSampler, OtelError> {
  OtelConfig::from_env().map(|config| config.sampler)
}

/// Installs the OpenTelemetry exporters, the W3C propagators and the tracing subscriber when
/// `OTEL_EXPORTER_OTLP_ENDPOINT` names a collector; `None` without one.
pub fn install() -> Result<Option<OtelStarterGuard>, OtelError> {
  if std::env::var_os("OTEL_EXPORTER_OTLP_ENDPOINT").is_none() {
    return Ok(None);
  }
  let mut config = OtelConfig::from_env()?;
  if std::env::var_os("OTEL_SERVICE_NAME").is_none() {
    config.service_name = SERVICE_NAME.to_owned();
  }
  install_from_config(config).map(Some)
}

/// What the logs never show: `password` / `token` / `secret` fields (by name, any case), the
/// cookies and authorization headers, and the secrets of `config` wherever they appear.
pub fn redaction(config: &LedgerConfig) -> RedactionPolicy {
  let policy = RedactionPolicy::new()
    .glob("*password*")
    .glob("*token*")
    .glob("*secret*")
    .field("cookie")
    .field("set-cookie")
    .field("authorization")
    .secret(config.auth.jwt_secret.expose().clone())
    .secret(config.auth.cookie_key.expose().clone())
    .secret(config.db.password.expose().clone());
  let policy = match &config.mail.sendgrid_api_key {
    Some(key) => policy.secret(key.expose().clone()),
    None => policy,
  };
  match &config.storage.secret_access_key {
    Some(key) => policy.secret(key.expose().clone()),
    None => policy,
  }
}
