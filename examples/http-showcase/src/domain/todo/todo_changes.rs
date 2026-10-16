//! [`TodoChanges`]: a partial update of a [`Todo`](super::Todo).

use id_effect::EffectData;

use super::Title;

/// The fields to change; absent fields stay as they are.
#[derive(Clone, Debug, Default, EffectData)]
pub struct TodoChanges {
  /// The new title.
  pub title: Option<Title>,
  /// The new completion state.
  pub completed: Option<bool>,
}

impl TodoChanges {
  /// Whether nothing would change.
  pub fn is_empty(&self) -> bool {
    self.title.is_none() && self.completed.is_none()
  }
}
