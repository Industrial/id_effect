//! [`update_todo_handler`]: `PATCH /todos/{id}`.

use axum::Json;
use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Response};
use id_effect_axum::execute;

use crate::application::{AppEnv, update_todo};
use crate::interface::http::decode::{decode_json_body, parse_todo_id};
use crate::interface::http::{IntoApiError, TodoResponse, UpdateTodoRequest};

/// `{"title"?: "..", "completed"?: bool}` -> `200` `Todo`.
pub async fn update_todo_handler(
  state: State<AppEnv>,
  Path(id): Path<String>,
  headers: HeaderMap,
  body: Bytes,
) -> Response {
  let decoded = parse_todo_id(&id).and_then(|id| {
    decode_json_body::<UpdateTodoRequest>(&headers, &body).map(|request| (id, request))
  });
  let (id, request) = match decoded {
    Ok(decoded) => decoded,
    Err(err) => return err.into_response(),
  };
  execute(state, move |_| {
    update_todo(id, request.title, request.completed)
      .map(|todo| Json(TodoResponse::from(todo)))
      .map_err(IntoApiError::into_api_error)
  })
  .await
}
