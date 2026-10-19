//! Redacted wrapper for sensitive configuration values (Effect `Config.secret`).

use std::fmt;

use subtle::ConstantTimeEq;
use zeroize::{Zeroize, ZeroizeOnDrop};

/// Wraps a sensitive value so it is never exposed via [`fmt::Debug`] or [`fmt::Display`], and
/// overwrites it with zeroes when the wrapper is dropped ([`Zeroize`]).
///
/// Mirrors Effect.ts `Config.secret` / `ConfigSecret`. Equality is defined only for
/// [`SecretString`] and [`SecretBytes`], and compares in constant time.
///
/// ```rust
/// use id_effect_config::Secret;
///
/// let token = Secret::new("my-api-key".to_string());
/// assert_eq!(format!("{token:?}"), "<redacted>");
/// assert_eq!(token.expose(), "my-api-key");
/// ```
// `None` only inside `into_inner`, which consumes the wrapper; `Drop` cannot move out of `T`.
pub struct Secret<T: Zeroize>(Option<T>);

/// A secret string (API key, password, token).
pub type SecretString = Secret<String>;

/// Secret bytes (key material).
pub type SecretBytes = Secret<Vec<u8>>;

impl<T: Zeroize> Secret<T> {
  /// Wrap `value` in a [`Secret`].
  #[inline]
  pub fn new(value: T) -> Self {
    Self(Some(value))
  }

  /// Access the inner value.  Call sites should be minimal and auditable.
  #[inline]
  pub fn expose(&self) -> &T {
    match &self.0 {
      Some(value) => value,
      None => unreachable!("a Secret holds its value until into_inner consumes it"),
    }
  }

  /// Consume the wrapper and return the inner value. The caller now owns it, so it is no longer
  /// zeroized on drop.
  #[inline]
  pub fn into_inner(mut self) -> T {
    match self.0.take() {
      Some(value) => value,
      None => unreachable!("a Secret holds its value until into_inner consumes it"),
    }
  }
}

impl<T: Zeroize> Drop for Secret<T> {
  fn drop(&mut self) {
    if let Some(value) = &mut self.0 {
      value.zeroize();
    }
  }
}

impl<T: Zeroize> ZeroizeOnDrop for Secret<T> {}

impl<T: Zeroize> fmt::Debug for Secret<T> {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    write!(f, "<redacted>")
  }
}

impl<T: Zeroize> fmt::Display for Secret<T> {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    write!(f, "<redacted>")
  }
}

impl<T: Zeroize + Clone> Clone for Secret<T> {
  fn clone(&self) -> Self {
    Self::new(self.expose().clone())
  }
}

/// Constant-time comparison ([`subtle::ConstantTimeEq`]); only the lengths are compared early.
impl PartialEq for SecretString {
  fn eq(&self, other: &Self) -> bool {
    self
      .expose()
      .as_bytes()
      .ct_eq(other.expose().as_bytes())
      .into()
  }
}

impl Eq for SecretString {}

/// Constant-time comparison ([`subtle::ConstantTimeEq`]); only the lengths are compared early.
impl PartialEq for SecretBytes {
  fn eq(&self, other: &Self) -> bool {
    self
      .expose()
      .as_slice()
      .ct_eq(other.expose().as_slice())
      .into()
  }
}

impl Eq for SecretBytes {}

/// Deserializes the inner value, so a whole-document extract (`password: Secret<String>`) keeps
/// secrets redacted from the moment they are loaded.
impl<'de, T: Zeroize + serde::Deserialize<'de>> serde::Deserialize<'de> for Secret<T> {
  fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
    T::deserialize(deserializer).map(Self::new)
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
    let s = Secret::new("value".to_string());
    assert_eq!(s.expose(), "value");
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
