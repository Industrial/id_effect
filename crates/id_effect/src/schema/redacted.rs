//! Sensitive values with masked [`std::fmt::Debug`] — schema-layer counterpart to config [`Secret`](https://docs.rs/id_effect_config/latest/id_effect_config/struct.Secret.html).

use std::fmt;

/// Wraps a value so logs and debug output never expose the inner payload.
///
/// ```rust
/// use id_effect::schema::Redacted;
///
/// let token = Redacted::new("my-api-key".to_string());
/// assert_eq!(format!("{token:?}"), "<redacted>");
/// assert_eq!(token.expose(), "my-api-key");
/// ```
pub struct Redacted<T>(T);

impl<T> Redacted<T> {
  /// Wrap `value`.
  #[inline]
  pub fn new(value: T) -> Self {
    Self(value)
  }

  /// Borrow the inner value (keep call sites auditable).
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

impl<T> fmt::Debug for Redacted<T> {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    write!(f, "<redacted>")
  }
}

impl<T> fmt::Display for Redacted<T> {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    write!(f, "<redacted>")
  }
}

impl<T: Clone> Clone for Redacted<T> {
  fn clone(&self) -> Self {
    Self(self.0.clone())
  }
}

impl<T: PartialEq> PartialEq for Redacted<T> {
  fn eq(&self, other: &Self) -> bool {
    self.0 == other.0
  }
}

impl<T: Eq> Eq for Redacted<T> {}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn debug_and_display_are_redacted() {
    let r = Redacted::new("secret".to_string());
    assert_eq!(format!("{r:?}"), "<redacted>");
    assert_eq!(format!("{r}"), "<redacted>");
  }

  #[test]
  fn expose_returns_inner() {
    let r = Redacted::new(42u32);
    assert_eq!(*r.expose(), 42);
  }
}
