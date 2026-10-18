//! Durable workflows for id_effect services (ADR 0010 Amendment 7). The engine (a history journal
//! replayed on resume, on `id_effect_sql`, with activities as `id_effect_jobs` jobs; release
//! 0.11.0 decision D6) is being built in this crate; so far it holds the workflow name.
//!
//! [`WorkflowName`] is the validated name a workflow is registered and started under: 1 to
//! [`WorkflowName::MAX_LEN`] ASCII letters, digits, `.`, `_` or `-`.
//!
//! ```
//! use id_effect_workflow::{WorkflowName, WorkflowNameError};
//!
//! let name = WorkflowName::new("close-account").unwrap();
//! assert_eq!(name.to_string(), "close-account");
//! assert_eq!(WorkflowName::new(""), Err(WorkflowNameError::Empty));
//! ```

#![deny(missing_docs)]

use std::fmt;

/// The name a workflow is registered and started under.
///
/// 1 to [`WorkflowName::MAX_LEN`] ASCII letters, digits, `.`, `_` or `-`, so it is safe as a
/// database key and in log lines.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct WorkflowName(String);

impl WorkflowName {
  /// The longest accepted name, in bytes.
  pub const MAX_LEN: usize = 128;

  /// Validates `name`.
  pub fn new(name: impl Into<String>) -> Result<Self, WorkflowNameError> {
    let name = name.into();
    if name.is_empty() {
      return Err(WorkflowNameError::Empty);
    }
    if let Some((index, ch)) = name
      .char_indices()
      .find(|&(_, ch)| !(ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-')))
    {
      return Err(WorkflowNameError::InvalidChar { ch, index });
    }
    if name.len() > Self::MAX_LEN {
      return Err(WorkflowNameError::TooLong { len: name.len() });
    }
    Ok(Self(name))
  }

  /// The name as a string slice.
  pub fn as_str(&self) -> &str {
    &self.0
  }
}

impl fmt::Display for WorkflowName {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.pad(&self.0)
  }
}

/// Why [`WorkflowName::new`] rejected a name.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum WorkflowNameError {
  /// The name is empty.
  Empty,
  /// The name is longer than [`WorkflowName::MAX_LEN`] bytes.
  TooLong {
    /// The length of the rejected name, in bytes.
    len: usize,
  },
  /// The name contains a character other than an ASCII letter, digit, `.`, `_` or `-`.
  InvalidChar {
    /// The first rejected character.
    ch: char,
    /// Its byte index in the name.
    index: usize,
  },
}

impl fmt::Display for WorkflowNameError {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    match self {
      Self::Empty => f.write_str("workflow name is empty"),
      Self::TooLong { len } => write!(
        f,
        "workflow name is {len} bytes, longer than {}",
        WorkflowName::MAX_LEN
      ),
      Self::InvalidChar { ch, index } => {
        write!(
          f,
          "workflow name has {ch:?} at byte {index}; use ASCII letters, digits, '.', '_' or '-'"
        )
      }
    }
  }
}

impl std::error::Error for WorkflowNameError {}
