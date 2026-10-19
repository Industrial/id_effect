//! web.rpc #7: OpenAPI 3.1 from the contract (r12-e12-4-openapi-asyncapi), validated offline
//! against the official OpenAPI 3.1 meta-schema, and the served documents closed in prod
//! unless guarded (ADR 0013 R7).

use axum::body::Body;
use axum::http::request::Parts;
use axum::http::{Request, StatusCode, header};
use axum::response::IntoResponse;
use http_body_util::BodyExt;
use id_effect_axum::route::GuardFuture;
use id_effect_rpc::contract::{
  Contract, ProcedureAccess, ProcedureContract, ProcedureKind, ServiceContract,
};
use id_effect_rpc::openapi::{ContractDocs, OpenApiOptions, document, to_json, to_yaml};
use id_effect_rpc::registry::RouteGuard;
use serde_json::{Value, json};
use tower::ServiceExt;

/// The official OpenAPI 3.1 document schema, vendored by id_effect_axum
/// (`crates/id_effect_axum/tests/fixtures/openapi/README.md`: source and sha256).
const META_SCHEMA: &str =
  include_str!("../../id_effect_axum/tests/fixtures/openapi/oas-3.1-schema-2022-10-07.json");

const DIALECT: &str = "https://json-schema.org/draft/2020-12/schema";

/// The macro's contract snapshot: Todos v1 (every kind) and Greeter v1, v2.
fn fixture() -> Contract {
  Contract::from_json(include_str!("../rpc.contract.json")).expect("fixture contract")
}

/// A contract whose schemas carry `$defs` with colliding type names, and two error types that
/// both declare `Missing` with other fields.
fn with_defs() -> Contract {
  let defs = json!({
    "app::todo::Item": {
      "type": "object",
      "properties": {
        "title": { "type": "string" },
        "tag": { "$ref": "#/$defs/app::tag::Item" },
      },
      "required": ["title"],
    },
    "app::tag::Item": { "type": "string", "const": { "$ref": "#/$defs/app::tag::Item" } },
  });
  let item = json!({ "$schema": DIALECT, "$ref": "#/$defs/app::todo::Item", "$defs": defs });
  let missing = |field: &str| {
    json!({
      "oneOf": [{
        "type": "object",
        "properties": { "_tag": { "const": "Missing" }, field: { "$ref": "#/$defs/app::todo::Item" } },
        "required": ["_tag", field],
      }],
      "$defs": defs,
    })
  };
  let mut put = ProcedureContract::new(
    "put",
    ProcedureKind::Mutation,
    ProcedureAccess::Public {},
    item.clone(),
    json!({ "$schema": DIALECT, "type": "null" }),
  )
  .with_error(missing("item"));
  put.deprecated = true;
  // An error schema that is no union, and a union arm without a `_tag`.
  let raw = ProcedureContract::new(
    "raw",
    ProcedureKind::Query,
    ProcedureAccess::Public {},
    json!({}),
    json!({}),
  )
  .with_error(json!({ "type": "string" }));
  let odd = ProcedureContract::new(
    "odd",
    ProcedureKind::Query,
    ProcedureAccess::Public {},
    json!({}),
    json!({}),
  )
  .with_error(json!({ "oneOf": [{ "type": "object" }] }));
  // The same definition key with another body, from another build.
  let dup = |n: i64| json!({ "$ref": "#/$defs/app::Dup", "$defs": { "app::Dup": { "const": n } } });
  let other = ServiceContract::new("Other", 1).with_procedure(ProcedureContract::new(
    "one",
    ProcedureKind::Query,
    ProcedureAccess::Public {},
    dup(1),
    dup(2),
  ));
  let store = ServiceContract::new("Store", 1)
    .with_procedure(put)
    .with_procedure(raw)
    .with_procedure(odd)
    .with_procedure(
      ProcedureContract::new(
        "find",
        ProcedureKind::Query,
        ProcedureAccess::Guarded {
          policies: vec!["staff".to_owned()],
        },
        json!({ "$schema": DIALECT, "anyOf": [{ "type": "string" }, { "type": "null" }] }),
        item,
      )
      .with_error(missing("hint")),
    );
  Contract::new().with_service(store).with_service(other)
}

fn options() -> OpenApiOptions {
  OpenApiOptions::new("Todos API", "1.0.0")
    .with_description("The fixture services.")
    .with_server("https://api.example")
}

fn assert_valid_openapi(doc: &Value) {
  let meta: Value = serde_json::from_str(META_SCHEMA).expect("meta-schema is JSON");
  let validator = jsonschema::draft202012::new(&meta).expect("meta-schema compiles");
  let errors: Vec<String> = validator
    .iter_errors(doc)
    .map(|e| format!("{e} at {}", e.instance_path()))
    .collect();
  assert!(
    errors.is_empty(),
    "invalid OpenAPI 3.1 document: {errors:#?}"
  );
  // The meta-schema does not look into Schema Objects: check them against JSON Schema 2020-12.
  for (name, schema) in doc["components"]["schemas"]
    .as_object()
    .into_iter()
    .flatten()
  {
    assert!(jsonschema::meta::is_valid(schema), "{name}: {schema}");
  }
  let mut all = Vec::new();
  refs(doc, &mut all);
  for r in all {
    let pointer = r.strip_prefix('#').expect("local reference");
    assert!(doc.pointer(pointer).is_some(), "dangling {r}");
  }
}

/// Every `$ref` in `value`, outside literal values.
fn refs(value: &Value, out: &mut Vec<String>) {
  match value {
    Value::Object(map) => {
      if let Some(Value::String(r)) = map.get("$ref") {
        out.push(r.clone());
      }
      for (key, child) in map {
        if key != "const" {
          refs(child, out);
        }
      }
    }
    Value::Array(items) => items.iter().for_each(|v| refs(v, out)),
    _ => {}
  }
}

/// Whether `instance` validates against the component `name` of `doc`.
fn conforms(doc: &Value, name: &str, instance: &Value) -> bool {
  let mut root = doc.clone();
  root["$schema"] = json!(DIALECT);
  root["$ref"] = json!(format!("#/components/schemas/{name}"));
  jsonschema::draft202012::new(&root)
    .expect("document schemas compile")
    .is_valid(instance)
}

#[test]
fn openapi_31_validates_against_meta_schema() {
  let doc = document(&fixture(), &options());
  assert_eq!(doc["openapi"], "3.1.0");
  assert_eq!(doc["jsonSchemaDialect"], DIALECT);
  assert_eq!(doc["info"]["title"], "Todos API");
  assert_eq!(doc["info"]["description"], "The fixture services.");
  assert_eq!(doc["servers"][0]["url"], "https://api.example");
  assert_eq!(
    doc["tags"],
    json!([
      { "name": "Greeter", "description": "Says hello, politely if asked." },
      { "name": "Todos", "description": "Keeps todos." },
    ])
  );
  assert_valid_openapi(&doc);
  assert_valid_openapi(&document(&with_defs(), &OpenApiOptions::default()));
  assert_valid_openapi(&document(&Contract::new(), &OpenApiOptions::default()));

  // The validator does reject a broken document.
  let meta: Value = serde_json::from_str(META_SCHEMA).unwrap();
  let validator = jsonschema::draft202012::new(&meta).unwrap();
  let mut broken = doc.clone();
  broken["paths"]["/rpc/Todos.get"]["get"]["parameters"][0]["in"] = json!("body");
  assert!(!validator.is_valid(&broken));
}

#[test]
fn openapi_error_union_is_one_of_with_discriminator() {
  let doc = document(&fixture(), &options());
  let schemas = &doc["components"]["schemas"];
  let union = &schemas["Todos-v1-get-Error"];
  let arms: Vec<&str> = union["oneOf"]
    .as_array()
    .unwrap()
    .iter()
    .map(|arm| arm["$ref"].as_str().unwrap())
    .collect();
  assert_eq!(
    arms,
    [
      "#/components/schemas/error-NotFound",
      "#/components/schemas/error-Locked",
      "#/components/schemas/error-TooLong",
      "#/components/schemas/error-InternalError",
      "#/components/schemas/error-InvalidInput",
      "#/components/schemas/error-DeadlineExceeded",
      "#/components/schemas/error-Cancelled",
      "#/components/schemas/error-ResourceExhausted",
    ]
  );
  assert_eq!(union["discriminator"]["propertyName"], "_tag");
  assert_eq!(
    union["discriminator"]["mapping"]["NotFound"],
    "#/components/schemas/error-NotFound"
  );
  assert_eq!(
    union["discriminator"]["mapping"]["InternalError"],
    "#/components/schemas/error-InternalError"
  );
  // The variant keeps its fields; the optional field stays optional; the internal variant is
  // not there.
  assert_eq!(schemas["error-NotFound"]["required"], json!(["_tag", "id"]));
  assert_eq!(schemas["error-TooLong"]["required"], json!(["_tag", "max"]));
  assert!(schemas.get("error-Internal").is_none());
  // A procedure without a typed error still lists the reserved failures.
  let count = &schemas["Greeter-v1-greet-Error"];
  assert_eq!(count["oneOf"].as_array().unwrap().len(), 5);

  // Real wire results conform: a success, a typed failure, a reserved one.
  let ok = json!({"tag": "Todos.get", "exit": {"_tag": "Success", "value": {"id": 1, "title": "a", "done": false}}});
  let typed = json!({"id": 3, "tag": "Todos.get", "exit": {"_tag": "Failure", "error": {"_tag": "NotFound", "id": 9}}});
  let internal = json!({"tag": "Todos.get", "exit": {"_tag": "Failure", "error": {"_tag": "InternalError", "id": "c-1"}}});
  for result in [&ok, &typed, &internal] {
    assert!(conforms(&doc, "Todos-v1-get-Result", result), "{result}");
  }
  let unknown = json!({"tag": "Todos.get", "exit": {"_tag": "Failure", "error": {"_tag": "Nope"}}});
  assert!(!conforms(&doc, "Todos-v1-get-Result", &unknown));
  let wrong_tag = json!({"tag": "Todos.add", "exit": {"_tag": "Success", "value": {"id": 1, "title": "a", "done": false}}});
  assert!(!conforms(&doc, "Todos-v1-get-Result", &wrong_tag));
}

#[test]
fn openapi_describes_both_query_forms_mutations_streams_and_the_batch_endpoint() {
  let doc = document(&fixture(), &options());
  let paths = &doc["paths"];

  // Queries: GET with `?input=` and an ETag, and POST with `x-rpc`.
  let get = &paths["/rpc/Todos.get"]["get"];
  assert_eq!(get["operationId"], "Todos.get_GET");
  assert_eq!(get["x-rpc-kind"], "query");
  assert_eq!(get["x-rpc-access"], json!({"public": {}}));
  let names = |op: &Value| -> Vec<String> {
    op["parameters"]
      .as_array()
      .unwrap()
      .iter()
      .map(|p| p["name"].as_str().unwrap().to_owned())
      .collect()
  };
  assert_eq!(
    names(get),
    [
      "x-rpc-version",
      "x-rpc-deadline-ms",
      "x-correlation-id",
      "input",
      "If-None-Match"
    ]
  );
  let input = &get["parameters"][3];
  assert_eq!(input["in"], "query");
  assert_eq!(input["required"], true);
  assert_eq!(
    input["content"]["application/json"]["schema"]["type"],
    "integer"
  );
  assert!(get["responses"]["200"]["headers"]["ETag"].is_object());
  assert!(get["responses"]["304"].is_object());
  assert!(get.get("requestBody").is_none());
  // A `null` input may be left out.
  let count = &paths["/rpc/Todos.count"]["get"];
  assert_eq!(count["parameters"][3]["required"], false);
  let post = &paths["/rpc/Todos.get"]["post"];
  assert_eq!(post["parameters"][0]["name"], "x-rpc");
  assert_eq!(post["parameters"][0]["required"], true);
  assert_eq!(post["parameters"][0]["schema"]["const"], "1");
  for status in [
    "200", "400", "403", "404", "413", "415", "422", "429", "500", "504",
  ] {
    assert!(post["responses"][status].is_object(), "POST {status}");
  }
  assert!(
    post["responses"]["400"]["content"]["application/problem+json"].is_object(),
    "400 also carries a Problem"
  );
  assert_eq!(
    post["responses"]["404"]["content"]["application/problem+json"]["schema"]["$ref"],
    "#/components/schemas/rpc-Problem"
  );

  // Mutations: POST only, guarded, idempotency key.
  let add = &paths["/rpc/Todos.add"];
  assert!(add.get("get").is_none());
  let add = &add["post"];
  assert_eq!(add["x-rpc-idempotent"], true);
  assert_eq!(
    add["x-rpc-access"],
    json!({"guarded": {"policies": ["staff"]}})
  );
  let key = add["parameters"]
    .as_array()
    .unwrap()
    .iter()
    .find(|p| p["name"] == "idempotency-key")
    .expect("Idempotency-Key");
  assert_eq!(key["schema"]["maxLength"], 128);
  assert_eq!(key["schema"]["minLength"], 1);
  assert!(add["responses"]["401"].is_object());
  assert!(add["responses"]["409"].is_object());
  assert!(add["responses"]["200"]["headers"]["idempotency-replayed"].is_object());
  // A public mutation needs the anonymous key length.
  let put = &document(&with_defs(), &OpenApiOptions::default())["paths"]["/rpc/Store.put"]["post"];
  let key = put["parameters"]
    .as_array()
    .unwrap()
    .iter()
    .find(|p| p["name"] == "idempotency-key")
    .unwrap();
  assert_eq!(key["schema"]["minLength"], 16);

  // Server streams answer NDJSON frames; client streams send NDJSON items with their caps.
  let upto = &paths["/rpc/Todos.upto"]["post"];
  assert_eq!(
    upto["responses"]["200"]["content"]["application/x-ndjson"]["schema"]["$ref"],
    "#/components/schemas/Todos-v1-upto-Frame"
  );
  assert!(upto["responses"].get("500").is_none());
  let sum = &paths["/rpc/Todos.sum"]["post"];
  assert!(sum["requestBody"]["content"]["application/x-ndjson"].is_object());
  assert_eq!(sum["x-rpc-max-items"], 100);
  assert_eq!(sum["x-rpc-max-bytes"], 4_194_304);
  // Two-way streams and subscriptions are AsyncAPI's.
  for tag in ["Todos.shout", "Todos.ticks", "Todos.titles"] {
    assert!(paths.get(format!("/rpc/{tag}")).is_none(), "{tag}");
  }

  // Versions: the path is the newest version; the envelope body lists every version.
  let greet = &paths["/rpc/Greeter.greet"]["get"];
  assert_eq!(greet["x-rpc-version"], 2);
  assert_eq!(greet["parameters"][0]["schema"]["enum"], json!([1, 2]));
  assert_eq!(greet["parameters"][0]["schema"]["default"], 2);
  let schemas = &doc["components"]["schemas"];
  assert_eq!(
    schemas["Greeter-v1-greet-Call"]["required"],
    json!(["tag", "payload", "v"])
  );
  assert_eq!(
    schemas["Greeter-v2-greet-Call"]["required"],
    json!(["tag", "payload"])
  );
  let calls = schemas["rpc-Call"]["anyOf"].as_array().unwrap();
  assert_eq!(
    calls.len(),
    6,
    "Greeter greet v1+v2, history; Todos add, count, get"
  );

  // The envelope route: one call or a batch of at most 50.
  let envelope = &paths["/rpc"]["post"];
  let body = &envelope["requestBody"]["content"]["application/json"]["schema"]["oneOf"];
  assert_eq!(body[1]["maxItems"], 50);
  assert!(
    envelope["parameters"]
      .as_array()
      .unwrap()
      .iter()
      .any(|p| p["name"] == "x-rpc-ordered")
  );
  let batch = json!([
    {"id": 1, "tag": "Todos.get", "payload": 1},
    {"id": 2, "tag": "Greeter.greet", "v": 1, "payload": "Ada"},
  ]);
  let mut root = doc.clone();
  root["$schema"] = json!(DIALECT);
  root["$ref"] = json!("#/paths/~1rpc/post/requestBody/content/application~1json/schema");
  let validator = jsonschema::draft202012::new(&root).unwrap();
  assert!(validator.is_valid(&batch));
  assert!(
    validator
      .is_valid(&json!({"tag": "Greeter.greet", "payload": {"name": "Ada", "polite": true}}))
  );
  assert!(
    !validator.is_valid(&json!({"tag": "Greeter.greet", "payload": "Ada"})),
    "v1 needs v"
  );
  assert!(!validator.is_valid(&json!([])));
}

#[test]
fn openapi_hoists_defs_under_stable_names() {
  let doc = document(
    &with_defs(),
    &OpenApiOptions::new("Store", "1").with_path("/api/"),
  );
  let schemas = &doc["components"]["schemas"];
  // `app::todo::Item` and `app::tag::Item` share their last segment.
  assert_eq!(schemas["todo.Item"]["required"], json!(["title"]));
  assert_eq!(
    schemas["todo.Item"]["properties"]["tag"]["$ref"],
    "#/components/schemas/tag.Item"
  );
  // A literal that looks like a reference is left alone.
  assert_eq!(
    schemas["tag.Item"]["const"],
    json!({"$ref": "#/$defs/app::tag::Item"})
  );
  let put = &doc["paths"]["/api/Store.put"]["post"];
  assert_eq!(
    put["requestBody"]["content"]["application/json"]["schema"],
    json!({"$ref": "#/components/schemas/todo.Item"})
  );
  // Two `Missing` variants with other fields: both named by hash, deterministically.
  let missing: Vec<&String> = schemas
    .as_object()
    .unwrap()
    .keys()
    .filter(|k| k.starts_with("error-Missing"))
    .collect();
  assert_eq!(missing.len(), 2);
  assert!(
    missing
      .iter()
      .all(|k| k.len() == "error-Missing-".len() + 8)
  );
  assert!(schemas.get("error-Missing").is_none());
  // A guarded GET documents the guard's refusals; an `anyOf` with `null` input is optional.
  let find = &doc["paths"]["/api/Store.find"]["get"];
  assert!(find["responses"]["401"].is_object());
  assert!(find["responses"]["403"].is_object());
  assert_eq!(find["parameters"][3]["required"], false);
  assert!(doc["paths"]["/api"]["post"].is_object());
  // One key with two bodies: the body that sorts first keeps the name.
  assert_eq!(schemas["Dup"], json!({"const": 1}));
  assert_eq!(schemas["Dup_2"], json!({"const": 2}));
  assert!(put["deprecated"].as_bool().unwrap());
  // Error schemas that are no `_tag` union are kept as arms.
  assert_eq!(
    schemas["Store-v1-raw-Error"]["oneOf"][0],
    json!({"type": "string"})
  );
  assert_eq!(
    schemas["Store-v1-odd-Error"]["oneOf"][0],
    json!({"type": "object"})
  );
  // At the root, the envelope is `/`.
  let root = document(&with_defs(), &OpenApiOptions::default().with_path("/"));
  assert!(root["paths"]["/"]["post"].is_object());
  assert!(root["paths"]["/Store.put"]["post"].is_object());
}

#[test]
fn openapi_output_is_deterministic() {
  let a = to_json(&fixture(), &options());
  let b = to_json(&fixture(), &options());
  assert_eq!(a, b);
  assert!(a.ends_with("}\n"));
  assert!(a.starts_with("{\n  \"components\""));
  // The same services added in another order give the same text.
  let services = fixture().services;
  let reversed = services
    .into_iter()
    .rev()
    .fold(Contract::new(), Contract::with_service);
  assert_eq!(to_json(&reversed, &options()), a);
  // Keys are sorted at every level.
  fn sorted(v: &Value) -> bool {
    match v {
      Value::Object(m) => {
        let keys: Vec<&String> = m.keys().collect();
        keys.windows(2).all(|w| w[0] <= w[1]) && m.values().all(sorted)
      }
      Value::Array(items) => items.iter().all(sorted),
      _ => true,
    }
  }
  assert!(sorted(&serde_json::from_str(&a).unwrap()));
  let yaml = to_yaml(&fixture(), &options()).unwrap();
  assert!(yaml.contains("openapi: 3.1.0"));
}

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

async fn status(app: &axum::Router, uri: &str, staff: bool) -> StatusCode {
  let mut req = Request::get(uri);
  if staff {
    req = req.header("x-staff", "yes");
  }
  let res = app
    .clone()
    .oneshot(req.body(Body::empty()).unwrap())
    .await
    .unwrap();
  res.status()
}

const DOCS: [&str; 3] = ["/rpc/openapi.json", "/rpc/asyncapi.json", "/rpc/_contract"];

#[tokio::test]
async fn r7_rpc_docs_closed_in_prod() {
  // Unguarded in prod: nothing is mounted.
  let docs = ContractDocs::new(fixture()).profile("prod");
  assert!(!docs.is_mounted());
  let prod: axum::Router = docs.router();
  for uri in DOCS {
    assert_eq!(
      status(&prod, uri, true).await,
      StatusCode::NOT_FOUND,
      "{uri}"
    );
  }
  // Next to the RPC routes, the path is an unknown procedure.
  let rpc: axum::Router = id_effect_rpc::RpcServer::new(id_effect_rpc::RpcGroup::new())
    .mount_default()
    .with_state(id_effect::DynEnv::new())
    .merge(ContractDocs::new(fixture()).profile("prod").router());
  assert_eq!(
    status(&rpc, "/rpc/openapi.json", false).await,
    StatusCode::NOT_FOUND
  );

  // Guarded: mounted in prod, behind the guard.
  let guarded: axum::Router = ContractDocs::new(fixture())
    .profile("prod")
    .guard(Staff)
    .router();
  for uri in DOCS {
    assert_eq!(
      status(&guarded, uri, false).await,
      StatusCode::FORBIDDEN,
      "{uri}"
    );
    assert_eq!(status(&guarded, uri, true).await, StatusCode::OK, "{uri}");
  }

  // Explicitly public: mounted and recorded.
  let public = ContractDocs::new(fixture())
    .profile("prod")
    .public_dangerous();
  assert!(public.is_mounted());
  assert!(format!("{public:?}").contains("public: true"));
  assert!(
    id_effect::observability::posture::dangerous_opt_outs()
      .iter()
      .any(|(id, _)| *id == "id_effect_rpc::openapi::ContractDocs::public_dangerous")
  );
  let public: axum::Router = public.router();
  assert_eq!(status(&public, DOCS[0], false).await, StatusCode::OK);

  // dev and test serve them unguarded.
  for profile in ["dev", "test", " test "] {
    assert!(
      ContractDocs::new(fixture()).profile(profile).is_mounted(),
      "{profile}"
    );
  }
  assert!(!ContractDocs::new(fixture()).profile("staging").is_mounted());
  // Without a profile, `ID_EFFECT_PROFILE` decides; unset means `dev`.
  if std::env::var_os(id_effect_rpc::openapi::PROFILE_ENV).is_none() {
    assert!(ContractDocs::new(fixture()).is_mounted());
  }
}

#[tokio::test]
async fn served_documents_are_the_emitted_ones_with_etags() {
  let contract = fixture();
  let options = OpenApiOptions::new("Todos API", "1.0.0").with_path("/api");
  let app: axum::Router = ContractDocs::new(contract.clone())
    .profile("dev")
    .openapi(options.clone())
    .asyncapi(id_effect_rpc::asyncapi::AsyncApiOptions::new("Todos", "1"))
    .router();
  let get = |uri: &str, etag: Option<&str>| {
    let mut req = Request::get(uri);
    if let Some(etag) = etag {
      req = req.header(header::IF_NONE_MATCH, etag);
    }
    req.body(Body::empty()).unwrap()
  };
  let res = app
    .clone()
    .oneshot(get("/api/openapi.json", None))
    .await
    .unwrap();
  assert_eq!(res.status(), StatusCode::OK);
  assert_eq!(res.headers()[header::CONTENT_TYPE], "application/json");
  assert_eq!(res.headers()[header::CACHE_CONTROL], "no-cache");
  let etag = res.headers()[header::ETAG].to_str().unwrap().to_owned();
  let body = res.into_body().collect().await.unwrap().to_bytes();
  assert_eq!(body, to_json(&contract, &options).as_bytes());
  for tag in [
    etag.as_str(),
    "*",
    &format!("W/{etag}"),
    &format!("\"x\", {etag}"),
  ] {
    let res = app
      .clone()
      .oneshot(get("/api/openapi.json", Some(tag)))
      .await
      .unwrap();
    assert_eq!(res.status(), StatusCode::NOT_MODIFIED, "{tag}");
  }
  let res = app
    .clone()
    .oneshot(get("/api/openapi.json", Some("\"other\"")))
    .await
    .unwrap();
  assert_eq!(res.status(), StatusCode::OK);
  let res = app
    .clone()
    .oneshot(get("/api/_contract", None))
    .await
    .unwrap();
  let body = res.into_body().collect().await.unwrap().to_bytes();
  assert_eq!(body, contract.to_json_pretty().as_bytes());
  let res = app.oneshot(get("/api/asyncapi.json", None)).await.unwrap();
  let body = res.into_body().collect().await.unwrap().to_bytes();
  let doc: Value = serde_json::from_slice(&body).unwrap();
  assert_eq!(doc["info"]["title"], "Todos");
  let debug = format!("{:?}", ContractDocs::new(contract).guard(Staff));
  assert!(debug.contains("staff"));
}
