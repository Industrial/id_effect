//! [`create_todos_handler`]: `POST /todos/batch`.

use axum::Json;
use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use id_effect_axum::execute;

use crate::application::{AppEnv, create_todos};
use crate::interface::http::decode::decode_json_body;
use crate::interface::http::{CreateTodosRequest, IntoApiError, TodoResponse};

/// `{"titles": [".."]}` -> `201` `[Todo]`, all or none.
pub async fn create_todos_handler(
  state: State<AppEnv>,
  headers: HeaderMap,
  body: Bytes,
) -> Response {
  let request = match decode_json_body::<CreateTodosRequest>(&headers, &body) {
    Ok(request) => request,
    Err(err) => return err.into_response(),
  };
  execute(state, move |_| {
    create_todos(request.titles)
      .map(|todos| {
        let todos = todos.iter().map(TodoResponse::from).collect::<Vec<_>>();
        (StatusCode::CREATED, Json(todos))
      })
      .map_err(IntoApiError::into_api_error)
  })
  .await
}
