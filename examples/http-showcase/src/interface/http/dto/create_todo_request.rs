//! [`CreateTodoRequest`]: the `POST /todos` body.

use id_effect::Schema;

/// `{"title": ".."}`.
#[derive(Clone, Debug, PartialEq, Eq, Schema)]
pub struct CreateTodoRequest {
  /// The title (trimmed; 1 to 200 characters).
  pub title: String,
}
