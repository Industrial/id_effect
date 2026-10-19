# OpenAPI, API docs and the route table

Typed routes already know their schemas, so the OpenAPI document is generated, not written.
`OpenApi::from_router(&router)` turns every route of a typed `Router` into an operation. Each type
a route declares as its body or a response becomes a `components/schemas` entry whose value is
`json_schema_for::<T>()`: the JSON Schema (draft 2020-12, the dialect OpenAPI 3.1 uses) of the
same schema the extractors decode with. Documentation and validation cannot drift: change a
constraint and both change.

Everything here is behind the `openapi` feature of `id_effect_axum`, and nothing is mounted unless
you merge it into your app:

| Need | API | Serves |
|---|---|---|
| Build the document | `openapi::OpenApi::from_router(&router)`, `.info(..)`, `.operation_mut(..)` | — |
| Declare authentication | `.bearer_jwt(name)`, `.cookie_session(name, cookie)`, `.openid_connect(name, url)` | — |
| Serve the document and the docs UI | `openapi::openapi_router(&doc, OpenApiUi::scalar())`, or `doc.routes()` | `GET /openapi.json`, `GET /docs` |
| Configure the docs page | `OpenApiUi::cdn_url`, `integrity`, `title`, `json_path`, `docs_path`, `spec_url`, `connect_src` | — |
| List the routes | `route::introspect::routes_router(router.registry())` | `GET /_routes` (JSON, or text) |

The runnable version of this chapter is
[`crates/id_effect_axum/examples/085_openapi.rs`](https://github.com/Industrial/id_effect/blob/main/crates/id_effect_axum/examples/085_openapi.rs).
It fetches the three endpoints, checks them and exits; with `--serve` you can open the docs in a
browser. [`tests/openapi_ui.rs`](https://github.com/Industrial/id_effect/blob/main/crates/id_effect_axum/tests/openapi_ui.rs)
serves the same app over a real socket.

```bash
cargo run -p id_effect_axum --features openapi --example 085_openapi            # checks and exits
cargo run -p id_effect_axum --features openapi --example 085_openapi -- --serve # http://127.0.0.1:3000/docs
```

```toml
[dependencies]
id_effect_axum = { version = "0.10", features = ["openapi"] }
```

## The document comes from the routes

Path parameters come from the `{name}`s of the template, typed by `.path::<P>()`. Query and header
structs give one parameter per field. Every route that declares input also documents the `422`
`application/problem+json` answer, plus `400` for JSON bodies. The result is plain serde data:
set a summary with `operation_mut`, then serve `doc.to_json()`.

```rust
use std::convert::Infallible;

use axum::http::{Method, StatusCode};
use id_effect::schema::{self, HasSchema, Schema, email, json_schema_for, minimum, non_empty};
use id_effect::{Effect, succeed};
use id_effect_axum::openapi::{Info, OpenApi};
use id_effect_axum::route::{Input, Route, Router};

/// `{id}`: a positive account number.
struct AccountId;

impl HasSchema for AccountId {
  type A = i64;
  type I = i64;
  type E = ();

  fn schema() -> Schema<i64, i64> {
    minimum(schema::i64(), 1).describe("Account number")
  }
}

/// The body of `POST /accounts`.
struct NewAccount;

impl HasSchema for NewAccount {
  type A = (String, String);
  type I = (String, String);
  type E = ();

  fn schema() -> Schema<Self::A, Self::I> {
    schema::struct_("owner", non_empty(schema::string()), "email", email(schema::string()))
  }
}

fn create(_: &mut (), input: Input<(), (), (), (String, String)>) -> Effect<String, Infallible, ()> {
  succeed(input.body.0)
}

fn show(_: &mut (), input: Input<i64>) -> Effect<String, Infallible, ()> {
  succeed(format!("account {}", input.path))
}

let router = Router::new()
  .route(
    Route::post("/accounts")
      .public()
      .body::<NewAccount>()
      .response::<NewAccount>(StatusCode::CREATED)
      .handler(create),
  )
  .route(
    Route::get("/accounts/{id}")
      .public()
      .path::<AccountId>()
      .response::<NewAccount>(StatusCode::OK)
      .handler(show),
  );

let mut doc = OpenApi::from_router(&router).info(Info::new("Ledger", "1.0.0"));
doc.operation_mut(&Method::POST, "/accounts").unwrap().summary = Some("Open an account".into());

let json = doc.to_json();
assert_eq!(json["openapi"], "3.1.0");
// The component schema *is* the decoding schema.
assert_eq!(json["components"]["schemas"]["NewAccount"], json_schema_for::<NewAccount>());
assert_eq!(json["components"]["schemas"]["NewAccount"]["properties"]["email"]["format"], "email");

let create = &json["paths"]["/accounts"]["post"];
assert_eq!(create["summary"], "Open an account");
assert!(create["responses"]["422"].is_object()); // the validation problem
assert!(create["responses"]["400"].is_object()); // a body that is not JSON
assert_eq!(create["security"], serde_json::json!([])); // declared public: no authentication

let show = &json["paths"]["/accounts/{id}"]["get"];
assert_eq!(show["parameters"][0]["in"], "path");
assert_eq!(show["parameters"][0]["schema"]["minimum"], 1);
```

## Security schemes

Declare your authentication once (`.bearer_jwt("bearer")`, `.cookie_session("session",
"id_session")`, `.openid_connect("oidc", discovery_url)`) and every route guarded with
`.guard(..)` (or `.authorize(..)` from `id_effect_authz`) requires it, with its policy names
listed. Guarded routes also document `401` and `403`; a route declared `.public()` has an empty
`security` list, OpenAPI's "no authentication".

```rust
use std::convert::Infallible;

use id_effect::{Effect, succeed};
use id_effect_authz::guard::Guard;
use id_effect_authz::require_role;
use id_effect_axum::openapi::OpenApi;
use id_effect_axum::route::{Input, Route, Router};
use serde_json::json;

fn close(_: &mut (), _: Input) -> Effect<&'static str, Infallible, ()> {
  succeed("closed")
}

let router = Router::new().route(
  Route::post("/ledger/close")
    .guard(Guard::new("ledger.close", require_role("controller")))
    .handler(close),
);

let json = OpenApi::from_router(&router).bearer_jwt("bearer").to_json();
assert_eq!(json["components"]["securitySchemes"]["bearer"]["scheme"], "bearer");
let op = &json["paths"]["/ledger/close"]["post"];
assert_eq!(op["security"], json!([{ "bearer": ["ledger.close"] }]));
assert!(op["responses"]["401"].is_object() && op["responses"]["403"].is_object());
```

## Serving `/openapi.json` and `/docs`

`openapi_router(&doc, ui)` returns an Axum router to merge into your app. `doc.routes()` is the
same with the default UI.

- **`GET /openapi.json`** is the document as `application/json`. It is serialized once, when the
  router is built, and has `Cache-Control: no-cache` and a strong `ETag`, so a client that
  revalidates gets `304 Not Modified`.
- **`GET /docs`** is a [Scalar](https://github.com/scalar/scalar) API reference. The page is a few
  lines of HTML. It loads Scalar's bundle from a CDN (jsDelivr, `@scalar/api-reference@1`) and
  starts it with one inline script. No JavaScript is vendored in `id_effect_axum`.

For an air-gapped deployment, mirror the bundle and point `.cdn_url(..)` at the mirror: an
absolute URL, or a path your app serves itself (for example with `serve_dir`). If you pin an exact
version, add its Subresource Integrity hash with `.integrity("sha384-…")`.

```rust
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use id_effect_axum::openapi::{OpenApi, OpenApiUi, openapi_router};
use tower::ServiceExt;

let doc = OpenApi::new().title("Ledger").version("1.0.0");
let ui = OpenApiUi::scalar().cdn_url("https://mirror.internal/scalar/api-reference@1.34.0.js");
let app: axum::Router = openapi_router(&doc, ui);

# tokio::runtime::Runtime::new().unwrap().block_on(async {
let get = |uri: &str| Request::get(uri).body(Body::empty()).unwrap();

let spec = app.clone().oneshot(get("/openapi.json")).await.unwrap();
assert_eq!(spec.status(), StatusCode::OK);
assert_eq!(spec.headers()[header::CONTENT_TYPE], "application/json");
let etag = spec.headers()[header::ETAG].clone();

let mut again = get("/openapi.json");
again.headers_mut().insert(header::IF_NONE_MATCH, etag);
assert_eq!(app.clone().oneshot(again).await.unwrap().status(), StatusCode::NOT_MODIFIED);

let docs = app.oneshot(get("/docs")).await.unwrap();
assert_eq!(docs.headers()[header::CONTENT_TYPE], "text/html; charset=utf-8");
let csp = docs.headers()[header::CONTENT_SECURITY_POLICY].to_str().unwrap();
assert!(csp.contains("script-src https://mirror.internal 'sha256-"));
# });
```

If the router is nested (`Router::new().nest("/api", ..)`), tell the page where the document is
with `.spec_url("/api/openapi.json")`. `.json_path(..)` and `.docs_path(..)` move the two routes.

### The docs page and Content-Security-Policy

The `/docs` response sets its own policy, `OpenApiUi::content_security_policy()`:

```text
default-src 'none'; script-src <bundle origin> 'sha256-<init script>';
style-src <bundle origin> 'unsafe-inline'; img-src 'self' data:; font-src 'self' data:;
connect-src 'self'; base-uri 'none'; form-action 'none'; frame-ancestors 'none'
```

- `script-src` allows the bundle's origin (`'self'` for a mirror on your own origin) and, by hash,
  exactly the one inline script that starts the page. There is no `'unsafe-inline'` and no
  `'unsafe-eval'` for scripts.
- `style-src 'unsafe-inline'` is needed by Scalar, which renders inline `style="…"` attributes. A
  nonce or hash cannot authorize those. It allows styles, never scripts.
- `connect-src 'self'` lets the page fetch `/openapi.json` and lets its "try it" client call your
  API. Add other API origins with `.connect_src(&["https://api.example"])`.
- Scalar's web fonts are off, so the page contacts no font CDN.

This works with the `SecurityLayer` from [Application host](./ch30-00-application.md) without
changes. `SecurityLayer` keeps a Content-Security-Policy header that the handler already set, so
`/docs` keeps its own policy and every other route keeps the app's nonce policy:

```rust
use axum::body::Body;
use axum::http::{Request, header};
use id_effect_axum::openapi::OpenApi;
use id_effect_axum::server::security::{CookieKey, SecurityLayer};
use tower::ServiceExt;

let app: axum::Router = axum::Router::new()
  .route("/hello", axum::routing::get(|| async { "hello" }))
  .merge(OpenApi::new().routes())
  .layer(SecurityLayer::recommended(CookieKey::generate()));

# tokio::runtime::Runtime::new().unwrap().block_on(async {
let get = |uri: &str| Request::get(uri).body(Body::empty()).unwrap();
let docs = app.clone().oneshot(get("/docs")).await.unwrap();
let docs_csp = docs.headers()[header::CONTENT_SECURITY_POLICY].to_str().unwrap();
assert!(docs_csp.starts_with("default-src 'none'; script-src https://cdn.jsdelivr.net 'sha256-"));

let hello = app.oneshot(get("/hello")).await.unwrap();
let app_csp = hello.headers()[header::CONTENT_SECURITY_POLICY].to_str().unwrap();
assert!(app_csp.contains("'nonce-"));
# });
```

## The route table: `/_routes`

`route::introspect` lists what every typed route declares: method, path template, handler name,
the schema of each request part by name, the documented responses and the guarding policies. The
handler name is the Rust type name of the function given to `.handler(..)` (`my_app::show`).

- `route_table(router.registry())` gives one `RouteEntry` per route (serde-serializable).
- `route_table_text(..)` renders an aligned text table.
- `routes_router(router.registry())` serves `GET /_routes`. It answers JSON, or the text table for
  `?format=text` and for clients that accept `text/plain` but not JSON. Responses are
  `Cache-Control: no-store`.

The table shows how your API is put together. Mount it in development builds, or behind an admin
guard.

```rust
use std::convert::Infallible;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use id_effect::schema::{self, HasSchema, Schema};
use id_effect::{Effect, succeed};
use id_effect_axum::route::introspect::{route_table, routes_router};
use id_effect_axum::route::{Input, Route, Router};
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

fn show(_: &mut (), input: Input<i64>) -> Effect<String, Infallible, ()> {
  succeed(format!("account {}", input.path))
}

let router = Router::new().route(
  Route::get("/accounts/{id}")
    .public()
    .path::<AccountId>()
    .response::<AccountId>(StatusCode::OK)
    .handler(show),
);

let table = route_table(router.registry());
assert_eq!(table[0].method, "GET");
assert!(table[0].handler.as_deref().unwrap().ends_with("::show"));
assert_eq!(table[0].path_params.as_deref(), Some("AccountId"));

let listing = routes_router(router.registry());
let app: axum::Router = router.into_axum().merge(listing);

# tokio::runtime::Runtime::new().unwrap().block_on(async {
let req = Request::get("/_routes?format=text").body(Body::empty()).unwrap();
let res = app.oneshot(req).await.unwrap();
let text = res.into_body().collect().await.unwrap().to_bytes();
let text = std::str::from_utf8(&text).unwrap();
assert!(text.starts_with("METHOD  PATH            HANDLER"));
assert!(text.contains("path=AccountId"));
# });
```

The text table of the example looks like this:

```text
METHOD  PATH            HANDLER                      INPUT               RESPONSES        POLICIES
GET     /accounts       085_openapi::list_accounts   query=ListAccounts  200 AccountList  -
POST    /accounts       085_openapi::create_account  body=NewAccount     201 Account      -
GET     /accounts/{id}  085_openapi::show_account    path=AccountId      200 Account      -
```

## What to remember

- `OpenApi::from_router` builds an OpenAPI 3.1 document from typed routes. Its component schemas
  are the schemas the extractors decode with, so the docs cannot drift from validation.
- Declare authentication once; routes guarded with `.authorize(..)` require it.
- `openapi_router` / `doc.routes()` serve `/openapi.json` (with an `ETag`) and a Scalar `/docs`
  page. The page loads Scalar from a CDN, or from your mirror with `.cdn_url(..)`.
- `/docs` sets its own strict CSP, so it works behind `SecurityLayer::recommended`.
- `routes_router` serves the `/_routes` table. Nothing is mounted unless you merge it.
