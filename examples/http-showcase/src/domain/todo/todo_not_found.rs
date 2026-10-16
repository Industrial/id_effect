//! [`TodoNotFound`]: no todo has the requested id.

use std::fmt;

use id_effect::EffectData;

use super::TodoId;

/// No todo has this id.
#[derive(Clone, Debug, EffectData)]
pub struct TodoNotFound(pub TodoId);

impl fmt::Display for TodoNotFound {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    write!(f, "no todo with id {}", self.0)
  }
}

impl std::error::Error for TodoNotFound {}
