//! Object storage for id_effect services: one store interface over the local file system,
//! in-memory and S3-compatible backends.
//!
//! The [`ObjectStore`] capability (installed as [`ObjectStorage`]) puts, gets, heads, deletes
//! and lists objects, streams uploads and downloads as core [`Stream`](id_effect::Stream)s
//! without holding the object in memory, and issues signed, expiring URLs. Backends:
//! [`MemoryStore`] (tests), [`LocalStore`] (a directory) and, with the `s3` feature, `S3Store`
//! (AWS S3, MinIO, R2, ... with presigned URLs). Local and in-memory stores sign URLs with a
//! [`UrlSigner`] the serving side verifies. `S3Store` never creates its bucket: provision it
//! before the service starts (infrastructure code, `aws s3 mb`, `mc mb`).
//!
//! Bodies are [`Bytes`] (re-exported from the `bytes` crate).
//!
//! [`ObjectKey`] is a validated key that means the same on every backend: 1 to
//! [`ObjectKey::MAX_LEN`] bytes of `/`-separated segments, none empty, `.` or `..`, and no control
//! characters, so a key can never escape a file-system store's root.
//!
//! ```
//! use id_effect::{Exit, Stream};
//! use id_effect_storage::{
//!   Bytes, KeyError, MemoryStore, ObjectKey, ObjectStorageEnv, StorageError, get, put_stream,
//! };
//!
//! let key = ObjectKey::new("avatars/carol.png").unwrap();
//! assert_eq!(key.segments().collect::<Vec<_>>(), ["avatars", "carol.png"]);
//! assert_eq!(ObjectKey::new("/avatars"), Err(KeyError::EmptySegment));
//!
//! let body = Stream::<Bytes, StorageError>::from_vec(vec![
//!   Bytes::from_static(b"he"),
//!   Bytes::from_static(b"llo"),
//! ]);
//! let program = put_stream::<ObjectStorageEnv, _>(key.clone(), body)
//!   .flat_map(move |_| get(key));
//! let got = program.provide_layer(MemoryStore::layer()).block_on();
//! assert!(matches!(got, Exit::Success(b) if b == "hello"));
//! ```
//!
//! Tier `platform` (ADR 0010 Amendment 6): the Ledger reference app under
//! `examples/reference/` depends on it.

#![forbid(unsafe_code)]
#![deny(missing_docs)]

mod local;
mod memory;
#[cfg(feature = "s3")]
mod s3;
mod signed;
mod store;

/// The body type of [`ObjectStore::put`], [`ObjectStore::get`] and the streamed uploads and
/// downloads (the `bytes` crate's, re-exported so callers need no dependency of their own).
pub use bytes::Bytes;
pub use local::LocalStore;
pub use memory::MemoryStore;
#[cfg(feature = "s3")]
pub use s3::{S3Config, S3Store};
pub use signed::{SignatureError, SignedMethod, UrlSigner};
pub use store::{
  DEFAULT_PART_SIZE, ObjectMeta, ObjectStorage, ObjectStorageEnv, ObjectStore, PutOutcome,
  StorageError, delete, exists, get, get_stream, head, list, object_storage_env,
  provide_object_storage, put, put_stream, signed_url,
};
/// The URL type of [`ObjectStore::signed_url`] and [`UrlSigner`].
pub use url::Url;

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
