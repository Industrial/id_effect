# Ledger — the id_effect reference app

A multi-tenant **double-entry ledger** service, built only through
[`id_effect_kit`](../../../crates/id_effect_kit). Organisations keep a chart of accounts and post
balanced journal entries; owners, accountants and viewers see and do what their role allows.

It is the application the 0.9 and 0.10 platform crates are tested against: CI's
`reference-ledger` job applies its migrations with `id-effect migrate run` / `check` and runs
its integration tests on real PostgreSQL, Redis, Vault, Keycloak, Mailpit and S3 (SeaweedFS)
containers. Copy from it.

```text
cargo nextest run -p ledger        # Docker needed; without it the container tests print SKIP
```

Since 0.10 it also runs background work: a monthly statement cron (one instance at a time) that
fans out to a job per account, stores each statement CSV in S3 and mails it; every posting
announced through a transactional outbox and Redis Streams to a balance-notification consumer;
self-service registration, email verification and password reset; and an operator console.

## Architecture

```text
            ┌──────────────────────────── axum::Router (app::router) ─────────────────────────────┐
request ──► │ RequestId → Compression → Timeout → BodyLimit → X-Ledger-Org → Authenticate →       │
            │ revoked tokens → SecurityLayer (CSRF, CSP, headers; bearer-only requests skip CSRF)  │
            │   └─ typed routes (id_effect_kit::axum::route) + one Guard per route                 │
            │        └─ handler: Caller (principal + organisation) → Effect<_, ApiError>           │
            │   └─ /auth/register … (AccountRoutes), /_console (console.admin), /_mail/previews    │
            └──────────────┬───────────────────────────────────┬───────────────────────────────────┘
                           │ writes, transactions, outbox       │ reports, balances, CSV
                     primary (PostgreSQL) ◄──────────── RoutingSqlClient ────────────► replicas
                           │                                    │
      sessions, audit chain, users, id_effect_jobs,     balance cache (Redis or in process)
      id_effect_outbox / id_effect_inbox
                           │
   background (jobs.run): worker (statements, mail) · statement cron (advisory-lock leader)
                          relay outbox → Redis Streams → balance-notification consumer
                          statements → S3 · mail → SMTP (dev) / SendGrid over traced HTTP (prod)
```

| Module | What it shows |
|---|---|
| `src/config.rs` | profiles `dev` / `test` / `prod` (`config/*.toml`), `LEDGER_*` env overrides, `prod` secrets from Vault |
| `src/db.rs` | the primary and its read replicas behind a `RoutingSqlClient`; `migrations/` applied at start through the `migrate` layer |
| `src/app.rs` | the shared `App`, the request's `Caller`, the router, the middleware stack, `/openapi.json`, `/docs`, `/_routes`, `/health` |
| `src/policy.rs` | roles → permissions (`Rbac`), the named policies, `can_post(account)` |
| `src/auth.rs` | organisations, password + TOTP login into a PostgreSQL session, API tokens (JWT bearer) and their revocation |
| `src/accounts.rs` | accounts: a tenant-scoped, soft-deleted, optimistically locked entity over `Repository` |
| `src/closure.rs` | account closure as a durable workflow (id_effect_workflow): freeze, an owner's approval signal, a 30-day grace timer, the final statement exported by an activity, close; a saga thaws the account on a rejection |
| `src/entries.rs` | journal entries posted in `SERIALIZABLE` transactions (retried on conflicts), batches with a savepoint per entry, the hash-chained audit record |
| `src/reports.rs` | balances (cached in Redis, `ETag`ged), the trial balance, the CSV export streamed from `query_stream`, `GET /audit/verify` |
| `src/signup.rs` | registration, email verification and password reset: id_effect_auth `Accounts` / `AccountRoutes` over the `users` table |
| `src/limits.rs` | the login limits per client and per organisation (`RateLimitKey::tenant`), reloaded from the profile file (`ProfileWatch`) |
| `src/notifications.rs` | the posting's outbox row, the relay to Redis Streams, the balance-notification consumer with inbox dedup |
| `src/statements.rs` | the statement CSV, its S3 key, attached or linked |
| `src/jobs.rs` | the statement cron (`CronLeader` over `PgAdvisoryLease`), `StatementsDue` → `MonthlyStatement` (unique `account@month`), the mail queue |
| `src/mail.rs` | the transport per profile (memory, SMTP, SendGrid over `TracedHttpClient`), minijinja templates in `templates/mail/`, `/_mail/previews` |
| `src/console.rs` | `/_console` (id_effect_console) behind `Guard::named(&policies, "console.admin")` |
| `src/observability.rs` | OpenTelemetry from the environment (sampler included), the `RedactionPolicy` |
| `src/background.rs` | the worker, the cron, the relay, the consumer and the limits watch of one instance |
| `src/error.rs` | every failure as RFC 9457 Problem+JSON |

Handlers are effects. A typed route declares its path, query, headers and body as schemas
(invalid input never reaches the handler: it gets a `422` listing every issue), and its handler
receives the request capabilities the authentication layer recorded
(`Route::handler_with_request_caps`), so it knows the caller:

```rust,ignore
fn show(env: &mut AppEnv, caps: RequestCaps, input: Input<i64>) -> Effect<Json<Account>, ApiError, AppEnv> {
  let app = Arc::clone(&env.0);
  as_caller(&caps, move |caller| find_account(&app, &caller, input.path).map(Json))
}

Route::get("/accounts/{id}").path::<AccountId>().response::<Account>(StatusCode::OK)
  .handler_with_request_caps(show)
  .authorize(Guard::named(&policies, "ledger.read"))
```

## Features, tests and the scorecard checks they exercise

| Feature | Where | Tests | Checks |
|---|---|---|---|
| `migrations/` applied at start (`migrate` layer), `id-effect migrate run` / `check` in CI | `db.rs`, CI job | `migrations::*` | data.migrations #1–#6, #8 |
| Accounts, journal entries, postings over typed routes; Problem+JSON; `/openapi.json`, `/docs`, `/_routes` | `accounts.rs`, `entries.rs`, `app.rs` | `accounts::*`, `http::openapi_documents_every_typed_route`, `authz::every_route_names_its_policy` | web.validation, data.jsonschema #2–#4 #7, web.server #8 |
| Posting in a `SERIALIZABLE` transaction with `retry_serialization_failures`, a savepoint per batch entry, `Repository` with a version column, CSV from `query_stream` | `entries.rs`, `accounts.rs`, `reports.rs` | `entries::*`, `accounts::accounts_crud_over_typed_routes` | data.sql #2 #3 #5 #6 #7, data.orm #1 #2 #5 #6 |
| Organisation-scoped repositories (tenant filter) and soft delete; hash-chained audit of postings, `GET /audit/verify` | `accounts.rs`, `entries.rs`, `reports.rs` | `accounts::organisations_never_see_each_other`, `entries::an_edited_audit_record_is_detected` | data.orm #8, sec.authz #5, eco.tenancy #1 #5 #6 |
| Password login → PostgreSQL session; API tokens (JWT bearer); OIDC + PKCE against Keycloak; TOTP for owners | `auth.rs`, `app.rs` | `authn::*`, `oidc::*` | sec.authn #1–#6 |
| Roles owner / accountant / viewer, `can_post(account)`, ownership, a guard on every route, recorded decisions, deny tests | `policy.rs` | `authz::*`, `authn::api_tokens_are_bearer_jwts_that_can_be_revoked` | sec.authz #1–#4 #6 #8 |
| Request id, compression, timeout, body limit, login rate limit, `ETag` on balances | `app.rs`, `reports.rs` | `http::*`, `authn::logins_are_rate_limited` | web.middleware, data.cache #7 |
| Profiles `dev` / `test` / `prod`; `prod` reads the database password from Vault | `config.rs`, `config/` | `config::*` | sec.config #6 #7 |
| Tests on `id_effect_test` containers, a database per test, fixtures, the in-process client | `tests/common/mod.rs` | every integration test | test.integration #2–#4 |
| Balance cache in Redis (`RedisCacheBackend`), a read-replica route (`RoutingSqlClient`) | `reports.rs`, `db.rs` | `cache::*`, `replica::*` | data.cache #2, data.backends #7 |
| Monthly statement cron (one leader) → a job per account (unique `account@month`, queue `statements`, priority, retries) → CSV in S3 → `deliver_later` mail with the CSV attached | `jobs.rs`, `statements.rs`, `mail.rs` | `statements::*` | msg.jobs #2–#6 #8, msg.email #1 #2 #5 #8, web.storage #3 #5 #7 |
| Posting → outbox row in the `SERIALIZABLE` transaction → relay → Redis Streams → balance notifications with inbox dedup; dead-letter stream | `notifications.rs`, `entries.rs` | `notifications::*` | msg.brokers #3 #5 #6 #7 #8 |
| Registration, email verification, password reset; mail to Mailpit; `/_mail/previews` in dev | `signup.rs`, `mail.rs` | `signup::*` | sec.authn #8, msg.email #3 #4 |
| `/_console`: fibers, supervisors, metrics, health, log level, jobs, behind `console.admin`; off in prod | `console.rs`, `policy.rs` | `console::*`, `posture::m6_ledger_console_disabled_in_prod_profile` | obs.console #1 #2 #3 #6 |
| The prod posture check at startup (ADR 0013 R10), passed with no accepted risk; every route, typed or plain, declared public or guarded, any other one `404` | `app.rs`, `main.rs`, `config/prod.toml` | `posture::ledger_prod_posture_is_clean`, `routes::*` | sec.web, sec.authz |
| Account closure as a durable workflow: signal, durable timer, activities on the job queue, saga compensation, across a restart | `closure.rs`, `background.rs`, `migrations/0008_workflows.up.sql` | `closure::*` | fault.workflows #2 #3 #4 #5 #7 |
| `SecurityLayer::recommended` (CSRF, CSP, headers), per-organisation login limit (`RateLimitKey::tenant`) | `app.rs`, `limits.rs` | `security::*`, `limits::an_organisation_shares_one_login_budget_across_clients` | sec.web, eco.tenancy #7 |
| Traced outbound HTTP (SendGrid), `db.query` spans, sampler from the environment, redacted `password` / `token` fields | `mail.rs`, `db.rs`, `observability.rs` | `observability::*` | obs.tracing #6 #7, web.client #4 #6, obs.logging #7 |
| Login limits hot-reloaded from the profile file | `limits.rs` | `limits::login_limits_follow_the_profile_file` | fault.reload #5 |
| A seeded fault plan (SQL latency, lost conflicts) on the posting path; the retry policy posts every entry once | `entries.rs` | `faults::*` | test.chaos #2 #7 |
| `id-effect dev`: rebuild and restart on change, `compose.yaml` services | this README | — | dx.repl #2 #3 |
| The accounts and journal over RPC under `/rpc` beside REST (ADR 0014): a `#[rpc::service]` calling the REST routes' functions behind the same policies, mutation `Idempotency-Key`s in PostgreSQL, a statement server stream; `rpc.contract.json` and the TypeScript client `web/rpc` checked in, `id-effect rpc check` in CI | `rpc.rs`, `idempotency.rs`, `web/rpc/` | `rpc::*` (Rust client; TypeScript client under Deno, `tests/ts/`) | web.rpc |

Regenerate the TypeScript client after a contract change (`UPDATE_GOLDEN=1 cargo nextest run -p
ledger --test rpc` rewrites `rpc.contract.json`):

```bash
id-effect rpc gen ts --contract rpc.contract.json --out web/rpc
```

`replica::reports_read_from_the_replica` runs the "replica" as a second, independent PostgreSQL
container with the same schema and seed rows — not streaming replication. The test copies rows
into it where replication would, which makes the routing observable: a posting the primary has
and the replica does not yet have is invisible to the reports.

## Run it

```text
docker compose up -d                       # PostgreSQL, Redis, Keycloak, Vault, Mailpit, S3
cargo run -p ledger                        # profile dev, http://127.0.0.1:8080
```

### While you work: `id-effect dev`

`id-effect dev` (from `crates/id_effect_tool`, `cargo install --path crates/id_effect_tool`)
builds the Ledger, runs it with `ID_EFFECT_PROFILE=dev`, and rebuilds and restarts it whenever
`src/`, `Cargo.toml`, `config/` or a `--watch` path changes. Only a successful build replaces
the running app (SIGTERM, then the drain: the worker finishes its jobs, the cron leader releases
its lock); a failed one keeps the old one running and waits for the next edit.

```text
cd examples/reference/ledger
id-effect dev --package ledger --services --watch migrations --watch templates
```

- `--services` runs `docker compose up -d` first (the services of `compose.yaml`).
- `--watch migrations --watch templates`: a new migration is applied by the restarted app; the
  mail templates are also re-read on every render in a debug build, so
  `http://localhost:8080/_mail/previews` shows an edited template on reload, without a restart.
- `--poll-ms`, `--debounce-ms` and `--grace-secs` tune the watcher (the files are polled, there
  is no `notify` dependency); `-- --profile …` passes arguments to the app.

The dev services: Mailpit's inbox at `http://localhost:8025` (every statement and account
mail of the `dev` profile ends there), the S3 API (SeaweedFS) at `http://localhost:8333` with
the `ledger` / `ledger-dev-secret` identity of `s3.json` (the bucket `ledger-statements` is
created on the first upload), Keycloak at `:8180`, Vault at `:8200`.

### Try it

Every unsafe request of a browser (or of curl with a cookie jar) echoes the CSRF token
`GET /auth/csrf` issues; a script with a bearer token and no cookie does not need it.

```text
# the CSRF cookie and token, then an organisation and its owner (signed in: keep the cookie)
CSRF=$(curl -s -c jar localhost:8080/auth/csrf | sed 's/.*"token":"\([^"]*\)".*/\1/')
post() { curl -s -b jar -c jar -H 'content-type: application/json' -H "x-csrf-token: $CSRF" "localhost:8080$1" -d "$2"; }
post /orgs '{"org": "acme", "username": "ada", "password": "correct horse battery"}'
post /accounts '{"code": "1000", "name": "Cash", "kind": "asset"}'
post /accounts '{"code": "4000", "name": "Sales", "kind": "income"}'
post /entries '{"memo": "first sale", "lines": [{"account_id": 1, "amount": 2500}, {"account_id": 2, "amount": -2500}]}'
curl -b jar localhost:8080/reports/trial-balance
# self-service: register, then open the link of the mail in Mailpit (http://localhost:8025)
post /auth/register '{"email": "carol@ledger.example", "password": "correct horse battery"}'
```

- `http://localhost:8080/docs` — the API reference (`/openapi.json` behind it).
- `http://localhost:8080/auth/oidc/login` — sign in through Keycloak as `alice` /
  `alice-password` (an accountant of `acme`; create `acme` first, as above).
- `POST /auth/totp/enrol`, then `POST /auth/totp/confirm {"code": …}`: owners need a TOTP login
  to manage users (`POST /users`).
- `POST /auth/tokens {"name": "ci"}` returns a bearer token for scripts; `DELETE
  /auth/tokens/{id}` revokes it.
- `http://localhost:8080/_console` — the operator console: sign in as a user with the
  `operator` role (insert one into `users`; no owner can give the role) who enrolled TOTP.
- The statement cron fires at 06:00 UTC on the first of every month (`statements.cron`); set
  `LEDGER_STATEMENTS__CRON='0 * * * * *'` to get last month's statements every minute.

### The prod profile

`config/prod.toml` holds no secret: the database password, the token signing secret, the
cookie key and the SendGrid API key are read from Vault (KV v2, `secret/ledger/db`,
`secret/ledger/auth` and `secret/ledger/mail`) when the service starts, with the token in
`VAULT_TOKEN`. Against the compose Vault:

```text
docker compose exec -e VAULT_TOKEN=dev-root vault vault kv put secret/ledger/db password=ledger-dev host=localhost
docker compose exec -e VAULT_TOKEN=dev-root vault vault kv put secret/ledger/auth jwt_secret="$(openssl rand -hex 32)" \
  cookie_key="$(openssl rand -base64 64 | tr -d '\n')"
docker compose exec -e VAULT_TOKEN=dev-root vault vault kv put secret/ledger/mail sendgrid_api_key=SG.your-key
LEDGER_VAULT__ADDR=http://localhost:8200 LEDGER_REDIS__URL=redis://localhost:6379/0 \
  VAULT_TOKEN=dev-root cargo run -p ledger -- --profile prod
```

(Prod cookies are `Secure` and `__Host-` prefixed: put the service behind TLS.)

Before serving, the service prints who may call each route and the security posture report;
under `prod` a failed probe stops it. The prod profile passes with no accepted risk: no console,
no `/docs`, CSRF on, the redaction policy on, load shedding on, outbound HTTP only to the SendGrid
API under the SSRF guard, and no opt-out recorded (there is no `[security.accepted_risks]`
table).

## Operating notes

- **Migrations.** The service applies `migrations/` when it starts (each migration in its own
  transaction under an advisory lock, so instances starting together apply it once); `id-effect
  migrate run` from this directory does the same as a deploy step, and `id-effect migrate check`
  fails on pending, changed or missing migrations.
- **Login rate limit.** Attempts count per client address as the reverse proxy reports it
  (`X-Forwarded-For`, first hop — the proxy must overwrite a header the client sent); requests
  the service cannot attribute share one bucket, so the limit always holds. A login that names
  its organisation (`X-Ledger-Org`) also draws from that organisation's bucket, and is refused
  when the user is not in it. With Redis the buckets are shared by every instance. The values
  are re-read from `config/` every `reload_limits_secs` (a file that does not parse keeps the
  old ones; the buckets restart full when the values change).
- **Background work.** Every instance with `jobs.run` runs a worker (`statements`, `mail`), a
  relay and the consumer; the statement cron runs on the instance holding the PostgreSQL
  advisory lock `ledger.cron` and moves to another within seconds when that session dies. Jobs
  and messages are delivered at least once, so every handler is idempotent: a statement rerun
  overwrites its S3 object and row, its mail job is unique per account and month, and the
  consumer's inbox row commits with the notifications.
- **Statements: attached or linked.** A statement CSV of at most `attach_max_bytes` (1 MiB) is
  attached: the mail job's payload, kept in `id_effect_jobs` until pruned, holds it, and
  id_effect_mail caps that at 2 MiB. A larger one is mailed as a presigned S3 link valid for
  `link_ttl_hours` (7 days). The account mails (verification, reset) never go through the job
  queue: their token would sit in its table.
- **CSRF.** Every unsafe request carrying a cookie, or none at all, must echo the
  `__Host-csrf` cookie's token in `X-CSRF-Token` (`GET /auth/csrf`); a bearer-only request (an
  `Authorization: Bearer` header and no cookie) is served without the check. The cookie key
  (`auth.cookie_key`, from Vault in prod) must be the same on every instance.
- **Conflicts.** Postings run `SERIALIZABLE` and are rerun (up to 10 times, jittered backoff)
  when they lose a serialization conflict; an account update carries the `version` it read and
  answers `409` when another write got there first.
- **Double entry, twice.** The schema refuses an unbalanced entry (`422`) and a deferred
  constraint trigger refuses one written behind the application's back.
- **Audit.** Every posting appends a hash-chained record in the posting's transaction;
  `GET /audit/verify` (owners) reports the first edited, removed or relinked record. Anchor the
  reported head outside the database to detect a complete rewrite.
