//! [`TodoResponse`]: a todo on the wire.

use serde::Serialize;

use crate::domain::todo::Todo;

/// `{"id": 1, "title": "..", "completed": false}`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct TodoResponse {
  /// The identity.
  pub id: i64,
  /// The title.
  pub title: String,
  /// Whether it is done.
  pub completed: bool,
}

impl From<&Todo> for TodoResponse {
  fn from(todo: &Todo) -> Self {
    Self {
      id: todo.id().get(),
      title: todo.title().as_str().to_owned(),
      completed: todo.is_completed(),
    }
  }
}

impl From<Todo> for TodoResponse {
  fn from(todo: Todo) -> Self {
    Self::from(&todo)
  }
}

#[cfg(test)]
mod tests {
  use serde_json::json;

  use super::*;
  use crate::domain::todo::{Title, TodoId};

  #[test]
  fn serializes_the_todo() {
    let todo = Todo::restore(TodoId::new(1), Title::parse("milk").unwrap(), true);
    let body = serde_json::to_value(TodoResponse::from(todo)).unwrap();
    assert_eq!(body, json!({"id": 1, "title": "milk", "completed": true}));
  }
}
