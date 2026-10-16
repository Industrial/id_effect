//! [`get_todo_handler`]: `GET /todos/{id}`.

use axum::Json;
use axum::extract::{Path, State};
use axum::response::{IntoResponse, Response};
use id_effect_axum::execute;

use crate::application::{AppEnv, get_todo};
use crate::interface::http::decode::parse_todo_id;
use crate::interface::http::{IntoApiError, TodoResponse};

/// `200` `Todo`.
pub async fn get_todo_handler(state: State<AppEnv>, Path(id): Path<String>) -> Response {
  let id = match parse_todo_id(&id) {
    Ok(id) => id,
    Err(err) => return err.into_response(),
  };
  execute(state, move |_| {
    get_todo(id)
      .map(|todo| Json(TodoResponse::from(todo)))
      .map_err(IntoApiError::into_api_error)
  })
  .await
}
