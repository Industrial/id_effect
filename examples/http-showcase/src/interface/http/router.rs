//! [`router`]: the service's HTTP routes over an [`AppEnv`].

use axum::Router;
use id_effect_axum::health_router;

use super::todo_routes;
use crate::application::AppEnv;

/// `GET /health` (liveness, always `200 ok`) and the `/todos` routes ([`todo_routes`]) over
/// `env`.
pub fn router(env: AppEnv) -> Router {
  Router::new()
    .merge(health_router())
    .merge(todo_routes().with_state(env))
}

#[cfg(test)]
mod tests {
  use std::time::Duration;

  use axum::body::Body;
  use axum::http::{Method, Request, header};
  use serde_json::{Value, json};
  use tower::ServiceExt;

  use super::*;
  use crate::application::{RequestPolicy, new_app_env};
  use crate::domain::todo::{TodoRepository, TodoRepositoryService};
  use crate::infrastructure::doubles::{FlakyTodoRepository, SlowTodoRepository};
  use crate::infrastructure::memory::InMemoryTodoRepository;

  fn policy(timeout: Duration) -> RequestPolicy {
    RequestPolicy::new(
      timeout,
      RequestPolicy::DEFAULT_RETRIES,
      Duration::from_millis(1),
    )
  }

  fn app_with(repo: impl TodoRepository, policy: RequestPolicy) -> Router {
    router(new_app_env(TodoRepositoryService::new(repo), policy))
  }

  fn app() -> Router {
    app_with(
      InMemoryTodoRepository::new(),
      policy(Duration::from_secs(5)),
    )
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
  async fn health_is_200() {
    let (status, _) = call(&app(), Method::GET, "/health", None).await;
    assert_eq!(status, 200);
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
  async fn ids_and_bodies_decode_to_422_with_a_message() {
    let app = app();
    let (status, json) = call(&app, Method::GET, "/todos/abc", None).await;
    assert_eq!(status, 422);
    assert_eq!(
      json["message"],
      json!("the todo id must be an integer, not \"abc\"")
    );
    let (status, json) = call(&app, Method::POST, "/todos", Some(r#"{"title":1}"#)).await;
    assert_eq!((status, &json["error"]), (422, &json!("invalid")));
    assert!(
      json["message"].as_str().unwrap().contains("`title`"),
      "{json}"
    );
    let (status, json) = call(&app, Method::PATCH, "/todos/1", Some(r#"{"done":true}"#)).await;
    assert_eq!((status, &json["error"]), (422, &json!("invalid")));
    assert!(
      json["message"].as_str().unwrap().contains("`done`"),
      "{json}"
    );
  }

  #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
  async fn a_repo_failing_twice_then_succeeding_answers_200_after_3_calls() {
    let repo = FlakyTodoRepository::new(InMemoryTodoRepository::new(), 2);
    let app = app_with(repo.clone(), policy(Duration::from_secs(5)));
    assert_eq!(
      call(&app, Method::GET, "/todos", None).await,
      (200, json!([]))
    );
    assert_eq!(repo.calls(), 3);
  }

  #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
  async fn a_repo_that_stays_down_answers_503() {
    let repo = FlakyTodoRepository::new(InMemoryTodoRepository::new(), u32::MAX);
    let app = app_with(repo.clone(), policy(Duration::from_secs(5)));
    let (status, json) = call(&app, Method::GET, "/todos", None).await;
    assert_eq!((status, &json["error"]), (503, &json!("unavailable")));
    assert_eq!(repo.calls(), 1 + RequestPolicy::DEFAULT_RETRIES as u32);
  }

  #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
  async fn an_open_circuit_breaker_answers_503_without_calling_the_store() {
    let repo = FlakyTodoRepository::new(InMemoryTodoRepository::new(), u32::MAX);
    let app = app_with(repo.clone(), policy(Duration::from_secs(5)));
    // Each request fails up to `1 + DEFAULT_RETRIES` attempts until the breaker opens.
    while repo.calls() < RequestPolicy::BREAKER_THRESHOLD {
      let (status, _) = call(&app, Method::GET, "/todos", None).await;
      assert_eq!(status, 503);
    }
    let calls = repo.calls();
    let (status, json) = call(&app, Method::GET, "/todos", None).await;
    assert_eq!((status, &json["error"]), (503, &json!("unavailable")));
    assert!(
      json["message"]
        .as_str()
        .unwrap()
        .contains("circuit breaker"),
      "{json}"
    );
    assert_eq!(
      repo.calls(),
      calls,
      "an open breaker does not call the store"
    );
  }

  #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
  async fn a_slow_repo_answers_504() {
    let repo = SlowTodoRepository::new(InMemoryTodoRepository::new(), Duration::from_secs(30));
    let app = app_with(repo, policy(Duration::from_millis(50)));
    let started = std::time::Instant::now();
    let (status, json) = call(&app, Method::GET, "/todos", None).await;
    assert_eq!((status, &json["error"]), (504, &json!("timeout")));
    assert!(started.elapsed() < Duration::from_secs(5));
  }
}
