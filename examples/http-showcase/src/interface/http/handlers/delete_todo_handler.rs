//! [`delete_todo_handler`]: `DELETE /todos/{id}`.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use id_effect_axum::execute;

use crate::application::{AppEnv, delete_todo};
use crate::interface::http::IntoApiError;
use crate::interface::http::decode::parse_todo_id;

/// `204`.
pub async fn delete_todo_handler(state: State<AppEnv>, Path(id): Path<String>) -> Response {
  let id = match parse_todo_id(&id) {
    Ok(id) => id,
    Err(err) => return err.into_response(),
  };
  execute(state, move |_| {
    delete_todo(id)
      .map(|()| StatusCode::NO_CONTENT)
      .map_err(IntoApiError::into_api_error)
  })
  .await
}
