//! [`AuthError`]: why a credential was not accepted.

use std::fmt;
use std::time::Duration;

/// Why a presented credential (bearer token, API key, Basic password) was not accepted, or why
/// it could not be checked.
///
/// Every rejection of the credential itself answers **401**
/// ([`http_status`](Self::http_status)); a backend that cannot be reached (user store, JWKS
/// endpoint) answers **503**, unusable key material or configuration **500**, and an attempt
/// refused by the [`LoginThrottle`](crate::throttle::LoginThrottle) **429**. Messages never
/// contain the credential.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum AuthError {
  /// The credential is not shaped like one (bad base64, not a JWT, wrong API-key format).
  Malformed(String),
  /// Wrong password, unknown user, unknown or revoked API key, bad signature.
  InvalidCredentials,
  /// The token's `exp` (plus leeway) has passed.
  Expired,
  /// The token's `nbf` (minus leeway) is still in the future.
  NotYetValid,
  /// A claim is missing or wrong (`iss`, `aud`, a required claim, the subject).
  InvalidClaims(String),
  /// No verification key for the token's `kid` (or algorithm), even after a JWKS refresh.
  UnknownKey(String),
  /// The store or key endpoint behind the check failed.
  Unavailable(String),
  /// Key material or signing failed (a configuration error, not the client's).
  Key(String),
  /// Too many failed attempts for the account or the client address: the attempt was refused
  /// before any password hashing. Try again after `retry_after`.
  TooManyAttempts {
    /// How long until the next attempt is allowed.
    retry_after: Duration,
  },
  /// The verifier is configured so that it cannot check credentials safely (a configuration
  /// error, not the client's): for example a `JwtVerifier` with neither an issuer nor an
  /// audience.
  Config(String),
}

impl AuthError {
  /// 401 for a rejected credential, 503 for [`Unavailable`](Self::Unavailable), 500 for
  /// [`Key`](Self::Key) and [`Config`](Self::Config), 429 for
  /// [`TooManyAttempts`](Self::TooManyAttempts).
  pub fn http_status(&self) -> u16 {
    match self {
      Self::Unavailable(_) => 503,
      Self::Key(_) | Self::Config(_) => 500,
      Self::TooManyAttempts { .. } => 429,
      _ => 401,
    }
  }

  /// `true` when the client's credential was rejected (the 401 cases).
  #[inline]
  pub fn is_rejection(&self) -> bool {
    self.http_status() == 401
  }
}

impl fmt::Display for AuthError {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    match self {
      Self::Malformed(m) => write!(f, "malformed credential: {m}"),
      Self::InvalidCredentials => f.write_str("invalid credentials"),
      Self::Expired => f.write_str("the token has expired"),
      Self::NotYetValid => f.write_str("the token is not valid yet"),
      Self::InvalidClaims(m) => write!(f, "invalid token claims: {m}"),
      Self::UnknownKey(m) => write!(f, "no verification key: {m}"),
      Self::Unavailable(m) => write!(f, "authentication backend unavailable: {m}"),
      Self::Key(m) => write!(f, "key material: {m}"),
      Self::TooManyAttempts { retry_after } => write!(
        f,
        "too many failed attempts; retry in {} s",
        retry_after_secs(*retry_after)
      ),
      Self::Config(m) => write!(f, "authentication is misconfigured: {m}"),
    }
  }
}

impl std::error::Error for AuthError {}

/// Whole seconds of `d`, rounded up and at least 1 (a `Retry-After` value).
pub(crate) fn retry_after_secs(d: Duration) -> u64 {
  d.as_secs()
    .saturating_add(u64::from(d.subsec_nanos() > 0))
    .max(1)
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn statuses_and_messages() {
    let cases = [
      (
        AuthError::Malformed("x".into()),
        401,
        "malformed credential: x",
      ),
      (AuthError::InvalidCredentials, 401, "invalid credentials"),
      (AuthError::Expired, 401, "expired"),
      (AuthError::NotYetValid, 401, "not valid yet"),
      (AuthError::InvalidClaims("aud".into()), 401, "claims: aud"),
      (AuthError::UnknownKey("k1".into()), 401, "key: k1"),
      (AuthError::Unavailable("db".into()), 503, "unavailable: db"),
      (AuthError::Key("pem".into()), 500, "key material: pem"),
      (
        AuthError::TooManyAttempts {
          retry_after: Duration::from_millis(1_500),
        },
        429,
        "retry in 2 s",
      ),
      (AuthError::Config("iss".into()), 500, "misconfigured: iss"),
    ];
    for (error, status, text) in cases {
      assert_eq!(error.http_status(), status, "{error:?}");
      assert_eq!(error.is_rejection(), status == 401);
      assert!(error.to_string().contains(text), "{error}");
    }
    assert_eq!(retry_after_secs(Duration::ZERO), 1);
    assert_eq!(retry_after_secs(Duration::from_secs(900)), 900);
  }
}
