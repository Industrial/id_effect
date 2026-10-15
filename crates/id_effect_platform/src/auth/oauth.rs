//! OAuth2 authorization-code trait surface (no IdP implementation in v1).
//!
//! Enable the `test-util` feature for `MemoryOAuthClient`, an in-memory test double.

use id_effect::next::Effect;
#[cfg(any(test, feature = "test-util"))]
use id_effect::next::{fail, succeed, suspend};
#[cfg(any(test, feature = "test-util"))]
use std::collections::HashMap;
#[cfg(any(test, feature = "test-util"))]
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
  /// The IdP rejected or did not recognise the authorization code.
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
  fn exchange_code(&self, code: &str) -> Effect<(OAuthTokens, OAuthUserInfo), OAuthError>;
}

/// Deterministic in-memory test double — maps registered codes to profiles.
///
/// Requires the `test-util` feature.
#[cfg(any(test, feature = "test-util"))]
#[derive(Clone, Debug, Default)]
pub struct MemoryOAuthClient {
  base_url: String,
  codes: Arc<Mutex<HashMap<String, OAuthUserInfo>>>,
}

#[cfg(any(test, feature = "test-util"))]
impl MemoryOAuthClient {
  /// Create a test double with issuer base URL.
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

#[cfg(any(test, feature = "test-util"))]
impl OAuthClient for MemoryOAuthClient {
  fn authorization_url(&self, state: &str) -> String {
    format!("{}/authorize?state={state}", self.base_url)
  }
  fn exchange_code(&self, code: &str) -> Effect<(OAuthTokens, OAuthUserInfo), OAuthError> {
    let code = code.to_owned();
    let codes = Arc::clone(&self.codes);
    suspend(move || {
      let user = match codes.lock() {
        Ok(guard) => guard.get(&code).cloned(),
        Err(e) => return fail(OAuthError::Protocol(e.to_string())),
      };
      let Some(user) = user else {
        return fail(OAuthError::InvalidCode);
      };
      let tokens = OAuthTokens {
        access_token: format!("access-{code}"),
        refresh_token: Some(format!("refresh-{code}")),
        expires_in: Some(3600),
      };
      succeed((tokens, user))
    })
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use id_effect::Exit;

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
    let client = MemoryOAuthClient::new("https://idp.test");
    client.register_code(
      "c",
      OAuthUserInfo {
        sub: "sub".into(),
        email: None,
      },
    );
    let Exit::Success((tokens, user)) = client.exchange_code("c").block_on() else {
      panic!("exchange failed");
    };
    assert_eq!(user.sub, "sub");
    assert_eq!(tokens.expires_in, Some(3600));
  }
}
