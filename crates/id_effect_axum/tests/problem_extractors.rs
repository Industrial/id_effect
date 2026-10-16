//! Schema-validated extraction answering RFC 9457 problems (web.validation #2 #3 #4 #5 #7): the
//! standalone `Valid*` extractors on plain Axum handlers and typed routes share one decoder and one
//! `application/problem+json` body with a JSON Pointer per issue.

use std::convert::Infallible;

use axum::body::Body;
use axum::extract::DefaultBodyLimit;
use axum::http::{Method, Request, StatusCode};
use axum::routing::{get, post};
use http_body_util::BodyExt;
use id_effect::schema::{self, HasSchema, Schema};
use id_effect::{Effect, succeed};
use id_effect_axum::PROBLEM_JSON;
use id_effect_axum::route::{
  BodyKind, Input, Problem, Route, Router, ValidForm, ValidHeaders, ValidJson, ValidPath,
  ValidQuery,
};
use serde_json::Value;
use tower::ServiceExt;

struct AccountId;

impl HasSchema for AccountId {
  type A = i64;
  type I = i64;
  type E = ();

  fn schema() -> Schema<i64, i64> {
    schema::i64()
  }
}

struct OrgPath;

impl HasSchema for OrgPath {
  type A = (String, i64);
  type I = (String, i64);
  type E = ();

  fn schema() -> Schema<Self::A, Self::I> {
    schema::struct_("org", schema::string(), "id", schema::i64())
  }
}

struct Paging;

impl HasSchema for Paging {
  type A = (i64, Vec<String>, Option<String>);
  type I = (i64, Vec<String>, Option<String>);
  type E = ();

  fn schema() -> Schema<Self::A, Self::I> {
    schema::struct3(
      "limit",
      schema::i64(),
      "tag",
      schema::array(schema::string()),
      "cursor",
      schema::optional(schema::string()),
    )
  }
}

struct Tenant;

impl HasSchema for Tenant {
  type A = (i64, String);
  type I = (i64, String);
  type E = ();

  fn schema() -> Schema<Self::A, Self::I> {
    schema::struct_("X-Tenant", schema::i64(), "X-Request-Id", schema::string())
  }
}

struct Signup;

impl HasSchema for Signup {
  type A = (String, i64);
  type I = (String, i64);
  type E = ();

  fn schema() -> Schema<Self::A, Self::I> {
    schema::struct_("name", schema::string(), "age", schema::i64())
  }
}

struct Order;

impl HasSchema for Order {
  type A = (String, Vec<(String, i64)>);
  type I = (String, Vec<(String, i64)>);
  type E = ();

  fn schema() -> Schema<Self::A, Self::I> {
    schema::struct_(
      "customer",
      schema::string(),
      "lines",
      schema::array(schema::struct_(
        "sku",
        schema::string(),
        "qty",
        schema::i64(),
      )),
    )
  }
}

/// Status, content type and body of a response.
async fn send(app: &axum::Router, req: Request<Body>) -> (StatusCode, String, Value) {
  let res = app.clone().oneshot(req).await.unwrap();
  let status = res.status();
  let content_type = res
    .headers()
    .get("content-type")
    .map(|v| v.to_str().unwrap().to_owned())
    .unwrap_or_default();
  let bytes = res.into_body().collect().await.unwrap().to_bytes();
  let body = serde_json::from_slice(&bytes)
    .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&bytes).into_owned()));
  (status, content_type, body)
}

/// The problem body, checked for its content type and standard members.
fn problem(status: StatusCode, content_type: &str, body: Value) -> Problem {
  assert_eq!(content_type, PROBLEM_JSON, "{body}");
  let problem: Problem = serde_json::from_value(body).expect("problem body");
  assert_eq!(problem.status, status);
  assert_eq!(problem.type_uri, "about:blank");
  assert_eq!(problem.title, status.canonical_reason().unwrap());
  problem
}

/// `(in, pointer)` of every issue.
fn issues(problem: &Problem) -> Vec<(String, String)> {
  problem
    .errors
    .iter()
    .map(|e| {
      let location = serde_json::to_value(e.location).unwrap();
      (location.as_str().unwrap().to_owned(), e.pointer.clone())
    })
    .collect()
}

fn pairs(items: &[(&str, &str)]) -> Vec<(String, String)> {
  items
    .iter()
    .map(|(a, b)| ((*a).to_owned(), (*b).to_owned()))
    .collect()
}

fn get_req(uri: &str) -> Request<Body> {
  Request::builder().uri(uri).body(Body::empty()).unwrap()
}

fn post_req(uri: &str, content_type: &str, body: &str) -> Request<Body> {
  Request::builder()
    .method(Method::POST)
    .uri(uri)
    .header("content-type", content_type)
    .body(Body::from(body.to_owned()))
    .unwrap()
}

const BOUNDARY: &str = "XyZ";

fn multipart(fields: &[(&str, Option<&str>, &str)]) -> String {
  let mut body = String::new();
  for (name, file, value) in fields {
    body.push_str(&format!("--{BOUNDARY}\r\n"));
    match file {
      Some(file) => body.push_str(&format!(
        "Content-Disposition: form-data; name=\"{name}\"; filename=\"{file}\"\r\nContent-Type: \
         application/octet-stream\r\n\r\n"
      )),
      None => body.push_str(&format!(
        "Content-Disposition: form-data; name=\"{name}\"\r\n\r\n"
      )),
    }
    body.push_str(value);
    body.push_str("\r\n");
  }
  body.push_str(&format!("--{BOUNDARY}--\r\n"));
  body
}

fn multipart_type() -> String {
  format!("multipart/form-data; boundary={BOUNDARY}")
}

// --- standalone extractors on plain Axum handlers -------------------------------------------

fn plain_app() -> axum::Router {
  axum::Router::new()
    .route(
      "/accounts/{id}",
      get(|ValidPath(id): ValidPath<AccountId>| async move { format!("id={id}") }),
    )
    .route(
      "/orgs/{org}/accounts/{id}",
      get(|path: ValidPath<OrgPath>| async move {
        let (org, id) = path.into_inner();
        format!("{org}/{id}")
      }),
    )
    .route(
      "/accounts",
      get(|query: ValidQuery<Paging>| async move {
        let (limit, tags, cursor) = &*query;
        format!("limit={limit} tags={} cursor={cursor:?}", tags.join(","))
      })
      .post(|ValidJson((name, age)): ValidJson<Signup>| async move { format!("{name}:{age}") }),
    )
    .route(
      "/tenant",
      get(
        |ValidHeaders((tenant, id)): ValidHeaders<Tenant>| async move { format!("{tenant}:{id}") },
      ),
    )
    .route(
      "/signup",
      post(|ValidForm((name, age)): ValidForm<Signup>| async move { format!("{name}:{age}") }),
    )
    .route(
      "/orders",
      post(|ValidJson(order): ValidJson<Order>| async move { format!("{}", order.1.len()) }),
    )
}

#[tokio::test]
async fn path_param_violation_is_422_problem() {
  let app = plain_app();
  let (status, _, body) = send(&app, get_req("/accounts/42")).await;
  assert_eq!((status, body), (StatusCode::OK, Value::from("id=42")));
  let (status, _, body) = send(&app, get_req("/orgs/acme/accounts/7")).await;
  assert_eq!((status, body), (StatusCode::OK, Value::from("acme/7")));

  let (status, ct, body) = send(&app, get_req("/accounts/forty-two")).await;
  let p = problem(StatusCode::UNPROCESSABLE_ENTITY, &ct, body);
  assert_eq!(issues(&p), pairs(&[("path", "")]));
  assert_eq!(p.instance.as_deref(), Some("/accounts/forty-two"));
  assert_eq!(p.errors[0].message, "expected i64");

  let (status2, ct, body) = send(&app, get_req("/orgs/acme/accounts/x")).await;
  let p = problem(StatusCode::UNPROCESSABLE_ENTITY, &ct, body);
  assert_eq!(issues(&p), pairs(&[("path", "/id")]));
  assert_eq!(status, status2);

  // Axum's own path rejection (invalid UTF-8) passes through, not as a problem.
  let (status, ct, _) = send(&app, get_req("/accounts/%FF")).await;
  assert_eq!(status, StatusCode::BAD_REQUEST);
  assert_ne!(ct, PROBLEM_JSON);

  // On a typed route as well.
  let typed: axum::Router = Router::new()
    .route(Route::get("/accounts/{id}").path::<AccountId>().handler(
      |_: &mut (), input: Input<i64>| succeed::<_, Infallible, ()>(format!("id={}", input.path)),
    ))
    .into_axum();
  let (status, ct, body) = send(&typed, get_req("/accounts/1.5")).await;
  let p = problem(StatusCode::UNPROCESSABLE_ENTITY, &ct, body);
  assert_eq!(issues(&p), pairs(&[("path", "")]));
  assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
  let (status, ct, _) = send(&typed, get_req("/accounts/%FF")).await;
  assert_eq!(status, StatusCode::BAD_REQUEST);
  assert_ne!(ct, PROBLEM_JSON);
}

#[tokio::test]
async fn query_is_validated_with_defaults_for_absent_keys() {
  let app = plain_app();
  let (status, _, body) = send(&app, get_req("/accounts?limit=2&tag=a&tag=b&cursor=c1")).await;
  assert_eq!(status, StatusCode::OK);
  assert_eq!(body, "limit=2 tags=a,b cursor=Some(\"c1\")");
  let (status, _, body) = send(&app, get_req("/accounts?limit=2")).await;
  assert_eq!(status, StatusCode::OK);
  assert_eq!(body, "limit=2 tags= cursor=None");

  let (_, ct, body) = send(&app, get_req("/accounts?limit=many&tag=x")).await;
  let p = problem(StatusCode::UNPROCESSABLE_ENTITY, &ct, body);
  assert_eq!(issues(&p), pairs(&[("query", "/limit")]));
  let (_, ct, body) = send(&app, get_req("/accounts")).await;
  let p = problem(StatusCode::UNPROCESSABLE_ENTITY, &ct, body);
  assert_eq!(issues(&p), pairs(&[("query", "/limit")]));
}

#[tokio::test]
async fn json_body_reports_every_issue_with_pointers() {
  let app = plain_app();
  let (status, _, body) = send(
    &app,
    post_req(
      "/accounts",
      "application/json",
      r#"{"name":"ada","age":36}"#,
    ),
  )
  .await;
  assert_eq!((status, body), (StatusCode::OK, Value::from("ada:36")));

  let (_, ct, body) = send(
    &app,
    post_req(
      "/orders",
      "application/json",
      r#"{"customer":7,"lines":[{"sku":"a","qty":1},{"sku":"b","qty":"x"},{"qty":2}]}"#,
    ),
  )
  .await;
  let p = problem(StatusCode::UNPROCESSABLE_ENTITY, &ct, body);
  assert_eq!(
    issues(&p),
    pairs(&[
      ("body", "/customer"),
      ("body", "/lines/1/qty"),
      ("body", "/lines/2/sku"),
    ])
  );
  assert_eq!(
    p.detail.as_deref(),
    Some("the request input failed validation (3 issues)")
  );

  let (_, ct, body) = send(&app, post_req("/accounts", "application/json", "{oops")).await;
  let p = problem(StatusCode::BAD_REQUEST, &ct, body);
  assert_eq!(issues(&p), pairs(&[("body", "")]));

  let limited = app.layer(DefaultBodyLimit::max(4));
  let (status, ct, _) = send(
    &limited,
    post_req(
      "/accounts",
      "application/json",
      r#"{"name":"ada","age":36}"#,
    ),
  )
  .await;
  assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
  assert_ne!(ct, PROBLEM_JSON);
}

#[tokio::test]
async fn missing_required_header_is_422() {
  let app = plain_app();
  let ok = Request::builder()
    .uri("/tenant")
    .header("x-tenant", "9")
    .header("x-request-id", "r-1")
    .body(Body::empty())
    .unwrap();
  let (status, _, body) = send(&app, ok).await;
  assert_eq!((status, body), (StatusCode::OK, Value::from("9:r-1")));

  let missing = Request::builder()
    .uri("/tenant")
    .header("x-tenant", "nine")
    .body(Body::empty())
    .unwrap();
  let (_, ct, body) = send(&app, missing).await;
  let p = problem(StatusCode::UNPROCESSABLE_ENTITY, &ct, body);
  assert_eq!(
    issues(&p),
    pairs(&[("header", "/X-Tenant"), ("header", "/X-Request-Id")])
  );

  let typed: axum::Router = Router::new()
    .route(Route::get("/tenant").headers::<Tenant>().handler(
      |_: &mut (), input: Input<(), (), (i64, String)>| {
        succeed::<_, Infallible, ()>(input.headers.1)
      },
    ))
    .into_axum();
  let (_, ct, body) = send(&typed, get_req("/tenant")).await;
  let p = problem(StatusCode::UNPROCESSABLE_ENTITY, &ct, body);
  assert_eq!(
    issues(&p),
    pairs(&[("header", "/X-Tenant"), ("header", "/X-Request-Id")])
  );
}

fn signup(_: &mut (), input: Input<(), (), (), (String, i64)>) -> Effect<String, Infallible, ()> {
  let (name, age) = input.body;
  succeed(format!("{name}:{age}"))
}

#[tokio::test]
async fn form_fields_validated() {
  let typed: axum::Router = Router::new()
    .route(Route::post("/signup").form::<Signup>().handler(signup))
    .into_axum();
  for app in [plain_app(), typed] {
    let form = "application/x-www-form-urlencoded";
    let (status, _, body) = send(&app, post_req("/signup", form, "name=ada&age=36")).await;
    assert_eq!((status, body), (StatusCode::OK, Value::from("ada:36")));

    let (_, ct, body) = send(&app, post_req("/signup", form, "age=old")).await;
    let p = problem(StatusCode::UNPROCESSABLE_ENTITY, &ct, body);
    assert_eq!(issues(&p), pairs(&[("body", "/name"), ("body", "/age")]));

    let ok = multipart(&[
      ("name", None, "grace"),
      ("avatar", Some("a.png"), "PNG"),
      ("age", None, "45"),
    ]);
    let (status, _, body) = send(&app, post_req("/signup", &multipart_type(), &ok)).await;
    assert_eq!((status, body), (StatusCode::OK, Value::from("grace:45")));

    // A file field never stands in for a text field.
    let bad = multipart(&[("name", Some("n.txt"), "x"), ("age", None, "old")]);
    let (_, ct, body) = send(&app, post_req("/signup", &multipart_type(), &bad)).await;
    let p = problem(StatusCode::UNPROCESSABLE_ENTITY, &ct, body);
    assert_eq!(issues(&p), pairs(&[("body", "/name"), ("body", "/age")]));

    // Axum's rejections pass through: wrong content type, broken multipart.
    let (status, ct, _) = send(&app, post_req("/signup", "text/plain", "name=ada")).await;
    assert_eq!(status, StatusCode::UNSUPPORTED_MEDIA_TYPE);
    assert_ne!(ct, PROBLEM_JSON);
    let (status, ct, _) = send(&app, post_req("/signup", "multipart/form-data", "x")).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_ne!(ct, PROBLEM_JSON);
    let truncated =
      format!("--{BOUNDARY}\r\nContent-Disposition: form-data; name=\"name\"\r\n\r\nada");
    let (status, ct, _) = send(&app, post_req("/signup", &multipart_type(), &truncated)).await;
    assert!(status.is_client_error(), "{status}");
    assert_ne!(ct, PROBLEM_JSON);
  }
}

struct Created;

impl HasSchema for Created {
  type A = String;
  type I = String;
  type E = ();

  fn schema() -> Schema<String, String> {
    schema::string()
  }
}

type PagingValue = <Paging as HasSchema>::A;

fn create(
  _: &mut (),
  input: Input<String, PagingValue, (), (String, i64)>,
) -> Effect<String, Infallible, ()> {
  succeed(format!("{}:{}", input.path, input.body.0))
}

#[tokio::test]
async fn query_and_body_errors_reported_together() {
  let router = Router::new().route(
    Route::post("/orgs/{org}/signups")
      .path::<Created>()
      .query::<Paging>()
      .body::<Signup>()
      .handler(create),
  );
  assert_eq!(router.routes().next().unwrap().body_kind, BodyKind::Json);
  let app = router.into_axum();
  let (status, _, body) = send(
    &app,
    post_req(
      "/orgs/acme/signups?limit=1",
      "application/json",
      r#"{"name":"ada","age":1}"#,
    ),
  )
  .await;
  assert_eq!((status, body), (StatusCode::OK, Value::from("acme:ada")));

  let (_, ct, body) = send(
    &app,
    post_req(
      "/orgs/acme/signups?limit=x&cursor=c",
      "application/json",
      r#"{"name":["ada"]}"#,
    ),
  )
  .await;
  let p = problem(StatusCode::UNPROCESSABLE_ENTITY, &ct, body);
  assert_eq!(
    issues(&p),
    pairs(&[("query", "/limit"), ("body", "/name"), ("body", "/age")])
  );
  assert_eq!(p.instance.as_deref(), Some("/orgs/acme/signups"));

  // A malformed body makes the whole request a 400, still listing the other parts' issues.
  let (_, ct, body) = send(
    &app,
    post_req("/orgs/acme/signups?limit=x", "application/json", "nope"),
  )
  .await;
  let p = problem(StatusCode::BAD_REQUEST, &ct, body);
  assert_eq!(issues(&p), pairs(&[("query", "/limit"), ("body", "")]));
  assert_eq!(
    p.detail.as_deref(),
    Some("the request is malformed (2 issues)")
  );
}

#[test]
fn form_routes_record_their_body_kind() {
  let info = Route::post("/signup").form::<Signup>().info();
  assert_eq!(info.body_kind, BodyKind::Form);
  assert!(info.body.is_some());
  assert_eq!(
    BodyKind::Form.media_types(),
    ["application/x-www-form-urlencoded", "multipart/form-data"]
  );
  assert_eq!(BodyKind::default().media_types(), ["application/json"]);
}

#[test]
fn extractor_wrappers_behave_like_their_value() {
  let mut path = ValidPath::<AccountId>(1);
  *path += 1;
  assert_eq!(*path.clone(), 2);
  assert_eq!(format!("{path:?}"), "ValidPath(2)");
  assert_eq!(path.into_inner(), 2);
  assert_eq!(
    format!("{:?}", ValidQuery::<Created>("q".into())),
    "ValidQuery(\"q\")"
  );
  assert_eq!(
    format!("{:?}", ValidHeaders::<AccountId>(3)),
    "ValidHeaders(3)"
  );
  assert_eq!(format!("{:?}", ValidJson::<AccountId>(4)), "ValidJson(4)");
  assert_eq!(format!("{:?}", ValidForm::<AccountId>(5)), "ValidForm(5)");
}
