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
Created service project `orders-api` in orders-api (12 files).

Next:
  cd orders-api
  cargo test
  cargo run
```

The project depends on one front door, `id_effect_kit` (feature `web`), plus `id_effect` for the
macros:

| File | What |
|---|---|
| `src/lib.rs` | `app()`: the router, testable without a socket |
| `src/main.rs` | reads `HOST`, `PORT`, `SHUTDOWN_TIMEOUT_SECS`, `COOKIE_KEY`; serves until SIGINT / SIGTERM |
| `tests/http.rs` | the routes in process, plus one test that boots the binary |
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
```

The service listens on `0.0.0.0:8080` unless `HOST` / `PORT` say otherwise, and every response
carries the headers of `SecurityLayer::recommended` (CSP, HSTS, `nosniff`, CSRF protection on
state-changing requests). Stop it with Ctrl-C: it drains in-flight requests first.

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
the layers so metrics and the security headers cover them:

```rust
# use std::convert::Infallible;
# use axum::Router;
# use id_effect::{Counter, Effect};
# use id_effect_kit::axum::metrics::{http_metrics_layer, metrics_router};
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
pub fn app(health: &HealthRegistry, cookie_key: CookieKey) -> Router {
    Router::new()
        .route("/", routing::get(index))
        .merge(metrics_router())
        .with_state(AppState::default())
        .merge(quotes::routes(quotes::Quotes::default()))
        .merge(health.routes())
        .layer(http_metrics_layer())
        .layer(SecurityLayer::recommended(cookie_key))
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

- `Cargo.toml`: the `sql-pg` feature, `features = ["web", "sql-pg"]`.
- `src/db.rs` (new) and `pub mod db;` in `src/lib.rs`: the pool from `DATABASE_URL`, the migration,
  and a `db` check behind `/ready`.
- `migrations/0001_init.sql` (new), run on every start, so it stays idempotent.
- `src/main.rs`: connect before serving and register the readiness check (below).
- `compose.yaml`, `.env.example`, `config/deploy.yml`: a `postgres:17` service and `DATABASE_URL`.

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

Now a second migration. Add `migrations/0002_orders.sql`:

```sql
-- orders-api: orders (idempotent: it runs on every start, after 0001).
CREATE TABLE IF NOT EXISTS orders (
    id BIGSERIAL PRIMARY KEY,
    item TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
)
```

In `src/db.rs`, embed it next to the first one,
`pub const MIGRATION_0002: &str = include_str!("../migrations/0002_orders.sql");`, and run both, in
order, in `Database::connect`:

```rust
# use id_effect::Exit;
# use id_effect_kit::axum::EffectExecutor;
# use id_effect_kit::sql::SqlClient;
# use id_effect_kit::sql::pg::{PgPoolConfig, PgSqlClient, pg_pool_from_config};
# pub const MIGRATION_0001: &str = "CREATE TABLE IF NOT EXISTS greetings (id BIGSERIAL PRIMARY KEY)";
# pub const MIGRATION_0002: &str = "CREATE TABLE IF NOT EXISTS orders (id BIGSERIAL PRIMARY KEY)";
# #[derive(Clone)]
# pub struct Database {
#     client: PgSqlClient,
# }
# impl Database {
/// Connects to `DATABASE_URL` and runs the migrations in order.
pub async fn connect() -> Result<Self, String> {
    let url = std::env::var("DATABASE_URL").map_err(|_| "DATABASE_URL is not set")?;
    let pool = pg_pool_from_config(PgPoolConfig::from_url(url))
        .await
        .map_err(|e| format!("database: {}", e.0))?;
    let client = PgSqlClient::new(pool);
    for (name, sql) in [("0001", MIGRATION_0001), ("0002", MIGRATION_0002)] {
        let migrate = client.execute(sql, &[]);
        if let Exit::Failure(cause) = EffectExecutor::global().run_effect(migrate).await {
            return Err(format!("migration {name}: {cause:?}"));
        }
    }
    Ok(Self { client })
}
# }
```

Start the database and the service:

```sh
docker compose up -d db
cp .env.example .env        # DATABASE_URL=postgres://postgres:postgres@localhost:5432/postgres
export DATABASE_URL=postgres://postgres:postgres@localhost:5432/postgres
cargo run
curl http://127.0.0.1:8080/ready   # now lists the `db` check
```

The service reads the process environment, not `.env`, hence the `export` (or use direnv). Once
the schema outgrows "idempotent SQL run on start", move to a migration tool (sqlx-cli, refinery);
the [data chapter](./part6/ch28-00-data.md) covers the SQL client.

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
  pass  versions  id_effect 0.8.0: id_effect, id_effect_axum, id_effect_cli, id_effect_config, id_effect_kit, …
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

proxy:
  app_port: 8080
  healthcheck:
    path: /health

registry:
  server: ghcr.io
  username: your-user
  password:
    - KAMAL_REGISTRY_PASSWORD

env:
  clear:
    HOST: "0.0.0.0"
    PORT: "8080"
  secret:
    - COOKIE_KEY
    - DATABASE_URL
```

The secrets come from `.kamal/secrets`; `COOKIE_KEY` is standard base64 of at least 64 random
bytes (`openssl rand -base64 64 | tr -d '\n'`), so signed cookies survive restarts and span
instances. Then:

```sh
kamal setup     # first time: installs Docker on the servers, boots the proxy and the app
kamal deploy    # every release after that
```

Kamal's proxy only routes traffic to a new container once `/health` answers `200`, and the
service drains in-flight requests on the `SIGTERM` of the old one. The generated
`.github/workflows/ci.yml` already runs fmt, clippy, the tests and a docker build on every push.

## Where next

- [Why effects?](./part1/ch01-00-why-effects.md): what `Effect<A, E, R>` buys you, from the start.
- [Capabilities](./part2/ch05-00-tags-context.md) and [layers](./part2/ch06-00-layers.md): `R`,
  `caps!`, and building the environment.
- [Axum host](./part2/ch07-08-axum-host.md): every way to run effects in handlers.
- [Observability](./part6/ch27-00-observability.md): `--otel` and what the metrics mean.
