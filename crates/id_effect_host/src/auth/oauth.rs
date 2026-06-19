//! OAuth2 authorization-code trait surface (no IdP implementation in v1).

use id_effect::Effect;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

/// Tokens returned from the token endpoint.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OAuthTokens {
  /// Bearer access token.
  pub access_token: String,
  /// Optional refresh token.
  pub refresh_token: Option<String>,
  /// Seconds until expiry when known.
  pub expires_in: Option<u64>,
}

/// Normalized user profile from the IdP.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OAuthUserInfo {
  /// Subject identifier.
  pub sub: String,
  /// Email when available.
  pub email: Option<String>,
}

/// OAuth client failures.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OAuthError {
  /// Configuration or wire error.
  Protocol(String),
  /// Unknown authorization code (stub).
  InvalidCode,
}

impl std::fmt::Display for OAuthError {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    match self {
      Self::Protocol(msg) => write!(f, "oauth: {msg}"),
      Self::InvalidCode => write!(f, "invalid authorization code"),
    }
  }
}

impl std::error::Error for OAuthError {}

/// Capability: build authorization URLs and exchange codes for tokens.
pub trait OAuthClient: Send + Sync {
  /// Authorization redirect URL including `state`.
  fn authorization_url(&self, state: &str) -> String;
  /// Exchange authorization code for tokens and user info.
  fn exchange_code(&self, code: &str) -> Effect<(OAuthTokens, OAuthUserInfo), OAuthError, ()>;
}

/// Deterministic stub for tests — maps registered codes to profiles.
#[derive(Clone, Default)]
pub struct MemoryOAuthClient {
  base_url: String,
  codes: Arc<Mutex<HashMap<String, OAuthUserInfo>>>,
}

impl MemoryOAuthClient {
  /// Create stub with issuer base URL.
  pub fn new(base_url: impl Into<String>) -> Self {
    Self {
      base_url: base_url.into(),
      codes: Arc::new(Mutex::new(HashMap::new())),
    }
  }
  /// Register a code that `exchange_code` will accept.
  pub fn register_code(&self, code: &str, user: OAuthUserInfo) {
    if let Ok(mut guard) = self.codes.lock() {
      guard.insert(code.to_owned(), user);
    }
  }
}

impl OAuthClient for MemoryOAuthClient {
  fn authorization_url(&self, state: &str) -> String {
    format!("{}/authorize?state={state}", self.base_url)
  }
  fn exchange_code(&self, code: &str) -> Effect<(OAuthTokens, OAuthUserInfo), OAuthError, ()> {
    let code = code.to_owned();
    let codes = Arc::clone(&self.codes);
    Effect::new(move |_r| {
      let guard = codes
        .lock()
        .map_err(|e| OAuthError::Protocol(e.to_string()))?;
      let user = guard.get(&code).cloned().ok_or(OAuthError::InvalidCode)?;
      let tokens = OAuthTokens {
        access_token: format!("access-{code}"),
        refresh_token: Some(format!("refresh-{code}")),
        expires_in: Some(3600),
      };
      Ok((tokens, user))
    })
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn oauth_error_display_variants() {
    assert!(OAuthError::InvalidCode.to_string().contains("invalid"));
    assert!(
      OAuthError::Protocol("wire".into())
        .to_string()
        .contains("wire")
    );
  }

  #[test]
  fn memory_client_register_and_exchange() {
    use id_effect::run_blocking;
    let client = MemoryOAuthClient::new("https://idp.test");
    client.register_code(
      "c",
      OAuthUserInfo {
        sub: "sub".into(),
        email: None,
      },
    );
    let (tokens, user) = run_blocking(client.exchange_code("c"), ()).unwrap();
    assert_eq!(user.sub, "sub");
    assert_eq!(tokens.expires_in, Some(3600));
  }
}
