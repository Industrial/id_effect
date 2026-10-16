//! [`TitleTaken`]: the title uniqueness rule was violated.

use std::fmt;

use id_effect::EffectData;

use super::Title;

/// Another todo already has this title.
#[derive(Clone, Debug, EffectData)]
pub struct TitleTaken(pub Title);

impl fmt::Display for TitleTaken {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    write!(f, "a todo titled {:?} already exists", self.0.as_str())
  }
}

impl std::error::Error for TitleTaken {}
