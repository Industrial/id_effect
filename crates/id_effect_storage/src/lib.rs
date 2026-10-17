//! Object storage for id_effect services: one store interface over the local file system,
//! in-memory and S3-compatible backends.
//!
//! [`ObjectKey`] is a validated key that means the same on every backend: 1 to
//! [`ObjectKey::MAX_LEN`] bytes of `/`-separated segments, none empty, `.` or `..`, and no control
//! characters, so a key can never escape a file-system store's root.
//!
//! ```
//! use id_effect_storage::{KeyError, ObjectKey};
//!
//! let key = ObjectKey::new("avatars/carol.png").unwrap();
//! assert_eq!(key.segments().collect::<Vec<_>>(), ["avatars", "carol.png"]);
//! assert_eq!(ObjectKey::new("/avatars"), Err(KeyError::EmptySegment));
//! ```
//!
//! Tier `integration` (ADR 0010 Amendment 6).

#![forbid(unsafe_code)]
#![deny(missing_docs)]

use std::fmt;

/// A validated object key.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ObjectKey(String);

impl ObjectKey {
  /// Longest key, in bytes (the S3 limit).
  pub const MAX_LEN: usize = 1024;

  /// Validates `key` as an object key.
  pub fn new(key: impl Into<String>) -> Result<Self, KeyError> {
    let key = key.into();
    if key.is_empty() {
      return Err(KeyError::Empty);
    }
    if key.len() > Self::MAX_LEN {
      return Err(KeyError::TooLong { len: key.len() });
    }
    if key.chars().any(char::is_control) {
      return Err(KeyError::ControlChar);
    }
    for segment in key.split('/') {
      match segment {
        "" => return Err(KeyError::EmptySegment),
        "." | ".." => return Err(KeyError::DotSegment),
        _ => {}
      }
    }
    Ok(ObjectKey(key))
  }

  /// The key.
  pub fn as_str(&self) -> &str {
    &self.0
  }

  /// The `/`-separated segments, first to last.
  pub fn segments(&self) -> impl Iterator<Item = &str> {
    self.0.split('/')
  }

  /// The last segment.
  pub fn file_name(&self) -> &str {
    self.0.rsplit('/').next().unwrap_or(&self.0)
  }
}

impl fmt::Display for ObjectKey {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.write_str(&self.0)
  }
}

/// Why a string is not a valid [`ObjectKey`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeyError {
  /// The key is empty.
  Empty,
  /// The key is longer than [`ObjectKey::MAX_LEN`] bytes.
  TooLong {
    /// Length of the key, in bytes.
    len: usize,
  },
  /// The key contains a control character.
  ControlChar,
  /// The key has an empty segment: a leading, trailing or doubled `/`.
  EmptySegment,
  /// A segment is `.` or `..`.
  DotSegment,
}

impl fmt::Display for KeyError {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    match self {
      KeyError::Empty => f.write_str("object key is empty"),
      KeyError::TooLong { len } => write!(
        f,
        "object key is {len} bytes, longer than {}",
        ObjectKey::MAX_LEN
      ),
      KeyError::ControlChar => f.write_str("object key contains a control character"),
      KeyError::EmptySegment => f.write_str("object key has an empty `/` segment"),
      KeyError::DotSegment => f.write_str("object key has a `.` or `..` segment"),
    }
  }
}

impl std::error::Error for KeyError {}
