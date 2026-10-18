//! [`RedisError`]: what a Redis operation fails with.

use std::fmt;

use id_effect::CacheBackendError;
use redis::ErrorKind;

/// A Redis operation failed.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum RedisError {
  /// The configuration is invalid (a malformed URL, a zero pool size).
  Config(String),
  /// The server could not be reached, or the connection dropped or timed out.
  Connection(String),
  /// The server rejected a command (wrong type, unknown command or module, script error).
  Command(String),
  /// A reply or a stored value did not decode to the requested type, or a value did not encode.
  Decode(String),
  /// `clear()` on a store whose namespace is empty: its glob is every key of the database. The
  /// explicit flush is `clear_all_dangerous()`.
  EmptyNamespace,
}

impl fmt::Display for RedisError {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    match self {
      Self::Config(m) => write!(f, "redis configuration: {m}"),
      Self::Connection(m) => write!(f, "redis connection: {m}"),
      Self::Command(m) => write!(f, "redis command: {m}"),
      Self::Decode(m) => write!(f, "redis value: {m}"),
      Self::EmptyNamespace => f.write_str(
        "redis store: clear() with an empty namespace would delete every key (clear_all_dangerous() does)",
      ),
    }
  }
}

impl std::error::Error for RedisError {}

impl From<redis::RedisError> for RedisError {
  fn from(e: redis::RedisError) -> Self {
    let m = e.to_string();
    if e.is_io_error() || e.is_connection_refusal() || e.is_timeout() || e.is_connection_dropped() {
      return Self::Connection(m);
    }
    match e.kind() {
      ErrorKind::InvalidClientConfig => Self::Config(m),
      ErrorKind::Parse | ErrorKind::UnexpectedReturnType => Self::Decode(m),
      _ => Self::Command(m),
    }
  }
}

impl From<serde_json::Error> for RedisError {
  fn from(e: serde_json::Error) -> Self {
    Self::Decode(e.to_string())
  }
}

impl From<RedisError> for CacheBackendError {
  fn from(e: RedisError) -> Self {
    CacheBackendError(e.to_string())
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn redis_errors_are_classified() {
    let config: redis::RedisError = (ErrorKind::InvalidClientConfig, "bad url").into();
    assert!(matches!(RedisError::from(config), RedisError::Config(m) if m.contains("bad url")));
    let parse: redis::RedisError = (ErrorKind::UnexpectedReturnType, "not a string").into();
    assert!(matches!(RedisError::from(parse), RedisError::Decode(_)));
    let io: redis::RedisError =
      std::io::Error::new(std::io::ErrorKind::ConnectionReset, "x").into();
    assert!(matches!(RedisError::from(io), RedisError::Connection(_)));
    let other: redis::RedisError = (ErrorKind::Client, "nope").into();
    assert!(matches!(RedisError::from(other), RedisError::Command(_)));
    let json = serde_json::from_str::<u32>("x").unwrap_err();
    assert!(matches!(RedisError::from(json), RedisError::Decode(_)));
  }

  #[test]
  fn display_and_cache_backend_error() {
    let cases = [
      (RedisError::Config("a".into()), "redis configuration: a"),
      (RedisError::Connection("b".into()), "redis connection: b"),
      (RedisError::Command("c".into()), "redis command: c"),
      (RedisError::Decode("d".into()), "redis value: d"),
      (
        RedisError::EmptyNamespace,
        "redis store: clear() with an empty namespace would delete every key (clear_all_dangerous() does)",
      ),
    ];
    for (e, shown) in cases {
      assert_eq!(e.to_string(), shown);
    }
    let backend = CacheBackendError::from(RedisError::Command("x".into()));
    assert_eq!(backend.0, "redis command: x");
  }
}
