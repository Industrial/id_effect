//! OpenAPI from typed routes: `/openapi.json`, a Scalar `/docs` page and the `/_routes` table.
//!
//! The routes declare their path, query, body and responses as schemas. `OpenApi::from_router`
//! turns those declarations into an OpenAPI 3.1 document whose component schemas are the JSON
//! Schemas of the very schemas the extractors decode with. Three opt-in routers serve it:
//! `doc.routes()` (`/openapi.json` with an `ETag`, `/docs` with its own strict CSP) and
//! `introspect::routes_router` (`/_routes`, JSON or a text table). Nothing is mounted unless merged.
//!
//! Run: `cargo run -p id_effect_axum --features openapi --example 085_openapi` (fetches the three
//! endpoints, checks them and exits), or add `-- --serve` to listen on `127.0.0.1:3000` until
//! Ctrl-C and open <http://127.0.0.1:3000/docs> in a browser:
//!
//! ```text
//! curl -s localhost:3000/openapi.json
//! curl -s 'localhost:3000/_routes?format=text'
//! ```

use std::convert::Infallible;
use std::sync::{Arc, Mutex};

use axum::Json;
use axum::body::Body;
use axum::http::{HeaderMap, Request, StatusCode, header};
use http_body_util::BodyExt;
use id_effect::schema::{
  self, HasSchema, Schema, Unknown, email, json_schema_for, minimum, non_empty, optional, range,
};
use id_effect::{Effect, fail, succeed};
use id_effect_axum::Problem;
use id_effect_axum::openapi::{Info, OpenApi};
use id_effect_axum::route::introspect::routes_router;
use id_effect_axum::route::{Input, Route, Router};
use serde::Serialize;
use serde_json::Value;
use tower::ServiceExt;

/// `{id}` path parameter: a positive account number.
pub struct AccountId;

impl HasSchema for AccountId {
  type A = i64;
  type I = i64;
  type E = ();

  fn schema() -> Schema<i64, i64> {
    minimum(schema::i64(), 1).describe("Account number")
  }
}

/// Query of `GET /accounts`: `?owner=ada&limit=10`, both optional.
pub struct ListAccounts;

impl HasSchema for ListAccounts {
  type A = (Option<String>, Option<i64>);
  type I = (Option<String>, Option<i64>);
  type E = ();

  fn schema() -> Schema<Self::A, Self::I> {
    schema::struct_(
      "owner",
      optional(schema::string()),
      "limit",
      optional(range(schema::i64(), 1, 100)),
    )
  }
}

/// JSON body of `POST /accounts`.
pub struct NewAccount;

impl HasSchema for NewAccount {
  type A = (String, String, i64);
  type I = (String, String, i64);
  type E = ();

  fn schema() -> Schema<Self::A, Self::I> {
    schema::struct3(
      "owner",
      non_empty(schema::string()),
      "email",
      email(schema::string()),
      "opening_balance",
      minimum(schema::i64(), 0),
    )
  }
}

/// An account as the API returns it.
#[derive(Clone, Debug, PartialEq, Serialize, id_effect::Schema)]
pub struct Account {
  pub id: i64,
  pub owner: String,
  pub email: String,
  pub balance: i64,
}

/// A list of accounts (the `200` body of `GET /accounts`).
pub struct AccountList;

impl HasSchema for AccountList {
  type A = Vec<Account>;
  type I = Vec<Unknown>;
  type E = ();

  fn schema() -> Schema<Vec<Account>, Vec<Unknown>> {
    schema::array(Account::schema())
  }
}

/// The router state: an in-memory account table.
#[derive(Clone, Default)]
pub struct Store {
  accounts: Arc<Mutex<Vec<Account>>>,
}

fn list_accounts(
  store: &mut Store,
  input: Input<(), (Option<String>, Option<i64>)>,
) -> Effect<Json<Vec<Account>>, Infallible, Store> {
  let (owner, limit) = input.query;
  let accounts = store.accounts.lock().unwrap();
  let found = accounts
    .iter()
    .filter(|a| owner.as_ref().is_none_or(|o| a.owner == *o))
    .take(limit.map_or(usize::MAX, |n| n as usize))
    .cloned()
    .collect();
  succeed(Json(found))
}

fn show_account(store: &mut Store, input: Input<i64>) -> Effect<Json<Account>, Problem, Store> {
  let accounts = store.accounts.lock().unwrap();
  match accounts.iter().find(|a| a.id == input.path) {
    Some(account) => succeed(Json(account.clone())),
    None => {
      fail(Problem::new(StatusCode::NOT_FOUND).with_detail(format!("no account {}", input.path)))
    }
  }
}

fn create_account(
  store: &mut Store,
  input: Input<(), (), (), (String, String, i64)>,
) -> Effect<(StatusCode, Json<Account>), Infallible, Store> {
  let (owner, email, balance) = input.body;
  let mut accounts = store.accounts.lock().unwrap();
  let account = Account {
    id: accounts.len() as i64 + 1,
    owner,
    email,
    balance,
  };
  accounts.push(account.clone());
  succeed((StatusCode::CREATED, Json(account)))
}

/// The typed routes of the API, all public (documented with an empty `security`).
pub fn routes() -> Router<Store> {
  Router::new()
    .route(
      Route::get("/accounts")
        .public()
        .query::<ListAccounts>()
        .response::<AccountList>(StatusCode::OK)
        .handler(list_accounts),
    )
    .route(
      Route::post("/accounts")
        .public()
        .body::<NewAccount>()
        .response::<Account>(StatusCode::CREATED)
        .handler(create_account),
    )
    .route(
      Route::get("/accounts/{id}")
        .public()
        .path::<AccountId>()
        .response::<Account>(StatusCode::OK)
        .handler(show_account),
    )
}

/// The OpenAPI document of [`routes`], with a summary on one operation.
pub fn document(router: &Router<Store>) -> OpenApi {
  let mut doc = OpenApi::from_router(router).info(Info::new("Ledger", "1.0.0"));
  if let Some(op) = doc.operation_mut(&axum::http::Method::POST, "/accounts") {
    op.summary = Some("Open an account".to_owned());
  }
  doc
}

/// The whole application: the typed routes, `/openapi.json` + `/docs`, and `/_routes`.
pub fn app(store: Store) -> axum::Router {
  let router = routes();
  let doc = document(&router);
  let (api, registry) = router.with_state::<()>(store).into_parts();
  api.merge(doc.routes()).merge(routes_router(&registry))
}

/// `GET uri` with `headers`: status, response headers and body text.
async fn get(
  app: &axum::Router,
  uri: &str,
  headers: &[(&str, &str)],
) -> (StatusCode, HeaderMap, String) {
  let mut req = Request::get(uri);
  for (name, value) in headers {
    req = req.header(*name, *value);
  }
  let res = app
    .clone()
    .oneshot(req.body(Body::empty()).unwrap())
    .await
    .unwrap();
  let (parts, body) = res.into_parts();
  let bytes = body.collect().await.unwrap().to_bytes();
  let text = String::from_utf8(bytes.to_vec()).unwrap();
  println!("GET {uri} -> {}", parts.status);
  (parts.status, parts.headers, text)
}

async fn demo(app: &axum::Router) {
  // The document: OpenAPI 3.1, one operation per typed route, schemas from the decoders.
  let (status, headers, body) = get(app, "/openapi.json", &[]).await;
  assert_eq!(status, StatusCode::OK);
  assert_eq!(headers[header::CONTENT_TYPE], "application/json");
  assert_eq!(headers[header::CACHE_CONTROL], "no-cache");
  let doc: Value = serde_json::from_str(&body).unwrap();
  assert_eq!(doc["openapi"], "3.1.0");
  assert_eq!(doc["info"]["title"], "Ledger");
  assert_eq!(
    doc["components"]["schemas"]["NewAccount"],
    json_schema_for::<NewAccount>()
  );
  let create = &doc["paths"]["/accounts"]["post"];
  assert_eq!(create["summary"], "Open an account");
  assert!(create["responses"]["422"].is_object());
  println!(
    "  paths: {:?}",
    doc["paths"].as_object().unwrap().keys().collect::<Vec<_>>()
  );

  // Revalidation: the same document answers 304.
  let etag = headers[header::ETAG].to_str().unwrap().to_owned();
  let (status, _, body) = get(app, "/openapi.json", &[("if-none-match", &etag)]).await;
  assert_eq!((status, body.as_str()), (StatusCode::NOT_MODIFIED, ""));

  // The docs page: Scalar from the CDN, started by one hashed inline script.
  let (status, headers, html) = get(app, "/docs", &[]).await;
  assert_eq!(status, StatusCode::OK);
  assert_eq!(headers[header::CONTENT_TYPE], "text/html; charset=utf-8");
  assert!(html.contains("<title>Ledger</title>"));
  assert!(html.contains(id_effect_axum::openapi::DEFAULT_SCALAR_URL));
  let csp = headers[header::CONTENT_SECURITY_POLICY].to_str().unwrap();
  assert!(csp.starts_with("default-src 'none'; script-src https://cdn.jsdelivr.net 'sha256-"));
  println!("  content-security-policy: {csp}");

  // The route table: method, path, handler and schemas, as JSON or text.
  let (status, _, body) = get(app, "/_routes", &[]).await;
  assert_eq!(status, StatusCode::OK);
  let table: Value = serde_json::from_str(&body).unwrap();
  assert_eq!(table.as_array().unwrap().len(), 3);
  assert_eq!(table[1]["method"], "POST");
  assert_eq!(table[1]["body"], "NewAccount");
  assert!(
    table[1]["handler"]
      .as_str()
      .unwrap()
      .ends_with("::create_account")
  );
  let (status, headers, text) = get(app, "/_routes?format=text", &[]).await;
  assert_eq!(status, StatusCode::OK);
  assert_eq!(headers[header::CONTENT_TYPE], "text/plain; charset=utf-8");
  assert!(text.starts_with("METHOD"));
  println!("{text}");
}

#[tokio::main]
async fn main() {
  let app = app(Store::default());
  if std::env::args().any(|a| a == "--serve") {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:3000")
      .await
      .unwrap();
    println!("docs at http://{}/docs", listener.local_addr().unwrap());
    axum::serve(listener, app)
      .with_graceful_shutdown(async {
        let _ = tokio::signal::ctrl_c().await;
      })
      .await
      .unwrap();
  } else {
    demo(&app).await;
    println!("085_openapi ok");
  }
}
