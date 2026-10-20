//! OpenAPI 3.1 generation from typed routes: the document validates against the official
//! OpenAPI 3.1 meta-schema, and its component schemas are the decoding schemas.
#![cfg(feature = "openapi")]

use std::convert::Infallible;

use axum::body::Body;
use axum::http::request::Parts;
use axum::http::{Method, Request, StatusCode};
use http_body_util::BodyExt;
use id_effect::schema::{
  self, Field, HasSchema, LazyAst, Schema, SchemaAst, SchemaKind, json_schema_for, min_length,
  optional, range,
};
use id_effect::{Effect, succeed};
use id_effect_axum::PROBLEM_JSON;
use id_effect_axum::openapi::{
  ApiKeyLocation, Info, OpenApi, ParameterIn, SecurityScheme, Server, Tag,
};
use id_effect_axum::route::{
  Endpoint, GuardFuture, Guarded, Input, Route, RouteGuard, RouteInfo, Router, SchemaRef,
};
use serde_json::{Value, json};
use tower::ServiceExt;

/// The official OpenAPI 3.1 document schema (see `tests/fixtures/openapi/README.md`).
const META_SCHEMA: &str = include_str!("fixtures/openapi/oas-3.1-schema-2022-10-07.json");

struct AccountId;

impl HasSchema for AccountId {
  type A = i64;
  type I = i64;
  type E = ();

  fn schema() -> Schema<i64, i64> {
    schema::minimum(schema::i64(), 1).describe("Account number")
  }
}

struct TenantAccount;

impl HasSchema for TenantAccount {
  type A = (String, i64);
  type I = (String, i64);
  type E = ();

  fn schema() -> Schema<Self::A, Self::I> {
    schema::struct_("tenant", schema::string(), "id", schema::i64())
  }
}

struct ListQuery;

impl HasSchema for ListQuery {
  type A = (Option<String>, i64, Vec<String>);
  type I = (Option<String>, i64, Vec<String>);
  type E = ();

  fn schema() -> Schema<Self::A, Self::I> {
    schema::struct3(
      "owner",
      optional(schema::string()),
      "limit",
      range(schema::i64(), 1, 100),
      "tag",
      schema::array(schema::string()).deprecated(),
    )
  }
}

struct Tenancy;

impl HasSchema for Tenancy {
  type A = (String, Option<String>);
  type I = (String, Option<String>);
  type E = ();

  fn schema() -> Schema<Self::A, Self::I> {
    schema::struct_(
      "x-tenant",
      schema::string(),
      "x-trace",
      optional(schema::string()),
    )
  }
}

struct NewAccount;

impl HasSchema for NewAccount {
  type A = (String, i64);
  type I = (String, i64);
  type E = ();

  fn schema() -> Schema<Self::A, Self::I> {
    schema::struct_(
      "owner",
      min_length(schema::string(), 1).describe("Account holder"),
      "balance",
      schema::i64(),
    )
  }
}

struct Account;

impl HasSchema for Account {
  type A = (i64, String, i64);
  type I = (i64, String, i64);
  type E = ();

  fn schema() -> Schema<Self::A, Self::I> {
    schema::struct3(
      "id",
      schema::i64(),
      "owner",
      schema::string(),
      "balance",
      schema::i64(),
    )
  }
}

struct Signup;

impl HasSchema for Signup {
  type A = (String, String);
  type I = (String, String);
  type E = ();

  fn schema() -> Schema<Self::A, Self::I> {
    schema::struct_(
      "email",
      schema::email(schema::string()),
      "password",
      min_length(schema::string(), 8),
    )
  }
}

struct Search;

impl HasSchema for Search {
  type A = String;
  type I = String;
  type E = ();

  fn schema() -> Schema<String, String> {
    schema::string()
  }
}

mod v2 {
  use id_effect::schema::{self, HasSchema, Schema};

  /// Same short name as the outer `Account`, another schema.
  pub struct Account;

  impl HasSchema for Account {
    type A = String;
    type I = String;
    type E = ();

    fn schema() -> Schema<String, String> {
      schema::string()
    }
  }
}

/// `Tree = { label: string, children: Tree[] }`, a named recursive definition. Only the wire
/// shape matters here, so the decoder is opaque.
fn tree_ast() -> SchemaAst {
  SchemaAst::new(SchemaKind::Lazy(LazyAst::new(Some("Tree".into()), || {
    SchemaAst::new(SchemaKind::Struct(vec![
      Field::required("label", SchemaAst::new(SchemaKind::String)),
      Field::required(
        "children",
        SchemaAst::new(SchemaKind::Array(Box::new(tree_ast()))),
      ),
    ]))
  })))
}

fn opaque(ast: SchemaAst) -> Schema<i64, i64> {
  Schema::<i64, i64, ()>::make(Ok, |n| n, |_| Ok(0)).with_ast(ast)
}

/// A request body holding a tree.
struct Forest;

impl HasSchema for Forest {
  type A = i64;
  type I = i64;
  type E = ();

  fn schema() -> Schema<i64, i64> {
    opaque(SchemaAst::new(SchemaKind::Struct(vec![Field::required(
      "root",
      tree_ast(),
    )])))
  }
}

/// A response that is a tree itself.
struct TreeDoc;

impl HasSchema for TreeDoc {
  type A = i64;
  type I = i64;
  type E = ();

  fn schema() -> Schema<i64, i64> {
    opaque(tree_ast())
  }
}

fn ok<P, Q, H, B>(_: &mut (), _: Input<P, Q, H, B>) -> Effect<&'static str, Infallible, ()> {
  succeed("ok")
}

/// A guard admitting everyone, named after its policy.
struct Policy(&'static str);

impl RouteGuard for Policy {
  fn policy(&self) -> &str {
    self.0
  }

  fn check<'a>(&'a self, _: &'a Parts) -> GuardFuture<'a> {
    Box::pin(async { Ok(()) })
  }
}

/// `endpoint` guarded by `policies` (what `id_effect_authz`'s `.authorize(..)` records).
fn guarded(endpoint: Endpoint, policies: &[&'static str]) -> Endpoint<(), Guarded> {
  let (first, rest) = policies
    .split_first()
    .expect("a guarded route has a policy");
  rest
    .iter()
    .fold(endpoint.guard(Policy(first)), |endpoint, policy| {
      endpoint.guard(Policy(policy))
    })
}

fn router() -> Router {
  Router::new()
    .route(
      Route::get("/accounts")
        .public()
        .query::<ListQuery>()
        .headers::<Tenancy>()
        .response::<Account>(StatusCode::OK)
        .handler(ok),
    )
    .route(guarded(
      Route::post("/accounts")
        .body::<NewAccount>()
        .response::<Account>(StatusCode::CREATED)
        .handler(ok),
      &["accounts.write", "staff"],
    ))
    .route(
      Route::get("/accounts/{id}")
        .public()
        .path::<AccountId>()
        .response::<Account>(StatusCode::OK)
        .response::<v2::Account>(StatusCode::OK)
        .response::<Search>(StatusCode::NOT_FOUND)
        .handler(ok),
    )
    .route(
      Route::get("/tenants/{tenant}/accounts/{id}")
        .public()
        .path::<TenantAccount>()
        .handler(ok),
    )
    .route(Route::post("/signup").public().form::<Signup>().handler(ok))
    .route(Route::get("/search").public().query::<Search>().handler(ok))
    .route(
      Route::put("/forests/{id}")
        .public()
        .body::<Forest>()
        .response::<TreeDoc>(StatusCode::OK)
        .handler(ok),
    )
    .route(Route::get("/files/{*rest}").public().handler(ok))
    .route(Route::get("/").public().handler(ok))
}

fn document() -> OpenApi {
  OpenApi::from_router(&router())
    .info(Info::new("Ledger", "1.2.0").with_summary("Accounts and entries"))
    .description("The **Ledger** reference API.")
    .server(Server::new("https://ledger.example").with_description("production"))
    .tag(Tag::new("accounts").with_description("Account management"))
    .bearer_jwt("bearer")
    .cookie_session("session", "id_session")
    .openid_connect(
      "oidc",
      "https://idp.example/.well-known/openid-configuration",
    )
}

fn assert_valid_openapi(doc: &Value) {
  let meta: Value = serde_json::from_str(META_SCHEMA).expect("meta-schema is JSON");
  let validator = jsonschema::draft202012::new(&meta).expect("meta-schema compiles");
  let errors: Vec<String> = validator
    .iter_errors(doc)
    .map(|e| format!("{} at {}", e, e.instance_path()))
    .collect();
  assert!(
    errors.is_empty(),
    "invalid OpenAPI 3.1 document: {errors:#?}"
  );
  // The meta-schema does not look into Schema Objects: check them against JSON Schema 2020-12.
  let schemas = doc["components"]["schemas"].as_object();
  for (name, schema) in schemas.into_iter().flatten() {
    assert!(jsonschema::meta::is_valid(schema), "{name}: {schema}");
  }
}

/// Every `$ref` in `value`.
fn refs(value: &Value, out: &mut Vec<String>) {
  match value {
    Value::Object(map) => {
      if let Some(Value::String(r)) = map.get("$ref") {
        out.push(r.clone());
      }
      map.values().for_each(|v| refs(v, out));
    }
    Value::Array(items) => items.iter().for_each(|v| refs(v, out)),
    _ => {}
  }
}

#[test]
fn the_document_validates_against_the_openapi_31_meta_schema() {
  let doc = document().to_json();
  assert_eq!(doc["openapi"], "3.1.0");
  assert_eq!(doc["info"]["title"], "Ledger");
  assert_eq!(doc["info"]["summary"], "Accounts and entries");
  assert_eq!(doc["servers"][0]["url"], "https://ledger.example");
  assert_valid_openapi(&doc);

  // The validator does reject a broken document.
  let meta: Value = serde_json::from_str(META_SCHEMA).unwrap();
  let validator = jsonschema::draft202012::new(&meta).unwrap();
  let mut broken = doc.clone();
  broken["paths"]["/accounts"]["get"]["parameters"][0]["in"] = json!("body");
  assert!(!validator.is_valid(&broken));
  assert!(!validator.is_valid(&json!({ "openapi": "3.1.0" })));

  // Every reference resolves inside the document.
  let mut all = Vec::new();
  refs(&doc, &mut all);
  assert!(!all.is_empty());
  for r in all {
    let pointer = r.strip_prefix('#').expect("local reference");
    assert!(doc.pointer(pointer).is_some(), "dangling {r}");
  }

  // An empty router still gives a valid document.
  assert_valid_openapi(&OpenApi::from_router(&Router::<()>::new()).to_json());
}

#[test]
fn component_schemas_are_the_schemas_the_extractors_decode_with() {
  let doc = document().to_json();
  let schemas = &doc["components"]["schemas"];
  assert_eq!(schemas["NewAccount"], json_schema_for::<NewAccount>());
  assert_eq!(schemas["Account"], json_schema_for::<Account>());
  assert_eq!(schemas["Signup"], json_schema_for::<Signup>());
  assert_eq!(schemas["Search"], json_schema_for::<Search>());
  // Same short name, another type: a second component, not a merge.
  assert_eq!(schemas["Account_2"], json_schema_for::<v2::Account>());

  let post = &doc["paths"]["/accounts"]["post"];
  assert_eq!(
    post["requestBody"]["content"]["application/json"]["schema"]["$ref"],
    "#/components/schemas/NewAccount"
  );
  assert_eq!(post["requestBody"]["required"], true);
  assert_eq!(
    post["responses"]["201"]["content"]["application/json"]["schema"]["$ref"],
    "#/components/schemas/Account"
  );
  // Constraints and annotations travel with the schema.
  assert_eq!(schemas["NewAccount"]["properties"]["owner"]["minLength"], 1);
  assert_eq!(
    schemas["NewAccount"]["properties"]["owner"]["description"],
    "Account holder"
  );

  // Two documented bodies for one status are alternatives.
  let get = &doc["paths"]["/accounts/{id}"]["get"];
  assert_eq!(
    get["responses"]["200"]["content"]["application/json"]["schema"],
    json!({ "anyOf": [
      { "$ref": "#/components/schemas/Account" },
      { "$ref": "#/components/schemas/Account_2" }
    ] })
  );
  assert_eq!(get["responses"]["200"]["description"], "OK");
  assert_eq!(get["responses"]["404"]["description"], "Not Found");
}

#[test]
fn parameters_come_from_the_path_query_and_header_schemas() {
  let doc = document();

  let by_id = doc.operation(&Method::GET, "/accounts/{id}").unwrap();
  assert_eq!(by_id.operation_id, "get_accounts_by_id");
  assert_eq!(by_id.tags, ["accounts"]);
  let id = &by_id.parameters[0];
  assert_eq!(
    (id.name.as_str(), id.location, id.required),
    ("id", ParameterIn::Path, true)
  );
  assert_eq!(id.description.as_deref(), Some("Account number"));
  let mut expected = json_schema_for::<AccountId>();
  expected.as_object_mut().unwrap().remove("$schema");
  assert_eq!(id.schema, expected);

  let nested = doc
    .operation(&Method::GET, "/tenants/{tenant}/accounts/{id}")
    .unwrap();
  let typed: Vec<(&str, &str)> = nested
    .parameters
    .iter()
    .map(|p| (p.name.as_str(), p.schema["type"].as_str().unwrap()))
    .collect();
  assert_eq!(typed, [("tenant", "string"), ("id", "integer")]);

  let list = doc.operation(&Method::GET, "/accounts").unwrap();
  let params: Vec<(&str, ParameterIn, bool, bool)> = list
    .parameters
    .iter()
    .map(|p| (p.name.as_str(), p.location, p.required, p.deprecated))
    .collect();
  assert_eq!(
    params,
    [
      // Absent nullable / array query keys decode as null / [], so they are optional.
      ("limit", ParameterIn::Query, true, false),
      ("owner", ParameterIn::Query, false, false),
      ("tag", ParameterIn::Query, false, true),
      ("x-tenant", ParameterIn::Header, true, false),
      ("x-trace", ParameterIn::Header, false, false),
    ]
  );
  assert_eq!(list.parameters[0].schema["maximum"], 100);

  // A scalar query schema is one free-form `form` parameter.
  let search = doc.operation(&Method::GET, "/search").unwrap();
  let p = &search.parameters[0];
  assert_eq!(p.name, "Search");
  assert_eq!((p.style.as_deref(), p.explode), (Some("form"), Some(true)));

  // Undeclared path parameters are strings; Axum wildcards are plain OpenAPI parameters.
  let files = doc.operation(&Method::GET, "/files/{rest}").unwrap();
  assert_eq!(files.operation_id, "get_files_by_rest");
  assert_eq!(files.parameters[0].schema, json!({ "type": "string" }));
  assert!(files.parameters[0].required);
  let root = doc.operation(&Method::GET, "/").unwrap();
  assert_eq!(root.operation_id, "get_root");
  assert!(root.tags.is_empty());
}

#[test]
fn bodies_list_their_media_types_and_problem_responses_are_documented() {
  let doc = document().to_json();
  let signup = &doc["paths"]["/signup"]["post"];
  let media: Vec<&String> = signup["requestBody"]["content"]
    .as_object()
    .unwrap()
    .keys()
    .collect();
  assert_eq!(
    media,
    ["application/x-www-form-urlencoded", "multipart/form-data"]
  );
  // A form body is not JSON: no 400 for malformed JSON, only the 422.
  assert!(signup["responses"].get("400").is_none());
  let problem = json!({ "$ref": "#/components/schemas/Problem" });
  assert_eq!(
    signup["responses"]["422"]["content"][PROBLEM_JSON]["schema"],
    problem
  );

  let post = &doc["paths"]["/accounts"]["post"];
  for status in ["400", "401", "403", "422"] {
    assert_eq!(
      post["responses"][status]["content"][PROBLEM_JSON]["schema"], problem,
      "{status}"
    );
  }
  assert_eq!(
    post["responses"]["422"]["description"],
    "Unprocessable Entity"
  );

  // A route without input or documented responses gets a default response only.
  let root = &doc["paths"]["/"]["get"]["responses"];
  assert_eq!(*root, json!({ "default": { "description": "Response" } }));
}

#[tokio::test]
async fn the_problem_schema_describes_what_the_extractors_answer() {
  let doc = document().to_json();
  let problem_schema = doc["components"]["schemas"]["Problem"].clone();
  let validator = jsonschema::validator_for(&problem_schema).expect("Problem schema compiles");

  let app = router().into_axum();
  let req = Request::builder()
    .method(Method::GET)
    .uri("/accounts?limit=500&owner=ada")
    .body(Body::empty())
    .unwrap();
  let res = app.oneshot(req).await.unwrap();
  assert_eq!(res.status(), StatusCode::UNPROCESSABLE_ENTITY);
  let bytes = res.into_body().collect().await.unwrap().to_bytes();
  let body: Value = serde_json::from_slice(&bytes).unwrap();
  assert!(!body["errors"].as_array().unwrap().is_empty());
  let errors: Vec<String> = validator
    .iter_errors(&body)
    .map(|e| e.to_string())
    .collect();
  assert!(errors.is_empty(), "{body}: {errors:?}");
}

#[test]
fn security_schemes_are_declared_and_required_by_guarded_operations() {
  let doc = document();
  let json = doc.to_json();
  let schemes = &json["components"]["securitySchemes"];
  assert_eq!(
    schemes["bearer"],
    json!({ "type": "http", "scheme": "bearer", "bearerFormat": "JWT" })
  );
  assert_eq!(
    schemes["session"],
    json!({ "type": "apiKey", "name": "id_session", "in": "cookie" })
  );
  assert_eq!(
    schemes["oidc"],
    json!({
      "type": "openIdConnect",
      "openIdConnectUrl": "https://idp.example/.well-known/openid-configuration"
    })
  );

  let post = &json["paths"]["/accounts"]["post"];
  assert_eq!(
    post["security"],
    json!([
      { "bearer": ["accounts.write", "staff"] },
      { "session": ["accounts.write", "staff"] },
      { "oidc": [] }
    ])
  );
  assert_eq!(post["x-policies"], json!(["accounts.write", "staff"]));
  // Public routes declare that they need no authentication, whatever schemes exist.
  assert_eq!(json["paths"]["/accounts"]["get"]["security"], json!([]));
  assert!(
    json["paths"]["/accounts"]["get"]
      .get("x-policies")
      .is_none()
  );

  // Re-declaring a scheme replaces it without duplicating the requirement.
  let doc = doc.security_scheme(
    "session",
    SecurityScheme::api_key(ApiKeyLocation::Header, "x-api-key").with_description("API key"),
  );
  let op = doc.operation(&Method::POST, "/accounts").unwrap();
  assert_eq!(op.security.as_ref().unwrap().len(), 3);
  assert_eq!(
    doc.to_json()["components"]["securitySchemes"]["session"],
    json!({ "type": "apiKey", "name": "x-api-key", "in": "header", "description": "API key" })
  );
  let basic = SecurityScheme::basic().with_description("staff only");
  assert_eq!(
    serde_json::to_value(&basic).unwrap(),
    json!({ "type": "http", "scheme": "basic", "description": "staff only" })
  );
  let oidc = SecurityScheme::openid_connect("https://idp/.well-known").with_description("IdP");
  assert_eq!(serde_json::to_value(&oidc).unwrap()["description"], "IdP");
  assert_valid_openapi(&doc.to_json());
}

#[test]
fn recursive_definitions_become_components_of_their_own() {
  let doc = document().to_json();
  let schemas = &doc["components"]["schemas"];
  // The `Tree` definition of both declaring types is hoisted once.
  let tree = &schemas["Tree"];
  assert_eq!(
    tree["properties"]["children"]["items"]["$ref"],
    "#/components/schemas/Tree"
  );
  assert!(schemas.get("Tree_2").is_none());
  assert_eq!(
    schemas["Forest"]["properties"]["root"]["$ref"],
    "#/components/schemas/Tree"
  );
  assert_eq!(schemas["TreeDoc"]["$ref"], "#/components/schemas/Tree");

  // Identity with the exported document, up to where `$defs` live.
  let mut exported = json_schema_for::<Forest>();
  let defs = exported
    .as_object_mut()
    .unwrap()
    .remove("$defs")
    .expect("named recursion is exported under $defs");
  assert_eq!(exported["properties"]["root"]["$ref"], "#/$defs/Tree");
  exported["properties"]["root"]["$ref"] = json!("#/components/schemas/Tree");
  assert_eq!(schemas["Forest"], exported);
  let mut def = defs["Tree"].clone();
  def["properties"]["children"]["items"]["$ref"] = json!("#/components/schemas/Tree");
  assert_eq!(*tree, def);
}

#[test]
fn documents_are_editable_serde_data() {
  let mut doc = document();
  doc
    .operation_mut(&Method::GET, "/accounts")
    .expect("documented")
    .summary = Some("List accounts".to_owned());
  assert!(doc.operation_mut(&Method::GET, "/nope").is_none());
  assert!(doc.operation(&Method::DELETE, "/accounts").is_none());
  let text = doc.to_json_pretty();
  let back: OpenApi = serde_json::from_str(&text).expect("round-trips");
  assert_eq!(back, doc);
  assert_eq!(
    back
      .operation(&Method::GET, "/accounts")
      .unwrap()
      .summary
      .as_deref(),
    Some("List accounts")
  );

  let doc = OpenApi::default()
    .title("T")
    .version("2")
    .tag(Tag::new("a"))
    .tag(Tag::new("a").with_description("replaced"));
  assert_eq!(
    (doc.info.title.as_str(), doc.info.version.as_str()),
    ("T", "2")
  );
  assert_eq!(doc.tags.len(), 1);
  assert_eq!(doc.tags[0].description.as_deref(), Some("replaced"));
  assert_eq!(
    Info::default().with_description("d").description.as_deref(),
    Some("d")
  );
}

#[test]
fn operation_ids_are_unique_and_undescribable_methods_are_skipped() {
  let routes = [
    RouteInfo::new(Method::GET, "/a-b"),
    RouteInfo::new(Method::GET, "/a_b"),
    RouteInfo::new(Method::CONNECT, "/tunnel"),
    RouteInfo::new(Method::HEAD, "/a-b"),
    RouteInfo::new(Method::OPTIONS, "/a-b"),
    RouteInfo::new(Method::TRACE, "/a-b"),
    RouteInfo::new(Method::PATCH, "/a-b"),
    RouteInfo::new(Method::DELETE, "/a-b"),
    RouteInfo::new(Method::PUT, "/a-b"),
  ];
  let mut doc = OpenApi::from_routes(&routes);
  assert_eq!(
    doc.operation(&Method::GET, "/a-b").unwrap().operation_id,
    "get_a_b"
  );
  assert_eq!(
    doc.operation(&Method::GET, "/a_b").unwrap().operation_id,
    "get_a_b_2"
  );
  assert!(!doc.paths.contains_key("/tunnel"));
  assert!(doc.operation(&Method::CONNECT, "/a-b").is_none());
  assert_eq!(doc.operations_mut().count(), 8);
  for method in [
    Method::HEAD,
    Method::OPTIONS,
    Method::TRACE,
    Method::PATCH,
    Method::DELETE,
    Method::PUT,
  ] {
    assert!(doc.operation(&method, "/a-b").is_some(), "{method}");
    assert!(doc.operation_mut(&method, "/a-b").is_some(), "{method}");
  }
  assert!(doc.operation_mut(&Method::CONNECT, "/a-b").is_none());
  assert_valid_openapi(&doc.to_json());
}

#[test]
fn declared_responses_win_over_the_standard_problem_ones() {
  // The route documents its own 400; the generated 422 is still added.
  let mut info = Route::post("/notes")
    .body::<Search>()
    .response::<v2::Account>(StatusCode::BAD_REQUEST)
    .info();
  // Header schemas that are not structs have no named parameters.
  info.headers = Some(SchemaRef::of::<Search>());
  let doc = OpenApi::from_routes([&info]);
  let op = doc.operation(&Method::POST, "/notes").unwrap();
  assert!(op.parameters.is_empty());
  assert_eq!(
    op.responses["400"].content["application/json"].schema,
    json!({ "$ref": "#/components/schemas/Account" })
  );
  assert!(op.responses["422"].content.contains_key(PROBLEM_JSON));
  assert_valid_openapi(&doc.to_json());
}
