//! Deny-by-default routing (H8, ADR 0013 R1): every typed route declares its access, the router
//! answers 404 for routes that do not, and the deprecated `route_unchecked` shim is recorded.
//! The compile-time half (`h8_route_without_access_decision_does_not_compile`) is in `tests/ui`.

use std::convert::Infallible;

use axum::body::Body;
use axum::http::request::Parts;
use axum::http::{Method, Request, StatusCode, header};
use axum::response::IntoResponse;
use http_body_util::BodyExt;
use id_effect::observability::posture::dangerous_opt_outs;
use id_effect::{Effect, succeed};
use id_effect_axum::PROBLEM_JSON;
use id_effect_axum::route::{
  AccessReport, Endpoint, GuardFuture, Input, Route, RouteAccess, RouteGuard, RouteInfo, Router,
};
use serde_json::Value;
use tower::ServiceExt;

fn ok(_: &mut (), _: Input) -> Effect<&'static str, Infallible, ()> {
  succeed("ok")
}

/// Admits requests whose `x-role` header names its policy; answers 403 otherwise.
struct Role(&'static str);

impl RouteGuard for Role {
  fn policy(&self) -> &str {
    self.0
  }

  fn check<'a>(&'a self, parts: &'a Parts) -> GuardFuture<'a> {
    let roles: Vec<&str> = parts
      .headers
      .get_all("x-role")
      .iter()
      .filter_map(|v| v.to_str().ok())
      .collect();
    let allowed = roles.contains(&self.0);
    Box::pin(async move {
      if allowed {
        Ok(())
      } else {
        Err((StatusCode::FORBIDDEN, self.0.to_owned()).into_response())
      }
    })
  }
}

async fn call(
  app: &axum::Router,
  method: Method,
  uri: &str,
  roles: &[&str],
) -> (StatusCode, String, String) {
  let mut req = Request::builder().method(method).uri(uri);
  for role in roles {
    req = req.header("x-role", *role);
  }
  let res = app
    .clone()
    .oneshot(req.body(Body::empty()).unwrap())
    .await
    .unwrap();
  let status = res.status();
  let content_type = res
    .headers()
    .get(header::CONTENT_TYPE)
    .and_then(|v| v.to_str().ok())
    .unwrap_or_default()
    .to_owned();
  let body = res.into_body().collect().await.unwrap().to_bytes();
  (
    status,
    content_type,
    String::from_utf8(body.to_vec()).unwrap(),
  )
}

#[tokio::test]
async fn h8_unregistered_axum_route_is_denied() {
  let plain = axum::Router::new()
    .route("/debug", axum::routing::get(|| async { "internals" }))
    .route("/hello", axum::routing::post(|| async { "posted" }));
  let app = Router::new()
    .route(Route::get("/hello").public().handler(ok))
    .merge_axum(plain.clone())
    .into_axum();

  assert_eq!(
    call(&app, Method::GET, "/hello", &[]).await.0,
    StatusCode::OK
  );
  assert_eq!(
    call(&app, Method::HEAD, "/hello", &[]).await.0,
    StatusCode::OK
  );
  // A plain route without an access decision answers 404 Problem Details, without running.
  let (status, content_type, body) = call(&app, Method::GET, "/debug", &[]).await;
  assert_eq!(
    (status, content_type.as_str()),
    (StatusCode::NOT_FOUND, PROBLEM_JSON)
  );
  let problem: Value = serde_json::from_str(&body).unwrap();
  assert_eq!(problem["status"], 404);
  assert_eq!(problem["instance"], "/debug");
  assert!(!body.contains("internals"));
  // Per method: the typed GET is declared, the plain POST on the same path is not.
  assert_eq!(
    call(&app, Method::POST, "/hello", &[]).await.0,
    StatusCode::NOT_FOUND
  );
  // Unknown paths keep axum's 404.
  assert_eq!(
    call(&app, Method::GET, "/nope", &[]).await.0,
    StatusCode::NOT_FOUND
  );

  // A plain handler serves once it is an endpoint with a decision.
  let declared = Endpoint::from_parts(
    RouteInfo::new(Method::GET, "/debug"),
    axum::routing::get(|| async { "internals" }),
  )
  .guard(Role("ops"));
  let app = Router::new().route(declared).into_axum();
  assert_eq!(
    call(&app, Method::GET, "/debug", &[]).await.0,
    StatusCode::FORBIDDEN
  );
  assert_eq!(
    call(&app, Method::GET, "/debug", &["ops"]).await.2,
    "internals"
  );

  // `access_layer` covers plain routes merged onto the Axum router afterwards.
  let typed = Router::new().route(Route::get("/hello").public().handler(ok));
  let access = typed.access_layer();
  let app = typed
    .into_axum()
    .route("/later", axum::routing::get(|| async { "later" }))
    .route_layer(access);
  assert_eq!(
    call(&app, Method::GET, "/hello", &[]).await.0,
    StatusCode::OK
  );
  assert_eq!(
    call(&app, Method::GET, "/later", &[]).await.0,
    StatusCode::NOT_FOUND
  );

  // The named opt-out serves them, and is recorded.
  let router = Router::new()
    .route(Route::get("/hello").public().handler(ok))
    .merge_axum(plain)
    .allow_unregistered_routes_dangerous();
  assert!(router.access_report().unregistered_allowed);
  let app = router.into_axum();
  assert_eq!(call(&app, Method::GET, "/debug", &[]).await.2, "internals");
  assert!(
    dangerous_opt_outs()
      .iter()
      .any(|(id, _)| *id == "id_effect_axum::route::Router::allow_unregistered_routes_dangerous")
  );
}

#[tokio::test]
#[allow(deprecated)]
async fn h8_route_unchecked_is_deprecated_and_recorded() {
  let id = "id_effect_axum::route::Router::route_unchecked";
  let decided = Router::new().route_unchecked(Route::get("/open").public().handler(ok));
  // A decided endpoint keeps its decision and is not an opt-out.
  assert_eq!(decided.routes().next().unwrap().access, RouteAccess::Public);
  assert!(!decided.access_report().has_opt_outs());

  let router = Router::new().route_unchecked(Route::get("/legacy").handler(ok));
  let info = router.routes().next().unwrap();
  assert_eq!(info.access, RouteAccess::Unchecked);
  let report = router.access_report();
  assert_eq!(report.unchecked, ["GET /legacy"]);
  assert!(report.public.is_empty() && report.has_opt_outs());
  let (_, reopens) = dangerous_opt_outs()
    .into_iter()
    .find(|(recorded, _)| *recorded == id)
    .expect("route_unchecked records its opt-out");
  assert!(reopens.starts_with("H8"), "{reopens}");
  // The shim keeps the 0.10 behaviour: the route serves, behind no guard.
  let app = router.into_axum();
  assert_eq!(call(&app, Method::GET, "/legacy", &[]).await.2, "ok");
}

#[tokio::test]
async fn h8_access_report_lists_public_routes() {
  let router = Router::new()
    .route(Route::get("/health").public().handler(ok))
    .route(Route::post("/login").public().handler(ok))
    .route(
      Route::get("/reports")
        .guard(Role("viewer"))
        .guard(Role("staff"))
        .handler(ok),
    )
    .route(Route::delete("/reports").handler(ok).guard(Role("admin")));
  let report = router.access_report();
  assert_eq!(report.public, ["GET /health", "POST /login"]);
  assert_eq!(
    report.guarded,
    [
      (
        "GET /reports".to_owned(),
        vec!["viewer".to_owned(), "staff".to_owned()]
      ),
      ("DELETE /reports".to_owned(), vec!["admin".to_owned()]),
    ]
  );
  assert!(report.unchecked.is_empty() && !report.has_opt_outs());
  let text = report.to_string();
  assert!(text.contains("public     GET /health"), "{text}");
  assert!(
    text.contains("guarded    GET /reports  [viewer, staff]"),
    "{text}"
  );
  assert_eq!(AccessReport::new(router.registry(), false), report);

  // The policies are recorded per route and enforced, the last guard outermost.
  let get = router.registry().find(&Method::GET, "/reports").unwrap();
  assert_eq!(
    (get.access, get.policies.as_slice()),
    (
      RouteAccess::Guarded,
      &["viewer".to_owned(), "staff".to_owned()][..]
    )
  );
  let app = router.into_axum();
  assert_eq!(
    call(&app, Method::GET, "/health", &[]).await.0,
    StatusCode::OK
  );
  assert_eq!(
    call(&app, Method::GET, "/reports", &["viewer"]).await.2,
    "staff"
  );
  assert_eq!(
    call(&app, Method::GET, "/reports", &["staff"]).await.2,
    "viewer"
  );
  assert_eq!(
    call(&app, Method::GET, "/reports", &["staff", "viewer"])
      .await
      .2,
    "ok"
  );
  assert_eq!(
    call(&app, Method::DELETE, "/reports", &["admin"]).await.0,
    StatusCode::OK
  );
}

#[test]
fn route_info_records_the_decision() {
  let undecided = Route::get("/x").info();
  assert_eq!(undecided.access, RouteAccess::Undecided);
  assert!(!undecided.access.is_served());
  assert_eq!(Route::get("/x").public().info().access, RouteAccess::Public);
  let guarded = Route::get("/x").guard(Role("a")).info();
  assert_eq!(
    (guarded.access, guarded.policies),
    (RouteAccess::Guarded, vec!["a".to_owned()])
  );
  // A guard on a public route makes it guarded.
  let both = Route::get("/x").public().guard(Role("b")).info();
  assert_eq!(both.access, RouteAccess::Guarded);
  for access in [
    RouteAccess::Public,
    RouteAccess::Guarded,
    RouteAccess::Unchecked,
  ] {
    assert!(access.is_served(), "{}", access.as_str());
  }
  assert!(format!("{:?}", Route::get("/x").guard(Role("a"))).contains("\"a\""));
}
