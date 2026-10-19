# Typed routes and validation

An HTTP handler should not parse strings. `id_effect_axum::route` lets a route **declare** what it
accepts (path parameters, query string, headers and body) as [`Schema`](../part4/ch14-00-schema.md)
types. Every request part is decoded before the handler runs, and the handler gets typed values.
Input that does not decode never reaches it. The client gets an RFC 9457 *problem* that lists
**every** issue in **every** part at once, each with a JSON Pointer.

| Need | API |
|---|---|
| Declare a route | `Route::get(path)` / `post` / `put` / `patch` / `delete`, then `.path::<P>()`, `.query::<Q>()`, `.headers::<H>()`, `.body::<B>()` or `.form::<B>()`, `.response::<R>(status)` |
| Decide who may call it | `.public()` or `.guard(g)` (any `RouteGuard`, e.g. `id_effect_authz::Guard`); `Router::route` does not compile without one |
| Handle it | `.handler(f)` with `f: Fn(&mut S, Input<P, Q, H, B>) -> Effect<A, E, S>` |
| Collect routes | `route::Router`: `.route(endpoint)`, `.merge(..)`, `.with_state(..)`, `.into_axum()` |
| List routes | `router.routes()`, `router.registry()`: one `RouteInfo` per route |
| Error body | `route::Problem` (`application/problem+json`) with `errors[]` of `Issue { in, pointer, message }` |
| Plain axum handlers | extractors `ValidPath<T>`, `ValidQuery<T>`, `ValidHeaders<T>`, `ValidJson<T>`, `ValidForm<T>` |

The runnable version of this chapter is
[`crates/id_effect_axum/examples/080_typed_routes.rs`](https://github.com/Industrial/id_effect/blob/main/crates/id_effect_axum/examples/080_typed_routes.rs),
a small accounts API:

```bash
cargo run -p id_effect_axum --example 080_typed_routes            # sends a few requests, prints them
cargo run -p id_effect_axum --example 080_typed_routes -- --serve # listens on 127.0.0.1:3000
```

## Declaring a typed route

A route part is any type that implements `HasSchema`. Its `A` (the decoded type) is what the
handler receives. This route takes one path parameter, a positive account id:

```rust
use std::convert::Infallible;

use axum::http::StatusCode;
use id_effect::schema::{self, HasSchema, ParseError, Schema, minimum};
use id_effect::{Effect, succeed};
use id_effect_axum::route::{Input, Route, Router};

/// `{id}`: a positive account number.
#[derive(Clone, Copy, Debug, PartialEq)]
struct AccountId(i64);

impl HasSchema for AccountId {
  type A = AccountId;
  type I = i64;
  type E = ();

  fn schema() -> Schema<AccountId, i64> {
    schema::transform(
      minimum(schema::i64(), 1),
      |n| Ok::<_, ParseError>(AccountId(n)),
      |id: AccountId| id.0,
    )
  }
}

/// The response body, documented with `.response::<Account>(..)`.
#[derive(Clone, Debug, PartialEq, serde::Serialize, id_effect::Schema)]
struct Account {
  id: i64,
  owner: String,
}

fn show(_: &mut (), input: Input<AccountId>) -> Effect<String, Infallible, ()> {
  succeed(format!("account {}", input.path.0))
}

let router = Router::new().route(
  Route::get("/accounts/{id}")
    .public()
    .path::<AccountId>()
    .response::<Account>(StatusCode::OK)
    .handler(show),
);
let app: axum::Router = router.into_axum();
# let _ = app;
```

The path template uses axum's syntax (`{id}`), which is also OpenAPI's. `.handler(..)` returns an
`Endpoint`, and `Router::route` mounts it and records what it declares. `into_axum()` gives back
a plain `axum::Router`. You can add layers to it, merge it with other routers or serve it.

### Every route declares its access

Routes are closed by default (ADR 0013 R1). Before `Router::route` accepts an endpoint, the route
says who may call it:

- `.public()`: anyone, anonymous callers included. `Router::access_report()` lists these routes,
  and `into_axum()` logs the report at startup.
- `.guard(g)`: a `RouteGuard` (such as `id_effect_authz::Guard`, see
  [Authorization](ch30-02-authorization.md)) decides before the handler runs. You can add more
  than one; the policy names go into `RouteInfo::policies` and the OpenAPI document.

Forget both and the route does not compile ("the route has no access decision"). `into_axum()` also
installs a deny-by-default layer: a plain axum route merged with `Router::merge_axum` declares no
access, so it answers **404** Problem Details. The deprecated `Router::route_unchecked` keeps the
0.10 behaviour for one minor version and records itself in the posture registry. See
`docs/MIGRATION-0.11.md`, "H8 routing".

## Schemas per part

Each part has its own way of turning the request into the `Unknown` value that the schema decodes:

| Part | Declared with | Schema shape | Decoded from |
|---|---|---|---|
| Path | `.path::<P>()` | a struct keyed by parameter name, or a scalar when the template has one parameter | the matched segments |
| Query | `.query::<Q>()` | a struct (or record) | `?k=v` pairs: an array field collects repeated keys and is `[]` when absent; an `optional` field is `None` when absent |
| Headers | `.headers::<H>()` | a struct whose field names are header names (case-insensitive) | request headers |
| JSON body | `.body::<B>()` | any | the body as JSON (the `Content-Type` is not checked) |
| Form body | `.form::<B>()` | a struct, like the query | `application/x-www-form-urlencoded`, or the text fields of `multipart/form-data` |

Path, query, header and form values arrive as strings. They are turned into the type the field's
schema expects (`"42"` becomes an integer for an `i64` field) and then decoded, so the schema's
validators (`minimum`, `range`, `email`, `non_empty`, …, see
[Validation](../part4/ch14-03-validation.md)) apply to every part. Use `transform` to decode into
your own type, or `#[derive(Schema)]` when the struct has no validators. `#[schema("x-tenant")]`
renames a field's wire key, which is how a derived struct names headers:

```rust
use std::convert::Infallible;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use id_effect::schema::{
  self, HasSchema, ParseError, Schema, email, minimum, non_empty, optional, range,
};
use id_effect::{Effect, succeed};
use id_effect_axum::route::{BodyKind, Input, Route, Router};
use tower::ServiceExt;

/// `?owner=ada&limit=10`, both optional; `limit` between 1 and 100.
#[derive(Clone, Debug, PartialEq)]
struct ListAccounts {
  owner: Option<String>,
  limit: Option<i64>,
}

impl HasSchema for ListAccounts {
  type A = ListAccounts;
  type I = (Option<String>, Option<i64>);
  type E = ();

  fn schema() -> Schema<ListAccounts, (Option<String>, Option<i64>)> {
    schema::transform(
      schema::struct_(
        "owner",
        optional(schema::string()),
        "limit",
        optional(range(schema::i64(), 1, 100)),
      ),
      |(owner, limit)| Ok::<_, ParseError>(ListAccounts { owner, limit }),
      |q: ListAccounts| (q.owner, q.limit),
    )
  }
}

/// The JSON body of `POST /orgs/{org}/accounts`.
#[derive(Clone, Debug, PartialEq)]
struct NewAccount {
  owner: String,
  email: String,
  opening_balance: i64,
}

impl HasSchema for NewAccount {
  type A = NewAccount;
  type I = (String, String, i64);
  type E = ();

  fn schema() -> Schema<NewAccount, (String, String, i64)> {
    schema::transform(
      schema::struct3(
        "owner",
        non_empty(schema::string()),
        "email",
        email(schema::string()),
        "opening_balance",
        minimum(schema::i64(), 0),
      ),
      |(owner, email, opening_balance)| {
        Ok::<_, ParseError>(NewAccount { owner, email, opening_balance })
      },
      |a: NewAccount| (a.owner, a.email, a.opening_balance),
    )
  }
}

/// Headers: `X-Tenant` is required, `X-Request-Id` may be missing.
#[derive(Clone, Debug, PartialEq, id_effect::Schema)]
struct TenantHeaders {
  #[schema("x-tenant")]
  tenant: String,
  #[schema("x-request-id")]
  request_id: Option<String>,
}

/// `{org}`: the one path parameter, so a scalar schema is enough.
struct OrgSlug;

impl HasSchema for OrgSlug {
  type A = String;
  type I = String;
  type E = ();

  fn schema() -> Schema<String, String> {
    non_empty(schema::string())
  }
}

fn create(
  _: &mut (),
  input: Input<String, ListAccounts, TenantHeaders, NewAccount>,
) -> Effect<(StatusCode, String), Infallible, ()> {
  let Input { path: org, headers, body, .. } = input;
  succeed((
    StatusCode::CREATED,
    format!("{org}/{}: {} for tenant {}", body.owner, body.email, headers.tenant),
  ))
}

let route = Route::post("/orgs/{org}/accounts")
  .public()
  .path::<OrgSlug>()
  .query::<ListAccounts>()
  .headers::<TenantHeaders>()
  .body::<NewAccount>();
let info = route.info();
assert_eq!(info.body_kind, BodyKind::Json);
assert_eq!(info.headers.as_ref().unwrap().short_name(), "TenantHeaders");

let app: axum::Router = Router::new().route(route.handler(create)).into_axum();

# tokio::runtime::Runtime::new().unwrap().block_on(async {
let request = |tenant: Option<&str>| {
  let mut req = Request::post("/orgs/acme/accounts?limit=5").header("content-type", "application/json");
  if let Some(tenant) = tenant {
    req = req.header("X-Tenant", tenant);
  }
  req
    .body(Body::from(r#"{"owner":"ada","email":"ada@example.com","opening_balance":0}"#))
    .unwrap()
};
let created = app.clone().oneshot(request(Some("t1"))).await.unwrap();
assert_eq!(created.status(), StatusCode::CREATED);
let no_tenant = app.clone().oneshot(request(None)).await.unwrap();
assert_eq!(no_tenant.status(), StatusCode::UNPROCESSABLE_ENTITY); // `in: header`, `/x-tenant`
# });
```

`.form::<NewAccount>()` instead of `.body::<NewAccount>()` reads the same schema from a form. For
`multipart/form-data` only the text fields are read and file fields are skipped. A request with
any other content type gets axum's own `415` response.

`Input<P, Q, H, B>` has the public fields `path`, `query`, `headers` and `body`. A part the route
does not declare is `()`, so `Input<AccountId>` is short for `Input<AccountId, (), (), ()>`.

## The handler is an effect

`.handler(f)` takes the same kind of function as the [`routing`](../part2/ch07-08-axum-host.md)
helpers, plus the decoded input: `Fn(&mut S, Input<..>) -> Effect<A, E, S>`, where `S` is the
router state. The effect runs as one request fiber. Success renders `A`, a typed failure renders
`E` (both `IntoResponse`), a defect is a `500` and an interrupt a `503`.

`Problem` implements `IntoResponse`, so it works well as `E`. Domain failures then have the same
shape as validation failures:

```rust
use std::sync::{Arc, Mutex};

use axum::Json;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use id_effect::schema::{self, HasSchema, Schema, email};
use id_effect::{Effect, fail, succeed};
use id_effect_axum::route::{Input, Issue, Location, Problem, Route, Router};
use tower::ServiceExt;

struct Email;

impl HasSchema for Email {
  type A = (String, String);
  type I = (String, String);
  type E = ();

  fn schema() -> Schema<Self::A, Self::I> {
    schema::struct_("owner", schema::string(), "email", email(schema::string()))
  }
}

#[derive(Clone, Default)]
struct Store {
  emails: Arc<Mutex<Vec<String>>>,
}

fn create(
  store: &mut Store,
  input: Input<(), (), (), (String, String)>,
) -> Effect<(StatusCode, Json<String>), Problem, Store> {
  let (owner, email) = input.body;
  let mut emails = store.emails.lock().unwrap();
  if emails.contains(&email) {
    return fail(
      Problem::new(StatusCode::CONFLICT)
        .with_detail("an account with this email exists")
        .with_error(Issue::new(Location::Body, "/email", "already registered")),
    );
  }
  emails.push(email);
  succeed((StatusCode::CREATED, Json(owner)))
}

let app: axum::Router = Router::new()
  .route(Route::post("/accounts").public().body::<Email>().handler(create))
  .with_state(Store::default())
  .into_axum();

# tokio::runtime::Runtime::new().unwrap().block_on(async {
let post = || {
  Request::post("/accounts")
    .header("content-type", "application/json")
    .body(Body::from(r#"{"owner":"ada","email":"ada@example.com"}"#))
    .unwrap()
};
assert_eq!(app.clone().oneshot(post()).await.unwrap().status(), StatusCode::CREATED);
assert_eq!(app.clone().oneshot(post()).await.unwrap().status(), StatusCode::CONFLICT);
# });
```

## Problem+JSON errors

A rejected request gets `Content-Type: application/problem+json` and a body that follows
[RFC 9457](https://www.rfc-editor.org/rfc/rfc9457). The `errors` member lists one issue per
failing value: `in` names the part (`path`, `query`, `header`, `body`), `pointer` is a JSON Pointer
into that part (`""` is the part itself), and `message` says what is wrong.

`POST /accounts` with `{"owner": "", "email": "not-an-email", "opening_balance": -5}` (the body
schema of the previous section) answers:

```json
{
  "type": "about:blank",
  "title": "Unprocessable Entity",
  "status": 422,
  "detail": "the request input failed validation (3 issues)",
  "instance": "/accounts",
  "errors": [
    { "in": "body", "pointer": "/owner", "message": "must not be empty" },
    { "in": "body", "pointer": "/email", "message": "must be a valid email address" },
    { "in": "body", "pointer": "/opening_balance", "message": "must be >= 0" }
  ]
}
```

A typed route decodes **all** of its parts before it answers, so a request with a bad query string
and a bad body gets the issues of both in one response:

```rust
use std::convert::Infallible;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use id_effect::schema::{self, HasSchema, Schema, minimum, range};
use id_effect::{Effect, succeed};
use id_effect_axum::route::{Input, Route, Router};
use serde_json::{Value, json};
use tower::ServiceExt;

struct Paging;

impl HasSchema for Paging {
  type A = (i64, Vec<String>);
  type I = (i64, Vec<String>);
  type E = ();

  fn schema() -> Schema<Self::A, Self::I> {
    schema::struct_("limit", range(schema::i64(), 1, 100), "tag", schema::array(schema::string()))
  }
}

struct Deposit;

impl HasSchema for Deposit {
  type A = (i64, String);
  type I = (i64, String);
  type E = ();

  fn schema() -> Schema<Self::A, Self::I> {
    schema::struct_("amount", minimum(schema::i64(), 1), "memo", schema::string())
  }
}

fn deposit(
  _: &mut (),
  _: Input<(), (i64, Vec<String>), (), (i64, String)>,
) -> Effect<&'static str, Infallible, ()> {
  succeed("never reached")
}

let app: axum::Router = Router::new()
  .route(
    Route::post("/deposits")
      .public()
      .query::<Paging>()
      .body::<Deposit>()
      .handler(deposit),
  )
  .into_axum();

# tokio::runtime::Runtime::new().unwrap().block_on(async {
let req = Request::post("/deposits?limit=500")
  .header("content-type", "application/json")
  .body(Body::from(r#"{"amount": 0}"#))
  .unwrap();
let res = app.oneshot(req).await.unwrap();
assert_eq!(res.status(), StatusCode::UNPROCESSABLE_ENTITY);
assert_eq!(res.headers()["content-type"], id_effect_axum::PROBLEM_JSON);
let body: Value = serde_json::from_slice(&res.into_body().collect().await.unwrap().to_bytes()).unwrap();
assert_eq!(
  body,
  json!({
    "type": "about:blank",
    "title": "Unprocessable Entity",
    "status": 422,
    "detail": "the request input failed validation (3 issues)",
    "instance": "/deposits",
    "errors": [
      { "in": "query", "pointer": "/limit", "message": "must be <= 100" },
      { "in": "body", "pointer": "/amount", "message": "must be >= 1" },
      { "in": "body", "pointer": "/memo", "message": "missing field memo" }
    ]
  })
);
# });
```

The status tells the client what kind of failure it was:

| Request | Status | Body |
|---|---|---|
| Every part decodes | the handler's | the handler's `A` or `E` |
| A part fails its schema | **422** | `Problem` with every issue |
| The JSON body is not well-formed | **400** | `Problem`, one `body` issue at pointer `""` (plus the issues of the other parts) |
| Axum rejects the request itself (body over `DefaultBodyLimit`, form with the wrong content type, invalid UTF-8 in a path segment) | axum's (`413`, `415`, `400`) | axum's, unchanged |

`Problem` is a plain serde type, so you can build your own: `Problem::new(status)` with
`with_detail`, `with_instance`, `with_type`, `with_error(Issue::new(..))` and
`with_extension(key, value)`. Clients can deserialize it as well. `PROBLEM_JSON` is the media type
constant.

## Standalone extractors in plain axum handlers

Handlers that stay `async fn` can still use the same decoding with the extractors in
`id_effect_axum::route`. Each one names a `HasSchema` type and gives you its decoded value. A
failure answers the same `Problem`:

| Extractor | Reads | `in` |
|---|---|---|
| `ValidPath<T>` | path parameters | `path` |
| `ValidQuery<T>` | the query string | `query` |
| `ValidHeaders<T>` | request headers | `header` |
| `ValidJson<T>` | a JSON body | `body` |
| `ValidForm<T>` | an urlencoded or multipart body (text fields) | `body` |

```rust
use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::routing::post;
use id_effect::schema::{self, HasSchema, Schema, minimum, non_empty};
use id_effect_axum::route::{ValidJson, ValidPath};
use tower::ServiceExt;

struct AccountId;

impl HasSchema for AccountId {
  type A = i64;
  type I = i64;
  type E = ();

  fn schema() -> Schema<i64, i64> {
    minimum(schema::i64(), 1)
  }
}

struct Rename;

impl HasSchema for Rename {
  type A = String;
  type I = String;
  type E = ();

  fn schema() -> Schema<String, String> {
    non_empty(schema::string())
  }
}

async fn rename(ValidPath(id): ValidPath<AccountId>, ValidJson(owner): ValidJson<Rename>) -> String {
  format!("account {id} now belongs to {owner}")
}

let app: axum::Router = axum::Router::new().route("/accounts/{id}/owner", post(rename));

# tokio::runtime::Runtime::new().unwrap().block_on(async {
let req = |uri: &str, body: &str| {
  Request::post(uri)
    .header("content-type", "application/json")
    .body(Body::from(body.to_owned()))
    .unwrap()
};
let ok = app.clone().oneshot(req("/accounts/3/owner", r#""grace""#)).await.unwrap();
assert_eq!(ok.status(), StatusCode::OK);
let bad = app.clone().oneshot(req("/accounts/0/owner", r#""grace""#)).await.unwrap();
assert_eq!(bad.status(), StatusCode::UNPROCESSABLE_ENTITY);
assert_eq!(bad.headers()["content-type"], id_effect_axum::PROBLEM_JSON);
# });
```

Axum runs extractors one at a time and stops at the first rejection. A plain handler with several
`Valid*` extractors therefore reports only the issues of the **first** failing part. Use a typed
route when the client should see everything at once.

## The registry and a route listing

The `route::Router` keeps a `RouteRegistry` with one `RouteInfo` per typed route: the method, the
path template, a `SchemaRef` for each declared part, the body kind and the documented responses.
`SchemaRef::json_schema()` gives the JSON Schema (draft 2020-12) of a part. OpenAPI generation reads
this registry, and you can serve it yourself:

```rust
use std::convert::Infallible;

use axum::Json;
use axum::http::{Method, StatusCode};
use id_effect::schema::{self, HasSchema, Schema};
use id_effect::{Effect, succeed};
use id_effect_axum::route::{Input, Route, RouteRegistry, Router};
use serde_json::{Value, json};

struct AccountId;

impl HasSchema for AccountId {
  type A = i64;
  type I = i64;
  type E = ();

  fn schema() -> Schema<i64, i64> {
    schema::i64()
  }
}

fn ok<P, Q, H, B>(_: &mut (), _: Input<P, Q, H, B>) -> Effect<&'static str, Infallible, ()> {
  succeed("ok")
}

fn listing(registry: &RouteRegistry) -> Value {
  registry
    .iter()
    .map(|r| {
      json!({
        "method": r.method.as_str(),
        "path": r.path,
        "path_params": r.path_params.as_ref().map(|s| s.short_name()),
        "responses": r.responses.iter().map(|x| x.status.as_u16()).collect::<Vec<_>>(),
      })
    })
    .collect()
}

let router = Router::new()
  .route(Route::get("/accounts").public().handler(ok))
  .route(
    Route::get("/accounts/{id}")
      .public()
      .path::<AccountId>()
      .response::<AccountId>(StatusCode::OK)
      .handler(ok),
  );

let info = router.registry().find(&Method::GET, "/accounts/{id}").unwrap();
assert_eq!(info.path_params.as_ref().unwrap().json_schema()["type"], "integer");

let (app, registry) = router.into_parts();
let routes = Json(listing(&registry));
let app: axum::Router = app.route("/_routes", axum::routing::get(move || async move { routes }));
# let _ = app;
assert_eq!(listing(&registry)[1]["path_params"], "AccountId");
```

`Router::merge` combines routers together with their registries. `with_state` keeps the registry
too. Routes added through the plain `routing` helpers, or directly on the `axum::Router` returned
by `into_axum` / `into_parts`, are served but are not in the registry.

## Migrating from `decode_json_schema`

`id_effect_axum::json::decode_json_schema` (0.8) stopped at the first schema error and answered
`{"path": "a.b", "message": ".."}` as `application/json`. It is deprecated in 0.9 and will be
removed in a later release. The 0.8 code:

```rust
# #![allow(deprecated)]
use axum::body::Bytes;
use axum::response::{IntoResponse, Response};
use id_effect::schema::{self, Schema};
use id_effect_axum::json::decode_json_schema;

fn person() -> Schema<(String, i64), (String, i64)> {
  schema::struct_("name", schema::string(), "age", schema::i64())
}

async fn create(body: Bytes) -> Response {
  match decode_json_schema(&person(), &body) {
    Ok((name, age)) => format!("{name}:{age}").into_response(),
    Err(e) => e.into_response(),
  }
}
# let _ = axum::Router::<()>::new().route("/people", axum::routing::post(create));
```

There are three replacements, from the smallest change to the most complete:

```rust
use std::convert::Infallible;

use axum::body::Bytes;
use axum::response::{IntoResponse, Response};
use id_effect::schema::{self, HasSchema, Schema};
use id_effect::{Effect, succeed};
use id_effect_axum::json::decode_json;
use id_effect_axum::route::{Input, Route, Router, ValidJson};

struct Person;

impl HasSchema for Person {
  type A = (String, i64);
  type I = (String, i64);
  type E = ();

  fn schema() -> Schema<Self::A, Self::I> {
    schema::struct_("name", schema::string(), "age", schema::i64())
  }
}

// 1. Same shape of handler: `decode_json` fails with a `Problem` that lists every issue.
async fn create_v1(body: Bytes) -> Response {
  match decode_json(&Person::schema(), &body) {
    Ok((name, age)) => format!("{name}:{age}").into_response(),
    Err(problem) => problem.into_response(),
  }
}

// 2. An extractor: the handler only runs with a valid body.
async fn create_v2(ValidJson((name, age)): ValidJson<Person>) -> String {
  format!("{name}:{age}")
}

// 3. A typed route: the body is declared, listed in the registry, and the handler is an effect.
fn create_v3(_: &mut (), input: Input<(), (), (), (String, i64)>) -> Effect<String, Infallible, ()> {
  let (name, age) = input.body;
  succeed(format!("{name}:{age}"))
}

let app: axum::Router = Router::new()
  .route(Route::post("/v3/people").public().body::<Person>().handler(create_v3))
  .into_axum()
  .route("/v1/people", axum::routing::post(create_v1))
  .route("/v2/people", axum::routing::post(create_v2));
# let _ = app;
```

What clients see changes as follows:

| | `decode_json_schema` (0.8) | 0.9 (`decode_json`, `ValidJson`, typed routes) |
|---|---|---|
| Content type | `application/json` | `application/problem+json` |
| Issues reported | the first | all of them, in `errors[]` |
| Location | `"path": "items.0.name"` | `"in": "body", "pointer": "/items/0/name"` |
| Not JSON | `400`, `"path": ""` | `400`, one issue at pointer `""` |
| Schema mismatch | `422` | `422` |

Clients that read `path` should read `errors[].pointer` instead.

## Where to look next

- Example: `crates/id_effect_axum/examples/080_typed_routes.rs`, an accounts API with path, query
  and body validation and a `/_routes` listing.
- Tests: `crates/id_effect_axum/tests/route_example.rs` (the example over a real socket),
  `tests/route_typed.rs` (every part, all-at-once errors, 400 vs 422),
  `tests/problem_extractors.rs` (the `Valid*` extractors, forms and multipart, headers),
  `tests/route_registry.rs` (the registry).
- [Axum host](../part2/ch07-08-axum-host.md) for serving the router, state and request fibers.
- [Validation](../part4/ch14-03-validation.md) for the validators used in route schemas.
- `cargo doc --open -p id_effect_axum`, module `route`.
