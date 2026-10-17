//! The typed-route registry: every route with its method, path template and part schemas.

use std::convert::Infallible;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use id_effect::schema::{self, HasSchema, Schema, SchemaKind};
use id_effect::{Effect, succeed};
use id_effect_axum::route::{Input, Route, RouteInfo, RouteRegistry, Router, SchemaRef};
use id_effect_axum::routing;
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

struct Account;

impl HasSchema for Account {
  type A = (String, i64);
  type I = (String, i64);
  type E = ();

  fn schema() -> Schema<Self::A, Self::I> {
    schema::struct_("name", schema::string(), "age", schema::i64())
  }
}

struct Filter;

impl HasSchema for Filter {
  type A = String;
  type I = String;
  type E = ();

  fn schema() -> Schema<String, String> {
    schema::string()
  }
}

struct Wrapper<T>(std::marker::PhantomData<T>);

impl<T: 'static> HasSchema for Wrapper<T> {
  type A = bool;
  type I = bool;
  type E = ();

  fn schema() -> Schema<bool, bool> {
    schema::bool_()
  }
}

fn ok<P, Q, H, B>(_: &mut (), _: Input<P, Q, H, B>) -> Effect<&'static str, Infallible, ()> {
  succeed("ok")
}

fn router() -> Router {
  Router::new()
    .route(
      Route::get("/accounts/{id}")
        .path::<AccountId>()
        .headers::<Account>()
        .response::<Account>(StatusCode::OK)
        .response::<Filter>(StatusCode::NOT_FOUND)
        .handler(ok),
    )
    .route(
      Route::put("/accounts/{id}")
        .path::<AccountId>()
        .body::<Account>()
        .handler(ok),
    )
    .route(Route::patch("/accounts/{id}").handler(ok))
    .route(Route::delete("/accounts/{id}").handler(ok))
    .route(
      Route::post("/accounts")
        .query::<Filter>()
        .body::<Account>()
        .response::<Account>(StatusCode::CREATED)
        .handler(ok),
    )
}

#[test]
fn registry_lists_every_route_with_method_path_and_schemas() {
  let router = router();
  let listed: Vec<(Method, &str)> = router
    .routes()
    .map(|r| (r.method.clone(), r.path.as_str()))
    .collect();
  assert_eq!(
    listed,
    vec![
      (Method::GET, "/accounts/{id}"),
      (Method::PUT, "/accounts/{id}"),
      (Method::PATCH, "/accounts/{id}"),
      (Method::DELETE, "/accounts/{id}"),
      (Method::POST, "/accounts"),
    ]
  );

  let registry = router.registry();
  assert_eq!(registry.len(), 5);
  assert!(!registry.is_empty());

  let get = registry.find(&Method::GET, "/accounts/{id}").unwrap();
  let path = get.path_params.as_ref().unwrap();
  assert!(path.type_name().ends_with("::AccountId"));
  assert_eq!(path.short_name(), "AccountId");
  assert_eq!(path.ast().kind, SchemaKind::Integer);
  assert_eq!(get.headers.as_ref().unwrap().short_name(), "Account");
  assert!(get.query.is_none() && get.body.is_none());
  let statuses: Vec<(StatusCode, &str)> = get
    .responses
    .iter()
    .map(|r| (r.status, r.schema.short_name()))
    .collect();
  assert_eq!(
    statuses,
    vec![
      (StatusCode::OK, "Account"),
      (StatusCode::NOT_FOUND, "Filter")
    ]
  );

  let post = registry.find(&Method::POST, "/accounts").unwrap();
  assert_eq!(post.query.as_ref().unwrap().short_name(), "Filter");
  let body = post.body.as_ref().unwrap().json_schema();
  assert_eq!(body["type"], "object");
  assert_eq!(body["properties"]["age"]["type"], "integer");
  assert_eq!(
    body["$schema"],
    "https://json-schema.org/draft/2020-12/schema"
  );

  let patch = registry.find(&Method::PATCH, "/accounts/{id}").unwrap();
  assert_eq!(*patch, RouteInfo::new(Method::PATCH, "/accounts/{id}"));
  assert!(registry.find(&Method::GET, "/nope").is_none());
  assert_eq!((&router.registry().clone()).into_iter().count(), 5);
}

#[test]
fn route_info_matches_before_and_after_the_handler() {
  let route = Route::post("/accounts")
    .query::<Filter>()
    .body::<Account>()
    .response::<Account>(StatusCode::CREATED);
  let before = route.info();
  let endpoint = route.handler(ok);
  assert_eq!(endpoint.info(), &before);
  let (info, _method_router) = endpoint.into_parts();
  assert_eq!(info, before);
  assert_eq!(Route::new(Method::HEAD, "/h").info().method, Method::HEAD);
}

#[test]
fn schema_ref_names_generic_types_and_compares_by_ast() {
  let r = SchemaRef::of::<Wrapper<AccountId>>();
  assert!(r.short_name().starts_with("Wrapper<"), "{}", r.short_name());
  assert_eq!(r.ast().kind, SchemaKind::Boolean);
  assert_eq!(SchemaRef::of::<AccountId>(), SchemaRef::of::<AccountId>());
  let manual = SchemaRef::from_schema("i64", &schema::i64::<()>());
  assert_eq!(manual.short_name(), "i64");
  assert_eq!(manual.json_schema()["type"], "integer");
}

#[tokio::test]
async fn merge_with_state_and_plain_routes_keep_the_registry() {
  #[derive(Clone)]
  struct St;
  fn typed(_: &mut St, _: Input) -> Effect<&'static str, Infallible, St> {
    succeed("typed")
  }
  let a = Router::<St>::new().route(Route::get("/a").handler(typed));
  let b = Router::default().route(Route::get("/b").handler(typed));
  let merged = a.merge(b);
  assert_eq!(merged.routes().count(), 2);

  let served: Router = merged.with_state(St);
  assert_eq!(served.registry().len(), 2);
  let (axum_router, registry) = served.into_parts();
  assert_eq!(
    registry.into_iter().map(|r| r.path).collect::<Vec<_>>(),
    ["/a", "/b"]
  );

  // Plain `routing` helpers mount next to typed routes.
  let app = axum_router.route(
    "/plain",
    routing::get(|_: &mut ()| succeed::<_, Infallible, ()>("plain")),
  );
  for uri in ["/a", "/b", "/plain"] {
    let res = app
      .clone()
      .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
      .await
      .unwrap();
    assert_eq!(res.status(), StatusCode::OK, "{uri}");
  }

  let from: axum::Router = Router::new().route(Route::get("/c").handler(ok)).into();
  let res = from
    .oneshot(Request::builder().uri("/c").body(Body::empty()).unwrap())
    .await
    .unwrap();
  assert_eq!(res.status(), StatusCode::OK);

  let mut extended = RouteRegistry::new();
  extended.extend([RouteInfo::new(Method::GET, "/x")]);
  extended.register(RouteInfo::new(Method::POST, "/x"));
  assert_eq!(extended.iter().count(), 2);
}

#[tokio::test]
async fn several_methods_share_one_path() {
  let app = router().into_axum();
  for method in [Method::GET, Method::PUT, Method::PATCH, Method::DELETE] {
    let req = Request::builder()
      .method(method.clone())
      .uri("/accounts/1")
      .header("name", "ada")
      .header("age", "3")
      .header("content-type", "application/json")
      .body(Body::from(r#"{"name":"ada","age":3}"#))
      .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK, "{method}");
  }
}

#[test]
#[should_panic(expected = "typed route PROPFIND /dav")]
fn extension_methods_cannot_be_routed() {
  let _ = Route::new(Method::from_bytes(b"PROPFIND").unwrap(), "/dav").handler(ok);
}
