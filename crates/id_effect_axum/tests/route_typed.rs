//! Typed routes: path / query / header / body extraction through `HasSchema`, and the 422
//! rejection for input that does not decode.

use std::convert::Infallible;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use axum::body::Body;
use axum::extract::DefaultBodyLimit;
use axum::http::{Method, Request, StatusCode};
use http_body_util::BodyExt;
use id_effect::schema::{self, HasSchema, Schema};
use id_effect::{Effect, fail, succeed};
use id_effect_axum::route::{Input, Route, Router};
use serde_json::{Value, json};
use tower::ServiceExt;

struct AccountPath;

impl HasSchema for AccountPath {
  type A = (String, i64);
  type I = (String, i64);
  type E = ();

  fn schema() -> Schema<Self::A, Self::I> {
    schema::struct_("org", schema::string(), "id", schema::i64())
  }
}

struct AccountId;

impl HasSchema for AccountId {
  type A = i64;
  type I = i64;
  type E = ();

  fn schema() -> Schema<i64, i64> {
    schema::i64()
  }
}

struct Paging;

impl HasSchema for Paging {
  type A = (i64, Vec<String>);
  type I = (i64, Vec<String>);
  type E = ();

  fn schema() -> Schema<Self::A, Self::I> {
    schema::struct_(
      "limit",
      schema::i64(),
      "tag",
      schema::array(schema::string()),
    )
  }
}

struct Tenant;

impl HasSchema for Tenant {
  type A = (i64, bool);
  type I = (i64, bool);
  type E = ();

  fn schema() -> Schema<Self::A, Self::I> {
    schema::struct_("X-Tenant", schema::i64(), "x-dry-run", schema::bool_())
  }
}

struct NewAccount;

impl HasSchema for NewAccount {
  type A = (String, i64);
  type I = (String, i64);
  type E = ();

  fn schema() -> Schema<Self::A, Self::I> {
    schema::struct_("name", schema::string(), "age", schema::i64())
  }
}

#[derive(Clone, Default)]
struct App {
  calls: Arc<AtomicUsize>,
}

type Out = Effect<String, Infallible, App>;

fn show(app: &mut App, input: Input<(String, i64), (i64, Vec<String>)>) -> Out {
  app.calls.fetch_add(1, Ordering::SeqCst);
  let Input {
    path: (org, id),
    query: (limit, tags),
    ..
  } = input;
  succeed(format!("{org}/{id} limit={limit} tags={}", tags.join(",")))
}

fn create(
  app: &mut App,
  input: Input<String, (i64, Vec<String>), (), (String, i64)>,
) -> Effect<(StatusCode, String), Infallible, App> {
  app.calls.fetch_add(1, Ordering::SeqCst);
  let (name, age) = input.body;
  succeed((
    StatusCode::CREATED,
    format!("{}:{name}:{age}:{}", input.path, input.query.0),
  ))
}

fn tenant(_: &mut App, input: Input<(), (), (i64, bool)>) -> Out {
  succeed(format!(
    "tenant={} dry={}",
    input.headers.0, input.headers.1
  ))
}

fn by_id(_: &mut App, input: Input<i64>) -> Effect<String, StatusCode, App> {
  if input.path == 404 {
    fail(StatusCode::NOT_FOUND)
  } else {
    succeed(format!("id={}", input.path))
  }
}

fn app(state: App) -> axum::Router {
  Router::new()
    .route(
      Route::get("/orgs/{org}/accounts/{id}")
        .path::<AccountPath>()
        .query::<Paging>()
        .handler(show),
    )
    .route(
      Route::post("/orgs/{org}/accounts")
        .path::<OrgName>()
        .query::<Paging>()
        .body::<NewAccount>()
        .handler(create),
    )
    .route(Route::get("/tenant").headers::<Tenant>().handler(tenant))
    .route(Route::get("/ids/{id}").path::<AccountId>().handler(by_id))
    .with_state(state)
    .into_axum()
}

struct OrgName;

impl HasSchema for OrgName {
  type A = String;
  type I = String;
  type E = ();

  fn schema() -> Schema<String, String> {
    schema::string()
  }
}

async fn send(app: &axum::Router, req: Request<Body>) -> (StatusCode, String) {
  let res = app.clone().oneshot(req).await.unwrap();
  let status = res.status();
  let bytes = res.into_body().collect().await.unwrap().to_bytes();
  (status, String::from_utf8(bytes.to_vec()).unwrap())
}

fn get(uri: &str) -> Request<Body> {
  Request::builder().uri(uri).body(Body::empty()).unwrap()
}

fn post(uri: &str, body: &str) -> Request<Body> {
  Request::builder()
    .method(Method::POST)
    .uri(uri)
    .header("content-type", "application/json")
    .body(Body::from(body.to_owned()))
    .unwrap()
}

fn errors(body: &str) -> Vec<(String, String)> {
  let v: Value = serde_json::from_str(body).expect("json error body");
  v["errors"]
    .as_array()
    .expect("errors array")
    .iter()
    .map(|e| {
      (
        e["in"].as_str().unwrap().to_owned(),
        e["path"].as_str().unwrap().to_owned(),
      )
    })
    .collect()
}

#[tokio::test]
async fn path_and_query_are_decoded_into_the_handler_input() {
  let state = App::default();
  let app = app(state.clone());
  let (status, body) = send(&app, get("/orgs/acme/accounts/42?limit=5&tag=a&tag=b")).await;
  assert_eq!(status, StatusCode::OK, "{body}");
  assert_eq!(body, "acme/42 limit=5 tags=a,b");
  assert_eq!(state.calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn invalid_path_param_is_422_and_skips_the_handler() {
  let state = App::default();
  let app = app(state.clone());
  let (status, body) = send(&app, get("/orgs/acme/accounts/abc?limit=1&tag=a")).await;
  assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
  assert_eq!(errors(&body), vec![("path".into(), "id".into())]);
  assert_eq!(state.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn missing_query_field_is_422() {
  let app = app(App::default());
  let (status, body) = send(&app, get("/orgs/acme/accounts/1?tag=x")).await;
  assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
  assert_eq!(errors(&body), vec![("query".into(), "limit".into())]);
}

#[tokio::test]
async fn typed_body_happy_path() {
  let app = app(App::default());
  let (status, body) = send(
    &app,
    post(
      "/orgs/acme/accounts?limit=2&tag=t",
      r#"{"name":"ada","age":36}"#,
    ),
  )
  .await;
  assert_eq!(status, StatusCode::CREATED, "{body}");
  assert_eq!(body, "acme:ada:36:2");
}

#[tokio::test]
async fn query_and_body_errors_are_reported_together() {
  let state = App::default();
  let app = app(state.clone());
  let (status, body) = send(
    &app,
    post(
      "/orgs/acme/accounts?limit=x&tag=t",
      r#"{"name":1,"age":"old"}"#,
    ),
  )
  .await;
  assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
  assert_eq!(
    errors(&body),
    vec![
      ("query".into(), "limit".into()),
      ("body".into(), "name".into()),
      ("body".into(), "age".into()),
    ]
  );
  assert_eq!(state.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn malformed_json_body_is_400() {
  let app = app(App::default());
  let (status, body) = send(&app, post("/orgs/acme/accounts?limit=1&tag=t", "{not json")).await;
  assert_eq!(status, StatusCode::BAD_REQUEST);
  assert_eq!(errors(&body), vec![("body".into(), String::new())]);
}

#[tokio::test]
async fn body_over_the_limit_keeps_axums_rejection() {
  let app = app(App::default()).layer(DefaultBodyLimit::max(4));
  let (status, _) = send(
    &app,
    post(
      "/orgs/acme/accounts?limit=1&tag=t",
      r#"{"name":"ada","age":36}"#,
    ),
  )
  .await;
  assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
}

#[tokio::test]
async fn headers_are_decoded_case_insensitively() {
  let app = app(App::default());
  let req = Request::builder()
    .uri("/tenant")
    .header("x-tenant", "7")
    .header("X-Dry-Run", "true")
    .body(Body::empty())
    .unwrap();
  let (status, body) = send(&app, req).await;
  assert_eq!(status, StatusCode::OK, "{body}");
  assert_eq!(body, "tenant=7 dry=true");

  let (status, body) = send(&app, get("/tenant")).await;
  assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
  assert_eq!(
    errors(&body),
    vec![
      ("header".into(), "X-Tenant".into()),
      ("header".into(), "x-dry-run".into()),
    ]
  );
}

#[tokio::test]
async fn scalar_path_and_typed_failures_map_like_routing_helpers() {
  let app = app(App::default());
  assert_eq!(
    send(&app, get("/ids/7")).await,
    (StatusCode::OK, "id=7".to_owned())
  );
  assert_eq!(send(&app, get("/ids/404")).await.0, StatusCode::NOT_FOUND);
  let (status, body) = send(&app, get("/ids/seven")).await;
  assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
  let v: Value = serde_json::from_str(&body).unwrap();
  assert_eq!(
    v,
    json!({"errors": [{"in": "path", "path": "", "message": "expected i64"}]})
  );
}

#[tokio::test]
async fn undeclared_parts_are_ignored_and_the_handler_is_a_plain_effect() {
  let app = Router::new()
    .route(
      Route::delete("/things/{id}").handler(|_: &mut (), input: Input| {
        let Input {
          path: (),
          query: (),
          headers: (),
          body: (),
        } = input;
        succeed::<_, Infallible, ()>(StatusCode::NO_CONTENT)
      }),
    )
    .into_axum();
  let req = Request::builder()
    .method(Method::DELETE)
    .uri("/things/x?whatever=1")
    .body(Body::from("ignored"))
    .unwrap();
  assert_eq!(send(&app, req).await.0, StatusCode::NO_CONTENT);
}
