# Ledger — the id_effect reference app

A multi-tenant **double-entry ledger** service, built only through
[`id_effect_kit`](../../../crates/id_effect_kit). Organisations keep a chart of accounts and post
balanced journal entries; owners, accountants and viewers see and do what their role allows.

It is the application the 0.9 platform crates are tested against: CI's `reference-ledger` job
applies its migrations with `id-effect migrate run` / `check` and runs its integration tests on
real PostgreSQL, Redis, Vault and Keycloak containers. Copy from it.

```text
cargo nextest run -p ledger        # Docker needed; without it the container tests print SKIP
```

## Architecture

```text
            ┌──────────────────────── axum::Router (app::router) ─────────────────────────┐
request ──► │ RequestId → Compression → Timeout → BodyLimit → Authenticate → revoked tokens │
            │   └─ typed routes (id_effect_kit::axum::route) + one Guard per route         │
            │        └─ handler: Caller (principal + organisation) → Effect<_, ApiError>   │
            └──────────────┬───────────────────────────────┬───────────────────────────────┘
                           │ writes, transactions          │ reports, balances, CSV
                     primary (PostgreSQL) ◄──────── RoutingSqlClient ────────► replicas
                           │                                   │
             sessions, audit chain, users             balance cache (Redis or in process)
```

| Module | What it shows |
|---|---|
| `src/config.rs` | profiles `dev` / `test` / `prod` (`config/*.toml`), `LEDGER_*` env overrides, `prod` secrets from Vault |
| `src/db.rs` | the primary and its read replicas behind a `RoutingSqlClient`; `migrations/` applied at start through the `migrate` layer |
| `src/app.rs` | the shared `App`, the request's `Caller`, the router, the middleware stack, `/openapi.json`, `/docs`, `/_routes`, `/health` |
| `src/policy.rs` | roles → permissions (`Rbac`), the named policies, `can_post(account)` |
| `src/auth.rs` | organisations, password + TOTP login into a PostgreSQL session, API tokens (JWT bearer) and their revocation |
| `src/accounts.rs` | accounts: a tenant-scoped, soft-deleted, optimistically locked entity over `Repository` |
| `src/entries.rs` | journal entries posted in `SERIALIZABLE` transactions (retried on conflicts), batches with a savepoint per entry, the hash-chained audit record |
| `src/reports.rs` | balances (cached in Redis, `ETag`ged), the trial balance, the CSV export streamed from `query_stream`, `GET /audit/verify` |
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

`replica::reports_read_from_the_replica` runs the "replica" as a second, independent PostgreSQL
container with the same schema and seed rows — not streaming replication. The test copies rows
into it where replication would, which makes the routing observable: a posting the primary has
and the replica does not yet have is invisible to the reports.

## Run it

```text
docker compose up -d                       # PostgreSQL, Redis, Keycloak, Vault
cargo run -p ledger                        # profile dev, http://127.0.0.1:8080
```

```text
# an organisation and its owner (signed in: keep the cookie)
curl -c jar -H 'content-type: application/json' localhost:8080/orgs \
  -d '{"org": "acme", "username": "ada", "password": "correct horse battery"}'
curl -b jar -H 'content-type: application/json' localhost:8080/accounts \
  -d '{"code": "1000", "name": "Cash", "kind": "asset"}'
curl -b jar -H 'content-type: application/json' localhost:8080/accounts \
  -d '{"code": "4000", "name": "Sales", "kind": "income"}'
curl -b jar -H 'content-type: application/json' localhost:8080/entries \
  -d '{"memo": "first sale", "lines": [{"account_id": 1, "amount": 2500}, {"account_id": 2, "amount": -2500}]}'
curl -b jar localhost:8080/reports/trial-balance
```

- `http://localhost:8080/docs` — the API reference (`/openapi.json` behind it).
- `http://localhost:8080/auth/oidc/login` — sign in through Keycloak as `alice` /
  `alice-password` (an accountant of `acme`; create `acme` first, as above).
- `POST /auth/totp/enrol`, then `POST /auth/totp/confirm {"code": …}`: owners need a TOTP login
  to manage users (`POST /users`).
- `POST /auth/tokens {"name": "ci"}` returns a bearer token for scripts; `DELETE
  /auth/tokens/{id}` revokes it.

### The prod profile

`config/prod.toml` holds no secret: the database password and the token signing secret are read
from Vault (KV v2, `secret/ledger/db` and `secret/ledger/auth`) when the service starts, with the
token in `VAULT_TOKEN`. Against the compose Vault:

```text
docker compose exec -e VAULT_TOKEN=dev-root vault vault kv put secret/ledger/db password=ledger-dev host=localhost
docker compose exec -e VAULT_TOKEN=dev-root vault vault kv put secret/ledger/auth jwt_secret="$(openssl rand -hex 32)"
LEDGER_VAULT__ADDR=http://localhost:8200 LEDGER_REDIS__URL=redis://localhost:6379/0 \
  VAULT_TOKEN=dev-root cargo run -p ledger -- --profile prod
```

(Prod cookies are `Secure` and `__Host-` prefixed: put the service behind TLS.)

## Operating notes

- **Migrations.** The service applies `migrations/` when it starts (each migration in its own
  transaction under an advisory lock, so instances starting together apply it once); `id-effect
  migrate run` from this directory does the same as a deploy step, and `id-effect migrate check`
  fails on pending, changed or missing migrations.
- **Login rate limit.** Attempts count per client address as the reverse proxy reports it
  (`X-Forwarded-For`, first hop — the proxy must overwrite a header the client sent); requests
  the service cannot attribute share one bucket, so the limit always holds. With Redis the
  buckets are shared by every instance.
- **Conflicts.** Postings run `SERIALIZABLE` and are rerun (up to 10 times, jittered backoff)
  when they lose a serialization conflict; an account update carries the `version` it read and
  answers `409` when another write got there first.
- **Double entry, twice.** The schema refuses an unbalanced entry (`422`) and a deferred
  constraint trigger refuses one written behind the application's back.
- **Audit.** Every posting appends a hash-chained record in the posting's transaction;
  `GET /audit/verify` (owners) reports the first edited, removed or relinked record. Anchor the
  reported head outside the database to detect a complete rewrite.
