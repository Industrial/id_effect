//! The `/todos` HTTP routes: decode the request (path id, JSON body), run the [`crate::app`]
//! use case as one request fiber ([`id_effect_axum::execute`]) and answer its outcome
//! ([`AppError`] for failures).
//!
//! | Method and path | Body | Success |
//! |---|---|---|
//! | `GET /todos` | | `200` `[Todo]` |
//! | `POST /todos` | `{"title": ".."}` | `201` `Todo` |
//! | `POST /todos/batch` | `{"titles": [".."]}` | `201` `[Todo]`, all or none |
//! | `GET /todos/{id}` | | `200` `Todo` |
//! | `PATCH /todos/{id}` | `{"title"?: "..", "completed"?: bool}` | `200` `Todo` |
//! | `DELETE /todos/{id}` | | `204` |
//!
//! A `Todo` is `{"id": 1, "title": "..", "completed": false}`. A malformed id or body is `422`.

use axum::extract::rejection::JsonRejection;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::Response;
use axum::routing::{get, post};
use axum::{Json, Router};
use id_effect::{Effect, fail};
use id_effect_axum::execute;
use serde::Deserialize;

use crate::app::{self, AppEnv};
use crate::error::AppError;

/// `POST /todos` body.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreateTodo {
  /// The title (trimmed; 1 to 200 characters).
  pub title: String,
}

/// `POST /todos/batch` body.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreateTodos {
  /// The titles; stored all together or not at all.
  pub titles: Vec<String>,
}

/// `PATCH /todos/{id}` body: the fields to change.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UpdateTodo {
  /// The new title.
  pub title: Option<String>,
  /// The new completion state.
  pub completed: Option<bool>,
}

/// The `/todos` routes over the [`AppEnv`] state.
pub fn todo_routes() -> Router<AppEnv> {
  Router::new()
    .route("/todos", get(list).post(create))
    .route("/todos/batch", post(create_batch))
    .route("/todos/{id}", get(show).patch(update).delete(remove))
}

/// A path id: a decimal `i64` (`422` otherwise).
fn parse_id(raw: &str) -> Result<i64, AppError> {
  raw
    .parse()
    .map_err(|_| AppError::Invalid(format!("the todo id must be an integer, not {raw:?}")))
}

/// A JSON body (`422` for a missing or wrong content type, bad JSON or a bad shape).
fn body<T>(body: Result<Json<T>, JsonRejection>) -> Result<T, AppError> {
  body
    .map(|Json(t)| t)
    .map_err(|rejection| AppError::Invalid(rejection.body_text()))
}

/// Runs `build(input)` as the request's effect, or fails it with the decode error.
async fn respond<T, A, F>(state: State<AppEnv>, input: Result<T, AppError>, build: F) -> Response
where
  T: Send + 'static,
  A: axum::response::IntoResponse + Send + 'static,
  F: FnOnce(T) -> Effect<A, AppError, AppEnv> + Send + 'static,
{
  execute(state, move |_| match input {
    Ok(input) => build(input),
    Err(e) => fail(e),
  })
  .await
}

async fn list(state: State<AppEnv>) -> Response {
  execute(state, |_| app::list_todos().map(Json)).await
}

async fn create(state: State<AppEnv>, input: Result<Json<CreateTodo>, JsonRejection>) -> Response {
  respond(state, body(input), |input: CreateTodo| {
    app::create_todo(&input.title).map(|todo| (StatusCode::CREATED, Json(todo)))
  })
  .await
}

async fn create_batch(
  state: State<AppEnv>,
  input: Result<Json<CreateTodos>, JsonRejection>,
) -> Response {
  respond(state, body(input), |input: CreateTodos| {
    app::create_todos(&input.titles).map(|todos| (StatusCode::CREATED, Json(todos)))
  })
  .await
}

async fn show(state: State<AppEnv>, Path(id): Path<String>) -> Response {
  respond(state, parse_id(&id), |id| app::get_todo(id).map(Json)).await
}

async fn update(
  state: State<AppEnv>,
  Path(id): Path<String>,
  input: Result<Json<UpdateTodo>, JsonRejection>,
) -> Response {
  let input = parse_id(&id).and_then(|id| body(input).map(|patch| (id, patch)));
  respond(state, input, |(id, patch): (i64, UpdateTodo)| {
    app::update_todo(id, patch.title.as_deref(), patch.completed).map(Json)
  })
  .await
}

async fn remove(state: State<AppEnv>, Path(id): Path<String>) -> Response {
  respond(state, parse_id(&id), |id| {
    app::delete_todo(id).map(|()| StatusCode::NO_CONTENT)
  })
  .await
}

#[cfg(test)]
mod tests {
  use std::time::Duration;

  use axum::body::Body;
  use axum::http::{Method, Request, header};
  use serde_json::{Value, json};
  use tower::ServiceExt;

  use super::*;
  use crate::app::{DEFAULT_RETRIES, RequestPolicy, app_env};
  use crate::repo::doubles::{FlakyRepo, SlowRepo};
  use crate::repo::{InMemoryTodoRepo, TodoRepo, TodoRepoService};

  fn policy(timeout: Duration) -> RequestPolicy {
    RequestPolicy::new(timeout, DEFAULT_RETRIES, Duration::from_millis(1))
  }

  fn app_with(repo: impl TodoRepo, policy: RequestPolicy) -> Router {
    crate::router(app_env(TodoRepoService::new(repo), policy))
  }

  fn app() -> Router {
    app_with(InMemoryTodoRepo::new(), policy(Duration::from_secs(5)))
  }

  /// Sends one request; returns the status and the JSON body (`Null` when empty).
  async fn call(app: &Router, method: Method, uri: &str, body: Option<&str>) -> (u16, Value) {
    let mut req = Request::builder().method(method).uri(uri);
    if body.is_some() {
      req = req.header(header::CONTENT_TYPE, "application/json");
    }
    let req = req
      .body(body.map_or_else(Body::empty, |b| Body::from(b.to_owned())))
      .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    let status = res.status().as_u16();
    let bytes = axum::body::to_bytes(res.into_body(), usize::MAX)
      .await
      .unwrap();
    let json = if bytes.is_empty() {
      Value::Null
    } else {
      serde_json::from_slice(&bytes).unwrap_or_else(|_| Value::String(format!("{bytes:?}")))
    };
    (status, json)
  }

  #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
  async fn todos_crud() {
    let app = app();
    assert_eq!(
      call(&app, Method::GET, "/todos", None).await,
      (200, json!([]))
    );
    let (status, milk) = call(&app, Method::POST, "/todos", Some(r#"{"title":" milk "}"#)).await;
    assert_eq!(status, 201);
    assert_eq!(milk, json!({"id": 1, "title": "milk", "completed": false}));
    assert_eq!(
      call(&app, Method::GET, "/todos/1", None).await,
      (200, milk.clone())
    );
    let (status, done) = call(
      &app,
      Method::PATCH,
      "/todos/1",
      Some(r#"{"completed":true}"#),
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(done, json!({"id": 1, "title": "milk", "completed": true}));
    let (status, batch) = call(
      &app,
      Method::POST,
      "/todos/batch",
      Some(r#"{"titles":["eggs","bread"]}"#),
    )
    .await;
    assert_eq!(status, 201);
    assert_eq!(batch.as_array().map(Vec::len), Some(2));
    let (status, all) = call(&app, Method::GET, "/todos", None).await;
    assert_eq!(status, 200);
    assert_eq!(all.as_array().map(Vec::len), Some(3));
    assert_eq!(
      call(&app, Method::DELETE, "/todos/1", None).await,
      (204, Value::Null)
    );
    let (status, gone) = call(&app, Method::GET, "/todos/1", None).await;
    assert_eq!((status, &gone["error"]), (404, &json!("not_found")));
  }

  #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
  async fn errors_map_to_404_409_and_422() {
    let app = app();
    call(&app, Method::POST, "/todos", Some(r#"{"title":"a"}"#)).await;
    call(&app, Method::POST, "/todos", Some(r#"{"title":"b"}"#)).await;
    let cases: [(Method, &str, Option<&str>, u16, &str); 12] = [
      (Method::GET, "/todos/99", None, 404, "not_found"),
      (Method::DELETE, "/todos/99", None, 404, "not_found"),
      (Method::PATCH, "/todos/99", Some("{}"), 404, "not_found"),
      (
        Method::POST,
        "/todos",
        Some(r#"{"title":"a"}"#),
        409,
        "conflict",
      ),
      (
        Method::PATCH,
        "/todos/2",
        Some(r#"{"title":"a"}"#),
        409,
        "conflict",
      ),
      (
        Method::POST,
        "/todos/batch",
        Some(r#"{"titles":["c","a"]}"#),
        409,
        "conflict",
      ),
      (Method::GET, "/todos/abc", None, 422, "invalid"),
      (
        Method::POST,
        "/todos",
        Some(r#"{"title":"  "}"#),
        422,
        "invalid",
      ),
      (Method::POST, "/todos", Some("{not json"), 422, "invalid"),
      (
        Method::POST,
        "/todos",
        Some(r#"{"name":"x"}"#),
        422,
        "invalid",
      ),
      (Method::POST, "/todos", None, 422, "invalid"),
      (
        Method::POST,
        "/todos/batch",
        Some(r#"{"titles":[]}"#),
        422,
        "invalid",
      ),
    ];
    for (method, uri, body, status, code) in cases {
      let (got, json) = call(&app, method.clone(), uri, body).await;
      assert_eq!(
        (got, &json["error"]),
        (status, &json!(code)),
        "{method} {uri} {body:?}: {json}"
      );
    }
    // The conflicting batch stored nothing.
    let (_, all) = call(&app, Method::GET, "/todos", None).await;
    assert_eq!(all.as_array().map(Vec::len), Some(2), "{all}");
  }

  #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
  async fn a_repo_failing_twice_then_succeeding_answers_200_after_3_calls() {
    let repo = FlakyRepo::new(InMemoryTodoRepo::new(), 2);
    let app = app_with(repo.clone(), policy(Duration::from_secs(5)));
    assert_eq!(
      call(&app, Method::GET, "/todos", None).await,
      (200, json!([]))
    );
    assert_eq!(repo.calls(), 3);
  }

  #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
  async fn a_repo_that_stays_down_answers_503() {
    let repo = FlakyRepo::new(InMemoryTodoRepo::new(), u32::MAX);
    let app = app_with(repo.clone(), policy(Duration::from_secs(5)));
    let (status, json) = call(&app, Method::GET, "/todos", None).await;
    assert_eq!((status, &json["error"]), (503, &json!("unavailable")));
    assert_eq!(repo.calls(), 1 + DEFAULT_RETRIES as u32);
  }

  #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
  async fn a_slow_repo_answers_504() {
    let repo = SlowRepo::new(InMemoryTodoRepo::new(), Duration::from_secs(30));
    let app = app_with(repo, policy(Duration::from_millis(50)));
    let started = std::time::Instant::now();
    let (status, json) = call(&app, Method::GET, "/todos", None).await;
    assert_eq!((status, &json["error"]), (504, &json!("timeout")));
    assert!(started.elapsed() < Duration::from_secs(5));
  }

  #[test]
  fn ids_and_bodies_decode() {
    assert_eq!(parse_id("42"), Ok(42));
    assert!(matches!(parse_id("4.2"), Err(AppError::Invalid(_))));
    let patch: UpdateTodo = serde_json::from_str(r#"{"title":"x"}"#).unwrap();
    assert_eq!((patch.title.as_deref(), patch.completed), (Some("x"), None));
  }
}
