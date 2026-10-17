# Integration Testing

The previous chapter started real dependencies; this one makes tests against them cheap to write
and safe to run side by side. `id_effect_test` adds three pieces:

| Piece | Feature | What it gives a test |
|-------|---------|----------------------|
| `isolated_db()` / `TemplateDb` | `sql` | a database nobody else sees: a rolled-back transaction, or a fresh clone of a migrated template |
| `Fixture` / `Factory` | `fixtures` | rows with deterministic, schema-valid defaults, inserted and read back with `FromRow` |
| `TestClient` | `http` | requests sent straight into the app's router — no socket, no port |
| `Contract` | `contract` | contract tests: each response checked against the schema the app's OpenAPI document declares for it |

```toml
[dev-dependencies]
id_effect_test = { version = "0.9", features = ["sql", "migrate", "fixtures", "http", "contract"] }
```

## A database per test: rollback mode

`isolated_db()` is a scoped `Layer`. It needs a `SqlClientService` — a pool over a shared
PostgreSQL container, a SQLite file, any `SqlClient` — begins **one transaction** on it, and
provides a client over that transaction as the `SqlClientService` the code under test already
reads. When the layer's scope closes the transaction is rolled back, whatever the test's `Exit`:
success, typed failure, defect or interrupt. Two tests writing the same row never see each
other, and the next test starts from the same rows.

Wire it on top of the client layer with `provide_to`, so the program only sees the isolated client:

```rust
use id_effect::{Effect, Exit, caps};
use id_effect_sql::sqlite::{SqlitePoolConfig, SqliteSqlClient};
use id_effect_sql::{SqlClientService, SqlError, execute, query_one_as};
use id_effect_test::isolated_db;
use id_effect_tokio::TokioRuntime;

type Db = caps!(SqlClientService);

/// The test body: writes account 1, counts what it sees.
fn opens_an_account() -> Effect<i64, SqlError, Db> {
  execute("INSERT INTO accounts VALUES (1, 'alice')", vec![])
    .flat_map(|_| query_one_as::<(i64,), Db, _>("SELECT COUNT(*) FROM accounts", vec![]))
    .map(|(n,)| n)
}

# fn main() {
# let rt = tokio::runtime::Runtime::new().unwrap();
# rt.block_on(async {
let file = std::env::temp_dir().join(format!("book_ch15_08_{}.db", std::process::id()));
# let _ = std::fs::remove_file(&file);
let client = || SqliteSqlClient::layer::<SqlError>(SqlitePoolConfig::file(&file));
let runtime = TokioRuntime::current().unwrap();

// The schema is committed once (migrations, in a real suite).
let schema: Effect<u64, SqlError, Db> =
  execute("CREATE TABLE accounts (id INTEGER PRIMARY KEY, name TEXT NOT NULL)", vec![]);
assert!(matches!(runtime.run(schema.provide_layer(client())).await, Exit::Success(_)));

// Two tests write the same row: each sees only its own.
for _test in 0..2 {
  let isolated = client().provide_to(isolated_db());
  let exit = runtime.run(opens_an_account().provide_layer(isolated)).await;
  assert!(matches!(exit, Exit::Success(1)), "{exit:?}");
}
# let _ = std::fs::remove_file(&file);
# });
# }
```

Inside the test transaction code behaves as against a real client:

- **Each statement runs under a savepoint.** A failing statement — the unique violation the test
  is about — undoes only itself, and the transaction stays usable. (PostgreSQL would otherwise
  abort the whole transaction on its first error.)
- **Transactions are savepoints.** `transaction(..)`, `with_transaction(..)` and
  `transaction_layer()` begin a savepoint: a commit releases it, a failure rolls back to it.
  Isolation options are ignored — the outer test transaction's level applies.
- The transaction itself is in the environment as `SqlTransactionService`.

Rollback mode is the cheapest isolation there is (a `BEGIN` and a `ROLLBACK`), and works on every
driver. Its limit is the transaction: code that must **commit** — and be seen committed by another
connection, a background job, a second service — needs a database of its own.

## A database per test: clone mode

A `TemplateDb` lives in a `static` and holds how to build the schema: usually the app's
migrations (`db::migrate`, feature `migrate`). The first test of a binary that asks for it creates
a template database on the server and runs the setup into it — once. Every test then gets
`CREATE DATABASE <fresh> TEMPLATE <template>`, a copy of the migrated schema in milliseconds, with a
pool over it; the database is dropped when the layer's scope closes (also after a failure or an
interrupt). The test commits freely; `IsolatedDatabase` (in the environment) carries the clone's
name and URL for code that opens connections of its own.

```rust,no_run
use id_effect::{Effect, caps, need};
use id_effect_sql::{SqlClientService, SqlError, execute, transaction};
use id_effect_test::db::migrate;
use id_effect_test::{ContainerError, IsolatedDatabase, Postgres, Shared, TemplateDb};

static PG: Shared<Postgres> = Shared::new(Postgres::new);
static LEDGER: TemplateDb =
  TemplateDb::new(|db| migrate(db, concat!(env!("CARGO_MANIFEST_DIR"), "/migrations")));

#[derive(Debug)]
enum TestError {
  Container(ContainerError),
  Sql(SqlError),
}
impl From<ContainerError> for TestError {
  fn from(e: ContainerError) -> Self { Self::Container(e) }
}
impl From<SqlError> for TestError {
  fn from(e: SqlError) -> Self { Self::Sql(e) }
}

type Cloned = caps!(SqlClientService, IsolatedDatabase);

/// Commits, then hands the database's URL to whatever must see the commit.
fn posts_a_journal_entry() -> Effect<String, TestError, Cloned> {
  let post: Effect<u64, SqlError, Cloned> =
    transaction(|tx| tx.execute("INSERT INTO entries (memo) VALUES ('rent')", &[]).narrow());
  post
    .map_err(TestError::from)
    .flat_map(|_| need::<IsolatedDatabase>().map(|db| db.url()))
}

# /* compiled as a plain fn in the doctest
#[tokio::test]
# */
async fn journal_entries_are_committed() {
  if !id_effect_test::require_docker() {
    return;
  }
  let program = posts_a_journal_entry().provide_layer(PG.layer().provide_to(LEDGER.isolated_db()));
  let exit = id_effect_tokio::TokioRuntime::current().unwrap().run(program).await;
  assert!(matches!(exit, id_effect::Exit::Success(_)), "{exit:?}");
}
# fn main() { let _ = journal_entries_are_committed; }
```

A failed setup (a broken migration) is remembered: every later test of the binary fails at once
with the same `SqlError`. Names are unique per process (`id_effect_tpl_<pid>_<n>`,
`id_effect_test_<pid>_<n>`), so several test processes can share one server.

| | Rollback (`isolated_db()`) | Clone (`TemplateDb::isolated_db()`) |
|-|----------------------------|-------------------------------------|
| Drivers | any `SqlClient` | PostgreSQL |
| Per-test cost | `BEGIN` / `ROLLBACK` | `CREATE DATABASE … TEMPLATE …`, a pool, `DROP DATABASE` |
| Code may commit | no (commits are savepoint releases) | yes |
| Other connections see the data | no | yes (`IsolatedDatabase::url`) |

## Fixtures and factories

A `Fixture` is the value a test inserts. Its defaults come from a proptest strategy built from
the fields' schemas (`from_schema`, `arb_schema`, `unknown_object`), so every default passes the
same refinements the application enforces; `insert_sql` is the statement inserting it and returning
the row, decoded as `Row: FromRow`. A `Factory` draws fixtures from a **seeded** generator — the
same seed gives the same values on every machine and every run — and applies overrides: `with`
for every value, `with_seq` with a running number for unique keys, `build_with` / `insert_with`
for one value. Only what the test is about is spelled out.

```rust
use id_effect::schema::{i64, refine, string};
use id_effect::{Effect, Exit, caps};
use id_effect_sql::sqlite::{SqlitePoolConfig, SqliteSqlClient};
use id_effect_sql::{FromRow, SqlClientService, SqlError, SqlValue, execute, params};
use id_effect_test::fixtures::{BoxedStrategy, Strategy, from_schema};
use id_effect_test::{Factory, Fixture, isolated_db};
use id_effect_tokio::TokioRuntime;

#[derive(Debug, PartialEq, FromRow)]
struct Account {
  id: i64,
  email: String,
  balance: i64,
}

#[derive(Clone, Debug)]
struct NewAccount {
  email: String,
  balance: i64,
}

impl Fixture for NewAccount {
  type Row = Account;

  fn arbitrary() -> BoxedStrategy<Self> {
    let email = from_schema(
      refine(string::<()>(), |s: &String| s.contains('@'), "an email"),
      "[a-z]{1,8}@example\\.test",
    );
    let balance = from_schema(refine(i64::<()>(), |n| *n >= 0, "non-negative"), 0..10_000i64);
    (email, balance).prop_map(|(email, balance)| NewAccount { email, balance }).boxed()
  }

  fn insert_sql(&self) -> (String, Vec<SqlValue>) {
    (
      "INSERT INTO accounts (email, balance) VALUES ($1, $2) RETURNING id, email, balance".into(),
      params![self.email.clone(), self.balance],
    )
  }
}

type Db = caps!(SqlClientService);

# fn main() {
// Deterministic: the same seed, the same accounts.
let a = Factory::<NewAccount>::seeded(7).build_many(3);
let b = Factory::<NewAccount>::seeded(7).build_many(3);
assert_eq!(format!("{a:?}"), format!("{b:?}"));

let mut accounts =
  Factory::<NewAccount>::seeded(7).with_seq(|a, n| a.email = format!("user-{n}@example.test"));
let rich = accounts.insert_with::<Db, _>(|a| a.balance = 1_000_000);
let others = accounts.insert_many::<Db, _>(2);
let program = rich.zip(others);

# let rt = tokio::runtime::Runtime::new().unwrap();
# rt.block_on(async {
let file = std::env::temp_dir().join(format!("book_ch15_08_fx_{}.db", std::process::id()));
# let _ = std::fs::remove_file(&file);
let client = || SqliteSqlClient::layer::<SqlError>(SqlitePoolConfig::file(&file));
let runtime = TokioRuntime::current().unwrap();
let schema: Effect<u64, SqlError, Db> = execute(
  "CREATE TABLE accounts (id INTEGER PRIMARY KEY, email TEXT UNIQUE NOT NULL, balance INTEGER NOT NULL)",
  vec![],
);
assert!(matches!(runtime.run(schema.provide_layer(client())).await, Exit::Success(_)));

let exit = runtime.run(program.provide_layer(client().provide_to(isolated_db()))).await;
let Exit::Success((rich, others)) = exit else { panic!("{exit:?}") };
assert_eq!((rich.id, rich.email.as_str(), rich.balance), (1, "user-1@example.test", 1_000_000));
assert_eq!(others.iter().map(|a| a.id).collect::<Vec<_>>(), [2, 3]);
# let _ = std::fs::remove_file(&file);
# });
# }
```

Override `Fixture::insert` to insert some other way — several statements, or a repository —
when one `INSERT … RETURNING` is not enough.

## The in-process HTTP client

`TestClient::new(router)` wraps any tower `Service` — an axum `Router`, the whole app with its
middleware — and sends requests to it as `oneshot` calls: no listener, no port, no server task.
`get` / `post` / `put` / `patch` / `delete` and `post_json` / `put_json` / `patch_json` build a
request (`header`, `bearer`, `json`, `body`; `default_header` on the client for every request);
`send()` is an `Effect` returning a `TestResponse`: status, headers, the whole body as `text()`,
`json::<T>()`, or an RFC 9457 `problem()`.

```rust
use axum::Json;
use axum::routing::{get, post};
use id_effect::Exit;
use id_effect_test::http::{StatusCode, TestClient};
use serde_json::{Value, json};

let app = axum::Router::new()
  .route("/health", get(|| async { "ok" }))
  .route(
    "/accounts",
    post(|Json(body): Json<Value>| async move { (StatusCode::CREATED, Json(body)) }),
  );
let client = TestClient::new(app).default_header("x-tenant", "acme");

let Exit::Success(created) = client
  .post_json("/accounts", &json!({"name": "alice"}))
  .bearer("t0k3n")
  .send()
  .block_on()
else {
  panic!("request failed")
};
created.assert_status(StatusCode::CREATED);
assert_eq!(created.json::<Value>().unwrap()["name"], "alice");

let Exit::Success(missing) = client.get("/nope").send().block_on() else { panic!() };
assert_eq!(missing.status(), StatusCode::NOT_FOUND);
```

`send()` runs on any executor the service runs on: `block_on` for a plain router, a tokio runtime
(`TokioRuntime::run`) when handlers need one — an app whose handlers read a database through
`isolated_db()`, for instance. Invalid input (a bad header, a body that does not serialize) fails
the effect with `HttpTestError::Request`; a service error with `HttpTestError::Service`.

## Contract tests against the OpenAPI document

An app built on `id_effect_axum`'s typed routes publishes an OpenAPI 3.1 document generated from
the same schemas its extractors decode with. Feature `contract` turns that document into a test
oracle: `Contract::new(document)` takes it as JSON (`OpenApi::to_json()`, or the app's own
`/openapi.json` fetched with the test client), and `response.assert_conforms(&contract)` checks a
`TestResponse` against it:

- the **operation** is the request's method and path, matched against the document's path
  templates (`/accounts/7` is `GET /accounts/{id}`); `check_operation(method, path, response)`
  names it explicitly;
- the **status** must be declared — the exact code, else its range (`4XX`), else `default`;
- the **`Content-Type`** must be one of that response's media types, and a response declared
  without content must have no body;
- the **body** must validate against the media type's JSON Schema (draft 2020-12, `$ref`s into
  `#/components` resolved).

`contract.check(&response)` returns the `ContractError` instead of panicking. A body that does not
match names every violation by **JSON pointer**:

```rust
use axum::Json;
use axum::http::StatusCode as Status;
use id_effect::{Effect, Exit, succeed};
use id_effect::schema::{self, HasSchema, Schema, minimum};
use id_effect_axum::openapi::OpenApi;
use id_effect_axum::route::{Input, Route, Router};
use id_effect_test::contract::{Contract, ContractError};
use id_effect_test::http::{Method, StatusCode, TestClient};
use serde_json::json;

struct AccountId;
impl HasSchema for AccountId {
  type A = i64;
  type I = i64;
  type E = ();
  fn schema() -> Schema<i64, i64> {
    minimum(schema::i64(), 1)
  }
}

#[derive(Clone, Debug, serde::Serialize, id_effect::Schema)]
struct Account {
  id: i64,
  name: String,
}

fn show(_: &mut (), input: Input<i64>) -> Effect<Json<Account>, std::convert::Infallible, ()> {
  succeed(Json(Account { id: input.path, name: "cash".into() }))
}

let routes = Router::<()>::new().route(
  Route::get("/accounts/{id}").path::<AccountId>().response::<Account>(Status::OK).handler(show),
);
let contract = Contract::new(OpenApi::from_router(&routes).to_json());
let (app, _) = routes.with_state::<()>(()).into_parts();

# tokio::runtime::Runtime::new().unwrap().block_on(async {
let runtime = id_effect_tokio::TokioRuntime::current().unwrap();
let Exit::Success(found) = runtime.run(TestClient::new(app).get("/accounts/7").send()).await else {
  panic!("request failed")
};
found.assert_status(StatusCode::OK).assert_conforms(&contract);

// A handler that drifted from the document: `id` became a string.
let drifted = axum::Router::new()
  .route("/accounts/{id}", axum::routing::get(|| async { Json(json!({"id": "7", "name": "cash"})) }));
let Exit::Success(res) = runtime.run(TestClient::new(drifted).get("/accounts/7").send()).await else {
  panic!("request failed")
};
let Err(err @ ContractError::Schema { .. }) = contract.check(&res) else { panic!("not caught") };
assert_eq!(
  err.to_string(),
  "`GET /accounts/{id}` 200: the body does not match the declared schema\n  at `/id`: \"7\" is not of type \"integer\""
);
assert!(contract.check_operation(&Method::GET, "/accounts/{id}", &found).is_ok());
# });
```

Statuses a route answers without declaring them fail the check too
(`ContractError::UndeclaredStatus` lists what the operation declares), so the document cannot
silently fall behind the handlers. `examples/reference/ledger/tests/http.rs`
(`responses_conform_to_the_openapi_document`) checks the Ledger's own `/openapi.json` this way —
`/me`, the account list and detail, a posted entry, and the 401, 403 and 422 refusals; the crate's
`tests/contract.rs` covers ranges, `default`, `$ref`'d responses, media types and every error.

## Putting it together

A typical integration test of a service built on id_effect:

1. a `Shared<Postgres>` for the binary (the previous chapter);
2. `isolated_db()` over its `sql_layer()` — or `TemplateDb::isolated_db()` when the code commits;
3. a `Factory` per table, seeded, with overrides for what the test is about;
4. the app's router, built over that environment, behind a `TestClient`.

`examples/020_isolated_db.rs` in `id_effect_test` runs the rollback mode and a factory against
SQLite, without Docker.
