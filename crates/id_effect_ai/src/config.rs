//! AI vendor configuration and API key loading.

use id_effect_config::Secret;

/// Vendor endpoints and API keys (secrets never logged via [`Secret`]).
#[derive(Debug, Clone)]
pub struct AiConfig {
  /// OpenAI API key (`OPENAI_API_KEY`).
  pub openai_api_key: Option<Secret<String>>,
  /// OpenAI API base URL.
  pub openai_base_url: String,
  /// Anthropic API key (`ANTHROPIC_API_KEY`).
  pub anthropic_api_key: Option<Secret<String>>,
  /// Anthropic API base URL.
  pub anthropic_base_url: String,
  /// Cursor API key (`CURSOR_API_KEY`).
  pub cursor_api_key: Option<Secret<String>>,
  /// Cursor Cloud Agents API base URL.
  pub cursor_base_url: String,
  /// Default max tokens for Anthropic requests.
  pub anthropic_max_tokens: u32,
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
      cursor_api_key: None,
      cursor_base_url: "https://api.cursor.com".to_string(),
      anthropic_max_tokens: 1024,
    }
  }
}

impl AiConfig {
  /// Load keys and optional base URL overrides from environment variables.
  pub fn from_env() -> Self {
    let mut cfg = Self::default();
    if let Some(key) = env_nonempty("OPENAI_API_KEY") {
      cfg.openai_api_key = Some(Secret::new(key));
    }
    if let Some(url) = env_nonempty("OPENAI_BASE_URL") {
      cfg.openai_base_url = url;
    }
    if let Some(key) = env_nonempty("ANTHROPIC_API_KEY") {
      cfg.anthropic_api_key = Some(Secret::new(key));
    }
    if let Some(url) = env_nonempty("ANTHROPIC_BASE_URL") {
      cfg.anthropic_base_url = url;
    }
    if let Some(key) = env_nonempty("CURSOR_API_KEY") {
      cfg.cursor_api_key = Some(Secret::new(key));
    }
    if let Some(url) = env_nonempty("CURSOR_BASE_URL") {
      cfg.cursor_base_url = url;
    }
    if let Some(n) = env_nonempty("ANTHROPIC_MAX_TOKENS")
      && let Ok(v) = n.parse()
    {
      cfg.anthropic_max_tokens = v;
    }
    cfg
  }

  /// Require OpenAI key or return error.
  pub fn require_openai_key(&self) -> Result<&Secret<String>, AiError> {
    self
      .openai_api_key
      .as_ref()
      .ok_or_else(|| AiError::Upstream("OPENAI_API_KEY not set".into()))
  }

  /// Require Anthropic key or return error.
  pub fn require_anthropic_key(&self) -> Result<&Secret<String>, AiError> {
    self
      .anthropic_api_key
      .as_ref()
      .ok_or_else(|| AiError::Upstream("ANTHROPIC_API_KEY not set".into()))
  }

  /// Require Cursor key or return error.
  pub fn require_cursor_key(&self) -> Result<&Secret<String>, AiError> {
    self
      .cursor_api_key
      .as_ref()
      .ok_or_else(|| AiError::Upstream("CURSOR_API_KEY not set".into()))
  }
}

use crate::error::AiError;
