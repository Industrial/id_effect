//! Feature `tenancy` (ADR 0015 D10): one `TenantResolver` feeds both the authorization tenant
//! (`TenantId`, a `TenantScope`) and the SQL tenant (`CurrentTenant`), per request through
//! `TenancyLayer` and outside requests through `Tenancy`.

#![cfg(all(feature = "tenancy", feature = "web"))]

use std::sync::{Arc, Mutex};

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::routing::get;
use id_effect_kit::authz::{
  BasicPrincipal, CurrentPrincipal, Decision, TenantId, predicate, principal_env,
};
use id_effect_kit::axum::RequestCaps;
use id_effect_kit::id_effect::{Exit, HNil};
use id_effect_kit::prelude::{Entity, FromRow};
use id_effect_kit::sql::{
  CurrentTenant, Dialect, Repository, SqlClientEnv, SqlError, SqlValue, TestSqlClient,
  sql_client_layer,
};
use id_effect_kit::tenancy::{
  Tenancy, TenancyEnv, TenancyLayer, TenantResolver, TenantRouter, sql_tenant,
};
use tower::ServiceExt;

/// A tenant-scoped entity: every statement filters by `org`.
#[allow(dead_code)] // only the statements are checked
#[derive(Clone, Debug, FromRow, Entity)]
#[from_row(crate = id_effect_kit::sql)]
#[entity(crate = id_effect_kit::sql, table = "docs", tenant = "org")]
struct Doc {
  #[entity(id)]
  id: i64,
  title: String,
}

/// What a handler saw: the extractor, the request's caps and its extension.
#[derive(Debug, Default)]
struct Seen {
  tenancy: Option<Tenancy>,
  cap_tenant: Option<TenantId>,
  cap_sql: Option<CurrentTenant>,
  ext_sql: Option<CurrentTenant>,
}

fn app(layer: TenancyLayer, seen: Arc<Mutex<Seen>>) -> axum::Router {
  axum::Router::new()
    .route(
      "/docs",
      get(move |tenancy: Option<Tenancy>, req: Request<Body>| {
        let seen = Arc::clone(&seen);
        async move {
          let caps = req.extensions().get::<RequestCaps>();
          *seen.lock().unwrap() = Seen {
            tenancy,
            cap_tenant: caps
              .and_then(RequestCaps::get::<TenantId>)
              .map(|t| TenantId::clone(&t)),
            cap_sql: caps
              .and_then(RequestCaps::get::<CurrentTenant>)
              .map(|t| CurrentTenant::clone(&t)),
            ext_sql: req.extensions().get::<CurrentTenant>().cloned(),
          };
          "ok"
        }
      }),
    )
    .layer(layer)
}

fn request(principal: Option<CurrentPrincipal>) -> Request<Body> {
  let mut req = Request::get("/docs").body(Body::empty()).unwrap();
  if let Some(principal) = principal {
    req.extensions_mut().insert(principal);
  }
  req
}

fn send(app: axum::Router, req: Request<Body>) -> StatusCode {
  let rt = tokio::runtime::Builder::new_current_thread()
    .enable_all()
    .build()
    .unwrap();
  rt.block_on(app.oneshot(req)).unwrap().status()
}

/// The SELECT a repository scoped by `repo` sends, with its parameters.
fn find_params(repo: Repository<Doc>) -> Vec<SqlValue> {
  let test = TestSqlClient::new();
  let found = repo
    .find::<SqlClientEnv, _>(1)
    .provide_layer(sql_client_layer::<SqlError>(test.clone()));
  assert!(matches!(found.block_on(), Exit::Success(None)));
  let queries = test.recorded_queries();
  assert_eq!(queries.len(), 1, "{queries:?}");
  assert!(queries[0].0.contains("\"org\" = $1"), "{queries:?}");
  queries[0].1.clone()
}

#[test]
fn kit_tenancy_bridge_scopes_sql_and_authz() {
  let resolver = TenantResolver::new();
  let ada = CurrentPrincipal::new(BasicPrincipal::new("ada").with_tenant("acme"));

  // One layer, one resolution: both sides land in the request.
  let seen = Arc::new(Mutex::new(Seen::default()));
  let status = send(
    app(TenancyLayer::new(&resolver), Arc::clone(&seen)),
    request(Some(ada.clone())),
  );
  assert_eq!(status, StatusCode::OK);
  let seen = std::mem::take(&mut *seen.lock().unwrap());
  let tenancy = seen.tenancy.expect("the request has a tenant");
  assert_eq!(tenancy.tenant(), &TenantId::new("acme"));
  assert_eq!(seen.cap_tenant.as_ref(), Some(tenancy.tenant()));
  assert_eq!(seen.cap_sql.as_ref(), Some(tenancy.sql_tenant()));
  assert_eq!(seen.ext_sql.as_ref(), Some(tenancy.sql_tenant()));
  let acme = SqlValue::Text("acme".into());
  assert_eq!(tenancy.sql_tenant(), &CurrentTenant(acme.clone()));

  // SQL: the repository built from the request's CurrentTenant, from the Tenancy, or from its
  // environment filters by the same tenant.
  let from_caps = Repository::<Doc>::new(Dialect::Postgres).with_tenant(seen.cap_sql.unwrap().0);
  assert_eq!(find_params(from_caps)[0], acme);
  assert_eq!(find_params(tenancy.repository(Dialect::Postgres))[0], acme);
  let from_env = Repository::<Doc>::for_current_tenant::<TenancyEnv, _>(Dialect::Postgres)
    .provide_all(tenancy.env())
    .block_on();
  let Exit::Success(from_env) = from_env else {
    panic!("for_current_tenant: {from_env:?}");
  };
  assert_eq!(find_params(from_env)[0], acme);

  // Authorization: the same tenant scopes policies. A policy allowing everyone still denies a
  // principal of another tenant.
  let anyone = || predicate("anyone", |_: &CurrentPrincipal, _: &TenantId| true);
  let decide = |principal: BasicPrincipal| {
    tenancy
      .check(anyone())
      .provide_all(principal_env(principal))
      .block_on()
  };
  let allowed = decide(BasicPrincipal::new("ada").with_tenant("acme"));
  assert!(
    matches!(allowed, Exit::Success(Decision::Allow)),
    "{allowed:?}"
  );
  let foreign = decide(BasicPrincipal::new("eve").with_tenant("globex"));
  assert!(
    matches!(foreign, Exit::Success(Decision::Deny(_))),
    "{foreign:?}"
  );

  // Tenant routing: statements inside `scope` act for the tenant.
  let routed = tenancy
    .scope(TenantRouter::routed_tenant::<SqlError, HNil>())
    .block_on();
  assert!(
    matches!(routed, Exit::Success(Some(ref t)) if t == "acme"),
    "{routed:?}"
  );

  // No tenant: neither side is set, and a required tenant answers 401 before the handler.
  let seen = Arc::new(Mutex::new(Seen::default()));
  let status = send(
    app(TenancyLayer::new(&resolver), Arc::clone(&seen)),
    request(None),
  );
  assert_eq!(status, StatusCode::OK);
  let seen = std::mem::take(&mut *seen.lock().unwrap());
  assert!(seen.tenancy.is_none() && seen.cap_tenant.is_none());
  assert!(seen.cap_sql.is_none() && seen.ext_sql.is_none());
  let required = TenancyLayer::new(&resolver).require_tenant();
  assert!(format!("{required:?}").contains("required: true"));
  let status = send(app(required, Arc::new(Mutex::default())), request(None));
  assert_eq!(status, StatusCode::UNAUTHORIZED);

  // The Tenancy extractor without a tenant: 403 for an authenticated principal of no tenant.
  let strict = axum::Router::new()
    .route(
      "/docs",
      get(|tenancy: Tenancy| async move { tenancy.tenant().to_string() }),
    )
    .layer(TenancyLayer::new(&resolver));
  let nobody = CurrentPrincipal::new(BasicPrincipal::new("root"));
  assert_eq!(
    send(strict.clone(), request(Some(nobody))),
    StatusCode::FORBIDDEN
  );
  assert_eq!(send(strict, request(Some(ada))), StatusCode::OK);
}

#[test]
fn tenancy_resolves_like_its_resolver() {
  let resolver = TenantResolver::new().with_host_allowlist([("acme.example.com", "acme")]);
  let visitor = CurrentPrincipal::anonymous();
  let tenancy = Tenancy::resolve(&resolver, &visitor, Some("acme.example.com"), None);
  assert_eq!(tenancy, Some(Tenancy::new("acme")));
  assert_eq!(
    Tenancy::resolve(&resolver, &visitor, Some("evil.example.com"), None),
    None
  );
  assert_eq!(
    Tenancy::from(TenantId::new("globex")),
    Tenancy::new("globex")
  );
  assert_eq!(
    sql_tenant(&TenantId::new("x")),
    CurrentTenant::new("x".to_owned())
  );
}
