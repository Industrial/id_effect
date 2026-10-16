//! Typed routes: a small accounts API whose path, query and body are declared as schemas.
//!
//! Each [`Route`] names a [`HasSchema`] type per request part; the handler receives the decoded
//! values in [`Input`] and returns an `Effect`. Input that does not decode never reaches the
//! handler: it is answered with an RFC 9457 [`Problem`] (`application/problem+json`, **422**)
//! listing every issue of every part with a JSON Pointer. The [`Router`] keeps a registry of
//! every route, served here as a `/_routes` listing.
//!
//! Run: `cargo run -p id_effect_axum --example 080_typed_routes` (sends a few requests and exits),
//! or add `-- --serve` to listen on `127.0.0.1:3000` until Ctrl-C:
//!
//! ```text
//! curl -i 'localhost:3000/accounts?limit=0'
//! curl -i -X POST localhost:3000/accounts -d '{"owner":"","email":"nope","opening_balance":-5}'
//! curl -s localhost:3000/_routes
//! ```

use std::convert::Infallible;
use std::sync::{Arc, Mutex};

use axum::Json;
use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use http_body_util::BodyExt;
use id_effect::schema::{
  self, HasSchema, ParseError, Schema, Unknown, email, minimum, non_empty, optional, range,
};
use id_effect::{Effect, fail, succeed};
use id_effect_axum::route::{Input, Issue, Location, Problem, Route, RouteRegistry, Router};
use serde::Serialize;
use serde_json::{Value, json};
use tower::ServiceExt;

/// `{id}` path parameter: a positive account number.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AccountId(pub i64);

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

/// Query of `GET /accounts`: `?owner=ada&limit=10`, both optional.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ListAccounts {
  pub owner: Option<String>,
  pub limit: Option<i64>,
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

/// JSON body of `POST /accounts`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NewAccount {
  pub owner: String,
  pub email: String,
  pub opening_balance: i64,
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
        Ok::<_, ParseError>(NewAccount {
          owner,
          email,
          opening_balance,
        })
      },
      |a: NewAccount| (a.owner, a.email, a.opening_balance),
    )
  }
}

/// An account as the API returns it (documented with `.response::<Account>(..)`).
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

fn not_found(id: AccountId) -> Problem {
  Problem::new(StatusCode::NOT_FOUND).with_detail(format!("no account {}", id.0))
}

fn list_accounts(
  store: &mut Store,
  input: Input<(), ListAccounts>,
) -> Effect<Json<Vec<Account>>, Infallible, Store> {
  let ListAccounts { owner, limit } = input.query;
  let accounts = store.accounts.lock().unwrap();
  let found = accounts
    .iter()
    .filter(|a| owner.as_ref().is_none_or(|o| a.owner == *o))
    .take(limit.map_or(usize::MAX, |n| n as usize))
    .cloned()
    .collect();
  succeed(Json(found))
}

fn show_account(
  store: &mut Store,
  input: Input<AccountId>,
) -> Effect<Json<Account>, Problem, Store> {
  let id = input.path;
  let accounts = store.accounts.lock().unwrap();
  match accounts.iter().find(|a| a.id == id.0) {
    Some(account) => succeed(Json(account.clone())),
    None => fail(not_found(id)),
  }
}

fn create_account(
  store: &mut Store,
  input: Input<(), (), (), NewAccount>,
) -> Effect<(StatusCode, Json<Account>), Problem, Store> {
  let new = input.body;
  let mut accounts = store.accounts.lock().unwrap();
  if accounts.iter().any(|a| a.email == new.email) {
    // A domain failure reported in the same shape as validation issues.
    return fail(
      Problem::new(StatusCode::CONFLICT)
        .with_detail("an account with this email exists")
        .with_error(Issue::new(Location::Body, "/email", "already registered")),
    );
  }
  let account = Account {
    id: accounts.len() as i64 + 1,
    owner: new.owner,
    email: new.email,
    balance: new.opening_balance,
  };
  accounts.push(account.clone());
  succeed((StatusCode::CREATED, Json(account)))
}

/// The typed routes of the API.
pub fn routes() -> Router<Store> {
  Router::new()
    .route(
      Route::get("/accounts")
        .query::<ListAccounts>()
        .response::<AccountList>(StatusCode::OK)
        .handler(list_accounts),
    )
    .route(
      Route::post("/accounts")
        .body::<NewAccount>()
        .response::<Account>(StatusCode::CREATED)
        .handler(create_account),
    )
    .route(
      Route::get("/accounts/{id}")
        .path::<AccountId>()
        .response::<Account>(StatusCode::OK)
        .handler(show_account),
    )
}

/// One entry per registered route: method, path template, and the schema type of each part.
pub fn route_listing(registry: &RouteRegistry) -> Value {
  let name =
    |part: &Option<id_effect_axum::route::SchemaRef>| part.as_ref().map(|s| s.short_name());
  registry
    .iter()
    .map(|route| {
      json!({
        "method": route.method.as_str(),
        "path": route.path,
        "path_params": name(&route.path_params),
        "query": name(&route.query),
        "headers": name(&route.headers),
        "body": route.body.as_ref().map(|s| s.json_schema()),
        "responses": route
          .responses
          .iter()
          .map(|r| json!({ "status": r.status.as_u16(), "schema": r.schema.short_name() }))
          .collect::<Vec<_>>(),
      })
    })
    .collect()
}

/// The whole application: the typed routes plus a plain `GET /_routes` built from their registry.
pub fn app(store: Store) -> axum::Router {
  let (router, registry) = routes().with_state::<()>(store).into_parts();
  let listing = Json(route_listing(&registry));
  router.route(
    "/_routes",
    axum::routing::get(move || async move { listing }),
  )
}

async fn send(app: &axum::Router, method: Method, uri: &str, body: Option<Value>) -> StatusCode {
  let mut req = Request::builder().method(method.clone()).uri(uri);
  if body.is_some() {
    req = req.header("content-type", "application/json");
  }
  let body = body.map_or_else(Body::empty, |b| Body::from(b.to_string()));
  let res = app.clone().oneshot(req.body(body).unwrap()).await.unwrap();
  let status = res.status();
  let bytes = res.into_body().collect().await.unwrap().to_bytes();
  let shown = serde_json::from_slice::<Value>(&bytes)
    .map(|v| serde_json::to_string_pretty(&v).unwrap())
    .unwrap_or_else(|_| String::from_utf8_lossy(&bytes).into_owned());
  println!("{method} {uri} -> {status}\n{shown}\n");
  status
}

async fn demo(app: &axum::Router) {
  let ada = json!({ "owner": "ada", "email": "ada@example.com", "opening_balance": 100 });
  assert_eq!(
    send(app, Method::POST, "/accounts", Some(ada.clone())).await,
    StatusCode::CREATED
  );
  assert_eq!(
    send(app, Method::GET, "/accounts?owner=ada&limit=10", None).await,
    StatusCode::OK
  );
  assert_eq!(
    send(app, Method::GET, "/accounts/1", None).await,
    StatusCode::OK
  );
  // Invalid path, query and body: 422 problems, the handler never runs.
  assert_eq!(
    send(app, Method::GET, "/accounts/0", None).await,
    StatusCode::UNPROCESSABLE_ENTITY
  );
  assert_eq!(
    send(app, Method::GET, "/accounts?limit=500", None).await,
    StatusCode::UNPROCESSABLE_ENTITY
  );
  let bad = json!({ "owner": "", "email": "not-an-email", "opening_balance": -5 });
  assert_eq!(
    send(app, Method::POST, "/accounts", Some(bad)).await,
    StatusCode::UNPROCESSABLE_ENTITY
  );
  // Typed failures of the handler's effect.
  assert_eq!(
    send(app, Method::GET, "/accounts/7", None).await,
    StatusCode::NOT_FOUND
  );
  assert_eq!(
    send(app, Method::POST, "/accounts", Some(ada)).await,
    StatusCode::CONFLICT
  );
  assert_eq!(
    send(app, Method::GET, "/_routes", None).await,
    StatusCode::OK
  );
}

#[tokio::main]
async fn main() {
  let app = app(Store::default());
  if std::env::args().any(|a| a == "--serve") {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:3000")
      .await
      .unwrap();
    println!("listening on http://{}", listener.local_addr().unwrap());
    axum::serve(listener, app)
      .with_graceful_shutdown(async {
        let _ = tokio::signal::ctrl_c().await;
      })
      .await
      .unwrap();
  } else {
    demo(&app).await;
    println!("080_typed_routes ok");
  }
}
