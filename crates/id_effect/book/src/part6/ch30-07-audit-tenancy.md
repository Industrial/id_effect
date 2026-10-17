# Tenancy and the audit log

A multi-tenant service keeps every tenant's rows in shared tables and must never let one tenant read or change another's. Many such services must also be able to show who changed what, and prove the record was not edited afterwards. This chapter puts together pieces from earlier chapters into one pattern, **row-level tenancy with a tamper-evident audit trail**:

1. **Authentication** turns the request into a principal that carries its tenant: `id_effect_auth`'s `Authenticate` layer ([Credentials and JWT](ch30-04-credentials-jwt.md)), `Principal::tenant()` from `id_effect_authz` ([Authorization](ch30-02-authorization.md)).
2. **Per-request capabilities**: two `CapOverrideLayer`s turn the principal into this request's `CurrentTenant` (which tenant's rows) and `AuditContext` (actor and tenant of audit records).
3. **The repository filter**: a `Repository` of a `#[entity(tenant = "org_id")]` entity built with `Repository::for_current_tenant` adds `org_id = <tenant>` to every statement and writes it on insert ([Query builder, entities and repositories](ch28-03-repositories.md)).
4. **The audit record** is appended with `audit::record` in the request's transaction, so it commits or rolls back with the change.
5. **Verification**: `audit::verify_chain` checks a tenant's hash chain and reports the first edited, removed or relinked record.

## The data side

Mark the tenant column on the entity. It need not be a field: the repository writes it on insert (whatever the struct says), filters every `SELECT`, `UPDATE` and `DELETE` by it, and never updates it.

```rust
use id_effect_sql::{Entity, FromRow};

#[derive(Clone, Debug, PartialEq, FromRow, Entity)]
#[entity(table = "invoices", tenant = "org_id")]
struct Invoice {
    #[entity(id)]
    id: i64,
    name: String,
}

assert_eq!(Invoice::TENANT, Some("org_id"));
assert_eq!(Invoice::COLUMNS, ["id", "name"]); // org_id is not a field
```

```sql
CREATE TABLE invoices (
  id      BIGINT PRIMARY KEY,
  org_id  TEXT   NOT NULL,
  name    TEXT   NOT NULL
);
CREATE INDEX invoices_org_id ON invoices (org_id);
```

Index the tenant column (every query filters on it), and include it in unique constraints that are per tenant (`UNIQUE (org_id, number)`).

The audit log needs the `audit` feature and its schema, applied once: with the `migrate` feature, `audit::migrator()` runs the bundled migration with its own tracking table; without it, copy `audit::MIGRATIONS` into your migrations. It creates `audit_log` and `audit_log_head`. Provide `AuditLog::new()` at startup (`AuditLog::new().layer()`, or as a value in your state).

## The request path

The router below authenticates API keys, derives both capabilities from the principal, and renames an invoice in one transaction together with its audit record. The axum handler builds the request's environment from its `RequestCaps`, provides `transaction_layer` (commit on success, roll back on failure) and runs the effect. The handler effect never sees a tenant argument: the filter comes from the environment.

```rust
use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::{Request, StatusCode, request::Parts};
use axum::response::{IntoResponse, Response};
use axum::routing::put;
use axum::Router;
use http_body_util::BodyExt;
use id_effect::{Effect, HCons, HNil, Layer, fail, hlist, need};
use id_effect_auth::{ApiKey, ApiKeys, AuthPrincipal, Authenticate, MemoryApiKeyStore};
use id_effect_authz::guard::principal_of;
use id_effect_axum::{CapOverrideLayer, RequestCaps, exit_response, run_with_env};
use id_effect_sql::audit::{self, AuditContext, AuditEntry, AuditLog};
use id_effect_sql::{
    CurrentTenant, Dialect, Entity, FromRow, Repository, SqlClient, SqlClientService, SqlError, SqlRow,
    SqlValue, SqlTransactionService, TestSqlClient, transaction_layer,
};
use serde_json::json;
use tower::ServiceExt;

#[derive(Clone, Debug, PartialEq, FromRow, Entity)]
#[entity(table = "invoices", tenant = "org_id")]
struct Invoice {
    #[entity(id)]
    id: i64,
    name: String,
}

/// One request's environment: its capabilities and its transaction.
type RequestEnv = HCons<SqlClientService, HCons<CurrentTenant, HCons<AuditContext,
    HCons<AuditLog, HCons<SqlTransactionService, HNil>>>>>;

#[derive(Debug)]
enum AppError {
    Sql(SqlError),
    NoTenant,
}
impl From<SqlError> for AppError {
    fn from(e: SqlError) -> Self { AppError::Sql(e) }
}
impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let status = match self {
            AppError::NoTenant => StatusCode::FORBIDDEN,
            AppError::Sql(SqlError::RowNotFound) => StatusCode::NOT_FOUND,
            AppError::Sql(SqlError::StaleVersion { .. }) => StatusCode::CONFLICT,
            AppError::Sql(_) => StatusCode::INTERNAL_SERVER_ERROR,
        };
        status.into_response()
    }
}

/// PUT /invoices/{id}: rename an invoice of the caller's tenant and audit it.
fn rename_invoice(id: i64, name: String) -> Effect<String, AppError, RequestEnv> {
    Repository::<Invoice>::for_current_tenant(Dialect::Postgres)
        .flat_map(move |repo| {
            need::<SqlTransactionService>().flat_map(move |tx| {
                let invoices = repo.on(tx.0.clone());
                let update = invoices.clone();
                invoices
                    .find(id) // WHERE org_id = <tenant> AND id = <id>
                    .flat_map(move |found| match found {
                        None => fail(SqlError::RowNotFound), // another tenant's invoice, too
                        Some(before) => update
                            .update(Invoice { name, ..before.clone() })
                            .map(move |after| (before, after)),
                    })
                    .narrow()
            })
        })
        .flat_map(move |(before, after)| {
            let entry = AuditEntry::new("invoice.rename", format!("invoice:{id}"))
                .with_before(json!({ "name": before.name }))
                .with_after(json!({ "name": after.name }));
            audit::record(entry).map(move |_record| after.name) // same transaction
        })
        .map_err(AppError::from)
}

#[derive(Clone)]
struct App {
    db: SqlClientService,
    audit: AuditLog,
}

async fn put_invoice(
    State(app): State<App>,
    caps: RequestCaps,
    Path(id): Path<i64>,
    name: String,
) -> Response {
    // Fail closed: a principal without a tenant has no CurrentTenant, so nothing runs.
    let (Some(tenant), Some(actor)) = (caps.get::<CurrentTenant>(), caps.get::<AuditContext>()) else {
        return AppError::NoTenant.into_response();
    };
    let request = Layer::succeed(hlist![
        app.db.clone(),
        CurrentTenant::clone(&tenant),
        AuditContext::clone(&actor),
        app.audit.clone(),
    ]);
    let handler = rename_invoice(id, name)
        .provide_layer(transaction_layer::<AppError>().provide_merge(request));
    exit_response(run_with_env(HNil, move |_| handler).await)
}

fn router(db: SqlClientService, keys: ApiKeys) -> Router {
    Router::new()
        .route("/invoices/{id}", put(put_invoice))
        // 2. The principal's tenant and identity become this request's capabilities.
        .layer(CapOverrideLayer::new(|parts: &Parts| {
            let principal = principal_of(parts);
            Some(CurrentTenant::new(principal.tenant()?.to_owned()))
        }))
        .layer(CapOverrideLayer::new(|parts: &Parts| {
            let principal = principal_of(parts);
            Some(AuditContext::for_tenant(principal.id(), principal.tenant()?))
        }))
        // 1. Authentication is the outermost layer, so it runs first.
        .layer(Authenticate::api_key(keys))
        .with_state(App { db, audit: AuditLog::new() })
}

// Two API keys: ada acts for tenant `acme`; a service account has no tenant.
let store = MemoryApiKeyStore::new();
let (ada_key, ada) = ApiKey::generate("ledger", AuthPrincipal::new("ada").with_tenant("acme")).unwrap();
let (svc_key, svc) = ApiKey::generate("ledger", AuthPrincipal::new("svc-reports")).unwrap();
store.insert(ada);
store.insert(svc);

let test = TestSqlClient::new();
# test.script_query(
#     r#"SELECT "id", "name" FROM "invoices" WHERE "org_id" = $1 AND "id" = $2 LIMIT $3"#,
#     vec![SqlRow::from_pairs([("id", SqlValue::I64(1)), ("name", "draft".into())])],
# );
# test.script_execute(r#"UPDATE "invoices" SET "name" = $1 WHERE "org_id" = $2 AND "id" = $3"#, 1);
# let at = id_effect_sql::jiff::Timestamp::from_second(1_800_000_000).unwrap();
# test.script_query(
#     "INSERT INTO audit_log_head AS h (chain, seq, hash) VALUES ($1, 0, $2) ON CONFLICT (chain) DO UPDATE SET seq = h.seq RETURNING h.seq, h.hash, now() AS at",
#     vec![SqlRow::from_pairs([("seq", SqlValue::I64(0)), ("hash", SqlValue::Bytes(vec![0; 32])), ("at", SqlValue::Timestamp(at))])],
# );
# test.script_query(
#     "INSERT INTO audit_log (chain, seq, actor, tenant, action, entity, before, after, at, prev_hash, hash) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11) RETURNING id",
#     vec![SqlRow::from_pairs([("id", SqlValue::I64(1))])],
# );
let _ = test.connect().block_on();
let app = router(SqlClientService::new(test.clone()), ApiKeys::new(store, "ledger"));
let rename_request = |key: &str| {
    Request::put("/invoices/1").header("x-api-key", key).body(Body::from("final")).unwrap()
};

let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
rt.block_on(async {
    let res = app.clone().oneshot(rename_request(ada_key.expose())).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body = res.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(&body[..], b"final");

    // A principal without a tenant is refused before any statement runs.
    let res = app.clone().oneshot(rename_request(svc_key.expose())).await.unwrap();
    assert_eq!(res.status(), StatusCode::FORBIDDEN);
});

// One transaction, committed: the tenant filter is in both statements...
assert_eq!(test.commits(), 1);
let queries = test.recorded_queries();
assert_eq!(queries[0].1, [SqlValue::Text("acme".into()), SqlValue::I64(1), SqlValue::I64(1)]);
let (update, params) = &test.recorded_executes()[0];
assert_eq!(update, r#"UPDATE "invoices" SET "name" = $1 WHERE "org_id" = $2 AND "id" = $3"#);
assert_eq!(params[1], SqlValue::Text("acme".into()));
// ... and the audit record names the actor and the tenant.
let (_, record) = queries.iter().find(|(sql, _)| sql.starts_with("INSERT INTO audit_log ")).unwrap();
assert_eq!(record[2..4], [SqlValue::Text("ada".into()), SqlValue::Text("acme".into())]);
```

Notes on the wiring:

- `principal_of(parts)` reads the principal the authentication layer installed (the guard reads it the same way). The CapOverrideLayers sit **inside** `Authenticate`: axum runs the layer added last first.
- `CurrentTenant` holds any `ToSqlValue` (`CurrentTenant::new(org_id)` for an integer or UUID column), and `AuditContext::for_tenant(actor, tenant)` takes strings; derive both from the same principal.
- Handlers on a `DynEnv` router (`execute_with_request_caps`) see both capabilities in their environment as well.
- The example puts the whole request in one transaction (`transaction_layer`). `audit::record` needs that `SqlTransactionService` in the environment; with an explicit transaction, use `AuditLog::append(&tx, &ctx, entry)` or `audit::append`.

## Fail closed

Every layer refuses rather than guessing:

- A principal without a tenant gets no `CurrentTenant`; the handler answers 403 before any statement runs.
- A tenant-scoped repository without a tenant fails every call with `SqlError::Unsupported` and sends nothing (`Repository::new(dialect).find_all()` on `Invoice` above).
- Another tenant's row is invisible: `find` returns `None`, `update` is `SqlError::RowNotFound` and `delete` returns `false`, so a guessed id answers 404, not 403, and does not reveal that the row exists.
- `unscoped()` lifts the filter for one repository value (support tools, data migrations). Search for it in review.

## The audit trail

A record is `{actor, tenant, action, entity, before, after, at}`; `at` is the database's `now()`. Each tenant has its own hash chain: record `n` stores `sha256(prev_hash ‖ record)`, and the chain's head row holds the last `(seq, hash)`. Concurrent writers in one tenant queue on the head row and never fork the chain, while different tenants don't wait on each other. Use the default `READ COMMITTED` isolation for audited writes, or wrap `REPEATABLE READ` / `SERIALIZABLE` work in `retry_serialization_failures`: under those levels a racing append fails with a serialization failure instead of waiting.

`audit::verify_chain(Some(tenant))` streams the chain in order and reports the first edited, removed or relinked record. Run it on a schedule or behind an admin route, and log the returned `ChainReport::head` somewhere outside the database, so that a complete rewrite of the chain can be detected too:

```rust
use std::sync::Arc;

use id_effect::{Effect, HCons, HNil, fail, succeed};
use id_effect_sql::audit::{self, AuditLog};
use id_effect_sql::migrate::MigrateError;
use id_effect_sql::{SqlClient, SqlClientService, SqlError};

/// At startup: the audit schema, tracked apart from the application's migrations.
fn audit_schema(client: Arc<dyn SqlClient>) -> Effect<(), MigrateError> {
    audit::migrator().run_on(client).map(|_applied| ())
}

/// Nightly: verify one tenant's chain and anchor its head outside the database.
fn verify_tenant(tenant: String) -> Effect<String, SqlError, HCons<AuditLog, HCons<SqlClientService, HNil>>> {
    audit::verify_chain(Some(tenant.as_str())).flat_map(move |report| match report.first_broken {
        Some(link) => fail(SqlError::Unsupported(format!("audit chain of {tenant} broken at {link}"))),
        None => succeed(format!("{tenant}: {} records, head {:?}", report.records, report.head)),
    })
}
# let _ = (audit_schema, verify_tenant);
```

What the chain detects: an edited record (its hash no longer matches), a removed or inserted record (the sequence or the next `prev_hash` no longer follows), and records removed from the end (the chain no longer reaches its head). Someone who can rewrite the whole chain and its head can only be caught by comparing with a head you anchored elsewhere.

## Per-tenant rate limits

A tenant's traffic can be limited as a whole: `RateLimitKey::tenant` keys `RateLimitLayer`'s
buckets by the request's tenant (`tenant:<id>`), so every user of one organisation draws from the
same bucket and one busy tenant cannot starve the others. The tenant is read from a value the
authentication layer recorded — the request's `RequestCaps` override of a type, else a request
extension of that type — through a closure, so `id_effect_axum` needs no dependency on the auth
or SQL crates. With `id_effect_auth`'s `Authenticate` (which records the `CurrentPrincipal` in
both places) that is `RateLimitKey::tenant(|p: &CurrentPrincipal| p.tenant().map(str::to_owned))`.
The rate limit layer goes **inside** the authentication layer (added to the router before it), so
the principal is there when the key is computed; a request without a tenant falls back to the key
given with `.or(..)`.

```rust
use std::sync::Arc;

use axum::body::Body;
use axum::extract::Request;
use axum::http::StatusCode;
use axum::middleware::{self, Next};
use axum::response::Response;
use axum::routing::get;
use id_effect_authz::{BasicPrincipal, CurrentPrincipal, Principal};
use id_effect_axum::middleware::{MemoryRateLimitStore, RateLimitKey, RateLimitLayer};
use tower::ServiceExt;

// A stand-in for the authentication layer: `x-user` signs in as a member of `x-org`.
async fn authenticate(mut req: Request, next: Next) -> Response {
  let signed_in = {
    let header = |name: &str| req.headers().get(name).and_then(|v| v.to_str().ok()).map(str::to_owned);
    (header("x-user"), header("x-org"))
  };
  if let (Some(user), Some(org)) = signed_in {
    let principal = BasicPrincipal::new(user).with_tenant(org);
    req.extensions_mut().insert(CurrentPrincipal::new(principal));
  }
  next.run(req).await
}

// Two requests per tenant (no refill, for the example); anonymous callers share one bucket.
let store = Arc::new(MemoryRateLimitStore::new(0.0, 2.0));
let per_tenant = RateLimitKey::tenant(|p: &CurrentPrincipal| p.tenant().map(str::to_owned))
  .or(RateLimitKey::global());
let app = axum::Router::new()
  .route("/reports", get(|| async { "report" }))
  .layer(RateLimitLayer::new(Arc::clone(&store)).key(per_tenant))
  .layer(middleware::from_fn(authenticate));

let call = |user: &str, org: &str| {
  let req = Request::builder().uri("/reports").header("x-user", user).header("x-org", org);
  app.clone().oneshot(req.body(Body::empty()).unwrap())
};
# tokio::runtime::Runtime::new().unwrap().block_on(async {
// Two acme users spend acme's bucket; globex is not affected.
assert_eq!(call("ada", "acme").await.unwrap().status(), StatusCode::OK);
assert_eq!(call("bob", "acme").await.unwrap().status(), StatusCode::OK);
assert_eq!(call("ada", "acme").await.unwrap().status(), StatusCode::TOO_MANY_REQUESTS);
assert_eq!(call("eve", "globex").await.unwrap().status(), StatusCode::OK);
# });
assert_eq!(store.len(), 2); // tenant:acme, tenant:globex
```

Every tenant gets the same rate. `RedisRateLimitStore` (feature `rate-limit` of `id_effect_redis`)
holds the per-tenant buckets across instances; tenants on different plans need a
`RateLimitStore` that sizes a bucket by its key, or one layer per plan.

## What this pattern does not cover

- **Database-enforced isolation.** The filter lives in the repository, so raw SQL (`query`, `execute`) and statements you build with the query builder are not filtered: add the tenant condition yourself, and keep such code close to the repository. For a second line of defence, PostgreSQL row-level security (`CREATE POLICY … USING (org_id = current_setting('app.tenant'))`, with `SET LOCAL app.tenant` at the start of each transaction) enforces the same rule in the database; `id_effect_sql` does not set it up for you.
- **Schema or database per tenant.** Stronger isolation (one schema per tenant selected with `search_path`, or one database per tenant with its own pool) is an alternative to row-level tenancy. Per-tenant connection routing is not part of 0.9; `RoutingSqlClient` routes reads to replicas, not tenants.
- **Per-tenant configuration and quotas** other than a request rate (storage, seats, a different
  rate per plan out of the box) are not provided; per-tenant request rate limits are (above).
- **Audit on other databases.** The audit log's SQL is PostgreSQL's. The repository and its tenant filter work on PostgreSQL, MySQL and SQLite.

## Tests and example

- `crates/id_effect_sql/tests/repository.rs`: `tenant_filter_scopes_every_query` runs on SQLite and PostgreSQL (two tenants, every operation, `CurrentTenant` from the environment, fail-closed without a tenant).
- `crates/id_effect_sql/tests/audit.rs` (PostgreSQL): `tampered_row_breaks_chain`, `audit_rolls_back_with_tx`, removed records, concurrent writers that never fork a chain, and the `REPEATABLE READ` race.
- `crates/id_effect_authz/tests/guards.rs`: principals installed by `CapOverrideLayer` reach the guards.
- `crates/id_effect_axum/src/middleware/rate_limit.rs`: `each_tenant_has_its_own_bucket` (per-tenant buckets behind a `CapOverrideLayer`) and `tenant_keys_read_request_caps_then_extensions`.
- `crates/id_effect_sql/examples/050_tenancy_audit.rs` runs the request path for two tenants: by default on SQLite in memory (tenant filter, transaction, fail-closed), with `--live` and `DATABASE_URL` on PostgreSQL including the audit chain, a rolled-back request, `verify_chain` and a detected edit:
  `cargo run -p id_effect_sql --example 050_tenancy_audit --features repository,sqlite,audit,migrate,pg -- --live`.
