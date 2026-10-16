//! [`list_todos_handler`]: `GET /todos`.

use axum::Json;
use axum::extract::State;
use axum::response::Response;
use id_effect_axum::execute;

use crate::application::{AppEnv, list_todos};
use crate::interface::http::{IntoApiError, TodoResponse};

/// `200` `[Todo]`, by id.
pub async fn list_todos_handler(state: State<AppEnv>) -> Response {
  execute(state, |_| {
    list_todos()
      .map(|todos| Json(todos.iter().map(TodoResponse::from).collect::<Vec<_>>()))
      .map_err(IntoApiError::into_api_error)
  })
  .await
}
