//! [`EmptyBatch`]: a batch create without titles.

use std::fmt;

use id_effect::EffectData;

/// `POST /todos/batch` was given no titles.
#[derive(Clone, Copy, Debug, EffectData)]
pub struct EmptyBatch;

impl fmt::Display for EmptyBatch {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.write_str("the batch must not be empty")
  }
}

impl std::error::Error for EmptyBatch {}
