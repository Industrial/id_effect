//! [`UpdateTodoRequest`]: the `PATCH /todos/{id}` body.

use id_effect::Schema;

/// `{"title"?: "..", "completed"?: bool}`: the fields to change (absent or `null`: unchanged).
#[derive(Clone, Debug, Default, PartialEq, Eq, Schema)]
pub struct UpdateTodoRequest {
  /// The new title.
  pub title: Option<String>,
  /// The new completion state.
  pub completed: Option<bool>,
}
