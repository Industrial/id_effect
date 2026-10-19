//! ADR 0014 C1 / finding H8 at the RPC surface: every procedure declares its access, the `/rpc`
//! mount is a declared typed route, and an unknown tag is a 404 Problem.

use std::marker::PhantomData;
use std::sync::Arc;

use axum::body::Body;
use axum::http::request::Parts;
use axum::http::{Request, StatusCode};
use axum::response::IntoResponse;
use http_body_util::BodyExt;
use id_effect::{DynEnv, schema, succeed};
use id_effect_axum::route::{GuardFuture, PROBLEM_JSON, RouteAccess, Router};
use id_effect_rpc::registry::{Decided, Guarded, Public, RouteGuard, Undecided};
use id_effect_rpc::{Procedure, ProcedureAccess, RpcGroup, RpcServer};
use tower::ServiceExt;

/// Admits requests carrying `x-staff: yes`.
struct Staff;

impl RouteGuard for Staff {
  fn policy(&self) -> &str {
    "staff"
  }

  fn check<'a>(&'a self, parts: &'a Parts) -> GuardFuture<'a> {
    let staff = parts.headers.get("x-staff").is_some_and(|v| v == "yes");
    Box::pin(async move {
      if staff {
        Ok(())
      } else {
        Err(StatusCode::FORBIDDEN.into_response())
      }
    })
  }
}

fn group() -> RpcGroup {
  let mut group = RpcGroup::new();
  group.register(
    Procedure::query("Greeter.greet").public(),
    Arc::new(schema::string::<()>()),
    Arc::new(schema::string::<()>()),
    &["message"],
    |name, _| succeed(format!("Hello, {name}!")),
  );
  group.register(
    Procedure::mutation("Greeter.reset").guard(Staff),
    Arc::new(schema::string::<()>()),
    Arc::new(schema::string::<()>()),
    &["message"],
    |_, _| succeed("reset".to_owned()),
  );
  group
}

async fn call(tag: &str, staff: bool) -> (StatusCode, Option<String>, serde_json::Value) {
  let app: axum::Router = RpcServer::new(group())
    .router()
    .with_state(DynEnv::new())
    .into_axum();
  let mut req = Request::builder()
    .method("POST")
    .uri("/rpc")
    .header("content-type", "application/json")
    .header("x-rpc", "1");
  if staff {
    req = req.header("x-staff", "yes");
  }
  let body = serde_json::json!({"tag": tag, "payload": "Ada"});
  let res = app
    .oneshot(req.body(Body::from(body.to_string())).unwrap())
    .await
    .unwrap();
  let status = res.status();
  let content_type = res
    .headers()
    .get("content-type")
    .map(|v| v.to_str().unwrap().to_owned());
  let bytes = res.into_body().collect().await.unwrap().to_bytes();
  let json = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
  (status, content_type, json)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rpc_unregistered_tag_is_404_problem() {
  let (status, content_type, body) = call("Greeter.nope", false).await;
  assert_eq!(status, StatusCode::NOT_FOUND);
  assert_eq!(content_type.as_deref(), Some(PROBLEM_JSON));
  assert_eq!(body["status"], 404);
  assert_eq!(body["detail"], "unknown rpc procedure");
  assert_eq!(body["tag"], "Greeter.nope");
  assert_eq!(body["instance"], "/rpc");

  let (status, _, body) = call("Greeter.greet", false).await;
  assert_eq!(status, StatusCode::OK);
  assert_eq!(body["exit"]["value"]["message"], "Hello, Ada!");
}

/// `Wrap::<A>::decided()` is the inherent `true` method only when `A: Decided`; otherwise the
/// trait fallback answers `false` — a run-time view of the bound `RpcGroup::register` requires.
struct Wrap<A>(PhantomData<A>);

trait NotDecided {
  fn decided(&self) -> bool {
    false
  }
}

impl<A> NotDecided for Wrap<A> {}

impl<A: Decided> Wrap<A> {
  fn decided(&self) -> bool {
    true
  }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rpc_group_requires_access_decision() {
  // The type-level rule: only `Public` and `Guarded` procedures register (an `Undecided` one is
  // a compile error, see the `compile_fail` example of `id_effect_rpc::registry`).
  assert!(!Wrap::<Undecided>(PhantomData).decided());
  assert!(Wrap::<Public>(PhantomData).decided());
  assert!(Wrap::<Guarded>(PhantomData).decided());
  assert_eq!(
    Procedure::query("A.q").route_access(),
    RouteAccess::Undecided
  );

  // The decision is recorded per procedure and enforced per call.
  let contract = group().service_contract("Greeter", 1);
  assert_eq!(
    contract.procedure("greet").unwrap().access,
    ProcedureAccess::Public {}
  );
  assert_eq!(
    contract.procedure("reset").unwrap().access,
    ProcedureAccess::Guarded {
      policies: vec!["staff".to_owned()]
    }
  );
  let (status, _, _) = call("Greeter.reset", false).await;
  assert_eq!(status, StatusCode::FORBIDDEN);
  let (status, _, body) = call("Greeter.reset", true).await;
  assert_eq!(status, StatusCode::OK);
  assert_eq!(body["exit"]["value"]["message"], "reset");

  // The `/rpc` mount itself is a declared route of the deny-by-default router.
  let router: Router<DynEnv> = RpcServer::new(group()).router();
  let info = router
    .registry()
    .find(&axum::http::Method::POST, "/rpc")
    .expect("declared");
  assert_eq!(info.access, RouteAccess::Public);
  assert!(!router.access_report().has_opt_outs());
}
