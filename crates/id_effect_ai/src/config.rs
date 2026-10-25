//! AI vendor configuration: endpoints, API keys, limits and the HTTP client.
//!
//! Secure by default (ADR 0013): API keys are [`Secret`]s (never printed by `Debug`), every
//! request goes through id_effect_platform's client built from [`AiConfig::http`] (the SSRF
//! policy, timeouts and body caps of [`HttpClientConfig::default`]), [`AiLimits`] are finite, and
//! prompts and completions are not recorded unless [`AiConfig::dangerous_log_prompts`] is taken.

use std::sync::Arc;
use std::time::Duration;

use id_effect::observability::posture::record_dangerous;
use id_effect_config::Secret;
use id_effect_platform::http::{HttpClient, HttpClientConfig, ReqwestHttpClient};

use crate::error::AiError;

/// Posture id of [`AiConfig::dangerous_log_prompts`] (what the prod posture check reads).
pub const DANGEROUS_LOG_PROMPTS: &str = "id_effect_ai::AiConfig::dangerous_log_prompts";

const REOPENS_LOG_PROMPTS: &str = "R3: prompts and completions are recorded in span annotations \
  (personal data and secrets in prompts reach the trace backend)";

/// Finite limits applied to every vendor call (ADR 0013 R4). None is infinite: each field is a
/// number, and the setters keep it at least 1.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct AiLimits {
  /// Generated-token cap sent with a request that sets none ([`ChatRequest::max_tokens`](crate::ChatRequest::max_tokens)).
  pub max_tokens: u32,
  /// Total time for one request, connect to last byte (also bounds a stream).
  pub request_timeout: Duration,
  /// Cap on a buffered response body.
  pub max_response_bytes: usize,
  /// Cap on the bytes of one streamed response.
  pub max_stream_bytes: usize,
  /// Cap on the server-sent events of one streamed response.
  pub max_stream_events: usize,
}

impl AiLimits {
  /// Default [`max_tokens`](Self::max_tokens): 1024.
  pub const DEFAULT_MAX_TOKENS: u32 = 1024;
  /// Default [`request_timeout`](Self::request_timeout): 60 s.
  pub const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(60);
  /// Default [`max_response_bytes`](Self::max_response_bytes): 4 MiB.
  pub const DEFAULT_MAX_RESPONSE_BYTES: usize = 4 * 1024 * 1024;
  /// Default [`max_stream_bytes`](Self::max_stream_bytes): 16 MiB.
  pub const DEFAULT_MAX_STREAM_BYTES: usize = 16 * 1024 * 1024;
  /// Default [`max_stream_events`](Self::max_stream_events): 65 536.
  pub const DEFAULT_MAX_STREAM_EVENTS: usize = 65_536;

  /// Replaces [`max_tokens`](Self::max_tokens) (at least 1).
  #[must_use]
  pub fn with_max_tokens(mut self, n: u32) -> Self {
    self.max_tokens = n.max(1);
    self
  }

  /// Replaces [`request_timeout`](Self::request_timeout) (at least 1 ms).
  #[must_use]
  pub fn with_request_timeout(mut self, timeout: Duration) -> Self {
    self.request_timeout = timeout.max(Duration::from_millis(1));
    self
  }

  /// Replaces [`max_response_bytes`](Self::max_response_bytes) (at least 1).
  #[must_use]
  pub fn with_max_response_bytes(mut self, n: usize) -> Self {
    self.max_response_bytes = n.max(1);
    self
  }

  /// Replaces [`max_stream_bytes`](Self::max_stream_bytes) (at least 1).
  #[must_use]
  pub fn with_max_stream_bytes(mut self, n: usize) -> Self {
    self.max_stream_bytes = n.max(1);
    self
  }

  /// Replaces [`max_stream_events`](Self::max_stream_events) (at least 1).
  #[must_use]
  pub fn with_max_stream_events(mut self, n: usize) -> Self {
    self.max_stream_events = n.max(1);
    self
  }
}

impl Default for AiLimits {
  fn default() -> Self {
    Self {
      max_tokens: Self::DEFAULT_MAX_TOKENS,
      request_timeout: Self::DEFAULT_REQUEST_TIMEOUT,
      max_response_bytes: Self::DEFAULT_MAX_RESPONSE_BYTES,
      max_stream_bytes: Self::DEFAULT_MAX_STREAM_BYTES,
      max_stream_events: Self::DEFAULT_MAX_STREAM_EVENTS,
    }
  }
}

/// Vendor endpoints, API keys ([`Secret`], never printed), [`AiLimits`] and the HTTP client
/// configuration.
///
/// ```
/// use id_effect_ai::AiConfig;
///
/// let cfg = AiConfig::default().with_openai_api_key("sk-test");
/// assert!(!format!("{cfg:?}").contains("sk-test"));
/// assert_eq!(cfg.limits.max_tokens, 1024);
/// ```
#[derive(Debug, Clone)]
pub struct AiConfig {
  /// OpenAI API key (`OPENAI_API_KEY`).
  pub openai_api_key: Option<Secret<String>>,
  /// OpenAI API base URL (`OPENAI_BASE_URL`).
  pub openai_base_url: String,
  /// Anthropic API key (`ANTHROPIC_API_KEY`).
  pub anthropic_api_key: Option<Secret<String>>,
  /// Anthropic API base URL (`ANTHROPIC_BASE_URL`).
  pub anthropic_base_url: String,
  /// Ollama base URL (`OLLAMA_BASE_URL`). The default, `http://localhost:11434`, is a loopback
  /// address: the SSRF policy refuses it until [`http`](Self::http) allows the host.
  pub ollama_base_url: String,
  /// Cursor API key (`CURSOR_API_KEY`).
  pub cursor_api_key: Option<Secret<String>>,
  /// Cursor Cloud Agents API base URL (`CURSOR_BASE_URL`).
  pub cursor_base_url: String,
  /// Finite limits of every call.
  pub limits: AiLimits,
  /// The platform HTTP client configuration [`http_client`](Self::http_client) builds from:
  /// SSRF policy, connect timeout, redirects.
  pub http: HttpClientConfig,
  log_prompts: bool,
}

fn env_nonempty(key: &str) -> Option<String> {
  std::env::var(key).ok().filter(|s| !s.is_empty())
}

impl Default for AiConfig {
  fn default() -> Self {
    Self {
      openai_api_key: None,
      openai_base_url: "https://api.openai.com".to_string(),
      anthropic_api_key: None,
      anthropic_base_url: "https://api.anthropic.com".to_string(),
      ollama_base_url: "http://localhost:11434".to_string(),
      cursor_api_key: None,
      cursor_base_url: "https://api.cursor.com".to_string(),
      limits: AiLimits::default(),
      http: HttpClientConfig::default(),
      log_prompts: false,
    }
  }
}

impl AiConfig {
  /// Load keys, base URL overrides and `AI_MAX_TOKENS` (`ANTHROPIC_MAX_TOKENS` is still read)
  /// from environment variables; everything else keeps its default.
  pub fn from_env() -> Self {
    Self::from_lookup(env_nonempty)
  }

  fn from_lookup(get: impl Fn(&str) -> Option<String>) -> Self {
    let mut cfg = Self {
      openai_api_key: get("OPENAI_API_KEY").map(Secret::new),
      anthropic_api_key: get("ANTHROPIC_API_KEY").map(Secret::new),
      cursor_api_key: get("CURSOR_API_KEY").map(Secret::new),
      ..Self::default()
    };
    let urls = [
      ("OPENAI_BASE_URL", &mut cfg.openai_base_url),
      ("ANTHROPIC_BASE_URL", &mut cfg.anthropic_base_url),
      ("OLLAMA_BASE_URL", &mut cfg.ollama_base_url),
      ("CURSOR_BASE_URL", &mut cfg.cursor_base_url),
    ];
    for (key, slot) in urls {
      if let Some(url) = get(key) {
        *slot = url;
      }
    }
    let max_tokens = get("AI_MAX_TOKENS").or_else(|| get("ANTHROPIC_MAX_TOKENS"));
    if let Some(n) = max_tokens.and_then(|n| n.parse().ok()) {
      cfg.limits = cfg.limits.with_max_tokens(n);
    }
    cfg
  }

  /// Sets the OpenAI API key.
  #[must_use]
  pub fn with_openai_api_key(mut self, key: impl Into<String>) -> Self {
    self.openai_api_key = Some(Secret::new(key.into()));
    self
  }

  /// Sets the OpenAI base URL.
  #[must_use]
  pub fn with_openai_base_url(mut self, url: impl Into<String>) -> Self {
    self.openai_base_url = url.into();
    self
  }

  /// Sets the Anthropic API key.
  #[must_use]
  pub fn with_anthropic_api_key(mut self, key: impl Into<String>) -> Self {
    self.anthropic_api_key = Some(Secret::new(key.into()));
    self
  }

  /// Sets the Anthropic base URL.
  #[must_use]
  pub fn with_anthropic_base_url(mut self, url: impl Into<String>) -> Self {
    self.anthropic_base_url = url.into();
    self
  }

  /// Sets the Ollama base URL.
  #[must_use]
  pub fn with_ollama_base_url(mut self, url: impl Into<String>) -> Self {
    self.ollama_base_url = url.into();
    self
  }

  /// Sets the Cursor API key.
  #[must_use]
  pub fn with_cursor_api_key(mut self, key: impl Into<String>) -> Self {
    self.cursor_api_key = Some(Secret::new(key.into()));
    self
  }

  /// Sets the Cursor base URL.
  #[must_use]
  pub fn with_cursor_base_url(mut self, url: impl Into<String>) -> Self {
    self.cursor_base_url = url.into();
    self
  }

  /// Replaces the [`AiLimits`].
  #[must_use]
  pub fn with_limits(mut self, limits: AiLimits) -> Self {
    self.limits = limits;
    self
  }

  /// Replaces the HTTP client configuration (for instance one whose SSRF policy allows a
  /// self-hosted endpoint with [`HttpClientConfig::allow_hosts`]).
  #[must_use]
  pub fn with_http(mut self, http: HttpClientConfig) -> Self {
    self.http = http;
    self
  }

  /// Records prompts and completions in the request span's annotations (`ai.prompt`,
  /// `ai.completion`), which every span hook (the OpenTelemetry bridge, the tracing buffer) then
  /// exports. Off by default (ADR 0013 R3): prompts carry personal data and secrets. Records
  /// [`DANGEROUS_LOG_PROMPTS`] in the posture registry, so the prod posture check refuses to
  /// start unless the operator accepted it.
  #[must_use]
  pub fn dangerous_log_prompts(mut self) -> Self {
    record_dangerous(DANGEROUS_LOG_PROMPTS, REOPENS_LOG_PROMPTS);
    self.log_prompts = true;
    self
  }

  /// Whether [`dangerous_log_prompts`](Self::dangerous_log_prompts) was taken.
  #[inline]
  pub fn logs_prompts(&self) -> bool {
    self.log_prompts
  }

  /// The platform HTTP client for [`http`](Self::http): the SSRF guard on every destination,
  /// no redirects followed, the configured timeouts.
  ///
  /// # Panics
  ///
  /// As `ReqwestHttpClient::from_config`: if the TLS backend cannot be initialised.
  pub fn http_client(&self) -> Arc<dyn HttpClient> {
    Arc::new(ReqwestHttpClient::from_config(&self.http))
  }

  /// Require OpenAI key or return error.
  pub fn require_openai_key(&self) -> Result<&Secret<String>, AiError> {
    require(self.openai_api_key.as_ref(), "OPENAI_API_KEY")
  }

  /// Require Anthropic key or return error.
  pub fn require_anthropic_key(&self) -> Result<&Secret<String>, AiError> {
    require(self.anthropic_api_key.as_ref(), "ANTHROPIC_API_KEY")
  }

  /// Require Cursor key or return error.
  pub fn require_cursor_key(&self) -> Result<&Secret<String>, AiError> {
    require(self.cursor_api_key.as_ref(), "CURSOR_API_KEY")
  }
}

fn require<'a>(key: Option<&'a Secret<String>>, name: &str) -> Result<&'a Secret<String>, AiError> {
  key.ok_or_else(|| AiError::Upstream(format!("{name} not set")))
}

#[cfg(test)]
mod tests {
  use super::*;
  use std::collections::HashMap;

  fn lookup(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
    let map: HashMap<String, String> = pairs
      .iter()
      .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
      .collect();
    move |k| map.get(k).cloned()
  }

  #[test]
  fn from_lookup_reads_keys_urls_and_max_tokens() {
    let cfg = AiConfig::from_lookup(lookup(&[
      ("OPENAI_API_KEY", "o"),
      ("ANTHROPIC_API_KEY", "a"),
      ("CURSOR_API_KEY", "c"),
      ("OPENAI_BASE_URL", "https://o.example"),
      ("ANTHROPIC_BASE_URL", "https://a.example"),
      ("OLLAMA_BASE_URL", "https://l.example"),
      ("CURSOR_BASE_URL", "https://c.example"),
      ("AI_MAX_TOKENS", "77"),
    ]));
    assert_eq!(cfg.require_openai_key().unwrap().expose(), "o");
    assert_eq!(cfg.require_anthropic_key().unwrap().expose(), "a");
    assert_eq!(cfg.require_cursor_key().unwrap().expose(), "c");
    assert_eq!(cfg.openai_base_url, "https://o.example");
    assert_eq!(cfg.anthropic_base_url, "https://a.example");
    assert_eq!(cfg.ollama_base_url, "https://l.example");
    assert_eq!(cfg.cursor_base_url, "https://c.example");
    assert_eq!(cfg.limits.max_tokens, 77);
  }

  #[test]
  fn legacy_anthropic_max_tokens_and_bad_numbers() {
    let cfg = AiConfig::from_lookup(lookup(&[("ANTHROPIC_MAX_TOKENS", "12")]));
    assert_eq!(cfg.limits.max_tokens, 12);
    let cfg = AiConfig::from_lookup(lookup(&[("AI_MAX_TOKENS", "lots")]));
    assert_eq!(cfg.limits.max_tokens, AiLimits::DEFAULT_MAX_TOKENS);
    let _ = AiConfig::from_env();
  }

  #[test]
  fn missing_keys_are_errors() {
    let cfg = AiConfig::default();
    assert!(cfg.require_openai_key().is_err());
    assert!(cfg.require_anthropic_key().is_err());
    assert!(cfg.require_cursor_key().is_err());
    assert!(!cfg.logs_prompts());
  }

  #[test]
  fn builders_set_fields() {
    let cfg = AiConfig::default()
      .with_openai_api_key("o")
      .with_openai_base_url("https://o")
      .with_anthropic_api_key("a")
      .with_anthropic_base_url("https://a")
      .with_ollama_base_url("https://l")
      .with_cursor_api_key("c")
      .with_cursor_base_url("https://c")
      .with_limits(AiLimits::default().with_max_tokens(5))
      .with_http(HttpClientConfig::default().allow_hosts(["127.0.0.1"]));
    assert_eq!(cfg.openai_base_url, "https://o");
    assert_eq!(cfg.anthropic_base_url, "https://a");
    assert_eq!(cfg.ollama_base_url, "https://l");
    assert_eq!(cfg.cursor_base_url, "https://c");
    assert_eq!(cfg.limits.max_tokens, 5);
    assert_eq!(cfg.http.ssrf.allow_hosts, vec!["127.0.0.1".to_string()]);
    let _client = cfg.http_client();
  }

  #[test]
  fn limit_setters_never_reach_zero() {
    let limits = AiLimits::default()
      .with_max_tokens(0)
      .with_request_timeout(Duration::ZERO)
      .with_max_response_bytes(0)
      .with_max_stream_bytes(0)
      .with_max_stream_events(0);
    assert_eq!(limits.max_tokens, 1);
    assert_eq!(limits.request_timeout, Duration::from_millis(1));
    assert_eq!(limits.max_response_bytes, 1);
    assert_eq!(limits.max_stream_bytes, 1);
    assert_eq!(limits.max_stream_events, 1);
  }
}
