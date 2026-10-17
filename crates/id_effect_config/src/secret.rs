//! Redacted wrapper for sensitive configuration values (Effect `Config.secret`).

use std::fmt;

/// Wraps a sensitive value so it is never exposed via [`fmt::Debug`] or [`fmt::Display`].
///
/// Mirrors Effect.ts `Config.secret` / `ConfigSecret`.
///
/// ```rust
/// use id_effect_config::Secret;
///
/// let token = Secret::new("my-api-key".to_string());
/// assert_eq!(format!("{token:?}"), "<redacted>");
/// assert_eq!(token.expose(), "my-api-key");
/// ```
pub struct Secret<T>(T);

impl<T> Secret<T> {
  /// Wrap `value` in a [`Secret`].
  #[inline]
  pub fn new(value: T) -> Self {
    Self(value)
  }

  /// Access the inner value.  Call sites should be minimal and auditable.
  #[inline]
  pub fn expose(&self) -> &T {
    &self.0
  }

  /// Consume the wrapper and return the inner value.
  #[inline]
  pub fn into_inner(self) -> T {
    self.0
  }
}

impl<T> fmt::Debug for Secret<T> {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    write!(f, "<redacted>")
  }
}

impl<T> fmt::Display for Secret<T> {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    write!(f, "<redacted>")
  }
}

impl<T: Clone> Clone for Secret<T> {
  fn clone(&self) -> Self {
    Self(self.0.clone())
  }
}

impl<T: PartialEq> PartialEq for Secret<T> {
  fn eq(&self, other: &Self) -> bool {
    self.0 == other.0
  }
}

impl<T: Eq> Eq for Secret<T> {}

/// Deserializes the inner value, so a whole-document extract (`password: Secret<String>`) keeps
/// secrets redacted from the moment they are loaded.
impl<'de, T: serde::Deserialize<'de>> serde::Deserialize<'de> for Secret<T> {
  fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
    T::deserialize(deserializer).map(Self)
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn debug_is_redacted() {
    let s = Secret::new("hunter2".to_string());
    assert_eq!(format!("{s:?}"), "<redacted>");
  }

  #[test]
  fn display_is_redacted() {
    let s = Secret::new(42u32);
    assert_eq!(format!("{s}"), "<redacted>");
  }

  #[test]
  fn expose_returns_inner() {
    let s = Secret::new("value");
    assert_eq!(*s.expose(), "value");
  }

  #[test]
  fn into_inner_unwraps() {
    let s = Secret::new(99u8);
    assert_eq!(s.into_inner(), 99);
  }

  #[test]
  fn deserializes_the_inner_value() {
    #[derive(serde::Deserialize)]
    struct Db {
      password: Secret<String>,
    }
    let db: Db = figment::Figment::from(figment::providers::Serialized::default(
      "password", "hunter2",
    ))
    .extract()
    .expect("extract");
    assert_eq!(db.password.expose(), "hunter2");
    assert_eq!(format!("{:?}", db.password), "<redacted>");
  }

  #[test]
  fn clone_and_eq() {
    let a = Secret::new("x".to_string());
    let b = a.clone();
    assert_eq!(a, b);
  }
}
