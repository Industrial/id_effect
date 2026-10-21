//! Tenant resolution (ADR 0015 D10): from the authenticated principal by default, from the Host
//! through an allowlist, from a header only through the recorded `dangerous_tenant_from_header`;
//! the axum layer hands the resolved `TenantId` to handlers and to `TenantScope` checks.

#![cfg(feature = "axum")]

use axum::Extension;
use axum::body::Body;
use axum::extract::Request;
use axum::http::{StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::Response;
use axum::routing::get;
use http_body_util::BodyExt;
use id_effect::observability::posture::dangerous_opt_outs;
use id_effect::{Exit, hlist};
use id_effect_authz::{
  BasicPrincipal, CurrentPrincipal, TenantId, TenantLayer, TenantResolver, allow_all, check_with,
};
use id_effect_axum::RequestCaps;
use tower::ServiceExt;

const HEADER_OPT_OUT: &str = "id_effect_authz::TenantResolver::dangerous_tenant_from_header";

/// The authentication layer under test: `x-user: <id>[@<tenant>]` becomes the principal.
async fn authenticate(mut req: Request, next: Next) -> Response {
  let principal = req
    .headers()
    .get("x-user")
    .and_then(|v| v.to_str().ok())
    .map(|user| match user.split_once('@') {
      Some((id, tenant)) => CurrentPrincipal::new(BasicPrincipal::new(id).with_tenant(tenant)),
      None => CurrentPrincipal::new(BasicPrincipal::new(user)),
    });
  if let Some(principal) = principal {
    req.extensions_mut().insert(principal);
  }
  next.run(req).await
}

/// `/tenant` answers the required tenant, `/maybe` the optional one, `/caps` the one in the
/// request's `RequestCaps`; the tenant layer runs inside the authentication layer.
fn app(layer: TenantLayer) -> axum::Router {
  axum::Router::new()
    .route(
      "/tenant",
      get(|tenant: TenantId| async move { tenant.to_string() }),
    )
    .route(
      "/maybe",
      get(|tenant: Option<TenantId>| async move {
        tenant.map_or_else(|| "none".to_owned(), |t| t.to_string())
      }),
    )
    .route(
      "/caps",
      get(|Extension(caps): Extension<RequestCaps>| async move {
        caps
          .get::<TenantId>()
          .map_or_else(|| "none".to_owned(), |t| t.to_string())
      }),
    )
    .layer(layer)
    .layer(middleware::from_fn(authenticate))
}

async fn call(app: &axum::Router, path: &str, headers: &[(&str, &str)]) -> (StatusCode, String) {
  let mut request = Request::builder().uri(path);
  for (name, value) in headers {
    request = request.header(*name, *value);
  }
  let response = app
    .clone()
    .oneshot(request.body(Body::empty()).expect("request"))
    .await
    .expect("infallible");
  let status = response.status();
  let body = response
    .into_body()
    .collect()
    .await
    .expect("body")
    .to_bytes();
  (status, String::from_utf8_lossy(&body).into_owned())
}

fn principal(id: &str, tenant: Option<&str>) -> CurrentPrincipal {
  let p = BasicPrincipal::new(id);
  CurrentPrincipal::new(match tenant {
    Some(t) => p.with_tenant(t),
    None => p,
  })
}

#[tokio::test]
async fn tenant_resolved_from_principal_by_default() {
  let resolver = TenantResolver::new();
  assert!(!resolver.has_host_allowlist() && resolver.tenant_header().is_none());
  let ada = principal("ada", Some("acme"));
  assert_eq!(
    resolver.resolve_with(&ada, None, None),
    Some(TenantId::new("acme"))
  );
  // The default reads nothing else: not the Host, not a header, not an anonymous request.
  assert_eq!(
    resolver.resolve_with(&ada, Some("globex.example.com"), Some("globex")),
    Some(TenantId::new("acme"))
  );
  let anon = CurrentPrincipal::anonymous();
  assert_eq!(
    resolver.resolve_with(&anon, Some("acme.example.com"), Some("acme")),
    None
  );
  assert_eq!(
    resolver.resolve_with(&principal("bob", None), None, None),
    None
  );

  let app = app(resolver.layer());
  let forged = [("x-user", "ada@acme"), ("x-tenant", "globex")];
  assert_eq!(
    call(&app, "/tenant", &forged).await,
    (StatusCode::OK, "acme".into())
  );
  assert_eq!(
    call(&app, "/caps", &forged).await,
    (StatusCode::OK, "acme".into())
  );
  // No tenant: the extractor refuses (401 anonymous, 403 authenticated), the option is empty.
  assert_eq!(call(&app, "/tenant", &[]).await.0, StatusCode::UNAUTHORIZED);
  assert_eq!(
    call(&app, "/tenant", &[("x-user", "bob")]).await.0,
    StatusCode::FORBIDDEN
  );
  assert_eq!(
    call(&app, "/maybe", &[("x-tenant", "acme")]).await,
    (StatusCode::OK, "none".into())
  );
}

#[test]
fn resolved_tenant_is_a_tenant_scope() {
  // Only principals of the resolved tenant pass a check on it.
  let on_acme = |p: CurrentPrincipal| match check_with(allow_all(), TenantId::new("acme"))
    .provide_all(hlist![p])
    .block_on()
  {
    Exit::Success(decision) => decision.is_allowed(),
    Exit::Failure(cause) => panic!("{cause:?}"),
  };
  assert!(on_acme(principal("ada", Some("acme"))));
  assert!(!on_acme(principal("eve", Some("globex"))));
  assert!(!on_acme(CurrentPrincipal::anonymous()));
}

#[tokio::test]
async fn dangerous_tenant_from_header_is_recorded() {
  let resolver = TenantResolver::new().dangerous_tenant_from_header("X-Tenant");
  assert!(
    dangerous_opt_outs()
      .iter()
      .any(|(id, _)| *id == HEADER_OPT_OUT),
    "the opt-out is in the posture registry"
  );
  assert_eq!(resolver.tenant_header(), Some("x-tenant"));
  assert!(format!("{resolver:?}").contains("x-tenant"));
  let anon = CurrentPrincipal::anonymous();
  assert_eq!(
    resolver.resolve_with(&anon, None, Some(" acme ")),
    Some(TenantId::new("acme"))
  );
  // The header is bounded and must agree with the principal.
  assert_eq!(resolver.resolve_with(&anon, None, Some("../globex")), None);
  assert_eq!(
    resolver.resolve_with(&anon, None, Some(&"a".repeat(129))),
    None
  );
  let ada = principal("ada", Some("acme"));
  assert_eq!(resolver.resolve_with(&ada, None, Some("globex")), None);
  assert_eq!(
    resolver.resolve_with(&ada, None, Some("acme")),
    Some(TenantId::new("acme"))
  );

  let app = app(resolver.layer());
  assert_eq!(
    call(&app, "/tenant", &[("x-tenant", "globex")]).await,
    (StatusCode::OK, "globex".into())
  );
  assert_eq!(
    call(
      &app,
      "/maybe",
      &[("x-user", "ada@acme"), ("x-tenant", "globex")]
    )
    .await,
    (StatusCode::OK, "none".into())
  );
}

#[tokio::test]
async fn host_outside_allowlist_has_no_tenant() {
  let resolver = TenantResolver::new().with_host_allowlist([
    ("acme.example.com", "acme"),
    ("globex.example.com.", "globex"),
  ]);
  assert!(resolver.has_host_allowlist());
  let anon = CurrentPrincipal::anonymous();
  assert_eq!(resolver.resolve_host("evil.example.com"), None);
  assert_eq!(
    resolver.resolve_with(&anon, Some("evil.example.com"), None),
    None
  );
  // The request's host is never a tenant id itself.
  assert_eq!(resolver.resolve_with(&anon, Some("acme"), None), None);
  assert_eq!(
    resolver.resolve_with(&anon, Some("ACME.Example.com:8443"), None),
    Some(TenantId::new("acme"))
  );
  assert_eq!(
    resolver.resolve_host("globex.example.com"),
    Some(TenantId::new("globex"))
  );
  // A principal of one tenant on another tenant's host has no tenant.
  let ada = principal("ada", Some("acme"));
  assert_eq!(
    resolver.resolve_with(&ada, Some("globex.example.com"), None),
    None
  );
  // A principal on a host outside the allowlist keeps its own tenant.
  assert_eq!(
    resolver.resolve_with(&ada, Some("evil.example.com"), None),
    Some(TenantId::new("acme"))
  );

  let layer = resolver.layer().require_tenant();
  assert!(format!("{layer:?}").contains("required: true"));
  let app = app(layer);
  let evil = [(header::HOST.as_str(), "evil.example.com")];
  // `require_tenant` answers before the handler: 401 anonymous, 403 authenticated.
  assert_eq!(
    call(&app, "/maybe", &evil).await.0,
    StatusCode::UNAUTHORIZED
  );
  assert_eq!(
    call(&app, "/maybe", &[evil[0], ("x-user", "bob")]).await.0,
    StatusCode::FORBIDDEN
  );
  assert_eq!(
    call(
      &app,
      "/tenant",
      &[(header::HOST.as_str(), "acme.example.com")]
    )
    .await,
    (StatusCode::OK, "acme".into())
  );
  // Without a Host header, the URI's authority is the host.
  assert_eq!(
    call(&app, "http://globex.example.com/tenant", &[]).await,
    (StatusCode::OK, "globex".into())
  );
}

#[tokio::test]
async fn tenant_from_request_caps_without_the_layer() {
  // A tenant recorded in the request's `RequestCaps` (by another layer) is found too.
  let app = axum::Router::new().route(
    "/tenant",
    get(|tenant: TenantId| async move { tenant.to_string() }),
  );
  let mut caps = RequestCaps::new();
  caps.insert(TenantId::new("acme"));
  let mut request = Request::builder()
    .uri("/tenant")
    .body(Body::empty())
    .expect("request");
  request.extensions_mut().insert(caps);
  let response = app.oneshot(request).await.expect("infallible");
  assert_eq!(response.status(), StatusCode::OK);
}
