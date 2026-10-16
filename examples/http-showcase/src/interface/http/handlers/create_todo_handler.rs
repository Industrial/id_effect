//! [`create_todo_handler`]: `POST /todos`.

use axum::Json;
use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use id_effect_axum::execute;

use crate::application::{AppEnv, create_todo};
use crate::interface::http::decode::decode_json_body;
use crate::interface::http::{CreateTodoRequest, IntoApiError, TodoResponse};

/// `{"title": ".."}` -> `201` `Todo`.
pub async fn create_todo_handler(
  state: State<AppEnv>,
  headers: HeaderMap,
  body: Bytes,
) -> Response {
  let request = match decode_json_body::<CreateTodoRequest>(&headers, &body) {
    Ok(request) => request,
    Err(err) => return err.into_response(),
  };
  execute(state, move |_| {
    create_todo(request.title)
      .map(|todo| (StatusCode::CREATED, Json(TodoResponse::from(todo))))
      .map_err(IntoApiError::into_api_error)
  })
  .await
}
