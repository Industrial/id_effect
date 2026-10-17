//! Ex 020 — route guards and decision events.
//!
//! An authentication layer turns an `x-user: <id>:<role>` header into the request's
//! [`CurrentPrincipal`]; each route carries a [`Guard`] (a named policy) attached with
//! `.authorize(..)`. Anonymous callers get 401, principals the policy refuses get 403, both as
//! `application/problem+json`. Every decision is delivered to an event sink, here an
//! [`AuthzTest`] harness's.
//!
//! Run: `cargo run -p id_effect_authz --example 020_route_guards --features axum,testing`

use axum::body::Body;
use axum::extract::Request;
use axum::middleware::{self, Next};
use axum::response::Response;
use axum::routing::{get, post};
use http_body_util::BodyExt;
use id_effect_authz::testing::AuthzTest;
use id_effect_authz::{
  AuthorizeExt, BasicPrincipal, CurrentPrincipal, Guard, Policies, Rbac, authenticated,
};
use tower::ServiceExt;

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

fn app(authz: &AuthzTest) -> axum::Router {
  let policies = authz.policies();
  axum::Router::new()
    .route(
      "/me",
      get(|| async { "you are signed in" })
        .authorize(Guard::new("signed-in", authenticated()).events(authz.sink())),
    )
    .route(
      "/ledger",
      // One guard per method: GET reads, POST posts.
      get(|| async { "entries" })
        .authorize(Guard::named(policies, "ledger.read"))
        .merge(post(|| async { "posted" }).authorize(Guard::named(policies, "ledger.post"))),
    )
    .layer(middleware::from_fn(authenticate))
}

async fn call(authz: &AuthzTest, method: &str, path: &str, user: Option<&str>) -> String {
  let mut req = Request::builder().method(method).uri(path);
  if let Some(user) = user {
    req = req.header("x-user", user);
  }
  let res = app(authz)
    .oneshot(req.body(Body::empty()).expect("request"))
    .await
    .expect("infallible");
  let status = res.status();
  let body = res.into_body().collect().await.expect("body").to_bytes();
  format!(
    "{method:<4} {path:<8} {:<16} {} {}",
    user.unwrap_or("(anonymous)"),
    status.as_u16(),
    String::from_utf8_lossy(&body)
  )
}

#[tokio::main]
async fn main() {
  let authz = AuthzTest::with_policies(policies());

  println!("{}", call(&authz, "GET", "/me", None).await);
  println!("{}", call(&authz, "GET", "/me", Some("vera:viewer")).await);
  println!(
    "{}",
    call(&authz, "GET", "/ledger", Some("vera:viewer")).await
  );
  println!(
    "{}",
    call(&authz, "POST", "/ledger", Some("vera:viewer")).await
  );
  println!(
    "{}",
    call(&authz, "POST", "/ledger", Some("carl:accountant")).await
  );

  println!("\ndecision events:");
  for event in authz.events() {
    println!("  {event}");
  }
}
