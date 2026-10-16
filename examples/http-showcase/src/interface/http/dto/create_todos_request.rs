//! [`CreateTodosRequest`]: the `POST /todos/batch` body.

use id_effect::Schema;

/// `{"titles": [".."]}`.
#[derive(Clone, Debug, PartialEq, Eq, Schema)]
pub struct CreateTodosRequest {
  /// The titles; stored all together or not at all.
  pub titles: Vec<String>,
}
