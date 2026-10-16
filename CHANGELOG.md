# Changelog

## Unreleased — 0.9.0 — Data & security

### Added

- New crates (ADR 0010 Amendment 5, tier `integration`): `id_effect_auth`, `id_effect_authz`,
  `id_effect_test`, `id_effect_redis`.
- `id_effect_redis`: pooled `RedisClient` layer; typed `RedisKv` / `RedisHash` / `RedisJson`
  stores; `RedisCacheBackend`; pub/sub as a `Stream`; `RedisSessionStore` (feature `session`);
  `RedisRateLimitStore` (feature `rate-limit`). Core `id_effect`: additive `CacheBackend` trait and
  `Cache::with_backend` (same `Cache` API, in-process single-flight kept, backend failures counted
  by `Cache::backend_errors`).
- `id_effect_config`: per-environment profiles (`Profiles`, `ID_EFFECT_PROFILE` / `--profile`)
  layering `config/base.toml` → `config/<profile>.toml` → `APP_*` env (env > profile > base); an
  unknown profile is `ConfigError::UnknownProfile`; `Profiles::overlay` takes a secret provider.
- `id_effect_axum::route`: typed routes —
  `Route::get("/accounts/{id}").path::<P>().query::<Q>().headers::<H>().body::<B>().response::<R>(status).handler(f)`
  with `HasSchema` parts, every decode issue reported at once (422); `route::Router` keeps a
  `RouteRegistry` of `RouteInfo` (method, path, schema per part, JSON Schema 2020-12) for OpenAPI
  and route listings. The existing `routing::get/post/…` are unchanged.
- `id_effect_axum`: RFC 9457 `Problem` (`application/problem+json`) with `errors[]` of
  `{in, pointer, message}`; typed routes answer invalid input with it (422, or 400 for malformed
  JSON) and list every issue. Standalone extractors `ValidPath`, `ValidQuery`, `ValidHeaders`,
  `ValidJson`, `ValidForm` (urlencoded and multipart text fields) share the same decoder.
  `Route::form::<T>()`, `RouteInfo::body_kind`, `json::decode_json`. Absent array / nullable
  query, form and header fields decode to `[]` / `null`.
- `id_effect_axum::middleware`: `CompressionLayer` (gzip / brotli / deflate), `RequestIdLayer`
  (`x-request-id` propagation, `RequestId` extractor, one `http.request` effect span per request),
  `TimeoutLayer` (503 Problem; interrupts the handler's fiber), `BodyLimitLayer` (413 Problem) and
  `RateLimitLayer` (429 Problem + `Retry-After`; token buckets behind a pluggable
  `RateLimitStore`). New dependency `id_effect_resilience`.
- `id_effect_axum`: `server::serve_dir` static files with strong `ETag` / `Last-Modified` / 304,
  `If-Match` / `If-Range`, precompressed `.br` / `.gz`, hashed-asset `Cache-Control`, traversal
  and hidden-file protection; `middleware::HttpCacheLayer` + `CacheControl` add `Cache-Control`
  and a body-hash `ETag` to GET responses and answer `If-None-Match` / `If-Modified-Since` with
  304. Book: part6/ch29-03-middleware.md; example `090_middleware`.
- `id_effect_axum` feature `openapi`: `OpenApi::from_router` generates an OpenAPI 3.1 document from
  typed routes. Component schemas are exactly the decoding schemas' JSON Schema; path / query /
  header parameters, request bodies per media type, documented and Problem
  (`application/problem+json`) responses, security schemes (bearer JWT, session cookie, OpenID
  Connect) with per-operation requirements from the new `RouteInfo::policies`.
  `Endpoint::from_parts` / `Endpoint::layer`; `id_effect_authz` guarded endpoints convert into an
  `Endpoint` and record their policies. Opt-in `openapi_router(&doc, OpenApiUi::scalar())` /
  `doc.routes()` serve `/openapi.json` (ETag, 304) and a Scalar `/docs` page loaded from a CDN or
  mirror (`cdn_url`, `integrity`) under its own strict CSP that works behind `SecurityLayer`;
  `route::introspect::routes_router` serves a `/_routes` table (method, path, handler, schemas,
  policies) as JSON or text; `RouteInfo::handler` records the handler's type name. Book:
  part6/ch29-02-openapi.md; example `085_openapi`.
- `id_effect_authz`: `Principal` and the `CurrentPrincipal` capability; `Policy<Res>` evaluated as an
  effect returning `Decision`, combined with `and` / `or` / `negate`; `Rbac` (roles → permissions,
  inheritance, wildcards); the named-policy registry `Policies` with `authorize` / `check`;
  ownership, tenant and ACL resource policies; claim (ABAC) policies; `AuthzError::http_status`
  (401 / 403 / 500). Feature `axum`: route guards (`Guard` layer; `.authorize(guard)` on an axum
  `MethodRouter` and on a typed `Endpoint`) answering 401 / 403 / 500 as `application/problem+json`.
  Every decision can be recorded as a serde `DecisionEvent` to the `AuthzEvents` sink capability.
  Feature `testing`: `AuthzTest` with `assert_allows` / `assert_denies` / `assert_denied_because`.
- `id_effect_auth`: argon2id `PasswordHasher` (OWASP defaults, rehash-on-verify, dummy verification
  for unknown users, hashing on the blocking pool) and server-side `Sessions` over a `SessionStore`
  (`MemorySessionStore`; idle and absolute timeouts, id rotation on login, rotate / logout /
  revoke-all, `__Host-` Secure HttpOnly SameSite=Lax cookie); feature `axum` adds `SessionLayer`.
  Credential middleware `Authenticate` (session → bearer JWT → API key → Basic, configurable
  order) installs a `CurrentPrincipal` (`AuthPrincipal`) for the authz guards; a bad credential is a
  401 Problem with `WWW-Authenticate`. JWT issue / verify (feature `jwt`: HS / RS / PS / ES / EdDSA,
  iss / aud / exp / nbf / leeway, required claims, claim → roles / permissions mapping);
  `JwksClient` with kid-rotation refetch, rate limiting and stale-cache fallback (feature `jwks`);
  hashed API keys with constant-time compare; Basic auth via `UserLookup` + `verify_or_dummy`.
  Feature `pg`: `PgSessionStore` keeps sessions in PostgreSQL (SHA-256 of the session id as the
  key, jsonb attributes, timestamptz expiries, upsert `save`), `pg_session_store_layer()`,
  `ensure_schema()` and the migration `migrations/0001_id_effect_sessions.{up,down}.sql`.
  Feature `oidc`: OpenID Connect login. `OidcClient` (discovery, authorization code + PKCE S256,
  state / nonce, ID tokens verified over the provider's JWKS with `iss` / `aud` / `exp` / `nonce`
  / `azp`, `ClaimMapping`, RP-initiated logout; implements `id_effect_platform`'s `OAuthClient`)
  and `OidcRoutes` (`/auth/oidc/{login,callback,logout}`, a separate flow session, login through
  `Sessions::login`). Tested against a wiremock IdP and a Keycloak container.
  Feature `totp`: TOTP second factor (totp-rs). Enrolment with an `otpauth://` URI, ±1-step
  verification on the executor clock with per-user replay protection, single-use recovery codes
  stored as argon2id hashes, and a session step-up with `amr` claims for the `require_mfa` authz
  policy.
- `id_effect_config`: HashiCorp Vault KV v2 secret provider behind the new `vault` feature
  (`VaultSettings`, `VaultProvider::layer`, `VaultAuth::token`). Secrets load once in a layer, come
  back as `Secret<T>`, and merge above the profile files and below the environment;
  `refresh_every` re-fetches and swaps atomically; failures are `ConfigError::Vault`.
  `Secret<T>` now implements `Deserialize`.
- `id_effect_sql`: typed SQL. `SqlValue` (bool, i16/i32/i64, f32/f64, text, bytes, uuid, jiff
  `Timestamp` / `DateTime` / `Date` / `Time`, `rust_decimal::Decimal`, JSON), `ToSqlValue` /
  `FromSqlValue`, `params!`, typed `SqlRow::get::<T>(name | index)` with errors naming the column,
  `SqlColumn` metadata, `Json<T>`. PostgreSQL binds NULL untyped and decodes by column type;
  unsupported column types are a clear error instead of a silent `""`. `testing::arb_sql_value`
  (feature `proptest`). Book: part6/ch28-01-typed-sql.md.
- `id_effect_sql`: `FromRow` and `#[derive(FromRow)]` (`rename`, `default`, `flatten`, `#[schema]`
  renames, `#[from_row(schema)]` validation into the new `SqlError::InvalidRow`), tuple `FromRow`,
  `query_as` / `query_one_as` / `query_opt_as`, `SqlError::RowNotFound`.
- `id_effect_sql`: nested `savepoint` scopes; isolation / read-only / deferrable transactions
  (`transaction_with`, `with_transaction_options`, `SqlClient::begin_with`);
  `retry_serialization_failures` over a `Schedule`; streaming queries (`query_stream`,
  `query_stream_as`) with bounded buffering and cancel-on-interrupt on PostgreSQL. `TestSqlClient`
  supports all three.
- `id_effect_sql` feature `migrate`: `Migrator` (`from_dir`, `from_files`, `from_sqlx` for
  `sqlx::migrate!` embedding) with `run` / `status` / `revert(n)` / `check` as effects and a startup
  `Layer`; sqlx-cli file naming and checksums, tracking table `_id_effect_migrations`, one
  transaction per migration under a PostgreSQL advisory lock with a timeout, typed `MigrateError`.
- `id_effect_sql` feature `sqlite`: `SqliteSqlClient` over sqlx SQLite (in-memory and file
  databases, savepoints, streaming, `Serializable` → `BEGIN IMMEDIATE`, busy → retryable 40001).
  `RoutingSqlClient` sends reads to replicas (round-robin or custom) and writes / transactions to
  the primary. One conformance suite runs on PostgreSQL and SQLite.
- `id_effect_sql` feature `mysql`: `MySqlSqlClient` / `MySqlPoolConfig` on sqlx; the shared
  conformance suite passes on MySQL 8. Isolation levels, `READ ONLY` transactions, savepoints,
  deadlocks and lock wait timeouts retryable as SQLSTATE 40001, bounded streaming with
  `KILL QUERY` on drop.
- `id_effect_sql` feature `query`: build statements with sea-query 1.0, render them per dialect
  (`BuildSql::build_sql(Dialect::{Postgres, MySql, Sqlite})` → SQL + `Vec<SqlValue>`, our own value
  bridge, no sea-query-binder) and run them with `execute_query` / `fetch_all_as` /
  `fetch_one_as` / `fetch_optional_as`.
- `id_effect_sql` feature `repository`: `#[derive(Entity)]` and `Repository<T>`. CRUD, paging,
  optimistic locking via `#[entity(version)]` (`SqlError::StaleVersion`), soft-delete / tenant /
  custom global filters on every generated statement with `with_deleted()` / `unscoped()` escape
  hatches (a tenant-scoped entity with no tenant fails closed); runs in transactions via
  `repo.on(tx)`. Book: part6/ch28-03-repositories.md; example `040_repository`.
- Book part6/ch30-07-audit-tenancy.md: the row-level tenant pattern end to end (authenticated
  principal → `CapOverrideLayer` setting `CurrentTenant` and `AuditContext` → tenant-filtered
  `Repository` → `audit::record` in the request's transaction → `verify_chain`), its fail-closed
  behaviour and the alternatives it does not cover (row-level security, schema or database per
  tenant); example `id_effect_sql/050_tenancy_audit`.
- `id_effect_sql` feature `audit`: hash-chained audit log. `AuditLog` layer, `append` / `record`
  inside the caller's transaction, one chain per tenant with `sha256(prev_hash ‖ record)` (the
  chain head is row-locked, so concurrent appends never fork it), `verify_chain` reports the first
  edited, removed or relinked record; bundled migration `migrations/audit/`.
- `id_effect_test`: effect-native test containers (testcontainers). Postgres (with a ready
  `SqlClient` layer under feature `sql`), MySQL, Redis, Keycloak, Vault and a generic
  `ContainerSpec` as scoped layers, removed when their scope closes (success, failure, defect or
  interrupt); `Shared` one-per-binary containers with the Ryuk reaper; readiness waits with typed
  timeouts (`ContainerError::NotReady`); `require_docker()` prints SKIP, or fails under
  `ID_EFFECT_REQUIRE_DOCKER=1`. Book: part4/ch15-07-containers.md.
  Per-test database isolation: `isolated_db()` (a rolled-back transaction over any `SqlClient`,
  with savepoints per statement and per nested transaction) and `TemplateDb` (a migrated template
  cloned per test with `CREATE DATABASE … TEMPLATE`, dropped on scope close); `Fixture` /
  `Factory` with seeded Schema-arbitrary defaults and `FromRow` rows (feature `fixtures`); an
  in-process `TestClient` over any tower `Service` (feature `http`). Book:
  part4/ch15-08-integration-testing.md; example `020_isolated_db`.
- `id_effect_kit`: features `openapi`, `sql`, `sql-sqlite`, `sql-mysql`, `migrate`, `repository`,
  `auth` (+ `auth-oidc`, `auth-totp`), `authz`, `redis`, `config-vault` and `test` (for
  dev-dependencies; not in `full`); `full` updated; the prelude gains typed routes, `Problem`,
  `Valid*`, middleware, `OpenApi`, `FromRow` / `Entity` / `Repository`, `Migrator`, the auth and
  authz types and `Profiles`. Derives used through the kit alone need
  `#[from_row(crate = id_effect_kit::sql)]`.
- `id_effect_tool` `service` template: applies `migrations/` with `id_effect_sql::migrate::run` on
  start (no more `include_str!` + execute on every start), serves a typed Problem+JSON route
  behind request-id / compression / timeout / body-limit middleware, and with `--db pg` its CI
  runs `id-effect migrate run` + `migrate check`.
- `id_effect_tool`: `id-effect migrate new|run|status|revert|check` (sqlx-cli file layout;
  sequential or timestamp numbering following the directory; `DATABASE_URL` / `.env`; `--json`;
  exit codes 0 / 1 / 2 / 3). `check` exits 1 on pending, drifted or missing migrations, for CI.
  Book: part6/ch28-02-migrations.md; example `id_effect_sql/examples/030_migrate.rs` (in memory by
  default, live on PostgreSQL with `DATABASE_URL` / `--live`).

### Changed

- **Breaking** `id_effect_sql`: statements take `&[SqlValue]`; rows are typed; `SqlError` is
  `#[non_exhaustive]`. See [MIGRATION-0.9.md](docs/MIGRATION-0.9.md).

### Deprecated

- `id_effect_sql::SqlParam`, `SqlRow::new`, `SqlRow::text_cells` (removed in 0.10).
- `id_effect_axum::json::decode_json_schema`, `JsonSchemaError`, `SchemaJsonErrorBody` (removed in
  0.10; see [MIGRATION-0.9.md](docs/MIGRATION-0.9.md)).

### Security

- `id_effect_sql`: TLS for PostgreSQL and MySQL (rustls on aws-lc-rs, no OpenSSL); 0.8 connected in
  plaintext. `PgPoolConfig` / `MySqlPoolConfig` take `with_tls_mode(PgTlsMode / MySqlTlsMode)`,
  `with_root_cert_file` / `with_root_cert_pem` and `effective_tls_mode()`, overriding the URL's
  `sslmode` / `ssl-mode`. The default stays `Prefer` / `Preferred` (unverified, may fall back to
  plaintext); use `VerifyFull` / `VerifyIdentity` in production. With sqlx 0.8, `VerifyCa` also
  checks the host name (stricter than its name).
- `id_effect_config`: `ProfiledConfig` and `FigmentConfigProvider` no longer print merged values in
  `Debug`, and errors from `Config::secret()` loaders no longer contain the raw value.

## 0.8.0 — 2026-10-16 — Trust & first hour (breaking)

First release under the plan of plans (`.maestro/specs/roadmap/PLAN-OF-PLANS.md`). Upgrading
from 0.7: see the [0.8 migration guide](docs/MIGRATION-0.8.md).

**License: MIT from 0.8.0** (was CC-BY-SA-4.0; versions up to 0.7.0 keep their license). A
`LICENSE` file, `license = "MIT"` in every manifest, and a DCO sign-off for contributions
(`CONTRIBUTING.md`). The labs `id_effect_jobs` obix outbox adapter was removed before the switch.

### Added

- **Trust & supply chain:** `SECURITY.md` (private reporting via GitHub advisories, response
  targets, support window), `GOVERNANCE.md`, `CODEOWNERS`. CI runs `cargo deny check`
  (`deny.toml`) and a report-only `cargo semver-checks` against 0.7.0 (enforcing from 0.8.0).
  Releases attach a CycloneDX SBOM per crate and the published `.crate` files with build
  provenance attestations. `docs/STABILITY.md` states the semver, deprecation and MSRV policy.
- **Web security (`id_effect_axum`):** `SecurityLayer::recommended(key)` — per-session CSRF
  tokens in a signed `__Host-` cookie with constant-time compare (on by default), nonce-based CSP
  (`CspNonce`, script hashes), HSTS / X-Frame-Options / nosniff / Referrer-Policy defaults, and
  `Cookies` with secure defaults and signed/private jars keyed by a `CookieKey` effect layer.
  `CorsConfig` builds a validated `tower_http` CORS layer.
- **Metrics:** `id_effect::observability::prometheus` renders a `MetricRegistry` as Prometheus
  text 0.0.4 (no dependency); `runtime_metrics` adds `id_effect_fibers_live` and
  `id_effect_fibers_started_total`. `id_effect_axum::metrics` serves `GET /metrics` and records
  HTTP requests and durations by route template (`http_metrics_layer()`).
  `id_effect_sql::metrics::InstrumentedSqlClient` records database operations and durations;
  `record_pg_pool` samples pool gauges.
- **Health:** `HealthRegistry` of effect-backed `HealthIndicator`s, run concurrently with
  per-check timeouts, answers `/ready` with JSON `{status, checks}` and 200 or 503; cache and
  `ReadinessCheck` adapters; `id_effect_sql::health::sql_ping` and `pg_pool_health`.
- **CLI (`id_effect_cli`):** `run_main` handles Ctrl-C gracefully (first SIGINT interrupts so
  finalizers run, exit 130; a second exits at once). New `completions` feature
  (`completions::{generate, CompletionsArgs, Shell}`); `examples/cli-minimal` gains
  `completions <shell>`.
- **Compile time:** `scripts/compile-time.sh` measures the template app's cold build, rebuild and
  boot; 0.8 figures in `docs/benchmarks/compile-0.8.json`.
- **Cron jobs:** `Cron` expressions with time-zone/DST-aware next-fire, `WallClock`,
  `Schedule::cron`; named `CronJob`s with overlap and missed-run policies, success/failure hooks
  and run history, declared in a `CronRegistry` and run directly or through a `Layer` that
  provides a `CronScheduler`. All of it runs on virtual time under `TestExecutor`.
- **Resilience (`id_effect_resilience`):** a `Fallback` policy and `Pipeline`, which composes
  fallback, retry, rate limiter, bulkhead, circuit breaker and timeout into one reusable policy
  with `PipelineEvent` listeners and a per-pipeline metric.
- **Streams:** `zip` / `zip_with` / `combine_latest`; `throttle` / `throttle_enforce` /
  `debounce` on the fiber clock; `group_by_key` / `group_by` substreams with bounded buffers and
  backpressure. Stopping early interrupts upstream and runs its finalizers.
- **Schema:** JSON Schema (2020-12) export for any `Schema` or `HasSchema` type
  (`schema.json_schema()`, `json_schema_for::<T>()`) with constraints, annotations and `$ref`
  recursion, read from the same schema value that validates. `decode_unknown_all` returns every
  error in one pass. Built-in validators `email`, `uuid`, `url`, `length`, `range`, `pattern`,
  `one_of` and friends.
- **Testing:** `run_test`, `assert_exit_*` / `expect_exit_*` and golden files (`assert_golden*`,
  `ID_EFFECT_BLESS=1`). The `proptest` feature returns as `testing::property`: schema-derived and
  framework-type generators, effect-aware properties and model-based testing (`StateMachine`).
  `TestExecutor::seeded(seed)` explores fiber interleavings reproducibly;
  `explore_interleavings` / `assert_interleavings` report the failing seed.
- **Book:** an idiom and style guide (appendix E); its snippets and the interleavings chapter's
  run as doctests.
- **`id_effect_kit` (new crate):** one dependency for apps, with feature bundles `web`,
  `sql-pg`, `otel`, `cluster`, `full`, a `prelude`, whole-crate re-exports and `graph`
  (`LayerGraph`: a layer dependency graph as text or DOT that flags missing providers).
- **`id_effect_tool` (new crate), the `id-effect` binary:** `id-effect new <name> --template
  service|worker|cli|cluster [--db pg] [--otel]` generates a project on the kit with tests, a
  Dockerfile, compose, `.env.example`, a CI workflow and (service) Kamal config; secure defaults,
  `/health`, `/ready` and `/metrics` are wired in. `id-effect doctor [--json] [--graph]` checks
  the toolchain, duplicate id_effect versions, env keys, database reachability, docker and the
  port. CI generates, builds and tests every template and docker-builds the service.
- **Diagnostics:** a gallery of the 20 most common mistakes (`tests/ui/gallery`,
  `docs/diagnostics.md`); 17 of their first errors now state the fix (capability, `~` bind,
  error-conversion and `caps!` errors).
- **Schema:** `#[derive(Schema)]` types export JSON Schema (`$ref`/`$defs` for nested types);
  `schema-regex` feature with a `regex` validator.
- **Docs:** a zero-to-production tutorial; 95 % of the book's Rust snippets now compile in CI
  (`examples/book-snippets`); `scripts/dx-kpis.sh` measures the first-hour KPIs (new service to
  `/health` in about 19 s cold).

### Changed

- **Shutdown:** every host shutdown path marks readiness `shutting_down` before draining, and
  `run_until_shutdown` keeps serving while it drains.
- **Streaming:** `Sink::fold_left` / `from_fold` / `collect` / `drain` / `collect_to_map` return
  `FoldSink`; `FoldSink::zip` and `FoldChannel::from_sink` reject non-fold sinks at compile time
  instead of panicking (`.into()` converts to `Sink`).
- **`RunMainConfig`** is `#[non_exhaustive]` with builder methods.

### Deprecated

- `CsrfConfig::new`, `csrf_middleware`, `csp_middleware`, `set_csrf_header` (removed in 0.9);
  the deprecated CSRF compare is now constant-time.

### Performance

- `Queue::offer` / `take` and `Channel::write` / `read` complete synchronously when they can;
  `retry` / `repeat` read the clock only for schedules that use it. Against 0.7.0:
  `coord/queue_bounded_10k` 0.48×, `coord/channel_spsc_10k` 0.50×,
  `bench_schedule_retry_overhead` 588 → 137 ns. All three are now budget gates.

### Scorecard

Measured by blind re-score (`docs/scorecard/REPORT-0.8.0.md`, METHOD 2.1):

| Index | 0.7.0 | 0.8.0 | Target |
|---|---|---|---|
| Enterprise (E) | 43.2 (#9) | 55.2 (#5) | 53.0 (#6) |
| Runtime (RT) | 56.1 (#6) | 64.4 (#2) | 63.3 (#3) |
| Rows < 40 | 39 | 31 | ≤ 31 |

27 rows up, 1 down (`msg.brokers`: the obix outbox was removed for the MIT relicense). One miss
carried over to 0.9: `data.jsonschema` 38 vs 50.

## 0.7.0 — 2026-10-15 — The runtime release (breaking)

Replaces the `!Send`, busy-polled kernel with an owned fiber runtime on an `Executor` trait
(tokio is the default backend), makes `Effect` `Send` with a typed `R`, adds the Effect.ts
parity layer and prunes the 0.6 deprecations. See
[ADR 0011](docs/adrs/0011-fiber-runtime-send-typed-r.md). Upgrading from 0.6: see the
[0.7 migration guide](docs/MIGRATION-0.7.md), which lists every removal with its replacement.

### Changed

- **Kernel:** `Effect<A, E = Never, R = HNil>` is `Send`; `LocalEffect` / `effect_local!` hold
  `!Send` values. Fibers have real wakers, interruption runs finalizers, and a parent interrupts
  its children when it exits. Effects run once: `retry`, `repeat` and `Layer::effect` take a
  factory.
- **Typed `R`:** capabilities are an HList (`HCons` / `HNil`, `caps!`, `need::<K>()`, `~K`); a
  missing capability is a compile error. `provide`, `provide_env`, `provide_all` and `narrow`
  replace `Env`, `Needs` and `Cap`.
- **Run edges:** `Effect::block_on`, `Runtime::run(..).await` and `Runtime::fork` return `Exit`.
  A blocking edge inside an async context refuses to run (`BlockOnInAsync`).
- **`effect!`** binds sequentially; `~(a, b, ..)` joins in parallel (0.6 parallelised
  independent binds implicitly).
- **`Cause::Die`** carries a `Defect` that keeps the panic payload; panics in fibers,
  finalizers and edges surface as `Die`.
- **Coordination and resources** (`Ref`, `Semaphore`, `Queue`, `Deferred`, `Latch`, `PubSub`,
  `Channel`, `Pool`, `Cache`, …) have plain constructors and suspend on wakers. A bounded
  `Queue::offer` waits; `KeyedPool` capacity is per key; `Cache` caches typed failures.
- **Streams** are owned pulls; `map_effect` is sequential (`map_effect_par(n, f)` for
  concurrency).
- **`Concurrency::Inherit`** takes its limit from the ambient `Fabric`, admitted asynchronously.
- **Satellite crates** are on the 0.7 kernel; their providers are `Layer`s. Highlights:
  `id_effect_axum` runs one fiber per request (panic → 500, disconnect → interrupt);
  `id_effect_cli::run_main` maps defects to 101 and interrupts to 130; `id_effect_config` reads
  only through `Config<T>` and `Config::all` accumulates errors; `id_effect_sql` transactions
  roll back on defects and interrupts and statements run on the transaction's connection;
  `id_effect_process` processes are fibers; `id_effect_node` waits are effects;
  `id_effect_resilience::hedged` is a race; `id_effect_fp` STM retry parks instead of spinning.
- **Labs** crates are migrated to the 0.7 kernel without new features.

### Added

- `Layer` values: `succeed`, `effect`, `scoped`, `acquire_release`, `merge`, `provide_to`,
  `provide_merge`, `fresh`, `build_scoped`, `launch`; `Effect::provide_layer`.
- `zip_par`, `zip_with_par`, `all`, `for_each_par`, `fork_all` with `Concurrency`; `race`,
  `race_all`, `race_first`, `timeout`, `timeout_fail`; `fork_daemon` inherits `FiberRef`s.
- Typed `Schedule<Out, In>` with `Effect::retry` / `retry_or_else` / `repeat` / `repeat_or_else`.
- `Effect::request` with `RequestResolver::make_batched` batching and `RequestCache`.
- `Random` / `TestRandom`; `TestExecutor` (virtual time); `TokioExecutor`.
- Metrics on atomics (`Counter`, `Gauge`, `Histogram`, `Summary`, `Frequency`) in a
  `MetricRegistry`; `tag_metrics`; spans with `SpanHook`; `tracing_hook()` for the in-process
  tracing buffer.
- Schema `tuple5`..`tuple12` and `struct5`..`struct12`; `Config::<T>::layer()`.
- `id_effect_axum::server_layer` / `serve*`, `id_effect_sql` scoped client and transaction
  layers, `id_effect_opentelemetry` registry bridge and `OtelSpanHook`.
- `examples/http-showcase` (axum + sql pg + config + otel, unpublished).

### Removed

- Crates `id_effect_tower` and `id_effect_sql_pg` (use `id_effect_axum` feature `tower`,
  `id_effect_sql` feature `pg`).
- `StreamV1`, `ComputeSupervisor`, `fabric.supervisor()`, `id_effect_platform::process`,
  `#[derive(SchemaParser)]`, `#[capability]`, the root `pipe1/2/3`, `memoize`, `flip`, `const_`,
  `tupled`, `untupled`, `MutableRef`, and the module aliases `id_effect::{channel, ref_, either,
  func, clock, duration, schedule, data, stream, snapshot}`.
- The 0.6 kernel: `run_blocking` / `run_async` / `run_fork` / `run_test*`, `Env` / `Cap` /
  `Needs`, the provider graph (`ProviderSpec`, `ProviderBox`, `build_env`, `run_with`), the
  `provide!` / `providers!` / `require!` / `mock_capability!` macros, `CancellationToken`,
  `Clock` / `TestClock`, the free `retry` / `repeat`, the 0.6 `Metric`, and the
  `testing::{snapshot, proptest, stream}` helpers with the `proptest` feature.
- The `id-effect-diagnose` binary of `id_effect_cli`.

### Fixed

- The request resolver awaits its fetch (no `run_blocking`) and fails entries with a typed
  error instead of panicking.
- `#[derive(Schema)]` compiles for `i32`, `Option<bool | f64 | i32>`, `Vec<i32>` and
  `Vec<T: HasSchema>` fields.
- `Stm::or_else` no longer leaks the retrying branch's writes into the alternative.
- `id_effect_jobs`' `apalis` feature compiles against `apalis-postgres` 1.0.0.

## 0.6.0 — 2026-10-14 — Consolidation (breaking)

Narrows the workspace to its core purpose and hardens what remains. See
[ADR 0010](docs/adrs/0010-crate-tiers.md) and [docs/QUALITY_BAR.md](docs/QUALITY_BAR.md).
0.5.0 was never published; 0.6.0 is the next crates.io release after 0.4.0.
Upgrading from 0.4 or 0.5: see the [0.6 migration guide](docs/MIGRATION-0.6.md).

### Runtime fixes

- `run`, `run_with`, `ThreadSleepRuntime::default()` and the default `TokioRuntime`
  keep a caller-installed Compute Fabric. They used to replace it. They now call the
  new `ensure_default_fabric()`. `install_default_fabric()` still overwrites, and its
  docs say so.
- Telemetry is lazy and never sleeps. `SysinfoTelemetry` creates its
  `sysinfo::System` on the first snapshot, which returns a neutral CPU reading.
  Later samples reuse the cached reading until enough real time has passed. A cold
  `run_with` takes about 1 ms (was about 400 ms); the first Tokio spawn takes about
  3 ms (was about 200 ms).
- New: `ensure_default_fabric`, `installed_fabric::<E>()`, `is_fabric_installed`,
  `uninstall_fabric`, `SysinfoTelemetry::new` / `is_initialized`,
  `ComputeFabric::telemetry`.
- **Breaking:** `SysinfoTelemetry` is no longer a unit struct. Use
  `SysinfoTelemetry::new()`.

### Streams are pull-based

- `map`, `map_serial`, `filter`, `filter_serial`, `take`, `take_while`, `drop_while`,
  `grouped`, `scan`, `map_effect`, `state_scan`, `transduce_items` /
  `via_transducer`, all windows, and `merge` / `combine_latest` / `keyed_join` now
  emit incrementally and work on infinite sources. Previously each one collected its
  whole input first.
- `from_iterable`, `range`, `unfold` and `unfold_effect` produce lazily.
- **Breaking:** `Stream::from_iterable` requires `I::IntoIter: 'static`.
- New `Stream::from_pull` constructor and `PullFn` alias. `Stream::new` and
  `Stream::from_effect` are documented as single-batch.
- `take(n)` never asks upstream for more than it still needs. `take_while` stops at
  the first false.
- Streams end at the first error: the next poll returns `Ok(None)`. This includes
  `chain`, which used to re-poll the failed side.
- `from_channel` and `from_duplex_queue_channel` read one item per pull.
- Fixed: `merge` dropped the rest of the longer side after 64+ elements.
- Fixed: `Channel::from_stream` applied `flat_map_out` only to the first element of
  each chunk. The rest were buffered unchanged, so `[1,2,3]` through
  `x ↦ [x,-x]` read as `1,2,3,-1`.
- `combine_latest` takes one chunk from each side per pull. It used to drain the
  whole left side first.
- `keyed_join`'s memory grows with the number of distinct keys, as documented.
- Time windows emit once the largest timestamp seen passes the window end.
  - `tumbling_by_time`: an item arriving after its bucket was emitted forms a new
    bucket.
  - `sliding_by_time`: items older than an already-emitted window start are dropped.
- Pulls respect a cancelled `CancellationToken` and yield to the executor while
  upstream keeps returning empty batches.
- `map_effect` runs nothing until the first pull.
- New test helper `id_effect::testing::assert_bounded_memory`.

### Workspace restructure (ADR 0010)

- **Labs:** `id_effect_rpc`, `id_effect_ai`, `id_effect_parse`, `id_effect_lint`,
  `id_effect_workflow`, `id_effect_events`, `id_effect_jobs` and `id_effect_graph` move
  to `labs/`. This is a separate Cargo workspace with `publish = false`. These crates
  are no longer published. They are built and tested by their own CI job, and main CI
  no longer builds rdkafka, OpenSSL, bundled SQLite or apalis.
  - `id_effect_jobs` now uses the stable apalis 1.0 family.
- **Merged:** `id_effect_tower` becomes `id_effect_axum::service` (feature `tower`).
  `id_effect_sql_pg` becomes `id_effect_sql::pg` (feature `pg`). The old crates are
  re-export shims for 0.6 only and will be removed in 0.7.
- `id_effect_fsm` no longer depends on `id_effect_workflow`. The durable bridge
  (`register_fsm`, `step_durable`, `restore_state`, `FsmSnapshot`, `WorkflowFsmError`)
  moves to `id_effect_workflow::fsm` (labs).
- `LogLevel` moves into core (`id_effect::LogLevel`); `id_effect_logger` re-exports
  it, and `id_effect_config` no longer depends on `id_effect_logger`.
- `id_effect_optics::transducer` re-exports core's transducer instead of keeping a
  duplicate. Core gains `transducer::take`.
- **Removed:**
  - `#[derive(Fsm)]` and `id_effect::Fsm` (a stub)
  - the generated `T::parser()` from the schema derive (use `id_effect_parse`'s
    `SchemaBridge::parser_for::<T>()`)
  - the empty `algebra` feature
  - the `id-effect` scaffolder binary and `id_effect_cli::generator` (stubs; the
    `id-effect-diagnose` binary stays)
- **Added:**
  - `#[derive(Schema)]` / `id_effect::Schema`. It now also supports `Option<T>` and
    `Vec<T>` fields.
  - `id_effect_platform` feature `test-util`, which `MemoryOAuthClient` now requires.
- **Renamed (old names deprecated):**
  - `ComputeSupervisor` is now `FabricGovernor`, and `ComputeFabric::supervisor()` is
    now `governor()`
  - `id_effect_platform::process` is now `command`
  - `SchemaParser` is now a deprecated alias of `Schema`
- **Changed:** `Monad::flatten` has a real default implementation and no longer calls
  `unimplemented!`.
- **Publishing:** publish order lives in one place
  (`scripts/validate-crate-packaging.sh --print-order`), and `publish.yml` loops over
  it.

### Slimmer core and the new `id_effect_fp` crate (breaking)

- New maintenance-tier crate **`id_effect_fp`**. It takes the code core does not use
  itself: `algebra`, `collections` (persistent structures), `stm`,
  `kernel::{result, reader, thunk}`, and the unused `foundation` pieces (`unit`,
  `never`, `function`, `product`, `coproduct`, `isomorphism`, `option_`). These paths
  move:
  - `id_effect::{algebra, collections, stm}` → `id_effect_fp::{algebra, collections, stm}`
  - `id_effect::kernel::{result, reader, thunk}` → `id_effect_fp::{result, reader, thunk}`
  - Root re-exports such as `EffectHashMap`, `EffectVector`, `RedBlackTree`, `Trie`,
    `MutableList`, `ChunkBuilder`, `Stm`, `TRef`, `TMap`, `atomically`, `Foldable`,
    `Alternative` and `traverse_vec` → `id_effect_fp::…`
- Placement moves to **`id_effect_node::placement`**: `PlacementMode`,
  `ClusterResourcePolicy`, `NodeCandidate`, `LoadReport`, `place`,
  `pick_placement`, `score_candidate` and `ClusterTelemetry`.
  `FabricGovernor::scale_out_placed` becomes the free function
  `id_effect_node::scale_out_placed`.
- New default-on **`compute`** feature: it gates `compute::*` and parallelism, and
  brings in rayon and sysinfo. With `--no-default-features`, core builds without
  rayon, sysinfo or im, and streams, sorts and runtimes run serially.
- `im` is no longer a dependency. The following now use std collections:
  `SpanRecord::annotations`, `TracingFiberRefs::span_annotations`,
  `Metric::snapshot_frequencies`, `Sink::collect_to_map` (`HashMap`) and
  `TimeBucketMap` (`BTreeMap`).
- tokio stays a required dependency (features `macros`, `rt`, `sync` only), because
  the coordination primitives are built on `tokio::sync`.
- Core no longer has a dev-dependency on `id_effect_config`.

### Hardened runtime crates

- **`id_effect_process`:**
  - A stopped process's mailbox is closed on every exit path. Sending to a dead pid
    now returns `SendError::Rejected`; it used to succeed silently.
  - A panic in a handler, `init` or `terminate` is caught. The process exits with
    `ExitReason::Error("panic: …")` and goes through the normal exit path:
    links and monitors fire, pending calls fail with `CallError::Down`, and the
    supervisor restarts it. Before, it never finalized and callers timed out.
  - New `Addr<DynamicSupervisor>::terminate_dyn_child`.
  - Tests went from 23 to 80.
- **`id_effect_fsm` (breaking):**
  - `Interpreter::step` and `run` take the machine by value and return an `Effect`
    yielding the updated machine. The new state is kept only if the action succeeds.
  - `Saga::run` and `compensate_indices` return `Effect`s.
  - Session types are removed (`SessionSend`, `SessionRecv`, `PingPong`, …).
  - `#[derive(Fsm)]` rejects duplicate edges even when the same event is spelled
    two ways (`Go` and `E::Go`).
  - New: a real `#[derive(Fsm)]` (`#[fsm(event = …, initial = …)]`,
    `#[fsm(on(Ev => Target))]`) with compile-time checks, and a gen_statem-style
    process behaviour (`FsmProcess`, `FsmSpec`, `FsmHandle`). fsm now depends on
    `id_effect_process`.
- **`id_effect_node`** (tests went from 30 to 203; bugs found while testing):
  - Call/spawn replies and monitor `Down` messages were broadcast to every peer, so
    one node's reply could complete another node's request. They now go only to the
    sender (new `Mesh::handle_envelope_from`).
  - Monitoring an unreachable node now fires `NoConnection`. Demonitor works (new
    `Mesh::remote_demonitor`). Pending requests are cleaned up on failure or timeout.
  - A child whose takeover fails is kept as orphaned and retried instead of being
    dropped (`DistributedSupervisor::orphaned` / `retry_orphaned`).
    - Placement tries the preferred node and then the local node.
    - Retries run in the background (`on_membership` now returns
      `Option<RetryHandle>`) and skip the departed node until it rejoins.
  - Monitors that peers hold on our processes are keyed by the peer's name and boot
    incarnation, and are removed when the peer goes down. A restarted node can no
    longer receive another incarnation's `Down`.
  - Monitor sinks run after the lock is released, so a sink can call
    `remote_monitor` or `remote_demonitor` without deadlocking.
  - In-memory transport:
    - `close()` closes both ends, and a dropped listener frees its address.
    - Frames that already arrived are delivered after a close or partition.
    - Connections killed by a partition stay dead after `heal`.
  - QUIC: it used to panic whenever it built a TLS config (two crypto providers were
    linked). It now uses an explicit provider and TLS 1.3 only. The server name is
    configurable (`QuicConfig::with_server_name`, default `"cluster"`).
  - **Breaking:** `PgAdvisoryLease` lock keys are now derived with SHA-256 (they
    used to come from std's hasher, which can change between Rust versions). Do not
    run old and new builds against the same singleton.

### Capability ergonomics

- New `Effect::widen_error::<E>()`: lifts an effect whose error is `()`, `Never` or
  `Infallible` into any error type `E`, with no `map_error(|()| unreachable!())`. On a
  real error type it is a compile error (sealed `kernel::InfallibleError`).
- New `Effect::into_env::<R>()` for `R = ()` effects. `~` binds any `()`-environment
  effect in any environment.
- **Breaking:** `EffectLogger` methods (`info`, `debug`, …) no longer take a type
  parameter. They return `Effect<(), EffectLoggerError>`, so write
  `~logger.info("…")` inside `effect!`, or `.into_env()` elsewhere. An explicit
  turbofish no longer compiles.
- `#[provides(Service, with = || …)]` (or `with = |deps: &Env| …`, or
  `with = Self::build`) defines a provider without a hand-written constructor, so
  `#[allow(clippy::new_ret_no_self)]` is no longer needed.
- `type AppCaps = caps!(A, B);` aliases work in `effect!(|r: &mut AppCaps| …)`.
- Capability diagnostics:
  - `#[track_caller]` on `Env::get`, `try_get`, `get_any`, `Needs::need`.
  - **Breaking:** `try_get` and `get_any` misses return
    `CapabilityError::MissingAt { cap, location }`. Match both shapes with
    `err.missing_capability()`.
  - `CapabilityId::name()` and `Display` show service names.
  - `#[diagnostic::on_unimplemented]` hints on `Needs`, `CapabilityKey`,
    `CapabilitySet`, `FromEnv`, `CapKeys`, `HasCap`, `CapWiden` and `CapBind`.
- `#[capability]` is documented as the deprecated no-op it has been since 0.3. It
  will be removed in 0.7.

### Integrations

- **`id_effect_axum` (breaking):**
  - Effects run on an `EffectExecutor`, which replaces `block_in_place`. It is an
    elastic pool of dedicated threads: each in-flight request gets its own thread with
    a private single-threaded runtime. Handler futures are `Send`, a blocking effect
    stalls only its own request, and current-thread runtimes work.
    - Bounded admission: `max_threads` (default 512) plus `queue_capacity` (default
      1024).
    - `SaturationPolicy::Wait` (default) or `Reject`, which answers 503 with
      `Retry-After`.
    - `DropPolicy::RunToCompletion` (default): an effect finishes even if the client
      disconnects. `DropPolicy::Cancel` is opt-in.
    - Configure it with `EffectExecutor::builder()`. A service can take its own
      executor via `EffectService::with_executor`.
  - `run_with_env` and `run_with_caps` now require `Send + 'static` closures and
    `Send` result and error types.
  - `tokio::spawn` inside an effect runs on an executor worker.
  - New request-scoped capability overrides: `CapOverrideLayer`, `RequestCaps`,
    `execute_with_request_caps`.
- **`id_effect_platform`:** the `auth` module is behind an opt-in `auth` feature.
  reqwest 0.13 only.
- **`id_effect_sql`:** live Postgres tests fail when `ID_EFFECT_REQUIRE_PG=1` and no
  database is available (CI's new `pg-integration` job sets it). Otherwise they print
  `SKIP`; they no longer pass silently.
- **`id_effect_cli`:** new example `010_exit_codes`. `examples/cli-minimal` is the
  template, and `scripts/check-version-drift.sh` (run in CI) catches stale
  `id_effect` version pins.
- **`id_effect_resilience`:**
  - New `retry_with_breaker`: retries on a `Schedule` but fails fast when the breaker
    is open.
  - Clock injection: `CircuitBreaker::make_with_clock`, `RateLimiter::make_with_clock`.
  - Bulkhead's docs now explain that it rejects instead of waiting.
- Every published crate has a README with an example and passes the
  quality-bar check for its tier (`scripts/tier-conformance.sh`).

### Tooling

- MSRV is declared: `rust-version = "1.94"`, checked by a CI `msrv` job.
- Every crate declares its tier in `[package.metadata.id_effect]`.
  `scripts/tier-conformance.sh` checks the quality bar for that tier.
- `scripts/readme-crates.sh` generates the README crate table.

## 0.5.0 — Process Mesh & distributed runtime (never published)

Semver-minor release introducing the **Process Mesh**: a BEAM/OTP-style local
process runtime (`id_effect_process`) and a distribution layer (`id_effect_node`)
for node identity, membership, remote processes, and distributed supervision.
Existing cluster/supervision/jobs/workflow stubs are wired into the mesh behind a
single placement scorer. See [ADR 0009](docs/adrs/0009-process-mesh.md) and
[book Part VII](crates/id_effect/book/src/part7/ch36-00-process-mesh.md).

### Added

- **New crate `id_effect_process`** — typed mailboxes, GenServer-shaped behaviors,
  links, monitors, `trap_exit`, exit-signal propagation, and multi-strategy OTP
  supervision trees, deterministic under `TestClock`.
- **New crate `id_effect_node`** — node identity, cookie-authenticated handshake,
  pluggable transports (InMemory/TCP/QUIC), SWIM membership, remote processes,
  process groups, and distributed supervision.
- **Unified placement API** — `id_effect::place` is the single pure entry point,
  fed by `LoadReport` (mesh telemetry DTO) with a `NodeCandidate` adapter. New
  `PlacementMode::RoundRobin` (deterministic rotation) joins `Affinity`,
  `LocalFirst`, and `Spread`.
- **Placement-aware distribution** — `mesh` gains `report_load` / `load_reports` /
  `remote_spawn_placed`; `DistributedSupervisor` places children via
  `id_effect::place`, honoring `spec.prefer` and excluding downed nodes on takeover.
- **Node-aware jobs** — `FabricJobRunner` honors `JobSpec.target_node` via
  `dequeue_for` / `pending_for`; `ComputeSupervisor::scale_out_placed` targets flow
  through to the runner.
- **Distributed workflow journal** — `RemoteStepJournal` backs `DistributedStepJournal`
  over a `JournalTransport` (`JournalServer` + `LoopbackTransport`) with a wire
  protocol and `WorkflowError::Transport`. `NetworkJournalStub` is retained as a
  test double.
- **`PgAdvisoryLease`** (feature `pg`) — `SingletonLease` backed by Postgres
  session-scoped advisory locks on a dedicated worker connection (CP production
  path). `MemoryLease` retained for tests.
- **`concurrency::RestartIntensity`** — shared sliding-window restart-intensity
  primitive (distinct from `RestartWithLimit`'s total-count cap).
- Examples: `132_distributed_supervisor_failover.rs`, `133_cluster_stream_fanout.rs`.

### Changed

- Capability API uses **service type names** directly: `caps!(Counter)`, `require!(Counter)`, `#[provides(Counter)]`.
- `Cap<T>` replaces generated `*Key` types; `#[capability]` is a no-op (removed from public flow).
- Trait-backed services use type aliases (`HttpClientService`, `ConfigProviderService`, …).
- `PlacementMode::LocalFirst` and `Spread` now diverge — previously they scored
  identically. `LocalFirst` prefers the least-loaded node; `Spread` picks lowest raw load.
- `id_effect_process` OTP supervisor delegates restart-intensity accounting to the
  shared `concurrency::RestartIntensity` primitive (behavior preserved).
- **Workspace versioning** — the root manifest owns `[workspace.package]`
  (version/edition/license/repository); every publishable member inherits via
  `version.workspace = true`, so future bumps propagate atomically.

### Removed

- Public `*Key` types and `define_capability!`.

### Version alignment

| Crate | Version |
|-------|---------|
| `id_effect` | **0.5.0** |
| `id_effect_process` | **0.5.0** (new) |
| `id_effect_node` | **0.5.0** (new) |
| workspace adapters | **0.5.0** |

## 0.4.0 — Implicit parallelism (breaking)

Semver-major release collapsing caller-facing parallelism policy into **Compute Fabric**. See [ADR 0008](docs/adrs/0008-implicit-parallelism.md) and [book ch13-05](crates/id_effect/book/src/part4/ch13-05-parallelism.md).

### Breaking

- **Removed public `Parallelism`** and all `*_with(policy, …)` dispatch on collections, `vec`, `order`, and `Stream`
- **Removed deprecated `*_par`**, `Stream::map_par_n`, `Stream::map_par_adaptive`, and public `compute::effective_threshold`
- Bulk and stream-chunk ops no longer accept caller policy — Fabric's `should_parallelize_current` is the sole threshold

### Added

- `compute::dispatch::parallel_if_profitable` — central Fabric-aware Rayon dispatch
- `Stream::map_effect` — admission-bounded concurrent effect mapping per chunk
- EDG parallel codegen for independent bind sets (`join_binds2` / `join_binds3` / `join_binds4`)
- Default Compute Fabric installation in `run()` and `run_with()`; `ensure_run_context()` at run boundaries
- Book rewrite: [Implicit Parallelism](crates/id_effect/book/src/part4/ch13-05-parallelism.md); Compute Fabric cross-refs in [ch12](crates/id_effect/book/src/part3/ch12-00-compute-fabric.md)
- Example: `122_compute_fabric_effect_parallel.rs` (`Stream::map_effect` under Fabric)

### Changed

- Primary `map`, `filter`, `map_values`, `sort_with`, and related bulk APIs parallelize implicitly when Fabric says it is profitable
- `effect!` may run independent `~` binds concurrently when the EDG finds no data or capability conflict
- `*_serial` and `#[effect(serial)]` remain the explicit escape hatches for `FnMut`, non-`Send`, and ordering

### Migration (from 0.3.x parallel-by-default)

| Old | New |
|-----|-----|
| `Parallelism::…` | removed — trust Fabric |
| `map_with(Parallelism::Serial, f)` | `map_serial(f)` |
| `map_with(Parallelism::ForceParallel, f)` | `map(f)` |
| `*_par(f)` | `map(f)` |
| `Stream::map_par_n(n, f)` | `map_effect(f)` |
| `Stream::map_par_adaptive(f)` | `map_effect(f)` |

Prior [ADR 0006](docs/adrs/0006-parallel-by-default.md) bulk behavior is retained internally; the public policy surface is superseded by [ADR 0008](docs/adrs/0008-implicit-parallelism.md).

### Version alignment

| Crate | Version |
|-------|---------|
| `id_effect` | **0.4.0** |
| workspace adapters | **0.4.0** |

## 0.3.0 — DI maturity (breaking)

Semver-major release completing capability-first DI adoption. See [appendix-b-migration.md](crates/id_effect/book/src/appendix-b-migration.md) and [ADR 0004](docs/adrs/0004-provider-parity-and-cap-subtyping.md).

### Removed

- `CapEnv1…CapEnv6` — use `CapList` / `caps!(K0, K1, …)` only
- `#[capability]` / `*Key` types — use service names with `Cap<T>`
- `require!(env, K)` — use `require!(K)` inside `effect!` or `Needs::<K>::need(env)`
- `ctx!`, `req!`, `service_key!`, `Layer`, `Stack`, `Effect::provide`, `IntoBind` (legacy paths)
- `id_effect_config::ambient` — use `Env::scoped` / `build_env`

### Added

- `CapList` + unbounded `caps!` arity
- `Cap<T>`, `#[derive(ProviderSpecDerive)]`, `#[provides(Service)]`, `#[named("variant")]`
- `CapWiden` for capability-set subtyping
- `ProviderSpec::optional_requires`, `shared()`, `refresh_interval()`, `on_refresh()`
- Fiber/request scoped overrides: `with_override`, `with_fiber_and_override`
- `id-effect-diagnose manifest` with TOML/JSON + `--json`
- Examples: `042_effectful_config_provider`, `043_named_variant_providers`, `id_effect_axum` `020_capability_run_with`
- Expanded trybuild corpus (12 cases) under `tests/ui/`

### Version alignment

| Crate | Version |
|-------|---------|
| `id_effect` | **0.3.0** |
| `id_effect_platform` | **4.0.0** |
| workspace adapters | **0.3.0** |

## Pre-0.3 history — "2.0.0" capability-first DI

Internal milestone numbered "2.0.0" before the workspace settled on 0.x semver. It was
never published under that number; its changes shipped as part of 0.3.0 above.
