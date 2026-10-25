# Tutorial: zero to production

This tutorial takes one HTTP service from nothing to a deployable image: generate it, run it, add a
route with a typed error and a capability, test it, add PostgreSQL and a migration, check it with
`id-effect doctor`, look at its layer graph, build the Docker image and fill in the deploy config.
It assumes Rust (stable, 1.94 or newer) and, for the last steps, Docker.

Every Rust snippet on this page is compiled by `cargo test --doc -p book_snippets`, so the code
below matches the crates it uses. The later chapters explain each piece in depth; this one only
shows the path.

## 1. Install `id-effect`

```sh
cargo install id_effect_tool
id-effect --version
```

`id_effect_tool` is the package; `id-effect` is the binary. Working from an id_effect checkout
instead? Install it with `cargo install --path crates/id_effect_tool` and pass
`--local-workspace /path/to/id_effect` to `id-effect new` below, which points the generated
project at the checkout's crates.

## 2. Generate a service

```sh
id-effect new orders-api --template service
cd orders-api
```

```text
Created service project `orders-api` in orders-api (21 files).

Next:
  cd orders-api
  cargo test
  cargo run
```

The project depends on one front door, `id_effect_kit` (features `web`, `rpc`, `http3`), plus
`id_effect` and `id_effect_rpc` for the macros:

| File | What |
|---|---|
| `src/lib.rs` | `app()`: the router (`/`, the typed `/hello/{name}`, health, metrics, the RPC service under `/rpc`) and its middleware, testable without a socket; `server_config()`: the HTTP/3 switch and the certificate |
| `src/rpc.rs` | a `#[rpc::service]` (`Greeter`) next to the REST routes |
| `rpc.contract.json` | the RPC contract, kept current by `tests/rpc.rs` |
| `src/main.rs` | reads `HOST`, `PORT`, `SHUTDOWN_TIMEOUT_SECS`, `COOKIE_KEY`, `TLS_CERT_FILE`, `TLS_KEY_FILE` and the profile; runs the posture check; serves until SIGINT / SIGTERM |
| `config/base.toml`, `dev.toml`, `prod.toml` | the profiles: `server.http3.enabled`, `[server.tls]`, accepted risks |
| `tests/http.rs` | the routes in process, plus one test that boots the binary |
| `tests/rpc.rs` | the RPC service over TCP and HTTP/3 on one port, the HTTP/3 switch, the contract |
| `Dockerfile`, `.dockerignore` | a multi-stage release image |
| `compose.yaml` | the image and the services it needs |
| `config/deploy.yml` | a [Kamal](https://kamal-deploy.org) deploy config |
| `.github/workflows/ci.yml` | fmt, clippy, test and a docker build on every push |
| `.env.example` | every variable the service reads |

## 3. Run it

```sh
cargo test
cargo run
```

In a second terminal:

```sh
curl -i http://127.0.0.1:8080/health   # 200: the process runs
curl http://127.0.0.1:8080/ready       # the readiness checks as JSON
curl http://127.0.0.1:8080/metrics     # Prometheus text
curl http://127.0.0.1:8080/hello/ada   # hello, ada
curl -i http://127.0.0.1:8080/hello/$(printf 'a%.0s' {1..33})   # 33 characters: 422
```

`/hello/{name}` is a *typed route*: `Route::get("/hello/{name}").public().path::<Name>()` decodes
the path through `Name`'s schema (1 to 32 characters) before the handler runs, so `hello` only ever
sees a valid name. `.public()` is the route's access decision: every typed route says who may call
it (`.public()` or `.guard(..)`), or it does not compile. A value the schema rejects never reaches
the handler and is answered `422` with an RFC 9457 Problem+JSON body:

```text
HTTP/1.1 422 Unprocessable Entity
content-type: application/problem+json

{"type":"about:blank","title":"Unprocessable Entity","status":422,"detail":"the request input failed validation (1 issue)","instance":"/hello/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","errors":[{"in":"path","pointer":"","message":"must be at most 32 characters"}]}
```

The service listens on `0.0.0.0:8080` unless `HOST` / `PORT` say otherwise. `app()` wraps every
route in the same middleware, outermost first: `RequestIdLayer` (an `x-request-id` header and a
span per request), `CompressionLayer` (when the client accepts it), `SecurityLayer::recommended`
(CSP, HSTS, `nosniff`, CSRF protection on state-changing requests), `http_metrics_layer` (the
`http_server_requests_total` counters), a `TimeoutLayer` of `REQUEST_TIMEOUT` (30 s, then `503`)
and a `BodyLimitLayer` of `BODY_LIMIT` (2 MiB, then `413`)
([Typed routes](./part6/ch29-01-typed-routes.md), [middleware](./part6/ch29-03-middleware.md)).
Stop it with Ctrl-C: it drains in-flight requests first.

### RPC next to the routes

`src/rpc.rs` declares a service the same way a trait is declared; every procedure says who may
call it (`#[public]` or `#[guard(G)]`), or it does not compile:

```text
#[rpc::service(version = 1)]
pub trait Greeter {
    #[query]
    #[public]
    fn greet(&self, request: GreetRequest) -> Effect<Greeting, GreetError, DynEnv>;
}
```

`app()` mounts it under `/rpc`, on the same port and through the same middleware as the routes:

```sh
curl -s -H 'content-type: application/json' -H 'x-rpc: 1' -d '{"name":"ada"}' \
  http://127.0.0.1:8080/rpc/Greeter.greet
id-effect rpc gen ts --contract rpc.contract.json --out web/src/rpc   # a typed TypeScript client
```

### HTTPS and HTTP/3 with `id-effect dev`

HTTP/3 is on in the template. It needs a certificate, which `cargo run` does not have, so the
service above answered plain HTTP. `id-effect dev` gives it one:

```sh
id-effect dev
```

```text
id-effect dev: created a development CA and dev certificate ~/.local/share/id-effect/dev-cert/cert.pem (localhost, valid until …, sha-256 …)
id-effect dev: trust the development CA once (then https://localhost works without warnings):
  …
id-effect dev: started target/debug/orders-api (pid …, ID_EFFECT_PROFILE=dev)
```

It creates a localhost CA (once; it prints how to trust it) and a 13-day ECDSA certificate that it
rotates before it expires, passes it to the app as `TLS_CERT_FILE` / `TLS_KEY_FILE`, and rebuilds
and restarts the app when a file changes. Now the service serves TLS on TCP (HTTP/2 and HTTP/1.1)
and HTTP/3 on UDP, **on the same port**:

```sh
curl -sI https://localhost:8080/health | grep -i alt-svc   # alt-svc: h3=":8080"; ma=86400
curl --http3-only -s https://localhost:8080/health          # the same answer over QUIC
```

Clients find HTTP/3 through `Alt-Svc` on a TCP response and fall back to HTTP/2 when UDP is
blocked; TCP is always served. One switch turns HTTP/3 off, `server.http3.enabled` in
`config/base.toml` (or `APP_SERVER__HTTP3__ENABLED=false`): for a deployment behind a load balancer
that terminates TLS. The prod profile refuses the development certificate (the posture probe "no
dev certificate"), and refuses HTTP/3 without any certificate, so prod needs certificate files,
ACME, or the switch off ([HTTP/3 deployment](./part6/ch29-07-http3-deployment.md), [certificates and ACME](./part6/ch29-09-certificates-acme.md)).

## 4. A route with a typed error and a capability

`GET /quote` answers the quote of the day. Where the quotes come from is a *capability*, `Quotes`:
the route's effect declares it in its environment type (`caps!(Quotes)`), reads it with `~Quotes`,
and the router provides it as state. Its failure is a *typed error*, `QuoteError`, whose
`IntoResponse` decides the HTTP answer. Create `src/quotes.rs`:

```rust
//! `GET /quote`: the quote of the day, read from the `Quotes` capability.

use axum::Router;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use id_effect::{Effect, caps, effect, hlist};
use id_effect_kit::axum::routing;

/// A capability: where the quotes come from. The app provides the defaults, tests their own.
pub struct Quotes(pub Vec<String>);

impl Default for Quotes {
    fn default() -> Self {
        Self(vec!["Make the change easy, then make the easy change.".to_string()])
    }
}

/// What `GET /quote` can fail with: part of its type, mapped to HTTP by `IntoResponse`.
#[derive(Debug)]
pub enum QuoteError {
    /// No quote is configured.
    NoQuotes,
}

impl IntoResponse for QuoteError {
    fn into_response(self) -> Response {
        match self {
            QuoteError::NoQuotes => (StatusCode::NOT_FOUND, "no quotes configured").into_response(),
        }
    }
}

/// The environment of the quote routes: exactly the capabilities they read.
pub type QuoteEnv = caps!(Quotes);

fn quote_of_the_day(_env: &mut QuoteEnv) -> Effect<String, QuoteError, QuoteEnv> {
    effect!({
        let quote = ~Quotes.0.first().cloned();
        quote.ok_or(QuoteError::NoQuotes)?
    })
}

/// The quote routes, with `quotes` provided as their environment.
pub fn routes(quotes: Quotes) -> Router {
    Router::new()
        .route("/quote", routing::get(quote_of_the_day))
        .with_state(hlist![quotes])
}
```

The handler's type is the contract: it succeeds with a `String`, fails only with a `QuoteError`,
and needs `Quotes` and nothing else. Reading a capability the environment does not hold, or
returning another error type, is a compile error, not a runtime surprise
([Reading capabilities](./part2/ch05-04-get-getmut.md),
[the diagnostics gallery](https://github.com/Industrial/id_effect/blob/main/docs/diagnostics.md)).
A panic in the handler is a defect and answers `500`; the typed error never becomes one.

Declare the module with `pub mod quotes;` in `src/lib.rs` and merge its routes into `app()`, before
the layers so the middleware covers them:

```rust
# use std::convert::Infallible;
# use std::time::Duration;
# use axum::Router;
# use id_effect::schema::{self, HasSchema, Schema};
# use id_effect::{Counter, Effect, succeed};
# use id_effect_kit::axum::metrics::{http_metrics_layer, metrics_router};
# use id_effect_kit::axum::middleware::{BodyLimitLayer, CompressionLayer, RequestIdLayer, TimeoutLayer};
# use id_effect_kit::axum::route::{self, Input, Route};
# use id_effect_kit::axum::server::security::{CookieKey, SecurityLayer};
# use id_effect_kit::axum::{HealthRegistry, routing};
# mod quotes {
#     use axum::Router;
#     use id_effect::{Effect, caps, effect, hlist};
#     use id_effect_kit::axum::routing;
#     #[derive(Default)]
#     pub struct Quotes(pub Vec<String>);
#     pub type QuoteEnv = caps!(Quotes);
#     fn quote(_env: &mut QuoteEnv) -> Effect<String, String, QuoteEnv> {
#         effect!({ ~Quotes.0.join(" ") })
#     }
#     pub fn routes(quotes: Quotes) -> Router {
#         Router::new().route("/quote", routing::get(quote)).with_state(hlist![quotes])
#     }
# }
# #[derive(Clone)]
# pub struct AppState { greetings: Counter }
# impl Default for AppState {
#     fn default() -> Self { Self { greetings: Counter::new("greetings") } }
# }
# fn index(state: &mut AppState) -> Effect<String, Infallible, AppState> {
#     state.greetings.increment().map(|()| "hello".to_string())
# }
# pub struct Name;
# impl HasSchema for Name {
#     type A = String;
#     type I = String;
#     type E = ();
#     fn schema() -> Schema<String, String> { schema::length(schema::string::<()>(), 1, 32) }
# }
# fn hello(_: &mut AppState, input: Input<String>) -> Effect<String, Infallible, AppState> {
#     succeed(format!("hello, {}", input.path))
# }
# pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
# pub const BODY_LIMIT: usize = 2 * 1024 * 1024;
pub fn app(health: &HealthRegistry, cookie_key: CookieKey) -> Router {
    // Typed routes are registered (method, path, access, schemas) for OpenAPI and route
    // listings. Each declares who may call it: `.public()` here, `.guard(..)` for the rest.
    let typed = route::Router::new().route(
        Route::get("/hello/{name}")
            .public()
            .path::<Name>()
            .handler(hello),
    );
    Router::new()
        .route("/", routing::get(index))
        .merge(typed.into_axum())
        .merge(metrics_router())
        .with_state(AppState::default())
        .merge(quotes::routes(quotes::Quotes::default()))
        .merge(health.routes())
        .layer(BodyLimitLayer::new(BODY_LIMIT))
        .layer(TimeoutLayer::new(REQUEST_TIMEOUT))
        .layer(http_metrics_layer())
        .layer(SecurityLayer::recommended(cookie_key))
        .layer(CompressionLayer::new())
        .layer(RequestIdLayer::new())
}
# let _ = app(&HealthRegistry::new(), CookieKey::generate());
```

`cargo run`, then `curl http://127.0.0.1:8080/quote`.

## 5. Test it

Because the capability is an argument, a test provides its own `Quotes` and checks both outcomes
without a socket. Add to `tests/http.rs` (it already has the `call` helper and `test_app()`):

<!-- The hidden lines stand in for the generated crate and tests/http.rs helpers; `tokio::main`
     stands in for `tokio::test`, whose functions a doctest would drop unchecked. -->
```rust
# mod tokio { pub use ::tokio::*; pub use ::tokio::main as test; }
# mod orders_api {
#     use axum::Router;
#     use id_effect_kit::axum::HealthRegistry;
#     use id_effect_kit::axum::server::security::{CookieKey, SecurityLayer};
#     pub mod quotes {
#         use axum::Router;
#         use axum::http::StatusCode;
#         use axum::response::{IntoResponse, Response};
#         use id_effect::{Effect, caps, effect, hlist};
#         use id_effect_kit::axum::routing;
#         pub struct Quotes(pub Vec<String>);
#         impl Default for Quotes {
#             fn default() -> Self { Self(vec!["Make the change easy.".to_string()]) }
#         }
#         #[derive(Debug)]
#         pub enum QuoteError { NoQuotes }
#         impl IntoResponse for QuoteError {
#             fn into_response(self) -> Response {
#                 (StatusCode::NOT_FOUND, "no quotes configured").into_response()
#             }
#         }
#         pub type QuoteEnv = caps!(Quotes);
#         fn quote_of_the_day(_env: &mut QuoteEnv) -> Effect<String, QuoteError, QuoteEnv> {
#             effect!({
#                 let quote = ~Quotes.0.first().cloned();
#                 quote.ok_or(QuoteError::NoQuotes)?
#             })
#         }
#         pub fn routes(quotes: Quotes) -> Router {
#             Router::new().route("/quote", routing::get(quote_of_the_day)).with_state(hlist![quotes])
#         }
#     }
#     pub fn app(health: &HealthRegistry, cookie_key: CookieKey) -> Router {
#         Router::new()
#             .merge(quotes::routes(quotes::Quotes::default()))
#             .merge(health.routes())
#             .layer(SecurityLayer::recommended(cookie_key))
#     }
# }
# use axum::Router;
# use axum::body::Body;
# use axum::http::{HeaderMap, Method, Request, StatusCode};
# use http_body_util::BodyExt;
# use id_effect_kit::axum::HealthRegistry;
# use id_effect_kit::axum::server::security::CookieKey;
# use tower::ServiceExt;
# use orders_api::app;
# fn test_app() -> Router {
#     app(&HealthRegistry::new(), CookieKey::generate())
# }
# async fn call(app: &Router, method: Method, uri: &str) -> (StatusCode, HeaderMap, String) {
#     let request = Request::builder().method(method).uri(uri).body(Body::empty()).unwrap();
#     let (parts, body) = app.clone().oneshot(request).await.unwrap().into_parts();
#     let bytes = body.collect().await.unwrap().to_bytes();
#     (parts.status, parts.headers, String::from_utf8_lossy(&bytes).into_owned())
# }
use orders_api::quotes::{self, Quotes};

#[tokio::test]
async fn quote_reads_the_capability() {
    let app = quotes::routes(Quotes(vec!["ship it".to_string()]));
    let (status, _, body) = call(&app, Method::GET, "/quote").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, "ship it");
}

#[tokio::test]
async fn no_quotes_is_a_typed_404() {
    let app = quotes::routes(Quotes(Vec::new()));
    let (status, _, body) = call(&app, Method::GET, "/quote").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body, "no quotes configured");
}

#[tokio::test]
async fn the_app_serves_quote_with_security_headers() {
    let (status, headers, _) = call(&test_app(), Method::GET, "/quote").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers["x-content-type-options"], "nosniff");
}
# quote_reads_the_capability();
# no_quotes_is_a_typed_404();
# the_app_serves_quote_with_security_headers();
```

```sh
cargo test
```

Effects that are not HTTP routes test the same way: build the effect, `.provide(..)` its
capabilities, run it ([Testing](./part4/ch15-00-testing.md)).

## 6. Add PostgreSQL and a migration

`--db pg` is a generator option, and `id-effect new` never writes into a non-empty directory. Generate
the PostgreSQL variant next to your project and carry its pieces over:

```sh
id-effect new orders-api --template service --db pg --dest ../orders-api-pg
diff -ru ../orders-api-pg . --exclude target
```

The differences are the database wiring:

- `Cargo.toml`: the `sql-pg` and `migrate` features, `features = ["web", "sql-pg", "migrate"]`.
- `src/db.rs` (new) and `pub mod db;` in `src/lib.rs`: the pool from `DATABASE_URL`, the
  migrations of `migrations/` applied on start, and a `db` check behind `/ready`.
- `migrations/0001_init.sql` (new): a plain `CREATE TABLE greetings`, applied once and recorded in
  `_id_effect_migrations`.
- `src/main.rs`: connect before serving and register the readiness check (below).
- `tests/http.rs`: `migrations_directory_is_valid` checks `migrations/` without a database, in
  place of the test that boots the binary (which now needs one).
- `Dockerfile`: copies `migrations/` into the image's working directory, `/app`.
- `.github/workflows/ci.yml`: a `migrations` job that applies them to a `postgres:17` service with
  `id-effect migrate run` and gates on `id-effect migrate check`.
- `compose.yaml`, `.env.example`, `config/deploy.yml`: a `postgres:17` service and `DATABASE_URL`;
  `README.md` describes the lot.

The `main.rs` change, after `let health = HealthRegistry::new();`:

```rust
# use id_effect_kit::axum::{HealthIndicator, HealthRegistry};
# struct Database;
# impl Database {
#     async fn connect() -> Result<Self, String> { Ok(Database) }
#     fn health_indicator(&self) -> HealthIndicator {
#         HealthIndicator::from_effect("db", || -> id_effect::Effect<(), String> { id_effect::succeed(()) })
#     }
# }
# fn fail(message: impl std::fmt::Display) -> std::process::ExitCode {
#     eprintln!("error: {message}");
#     std::process::ExitCode::FAILURE
# }
# async fn main_body() -> std::process::ExitCode {
let health = HealthRegistry::new();
let health = match Database::connect().await {
    Ok(db) => health.with_indicator(db.health_indicator()),
    Err(e) => return fail(e),
};
# let _ = health;
# std::process::ExitCode::SUCCESS
# }
```

`Database::connect` in `src/db.rs` applies the migrations before the service answers anything:

```rust
# use id_effect::{Cause, Exit};
# use id_effect_kit::axum::EffectExecutor;
# use id_effect_kit::sql::migrate;
# use id_effect_kit::sql::pg::{PgPoolConfig, PgSqlClient, pg_pool_from_config};
# use id_effect_kit::sql::{SqlClientEnv, sql_client_env};
# pub const MIGRATIONS_DIR: &str = "migrations";
# #[derive(Clone)]
# pub struct Database {
#     client: PgSqlClient,
# }
# impl Database {
/// Connects to `DATABASE_URL` and applies the pending migrations of [`MIGRATIONS_DIR`]: each
/// in its own transaction under an advisory lock (replicas starting together apply it once),
/// recorded in `_id_effect_migrations`. A migration changed after it was applied stops the
/// start.
pub async fn connect() -> Result<Self, String> {
    let url = std::env::var("DATABASE_URL").map_err(|_| "DATABASE_URL is not set")?;
    let pool = pg_pool_from_config(PgPoolConfig::from_url(url))
        .await
        .map_err(|e| format!("database: {}", e.0))?;
    let client = PgSqlClient::new(pool);
    let migrations = migrate::run::<SqlClientEnv, _>(MIGRATIONS_DIR)
        .provide_all(sql_client_env(client.clone()));
    match EffectExecutor::global().run_effect(migrations).await {
        Exit::Success(_applied) => Ok(Self { client }),
        Exit::Failure(Cause::Fail(e)) => Err(format!("migrations: {e}")),
        Exit::Failure(cause) => Err(format!("migrations: {cause:?}")),
    }
}
# }
```

`MIGRATIONS_DIR` is `"migrations"`, read relative to the working directory: the project root under
`cargo run`, `/app` in the image. So a second migration is a new file, not new code. Let
`id-effect` create it; it numbers after the newest one:

```sh
id-effect migrate new orders
```

```text
Created migrations/0002_orders.sql
```

Fill it in. Each migration runs once, so it is plain DDL, no `IF NOT EXISTS`:

```sql
-- orders: the migration (`id-effect migrate run`, one transaction).
CREATE TABLE orders (
    id BIGSERIAL PRIMARY KEY,
    item TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
)
```

`migrations_directory_is_valid` in `tests/http.rs` pins the versions it finds; make it
`assert_eq!(versions, [1, 2]);` and `cargo test` passes again.

Start the database and the service; the start applies `0001` and `0002`:

```sh
docker compose up -d db
cp .env.example .env        # DATABASE_URL=postgres://postgres:postgres@localhost:5432/postgres
export DATABASE_URL=postgres://postgres:postgres@localhost:5432/postgres
cargo run
curl http://127.0.0.1:8080/ready   # now lists the `db` check
```

The service reads the process environment, not `.env`, hence the `export` (or use direnv); the
`id-effect migrate` commands read either. Ask the database what ran:

```sh
id-effect migrate status
id-effect migrate check    # exit 0: up to date; 1: pending, changed or missing
```

```text
id-effect migrate status: migrations (table _id_effect_migrations)
  applied  1  2026-10-16 23:41:32Z  init
  applied  2  2026-10-16 23:41:32Z  orders

2 applied, 0 pending, 0 drifted, 0 missing
Up to date: 2 migrations applied, none pending, changed or missing.
```

Never edit a migration once it ran: its checksum no longer matches, `check` fails and the next start
stops with the drift. Change the schema with another `id-effect migrate new` (`--reversible`
writes an `.up.sql` / `.down.sql` pair that `id-effect migrate revert` can undo). The generated
CI's `migrations` job runs `migrate run` and `migrate check` on every push.
[Migrations](./part6/ch28-02-migrations.md) covers the engine and the
[data chapter](./part6/ch28-00-data.md) the SQL client.

## 7. `id-effect doctor` and the layer graph

When the service does not start on some machine, ask doctor first. It checks the toolchain, that
every id_effect crate resolves to one version, that each `.env.example` key is set, that the
database accepts connections, the docker CLIs and that `PORT` is free:

```sh
id-effect doctor
```

```text
id-effect doctor: /home/me/orders-api
  pass  project   package `orders-api`
  pass  rustc     rustc 1.95.0 >= id_effect MSRV 1.94; the project sets no rust-version
  pass  versions  id_effect 0.13.0: id_effect, id_effect_axum, id_effect_cli, id_effect_config, id_effect_kit, …
  pass  env       all 6 .env.example keys set (6 from .env)
  pass  database  localhost:5432 accepts TCP connections
  pass  docker    Docker version 29.8.1; Docker Compose version 5.6.0 (daemon not checked)
  pass  port      127.0.0.1:8080 (PORT from .env) is free

7 passed, 0 warnings, 0 failed, 0 skipped
```

It exits `1` when a check fails, and `--json` gives the same report to scripts. The service's
`main` registers the layers it composes in a `LayerGraph`; `--describe-layers` prints that graph
and exits before anything starts, and `id-effect doctor --graph` builds the project and runs it
for you:

```sh
cargo run -- --describe-layers         # or --describe-layers=dot | dot -Tsvg > layers.svg
id-effect doctor --graph
```

```text
layer graph: 2 layers, 1 edges, 0 missing
env
  provides ConfigProviderService
host-config
  provides HostConfig
  needs    ConfigProviderService <- env
```

As the app grows, register each layer you compose with `.with("name", &layer)`, and the graph shows
what each one needs and who provides it, with the missing capabilities flagged.
[Doctor and the layer graph](./part6/ch34-01-doctor-graph.md) lists every check.

## 8. Build the Docker image

```sh
docker build -t orders-api .
docker run --rm -p 8080:8080 orders-api
curl -i http://127.0.0.1:8080/health
```

The `Dockerfile` builds a release binary in `rust:1-bookworm` and copies it into
`debian:bookworm-slim`, running as an unprivileged user with `HOST=0.0.0.0 PORT=8080`. With the
database, `docker compose up --build` starts the image and PostgreSQL together. (A project
generated with `--local-workspace` points at the checkout by absolute path, which the image cannot
see; build images from a crates.io project, or see `scripts/template-smoke.sh --docker` for how the
repository tests it.)

## 9. Deploy config

`config/deploy.yml` is a [Kamal 2](https://kamal-deploy.org) config. Fill in the image, the
servers and the registry:

```yaml
service: orders-api
image: ghcr.io/your-org/orders-api

servers:
  web:
    - 203.0.113.10

drain_timeout: 35

proxy:
  app_port: 8080
  healthcheck:
    path: /ready
    interval: 1
    timeout: 5

registry:
  server: ghcr.io
  username: your-user
  password:
    - KAMAL_REGISTRY_PASSWORD

env:
  clear:
    ID_EFFECT_PROFILE: "prod"
    APP_SERVER__HTTP3__ENABLED: "false"   # kamal-proxy terminates TLS
    HOST: "0.0.0.0"
    PORT: "8080"
    SHUTDOWN_TIMEOUT_SECS: "30"
  secret:
    - COOKIE_KEY
    - DATABASE_URL
```

`kamal-proxy` terminates TLS and forwards HTTP/1.1, so the service turns HTTP/3 off with its one
switch; the prod posture check would otherwise refuse HTTP/3 without a certificate. To serve
HTTP/3 straight from the app instead, publish UDP and TCP 443, drop that line and give the service
a certificate (`TLS_CERT_FILE` / `TLS_KEY_FILE` or `[server.tls]` in `config/prod.toml`).

The secrets come from `.kamal/secrets`; `COOKIE_KEY` is standard base64 of at least 64 random
bytes (`openssl rand -base64 64 | tr -d '\n'`), so signed cookies survive restarts and span
instances. Then:

```sh
kamal setup     # first time: installs Docker on the servers, boots the proxy and the app
kamal deploy    # every release after that
```

Kamal's proxy only routes traffic to a new container once `/ready` answers `200` (here: once the
`db` check passes), then gives the old one up to `drain_timeout` seconds. On its `SIGTERM` the old
service turns `/ready` `503` and drains in-flight requests for up to `SHUTDOWN_TIMEOUT_SECS`, so
`drain_timeout` stays above it ([Rolling restarts and
drain](./part6/ch34-00-dx-ship.md#rolling-restarts-and-drain)). The generated
`.github/workflows/ci.yml` already runs fmt, clippy, the tests and a docker build on every push.

## Where next

- [Why effects?](./part1/ch01-00-why-effects.md): what `Effect<A, E, R>` buys you, from the start.
- [Capabilities](./part2/ch05-00-tags-context.md) and [layers](./part2/ch06-00-layers.md): `R`,
  `caps!`, and building the environment.
- [Axum host](./part2/ch07-08-axum-host.md): every way to run effects in handlers.
- [Observability](./part6/ch27-00-observability.md): `--otel` and what the metrics mean.
