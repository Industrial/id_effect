# Migrating to id_effect 0.10

This guide is the "if you used X, now do Y" companion to the `0.10.0` section of
[CHANGELOG.md](../CHANGELOG.md). It assumes you are on 0.9. If you are on 0.8, apply
[MIGRATION-0.9.md](MIGRATION-0.9.md) first.

Every intentional public-API break in 0.10 is listed here. A break reported by
`cargo semver-checks` (the `semver` CI job, baseline 0.9.0) that is not listed below is a bug.
The deprecation rule and semver policy are in [STABILITY.md](STABILITY.md).

Each 0.10 change appends its entry to the matching section below.

## Observability: `LogRecord` is `#[non_exhaustive]`

- `id_effect_logger::LogRecord` gains the field `target: Option<Cow<str>>` and is
  `#[non_exhaustive]`, so it can no longer be built with a struct literal outside the crate. Its
  fields stay public for backends to read (and for a `RedactionPolicy` to rewrite).

  ```rust,ignore
  // 0.9
  let record = LogRecord {
    level: LogLevel::Info,
    message: "charged".into(),
    annotations: fields,
    spans: vec!["checkout".into()],
  };
  // 0.10
  let record = LogRecord::new(LogLevel::Info, "charged")
    .with_annotations(fields)
    .with_spans(vec!["checkout".into()])
    .with_target("billing::invoice"); // optional
  ```

- A custom `LogBackend` that destructures `LogRecord { level, message, annotations, spans }`
  needs `..` in the pattern. JSON, structured and `tracing` output gains a `target` only when one
  is set (`EffectLogger::log_target`).

## Observability: the composite backend and redaction policy are process-wide

- `provide_composite_logger` and the new `provide_redaction_policy` install into one process-wide
  slot instead of a thread-local. In 0.9 a composite installed on the main thread was skipped by
  lines logged on other threads (a server's worker threads), which went to `tracing` directly.
  Those lines now go through the composite, so expect them in your JSON / structured sinks.
- Each layer build replaces the previously installed composite (or policy) for the whole process.
  Tests that each installed their own composite and ran in parallel threads now share one: give
  such tests their own process (nextest does by default) or assert on a shared backend.
- The minimum level (`provide_minimum_log_level`), annotations and spans are still per fiber /
  thread. The new `set_global_minimum_log_level(Some(level))` overrides the minimum level for the
  whole process while it is set (the operator console uses it).

## Observability: `OtelConfig` has a `sampler`

- `id_effect_opentelemetry::OtelConfig` gains `pub sampler: OtelSampler`. A struct literal must
  add it; prefer starting from a constructor:

  ```rust,ignore
  // 0.9
  let cfg = OtelConfig { endpoint, protocol, service_name, /* … */ env_filter: None };
  // 0.10
  let cfg = OtelConfig { endpoint, protocol, service_name, /* … */ env_filter: None,
    sampler: OtelSampler::default() };
  // or
  let cfg = OtelConfig { endpoint, ..OtelConfig::localhost("billing") };
  ```

- `OtelSampler::default()` is `ParentBasedAlwaysOn`, the OpenTelemetry SDK default, so traces
  are sampled as in 0.9. `OtelConfig::from_env` now reads `OTEL_TRACES_SAMPLER` /
  `OTEL_TRACES_SAMPLER_ARG` and fails with `OtelError::Config` on an unknown sampler name or a
  ratio outside `[0, 1]`.

## Web: HTTP client timeouts and retries

- `id_effect_platform`'s reqwest layer (`ReqwestHttpClientProvider::layer`,
  `provide_reqwest_http_client`) and `ReqwestHttpClient::default_client()` had no timeout. They
  now use a 10 s connect timeout and a 30 s total timeout per request (a request's own
  `HttpRequest::timeout` replaces the total).
- The layer wraps the client in a `RetryingHttpClient` with `RetryPolicy::default()`: up to 3
  attempts, exponential backoff from 100 ms with jitter, on connect errors, `5xx` and `429`,
  for `GET`, `PUT` and `DELETE` only (`POST` / `PATCH` only when you opt in with
  `with_methods`). A `Retry-After` up to 30 s sets the minimum wait; a longer one returns the
  response. An idempotent request answering `5xx` / `429` can therefore reach the server up to 3
  times. To keep 0.9's single attempt, or change the timeouts:

  ```rust,ignore
  use id_effect_platform::http::{HttpClientConfig, ReqwestHttpClientProvider, RetryPolicy};

  // 0.9
  let http = ReqwestHttpClientProvider::layer();
  // 0.10, one attempt per call as before
  let http = ReqwestHttpClientProvider::layer_with(
    HttpClientConfig::default().with_retry(RetryPolicy::none()),
  );
  // 0.10, longer calls
  let http = ReqwestHttpClientProvider::layer_with(
    HttpClientConfig::default().with_timeout(Duration::from_secs(120)),
  );
  ```

- `HttpMethod` has no `HEAD` / `OPTIONS` variants, so those are not retried.
- Unchanged: `ReqwestClientLive::layer()` and `provide_reqwest_pool` still build a client with no
  timeout and no retry.

## Data: connection and TLS errors, SQLSTATEs

- `SqlError` gains `ConnectionFailed { message }` (the database could not be reached: refused,
  reset, network timeout) and `TlsFailed { message }` (TLS required but not offered, certificate
  not verified, handshake failed). Both used to be `QueryFailed`. `SqlError` was already
  `#[non_exhaustive]`; code that classifies errors by variant should handle the new ones:

  ```rust,ignore
  // 0.9
  Err(SqlError::QueryFailed { message, .. }) if message.contains("Connection refused") => retry(),
  // 0.10
  Err(SqlError::ConnectionFailed { .. }) => retry(),
  Err(SqlError::TlsFailed { message, .. }) => fail_config(message),
  ```

  The two new variants are themselves `#[non_exhaustive]`: match them with `{ message, .. }` and
  build them with `SqlError::connection_failed(..)` / `SqlError::tls_failed(..)` (test doubles).
- A server error's `QueryFailed` / `TransactionFailed` message now ends with `(SQLSTATE xxxxx)` on
  every backend, plain PostgreSQL `query` / `execute` / `connect` / `begin` errors included (0.9
  added it only on PostgreSQL transaction paths, MySQL, and busy SQLite). Read the code with
  `err.sqlstate()` rather than parsing the message; a test that compares a PostgreSQL error
  message for equality must allow the suffix.
- A MySQL duplicate key (`ER_DUP_ENTRY`, SQLSTATE `23000` in 0.9) and a SQLite unique or primary
  key violation (no SQLSTATE in 0.9) report `23505`, as PostgreSQL does. Check for it portably:

  ```rust,ignore
  // 0.9
  Err(SqlError::QueryFailed { message, .. }) if message.contains("duplicate key") => conflict(),
  // 0.10
  Err(e) if e.is_unique_violation() => conflict(),
  ```

## Data: `SqlClient` for `Box` and `Arc`

- `id_effect_sql` implements `SqlClient` for `Box<T>` and `Arc<T>` (`T: SqlClient + ?Sized`),
  forwarding every method including `begin_with` and `query_stream`. Remove an
  `impl SqlClient for Box<YourClient>` of your own (it now conflicts) and any wrapper type that
  existed only to forward an `Arc<dyn SqlClient>`.

## Data: `#[from_row(schema)]` reports every invalid column

- A `#[derive(Schema)]` type now decodes every field in `decode_unknown_all` (nested derived
  types, `Vec` elements and `Option` contents included) instead of stopping at the first error.
  `#[from_row(schema)]` on such a type therefore returns `SqlError::InvalidRow { issues }` listing
  every invalid column, not just the first. Code or tests that read `issues[0]` as "the" problem,
  or assert `issues.len() == 1`, should look for the column they expect. `decode` /
  `decode_unknown` still stop at the first error.

## Data: sqlx's `time` feature is off again

- `id_effect_sql`'s `pg`, `mysql` and `sqlite` features no longer enable sqlx's `time` feature,
  and the crate no longer depends on `time` (this reverses the 0.9 section "sqlx's `time` feature
  is on"). Date-times are converted by `id_effect_sql` itself, from each database's wire format
  into jiff; the public API (`SqlValue::Timestamp` / `DateTime` / `Date` / `Time`) is unchanged.
  With sqlx's `chrono` in your build, sqlx's `query!` / `query_as!` macros type date-time columns
  as chrono types again, and dependencies written against that mapping compile again. Code of
  your own that decodes sqlx rows into `time` types (`OffsetDateTime`, `PrimitiveDateTime`) must
  now enable sqlx's `time` feature itself (`sqlx = { version = "0.8", features = ["time"] }`).
  CI fails if `id_effect_sql` or `id_effect_kit` brings either back.
- The `query` feature enables sea-query's `with-jiff` only (not `with-time`). Build statements
  with jiff values (`Expr::val(jiff::Timestamp)`, or `Expr::val(x.to_sql_value())`); a
  `sea_query::Value::TimeDateTime*` / `TimeDate` / `TimeTime` value, which now exists only when
  another crate turns on sea-query's `with-time`, is `SqlError::Unsupported` in `sql_values` and
  `SqlValue::try_from`.
- PostgreSQL: `TIMESTAMPTZ` / `TIMESTAMP` / `DATE` / `TIME` values that jiff cannot hold
  (`infinity`, `-infinity`, years beyond ±9999) and values before PostgreSQL's earliest date
  (4714-11-24 BC) are `SqlError::ValueOutOfRange`, never a panic.
- MySQL: `DATE`, `DATETIME` and `TIMESTAMP` parameters are sent as MySQL literals
  (`'2024-02-29 12:34:56.123456'`, a `Timestamp` as its UTC wall clock) instead of typed
  date-time parameters; the server converts them when it stores or compares them, so inserts,
  updates and `WHERE` clauses behave as in 0.9. A parameter with no column to convert to (a bare
  `SELECT ?`, `COALESCE(?, …)`) now reads back as `SqlValue::Text`: write
  `CAST(? AS DATETIME(6))` (or `DATE`) where the type matters. `TIME` parameters are unchanged.

## Events: `id_effect_events` from labs to published

`id_effect_events` moved from `labs/` to `crates/` (tier `integration`) and runs on
`id_effect_sql`. Code written against the labs crate changes as follows (the crates.io
`id_effect_events` 0.4.0 is an older, unrelated line).

- Dispatch and projections are `Effect`s, not synchronous `Result`s or `async fn`s, and take the
  projection by value:

  ```rust,ignore
  // labs
  let state = dispatch_command(&handler, &store, "acct-1", &projection, cmd)?;
  let out = runner.run_all(&store, "acct-1", 1, &projections).await?;
  // 0.10: build the effect, then run it like any other
  let state: Effect<S, DispatchError<_>> =
    dispatch_command(&handler, &store, "acct-1", projection, cmd);
  let out = runner.run_all(&store, "acct-1", 1, projections); // HashMap by value
  ```

  `query_projection` and `run_projection_from_store` follow the same shape. `dispatch_command`
  needs a `Clone` store (`MemoryEventStore` and `SqlEventJournal` are shared handles; wrap others
  in an `Arc`, which is an `EventStore` too).
- `EventStore` gains the required method `append_expected(stream_id, expected_version, events)`:
  a store of your own must implement it (append nothing and fail with
  `EventStoreError::VersionConflict { expected, actual }` when the stream is not at
  `expected_version`). Use it instead of `append` wherever a command was decided on a state read
  earlier.
- `EventStoreError` is `#[non_exhaustive]` and gains `Sql(SqlError)`: add a `_ =>` arm.
- `SqlEventJournal` runs over any `SqlClient` instead of a `SqlJournalBackend`
  (`SqlJournalBackend` and `TestSqlJournalBackend` are removed; test with `TestSqlClient` or
  SQLite in memory):

  ```rust,ignore
  // labs
  let journal = SqlEventJournal::<Evt>::new(Arc::new(my_backend));
  // 0.10
  let journal = SqlEventJournal::<Evt>::new(client); // Arc<dyn SqlClient>
  let journal = SqlEventJournal::<Evt>::from_service(&sql_service);
  ```

- The feature `es-entity` is removed with `EsEntityEventStore`, `EsEntityPgBackend`,
  `ES_ENTITY_EVENT_JOURNAL_DDL`, `apply_es_entity_journal_ddl`, the providers
  (`provide_es_entity_events*`, `EventJournalBackend`, `EventJournalEnv`) and
  `dispatch_command_es_entity`. Use `SqlEventJournal` with the `pg` (or `sqlite`) feature.
- The journal is the table `id_effect_event_journal` (`SQL_EVENT_JOURNAL_DDL`, the same on
  PostgreSQL and SQLite; payloads are JSON text) with a global `position BIGINT NOT NULL UNIQUE`
  read by `EventLog::read_all` and catch-up subscriptions. Create it in a migration or with
  `SqlEventJournal::create_table`. Rows from es-entity's `es_entity_event_journal` (same columns,
  `JSONB` payload) can be copied; the original cross-stream order is not recorded there, so pick
  one:

  ```sql
  INSERT INTO id_effect_event_journal (position, stream_id, version, event_id, payload)
  SELECT ROW_NUMBER() OVER (ORDER BY stream_id, version), stream_id, version, event_id,
         payload::text
  FROM es_entity_event_journal;
  ```

  A journal table created by a pre-release build of the published crate (without `position`)
  must be recreated the same way.
- Projection ordering no longer depends on `id_effect_graph`; `EventStoreError::Graph` is still
  the error for a dependency cycle.

## Jobs and messaging: labs `id_effect_jobs` is removed

`labs/id_effect_jobs` (with its apalis, obix, `job` and rdkafka dependencies) and
`labs/id_effect_graph` are gone. The published `id_effect_jobs` reuses the crate name with a new
API (the crates.io `id_effect_jobs` 0.4.0 is an older, unrelated line); messaging moved to
`id_effect_messaging`.

| labs `id_effect_jobs` | 0.10 |
|---|---|
| `JobSpec`, `JobRunner`, `MemoryJobRunner`, `FabricJobRunner`, `drain_jobs` | `id_effect_jobs::{Job, JobRegistry, JobQueue, MemoryJobQueue, PgJobQueue, Worker}` (`Worker::run_until_idle` drains) |
| `ApalisJobQueue`, `ApalisJobPayload`, `JobQueue` (feature `apalis`) | `id_effect_jobs::PgJobQueue` (PostgreSQL, `SKIP LOCKED`; `migrator()`), `enqueue_in(&tx, …)` |
| `JobRecord`, `JobState`, `JobError` | `id_effect_jobs::{JobRecord, JobState, JobError}` (new shapes: states `scheduled`, `running`, `failed`, `succeeded`, `dead`, `cancelled`) |
| `MessageBroker`, `MemoryBroker`, `KafkaBrokerStub` | `id_effect_messaging::{MessageBroker, MemoryBroker}` |
| `RdKafkaBroker`, `RdKafkaConfig` (feature `kafka`) | no Kafka adapter in 0.10; `id_effect_redis::RedisStreamsBroker` (feature `streams`) |
| `MemoryOutbox`, `OutboxRecord`, `OutboxTable`, `relay_outbox`, `OutboxError` | `id_effect_messaging::{Outbox, OutboxStore, MemoryOutbox, PgOutbox, Relay, OutboxError}` |
| `ObixInbox`, `InboxPersistResult` (feature `obix`) | `id_effect_messaging::{Inbox, MemoryInbox}` (`Inbox::once`, `Inbox::handler`) |
| `labs/id_effect_graph` | none (it had no other consumer) |

A job is a `Schema` payload type with a `Job` impl instead of a name plus bytes:

```rust,ignore
// labs
let runner = MemoryJobRunner::new();
run(runner.enqueue(JobSpec::new("send_receipt", serde_json::to_vec(&order)?)));
run(drain_jobs(runner, 10, |job| send_receipt(&job.spec.payload)));

// 0.10
#[derive(Clone, Schema)]
struct SendReceipt { order: i64 }

impl Job for SendReceipt {
  const NAME: &'static str = "shop.send_receipt";
  const QUEUE: &'static str = "mail";
}

let queue = PgJobQueue::new(client); // or MemoryJobQueue::new(clock) in tests
run(queue.enqueue(SendReceipt { order: 1 }, JobOptions::new()));
let registry = JobRegistry::<JobQueueEnv>::new()
  .register::<SendReceipt, JobFailure>(|job, _ctx| send_receipt(job.order))?;
let worker = Worker::new(registry).queue("mail", 2);
// worker.run() in the service, or worker.run_until_idle() in a test, with a JobQueueService
```

Book: part6/ch31-01-jobs.md and ch31-02-messaging.md; `id-effect generate job <name>` writes
this boilerplate in a generated project.

## Tooling: `id_effect_tool`

- `cli::NewArgs` gains the public fields `jobs` and `console` (the `--jobs` / `--console` flags).
  Code that builds `NewArgs` with a struct literal must set them (`false` for 0.9's output).
- `render::Options` gains the same two fields, and `template::Condition` gains the variants
  `Jobs` and `Console`. Struct literals of `Options` must set them; a `match` on `Condition`
  needs the two new arms (or `_ =>`).
- `cli::Command` gains `Dev` and `Generate`; it is already `#[non_exhaustive]`, so a `_ =>` arm
  covers them. Running the `id-effect` binary is unaffected.
- Projects generated by 0.9 keep working. To pick up 0.10's service changes (readiness that turns
  503 on SIGTERM, request draining, the Kamal `/ready` check and drain timeout), compare with a
  freshly generated project: `src/lib.rs` gains `serve(host, app)` (`track_in_flight` +
  `serve_router`), `src/main.rs` builds the health registry `with_readiness(host.readiness)` and
  runs `serve` under `HostBuilder::run_until_shutdown`, and `config/deploy.yml` checks `/ready`
  with `drain_timeout 35` and `SHUTDOWN_TIMEOUT_SECS "30"`.

## Reference app: Ledger requires CSRF tokens

- Only for clients of the Ledger example (`examples/reference/ledger`), not a library change: it
  now runs behind `SecurityLayer::recommended`, so unsafe requests authenticated by cookie (or
  anonymous) need the `X-CSRF-Token` header from `GET /auth/csrf`. Bearer-only requests are
  exempt (`SecurityLayer::csrf_exempt_bearer`).

## Not breaking but notable

Defaults and behaviour that changed without a signature change:

- `id_effect_platform`: the reqwest layer times out after 10 s connecting / 30 s in total and
  retries idempotent requests (see "Web: HTTP client timeouts and retries").
- `id_effect_logger`: the composite backend and the redaction policy apply on every thread (see
  above); worker-thread lines that used to reach `tracing` directly now reach your sinks.
- `id_effect_config`: `ConfigError` messages from a malformed profile file keep the key path,
  file, line / column and error kind but no longer quote the source line or the values found.
  Tests that matched on figment's quoted text must match on the kind or key instead.
- `id_effect_sql`: wrapping a `PgSqlClient` in `InstrumentedSqlClient` keeps server-side
  `query_stream` and `begin_with` options (0.9 silently fell back to the trait defaults) and
  emits a `db.query` span per statement.
- `id_effect_node`: `PgAdvisoryLease::from_pool_config` honours `tls_mode` / `root_cert` from the
  pool config (0.9 connected with the URL only). `ClusterSingleton::resign` clears the local pid
  even when the release fails; `PgAdvisoryLease::try_acquire` pings its session and fails
  `Unavailable` after the session died, instead of reporting a lease it no longer holds.
- `id_effect_node` feature `quic`: quinn is built on rustls' aws-lc-rs provider (no `ring`), and
  `QuicConfig`'s fallback provider is aws-lc-rs.
- `id_effect_kit`: the `sql` feature enables `id_effect_opentelemetry/sql` when `otel` is on.
- `id_effect_jobs`: `JobQueue::retry_now` also brings a `failed` job's next attempt forward
  (attempts and last error kept), besides re-running `dead` and `cancelled` jobs. (The crate is
  new in 0.10; noted for code written against pre-release builds.)
- `id_effect_test`: a container start that fails on a port-publish race is retried (up to 4
  times, with fresh host ports); each attempt is labelled `org.id_effect.test.attempt`.
- `id_effect_tool`: every generated project has a `.devcontainer/`; the service template's
  `--db pg --otel` variant exposes `Database::sql()` with a `db.query` span per statement.

## Checked against cargo semver-checks

`cargo semver-checks --workspace --baseline-version 0.9.0` (the six crates new in 0.10 excluded:
they have no 0.9.0 baseline) reports breaks in three crates, all covered above:

| Crate | Reported break | Section |
|---|---|---|
| `id_effect_logger` | `LogRecord` marked `#[non_exhaustive]` | Observability: `LogRecord` |
| `id_effect_opentelemetry` | `OtelConfig` adds field `sampler` | Observability: `OtelConfig` |
| `id_effect_tool` | `NewArgs` and `render::Options` add `jobs`, `console`; `template::Condition` adds `Jobs`, `Console` | Tooling |

Every other published crate reports no semver update required. The remaining sections are
behaviour changes the tool cannot see (process-wide logger state, HTTP defaults, SQL error
mapping, sqlx `time`, derived-schema errors), API changes in `#[non_exhaustive]` types
(`SqlError`), additions that can conflict with user impls (`SqlClient` for `Box` / `Arc`), and the
labs-to-published moves of `id_effect_events` and `id_effect_jobs`.

## 0.10.1: stricter security defaults

0.10.1 is API-compatible with 0.10.0, but several defaults are stricter. Code that compiled keeps
compiling; these are the runtime differences to check. Every opt-out below re-opens a finding of
the 2026-10-18 security review, so use it only where you accept that (tests, throwaway clusters).

| Area | What changes at runtime | What to do | Opt-out |
|---|---|---|---|
| Mesh (`id_effect_node`) | The handshake protocol is version 2: 0.10.0 and 0.10.1 nodes refuse each other with `VersionMismatch`. | Upgrade every node of a cluster together. | none |
| Mesh, QUIC | Dialing without trusted roots or a pinned certificate fails (`QuicTransport::try_new` returns `QuicConfigError::NoServerTrust`; `QuicTransport::new` fails at dial). | Configure roots, or pin the peer's leaf certificate with `QuicConfig::with_pinned_server_cert_sha256`. | `QuicConfig::dangerous_accept_any_server_cert()` |
| Mesh, cookie | A cookie shorter than 32 bytes is refused before the handshake starts. | Use `Cookie::try_new` with 32+ random bytes. | `Cookie::dangerous_allow_short()` |
| Mesh, handshake | A handshake that has not finished in 5 s is dropped; frames are capped at 4 KiB until authentication. | Raise the timeout with `Cookie::with_handshake_timeout` on slow links. | none for the frame cap |
| JWT (`id_effect_auth`) | A `JwtVerifier` with neither `.issuer(..)` nor `.audience(..)` fails every token with `AuthError::Config`. | Set the issuer and the audience your tokens carry. | `.any_issuer_dangerous()` plus `.any_audience_dangerous()` |
| JWT, HMAC | HMAC secrets shorter than 32 bytes fail with `AuthError::Key` when verifying and issuing. | Use a secret of 32+ random bytes. | none |
| JWKS | Only https URLs (plain http only for loopback hosts), no redirects, documents up to 1 MiB. | Point the JWKS URL at the final https location. | `allow_http_dangerous()` |
| Basic auth | Failed logins are throttled per account and per IP (5 free, then a doubling delay up to 15 min; 429 with `Retry-After`); passwords over 256 bytes are rejected. | Nothing for most apps. Behind a proxy, see the client IP row. | `BasicAuth::without_throttle_dangerous()` |
| TOTP | After 5 wrong codes, verification fails with `TotpError::Locked` until the lockout ends. | Show `retry_after` to the user. | `without_lockout_dangerous()` |
| Password hashing | At most 2 × available parallelism hashes run at once; a call that waits over 5 s fails with `PasswordError::Busy` (503 in the account flows). | Tune with `PasswordHasher::with_max_concurrent` / `with_busy_timeout`. | none |
| HTTP server (`id_effect_axum`) | Connections that send no complete headers within 10 s are closed; at most 10,000 connections; keep-alive ends after 1,000 requests per connection. | Set other values with `ConnectionLimits` and the `*_with_limits` serve functions. | `dangerous_no_header_read_timeout`, `dangerous_unlimited_connections`, `dangerous_unlimited_requests_per_connection` |
| Client IP | Rate limits now see the socket peer. `X-Forwarded-For` counts only from a trusted proxy. | Behind a load balancer, list its addresses with `TrustedProxies` and `RateLimitKey::client_ip`, or every client shares the proxy's bucket. | none |
| HTTP client (`id_effect_platform`) | A response with a `Content-Length` over `max_body_bytes` fails before its body is read. | Raise `max_body_bytes` for large downloads. | none |
