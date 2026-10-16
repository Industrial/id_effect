//! The in-process HTTP test client (feature `http`) against a small axum router: no socket, the
//! whole router (routes, extractors, fallback) answering `TestClient` requests.

#![cfg(feature = "http")]

use std::convert::Infallible;
use std::future::Ready;
use std::task::{Context, Poll};

use axum::Json;
use axum::extract::Path;
use axum::http::HeaderMap as AxumHeaders;
use axum::response::IntoResponse;
use axum::routing::{delete, get, patch, post, put};
use id_effect::{Cause, Effect, Exit};
use id_effect_test::http::{
  HttpTestError, Method, Problem, Request, Response, StatusCode, TestBody, TestClient,
  TestResponse, header,
};
use id_effect_tokio::TokioRuntime;
use serde_json::{Value, json};

fn problem(status: StatusCode, title: &str) -> axum::response::Response {
  let body = json!({"type": "https://example.test/problems/invalid", "title": title, "status": status.as_u16(), "errors": ["name"]});
  (
    status,
    [(axum::http::header::CONTENT_TYPE, "application/problem+json")],
    body.to_string(),
  )
    .into_response()
}

fn app() -> axum::Router {
  axum::Router::new()
    .route("/health", get(|| async { "ok" }))
    .route(
      "/accounts",
      post(|Json(body): Json<Value>| async move {
        match body.get("name").and_then(Value::as_str) {
          Some(name) if !name.is_empty() => {
            (StatusCode::CREATED, Json(json!({"id": 1, "name": name}))).into_response()
          }
          _ => problem(StatusCode::UNPROCESSABLE_ENTITY, "Invalid account"),
        }
      }),
    )
    .route(
      "/accounts/{id}",
      put(|Path(id): Path<i64>, Json(body): Json<Value>| async move {
        Json(json!({"id": id, "replaced": body}))
      })
      .patch(|Path(id): Path<i64>, Json(body): Json<Value>| async move {
        Json(json!({"id": id, "patched": body}))
      })
      .delete(|Path(id): Path<i64>| async move { (StatusCode::NO_CONTENT, format!("{id}")) }),
    )
    .route(
      "/whoami",
      get(|headers: AxumHeaders| async move {
        let get = |name: &str| {
          headers
            .get(name)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("-")
            .to_owned()
        };
        (
          [("x-seen-by", "router")],
          format!("{} {}", get("authorization"), get("x-tenant")),
        )
      }),
    )
    .route(
      "/text",
      post(|body: String| async move { body.to_uppercase() }),
    )
    .route("/patch-only", patch(|| async { "patched" }))
    .route("/delete-only", delete(|| async { "deleted" }))
    .route("/post-only", post(|| async { "posted" }))
}

fn send(request: id_effect_test::TestRequest<axum::Router>) -> TestResponse {
  match request.send().block_on() {
    Exit::Success(response) => response,
    other => panic!("request failed: {other:?}"),
  }
}

#[test]
fn get_reads_status_headers_and_text() {
  let client = TestClient::new(app());
  let health = send(client.get("/health"));
  health.assert_status(StatusCode::OK);
  assert_eq!(health.text(), "ok");
  assert_eq!(health.bytes().as_ref(), b"ok");
  assert!(
    health
      .header("content-type")
      .unwrap()
      .starts_with("text/plain")
  );
  assert!(health.headers().contains_key(header::CONTENT_LENGTH));
  assert_eq!(send(client.get("/missing")).status(), StatusCode::NOT_FOUND);
}

#[test]
fn json_bodies_go_in_and_come_back_out() {
  let client = TestClient::new(app());
  let created = send(client.post_json("/accounts", &json!({"name": "alice"})));
  created.assert_status(StatusCode::CREATED);
  assert_eq!(
    created.json::<Value>().unwrap(),
    json!({"id": 1, "name": "alice"})
  );
  let replaced = send(client.put_json("/accounts/7", &json!({"name": "bob"})));
  assert_eq!(
    replaced.json::<Value>().unwrap(),
    json!({"id": 7, "replaced": {"name": "bob"}})
  );
  let patched = send(client.patch_json("/accounts/7", &json!({"name": "carol"})));
  assert_eq!(patched.json::<Value>().unwrap()["patched"]["name"], "carol");
  let deleted = send(client.delete("/accounts/7"));
  assert_eq!(
    (deleted.status(), deleted.text()),
    (StatusCode::NO_CONTENT, "7".to_owned())
  );
  // A body that is not the JSON asked for.
  let err = send(client.get("/health")).json::<Value>().unwrap_err();
  assert!(
    matches!(&err, HttpTestError::Body(m) if m.contains("ok")),
    "{err}"
  );
}

#[test]
fn problems_are_parsed_and_only_from_problem_responses() {
  let client = TestClient::new(app());
  let invalid = send(client.post_json("/accounts", &json!({"name": ""})));
  invalid.assert_status(StatusCode::UNPROCESSABLE_ENTITY);
  let problem: Problem = invalid.problem().expect("a problem");
  assert_eq!(problem.type_uri, "https://example.test/problems/invalid");
  assert_eq!(problem.title.as_deref(), Some("Invalid account"));
  assert_eq!(problem.status, Some(422));
  assert_eq!(problem.extensions["errors"], json!(["name"]));
  assert_eq!((problem.detail, problem.instance), (None, None));
  let not_a_problem = send(client.get("/health")).problem().unwrap_err();
  assert!(matches!(&not_a_problem, HttpTestError::Body(m) if m.contains("not a problem")));
}

#[test]
fn headers_default_and_per_request() {
  let client = TestClient::new(app()).default_header("x-tenant", "acme");
  let me = send(client.get("/whoami").bearer("t0k3n"));
  assert_eq!(me.text(), "Bearer t0k3n acme");
  assert_eq!(me.header("x-seen-by"), Some("router"));
  let other = send(client.get("/whoami").header("x-tenant", "globex"));
  assert_eq!(other.text(), "- globex");
  assert!(format!("{client:?}").contains("x-tenant"));
  assert!(format!("{:?}", client.get("/whoami")).contains("/whoami"));
  let _router: &axum::Router = client.service();
}

#[test]
fn raw_bodies_and_every_method() {
  let client = TestClient::new(app());
  let upper = send(client.post("/text").body("shout", "text/plain"));
  assert_eq!(upper.text(), "SHOUT");
  assert_eq!(send(client.patch("/patch-only")).text(), "patched");
  assert_eq!(send(client.delete("/delete-only")).text(), "deleted");
  assert_eq!(
    send(client.request(Method::POST, "/post-only")).text(),
    "posted"
  );
  assert_eq!(
    send(client.put("/post-only")).status(),
    StatusCode::METHOD_NOT_ALLOWED
  );
}

#[test]
fn invalid_requests_fail_when_sent() {
  let client = TestClient::new(app());
  let fails = |request: id_effect_test::TestRequest<axum::Router>| match request.send().block_on() {
    Exit::Failure(Cause::Fail(HttpTestError::Request(message))) => message,
    other => panic!("expected a request error, got {other:?}"),
  };
  assert!(fails(client.get("/health").header("bad name", "v")).contains("header name"));
  assert!(fails(client.get("/health").header("x-ok", "bad\nvalue")).contains("value"));
  assert!(fails(client.get("not a uri")).contains("not a uri"));
  let unserializable = std::collections::HashMap::from([((1, 2), 3)]);
  assert!(fails(client.post_json("/accounts", &unserializable)).contains("JSON"));
}

#[test]
#[should_panic(expected = "unexpected status")]
fn assert_status_shows_the_body() {
  send(TestClient::new(app()).get("/missing")).assert_status(StatusCode::OK);
}

/// A service that fails instead of answering.
#[derive(Clone)]
struct Broken;

impl tower::Service<Request<TestBody>> for Broken {
  type Response = Response<TestBody>;
  type Error = String;
  type Future = Ready<Result<Self::Response, String>>;

  fn poll_ready(&mut self, _: &mut Context<'_>) -> Poll<Result<(), String>> {
    Poll::Ready(Ok(()))
  }

  fn call(&mut self, _: Request<TestBody>) -> Self::Future {
    std::future::ready(Err("connection refused".to_owned()))
  }
}

/// A service whose body fails to read.
#[derive(Clone)]
struct BadBody;

struct FailingBody;

impl http_body::Body for FailingBody {
  type Data = bytes::Bytes;
  type Error = String;

  fn poll_frame(
    self: std::pin::Pin<&mut Self>,
    _: &mut Context<'_>,
  ) -> Poll<Option<Result<http_body::Frame<bytes::Bytes>, String>>> {
    Poll::Ready(Some(Err("reset".to_owned())))
  }
}

impl tower::Service<Request<TestBody>> for BadBody {
  type Response = Response<FailingBody>;
  type Error = Infallible;
  type Future = Ready<Result<Self::Response, Infallible>>;

  fn poll_ready(&mut self, _: &mut Context<'_>) -> Poll<Result<(), Infallible>> {
    Poll::Ready(Ok(()))
  }

  fn call(&mut self, _: Request<TestBody>) -> Self::Future {
    std::future::ready(Ok(Response::new(FailingBody)))
  }
}

#[test]
fn service_and_body_failures_are_typed() {
  let broken = TestClient::new(Broken).get("/").send().block_on();
  assert!(
    matches!(broken, Exit::Failure(Cause::Fail(HttpTestError::Service(m))) if m == "connection refused")
  );
  let bad_body = TestClient::new(BadBody).get("/").send().block_on();
  assert!(matches!(bad_body, Exit::Failure(Cause::Fail(HttpTestError::Body(m))) if m == "reset"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn requests_compose_as_effects_on_tokio() {
  let client = TestClient::new(app());
  let both: Effect<(StatusCode, Value), HttpTestError> = client
    .post_json("/accounts", &json!({"name": "dave"}))
    .send()
    .flat_map(move |created| {
      let id = created.json::<Value>().map(|v| v["id"].clone());
      client
        .get("/health")
        .send()
        .map(move |health| (health.status(), id))
    })
    .flat_map(|(status, id)| match id {
      Ok(id) => id_effect::succeed((status, id)),
      Err(e) => id_effect::fail(e),
    });
  let exit = TokioRuntime::current().expect("tokio").run(both).await;
  assert!(
    matches!(exit, Exit::Success((StatusCode::OK, ref id)) if *id == json!(1)),
    "{exit:?}"
  );
}
