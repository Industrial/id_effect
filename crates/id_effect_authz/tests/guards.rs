//! End-to-end route guards: an authentication layer puts the principal in the request, guarded
//! axum and typed routes answer 200 / 401 / 403 / 500 with Problem Details, and every decision
//! reaches the event sink an `AuthzTest` harness reads (sec.authz #2, #8).

use axum::body::Body;
use axum::extract::Request;
use axum::http::{StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::Response;
use axum::routing::get;
use http_body_util::BodyExt;
use id_effect::{Effect, succeed};
use id_effect_authz::guard::principal_of;
use id_effect_authz::testing::AuthzTest;
use id_effect_authz::{
  AuthorizeExt, AuthzError, BasicPrincipal, CurrentPrincipal, Guard, Outcome, Policies, PolicyExt,
  Rbac, authenticated, claim_equals,
};
use id_effect_axum::CapOverrideLayer;
use id_effect_axum::route::{Input, Route, Router};
use serde_json::Value;
use tower::ServiceExt;

/// The authentication layer under test: `x-user: <id>:<role>` becomes the request's principal.
async fn authenticate(mut req: Request, next: Next) -> Response {
  let principal = req
    .headers()
    .get("x-user")
    .and_then(|v| v.to_str().ok())
    .and_then(|v| v.split_once(':'))
    .map(|(id, role)| CurrentPrincipal::new(BasicPrincipal::new(id).with_role(role)));
  if let Some(principal) = principal {
    req.extensions_mut().insert(principal);
  }
  next.run(req).await
}

fn policies() -> Policies {
  let rbac = Rbac::builder()
    .grant("viewer", ["ledger:read"])
    .grant("accountant", ["ledger:post"])
    .inherit("accountant", "viewer")
    .build();
  Policies::builder()
    .define::<()>("ledger.read", rbac.require("ledger:read"))
    .define::<()>("ledger.post", rbac.require("ledger:post"))
    .build()
}

#[derive(Clone)]
struct App;

fn report(_: &mut App, _: Input) -> Effect<&'static str, std::convert::Infallible, App> {
  succeed("report")
}

fn app(authz: &AuthzTest) -> axum::Router {
  let policies = authz.policies();
  let typed = Router::<App>::new();
  let (plain, mut registry) = typed.into_parts();
  let plain = Route::get("/reports")
    .handler(report)
    .authorize(Guard::new("reports", authenticated()).events(authz.sink()))
    .mount(plain, &mut registry);
  assert_eq!(registry.len(), 1);
  plain
    .route(
      "/ledger",
      get(|| async { "entries" })
        .authorize(Guard::named(policies, "ledger.read"))
        .post(|| async { "posted" }),
    )
    .route(
      "/ledger/post",
      get(|| async { "posted" })
        .authorize(Guard::named(policies, "ledger.post").expose_reason(true)),
    )
    .route(
      "/broken",
      get(|| async { "never" }).authorize(Guard::named(policies, "no.such.policy")),
    )
    .with_state(App)
    .layer(middleware::from_fn(authenticate))
}

async fn send(app: axum::Router, path: &str, user: Option<&str>) -> (StatusCode, String, Value) {
  let mut req = Request::get(path);
  if let Some(user) = user {
    req = req.header("x-user", user);
  }
  let res = app
    .oneshot(req.body(Body::empty()).expect("request"))
    .await
    .expect("infallible");
  let status = res.status();
  let content_type = res
    .headers()
    .get(header::CONTENT_TYPE)
    .and_then(|v| v.to_str().ok())
    .unwrap_or_default()
    .to_owned();
  let bytes = res.into_body().collect().await.expect("body").to_bytes();
  let body = serde_json::from_slice(&bytes)
    .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&bytes).into_owned()));
  (status, content_type, body)
}

#[tokio::test]
async fn guards_answer_200_401_403_and_500() {
  let authz = AuthzTest::with_policies(policies());

  let (status, _, body) = send(app(&authz), "/ledger", Some("vera:viewer")).await;
  assert_eq!((status, body), (StatusCode::OK, Value::from("entries")));

  let (status, content_type, body) = send(app(&authz), "/ledger", None).await;
  assert_eq!(status, StatusCode::UNAUTHORIZED);
  assert_eq!(content_type, "application/problem+json");
  assert_eq!(body["status"], 401);
  assert_eq!(body["title"], "Unauthorized");
  assert_eq!(body["policy"], "ledger.read");

  let (status, _, body) = send(app(&authz), "/ledger/post", Some("vera:viewer")).await;
  assert_eq!(status, StatusCode::FORBIDDEN);
  assert_eq!(body["detail"], "missing permission ledger:post");
  assert_eq!(body["policy"], "ledger.post");

  let (status, _, body) = send(app(&authz), "/ledger/post", Some("carl:accountant")).await;
  assert_eq!((status, body), (StatusCode::OK, Value::from("posted")));

  let (status, _, body) = send(app(&authz), "/broken", Some("carl:accountant")).await;
  assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
  assert_eq!(body["detail"], "authorization could not be evaluated");
}

#[tokio::test]
async fn only_guarded_methods_are_checked() {
  let authz = AuthzTest::with_policies(policies());
  let res = app(&authz)
    .oneshot(
      Request::post("/ledger")
        .body(Body::empty())
        .expect("request"),
    )
    .await
    .expect("infallible");
  assert_eq!(res.status(), StatusCode::OK);
  assert!(authz.events().is_empty());
}

#[tokio::test]
async fn typed_endpoints_are_guarded_and_registered() {
  let authz = AuthzTest::new();
  let endpoint = Route::get("/reports")
    .handler(report)
    .authorize(Guard::new("reports", authenticated()))
    .authorize(Guard::new(
      "staff",
      claim_equals("staff", true).or(authenticated()),
    ));
  assert_eq!(endpoint.policies(), ["reports", "staff"]);
  assert_eq!(endpoint.info().path, "/reports");
  assert!(format!("{endpoint:?}").contains("staff"));
  let (info, _) = endpoint.into_parts();
  assert_eq!(info.method, axum::http::Method::GET);

  let (status, _, _) = send(app(&authz), "/reports", None).await;
  assert_eq!(status, StatusCode::UNAUTHORIZED);
  let (status, _, body) = send(app(&authz), "/reports", Some("ada:auditor")).await;
  assert_eq!((status, body), (StatusCode::OK, Value::from("report")));
}

#[tokio::test]
async fn every_decision_is_an_event() {
  let authz = AuthzTest::with_policies(policies());
  let _ = send(app(&authz), "/ledger", Some("vera:viewer")).await;
  let _ = send(app(&authz), "/ledger/post", Some("vera:viewer")).await;
  let _ = send(app(&authz), "/reports", None).await;
  let _ = send(app(&authz), "/broken", None).await;

  let events = authz.take_events();
  let seen: Vec<_> = events
    .iter()
    .map(|e| (e.principal.as_str(), e.policy.as_str(), e.outcome))
    .collect();
  assert_eq!(
    seen,
    [
      ("vera", "ledger.read", Outcome::Allow),
      ("vera", "ledger.post", Outcome::Deny),
      ("", "reports", Outcome::Deny),
      ("", "no.such.policy", Outcome::Error),
    ]
  );
  assert_eq!(events[2].resource, "GET /reports");
  assert_eq!(
    events[1].reason.as_deref(),
    Some("missing permission ledger:post")
  );
  let json = serde_json::to_value(&events[1]).expect("serialise");
  assert_eq!(json["outcome"], "deny");
}

#[tokio::test]
async fn principals_installed_by_cap_override_layer_are_seen() {
  let guarded: axum::Router = axum::Router::new()
    .route(
      "/me",
      get(|req: Request| async move { principal_of(&req.into_parts().0).id().to_owned() })
        .authorize(Guard::new("me", authenticated())),
    )
    .layer(CapOverrideLayer::new(
      |parts: &axum::http::request::Parts| {
        let id = parts.headers.get("x-api-key")?.to_str().ok()?;
        Some(CurrentPrincipal::new(BasicPrincipal::new(id)))
      },
    ));
  let (status, _, body) = send(guarded.clone(), "/me", None).await;
  assert_eq!(status, StatusCode::UNAUTHORIZED);
  assert_eq!(body["detail"], "authentication required");
  let res = guarded
    .oneshot(
      Request::get("/me")
        .header("x-api-key", "svc-ledger")
        .body(Body::empty())
        .expect("request"),
    )
    .await
    .expect("infallible");
  assert_eq!(res.status(), StatusCode::OK);
}

#[tokio::test]
async fn handlers_can_fail_with_authz_error() {
  let app: axum::Router = axum::Router::new().route(
    "/fail",
    get(|| async { Err::<(), _>(AuthzError::Unauthenticated) }),
  );
  let (status, content_type, body) = send(app, "/fail", None).await;
  assert_eq!(status, StatusCode::UNAUTHORIZED);
  assert_eq!(content_type, "application/problem+json");
  assert!(body.get("policy").is_none());
}

#[test]
fn authz_test_asserts_policies_directly() {
  let authz = AuthzTest::with_policies(policies());
  let vera = CurrentPrincipal::new(BasicPrincipal::new("vera").with_role("viewer"));
  authz.assert_allows_named(&vera, "ledger.read", &());
  let denial = authz.assert_denies_named(&vera, "ledger.post", &());
  assert_eq!(denial.policy(), Some("ledger.post"));
  authz.assert_denied_because(
    &vera,
    &authenticated().negate("signed in"),
    &(),
    "signed in",
  );
  assert_eq!(authz.events().len(), 3);
}
