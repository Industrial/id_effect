//! [`Todo`]: the aggregate root of the todo context.

use id_effect::EffectData;

use super::{Title, TodoChanges, TodoId};

/// A todo: an identity, a unique [`Title`] and a completion state. Its fields change only
/// through its behaviour ([`Self::rename`], [`Self::complete`], [`Self::reopen`],
/// [`Self::apply`]).
#[derive(Clone, Debug, EffectData)]
pub struct Todo {
  id: TodoId,
  title: Title,
  completed: bool,
}

impl Todo {
  /// Rebuilds a stored todo (repositories only: new todos get their id from the store).
  pub fn restore(id: TodoId, title: Title, completed: bool) -> Self {
    Self {
      id,
      title,
      completed,
    }
  }

  /// The identity.
  pub fn id(&self) -> TodoId {
    self.id
  }

  /// The title.
  pub fn title(&self) -> &Title {
    &self.title
  }

  /// Whether it is done.
  pub fn is_completed(&self) -> bool {
    self.completed
  }

  /// Changes the title.
  pub fn rename(&mut self, title: Title) {
    self.title = title;
  }

  /// Marks it done.
  pub fn complete(&mut self) {
    self.completed = true;
  }

  /// Marks it not done.
  pub fn reopen(&mut self) {
    self.completed = false;
  }

  /// This todo with `changes` applied.
  #[must_use]
  pub fn apply(mut self, changes: TodoChanges) -> Self {
    if let Some(title) = changes.title {
      self.rename(title);
    }
    match changes.completed {
      Some(true) => self.complete(),
      Some(false) => self.reopen(),
      None => {}
    }
    self
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  fn milk() -> Todo {
    Todo::restore(TodoId::new(1), Title::parse("milk").unwrap(), false)
  }

  #[test]
  fn behaviour_changes_the_state() {
    let mut todo = milk();
    todo.complete();
    assert!(todo.is_completed());
    todo.reopen();
    assert!(!todo.is_completed());
    todo.rename(Title::parse("eggs").unwrap());
    assert_eq!(todo.title().as_str(), "eggs");
    assert_eq!(todo.id(), TodoId::new(1));
  }

  #[test]
  fn apply_changes_only_the_present_fields() {
    assert_eq!(milk().apply(TodoChanges::default()), milk());
    let done = milk().apply(TodoChanges {
      title: None,
      completed: Some(true),
    });
    assert!(done.is_completed());
    assert_eq!(done.title().as_str(), "milk");
    let renamed = done.apply(TodoChanges {
      title: Some(Title::parse("bread").unwrap()),
      completed: Some(false),
    });
    assert_eq!(renamed.title().as_str(), "bread");
    assert!(!renamed.is_completed());
  }

  #[test]
  fn empty_changes() {
    assert!(TodoChanges::default().is_empty());
    assert!(
      !TodoChanges {
        title: None,
        completed: Some(true)
      }
      .is_empty()
    );
  }
}
