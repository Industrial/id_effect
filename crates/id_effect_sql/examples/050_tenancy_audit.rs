//! Row-level multi-tenancy with an audit trail, the request path of the book chapter "Tenancy and
//! the audit log": the authenticated caller's tenant becomes the `CurrentTenant` and
//! `AuditContext` capabilities of one request, every statement of a tenant-scoped `Repository`
//! is filtered by that tenant, the change and its audit record share the request's transaction,
//! and `verify_chain` checks the tenant's hash chain afterwards.
//!
//! By default it runs on a private in-memory SQLite database: the tenant filter, the transaction
//! and the fail-closed checks run, and the audit steps are skipped (the audit log's SQL is
//! PostgreSQL's). With `DATABASE_URL` set, or with `--live`, everything runs on PostgreSQL,
//! audit included: it applies the bundled audit schema, uses fresh tenant names per run and drops
//! its invoice table at the end, so it can be run again.
//!
//! In an axum service the caller comes from `id_effect_auth`'s `Authenticate` layer and the two
//! capabilities from `CapOverrideLayer`s (see the chapter); here `Caller` stands in for the
//! authenticated principal.
//!
//! Run: `cargo run -p id_effect_sql --example 050_tenancy_audit --features
//! repository,sqlite,audit,migrate,pg`
//!
//! Live: `DATABASE_URL=postgres://… cargo run -p id_effect_sql --example 050_tenancy_audit
//! --features repository,sqlite,audit,migrate,pg -- --live`

use std::process::ExitCode;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use id_effect::{Cause, Effect, Exit, HCons, HNil, Layer, fail, hlist, need, succeed};
use id_effect_sql::audit::{self, AuditContext, AuditEntry, AuditLog};
use id_effect_sql::pg::{PgPoolConfig, PgSqlClient, pg_pool_from_config};
use id_effect_sql::sqlite::{SqlitePoolConfig, SqliteSqlClient, sqlite_pool_from_config};
use id_effect_sql::{
  CurrentTenant, Dialect, Entity, FromRow, Repository, SqlClient, SqlClientService, SqlError,
  SqlTransactionService, transaction_layer,
};
use id_effect_tokio::TokioRuntime;
use serde_json::{Value, json};

/// An invoice row. `org_id` is the tenant column: not a field, written by the repository on
/// insert and AND-ed into every statement it builds.
#[derive(Clone, Debug, PartialEq, FromRow, Entity)]
#[entity(table = "example_050_invoices", tenant = "org_id")]
struct Invoice {
  #[entity(id)]
  id: i64,
  name: String,
  amount: i64,
  #[entity(version)]
  version: i64,
}

impl Invoice {
  fn new(id: i64, name: &str, amount: i64) -> Self {
    Self {
      id,
      name: name.into(),
      amount,
      version: 1,
    }
  }

  /// The audited state.
  fn state(&self) -> Value {
    json!({ "name": self.name, "amount": self.amount, "version": self.version })
  }
}

const TABLE: &str = "CREATE TABLE example_050_invoices (
  id BIGINT PRIMARY KEY,
  org_id TEXT NOT NULL,
  name TEXT NOT NULL,
  amount BIGINT NOT NULL,
  version BIGINT NOT NULL
)";

/// The authenticated principal of a request (`id_effect_authz::Principal`: `id()`, `tenant()`).
struct Caller {
  user: &'static str,
  tenant: Option<String>,
}

/// What one request's effect runs in: the per-request capabilities and the request's
/// transaction (`transaction_layer`).
type RequestEnv = HCons<
  SqlClientService,
  HCons<CurrentTenant, HCons<AuditContext, HCons<AuditLog, HCons<SqlTransactionService, HNil>>>>,
>;

/// A request's failure.
#[derive(Debug)]
enum AppError {
  Sql(SqlError),
  /// No tenant on the principal: refused before any statement runs (403).
  NoTenant,
  /// A later step of the request failed, after the change and its audit record were written.
  Downstream(&'static str),
}

impl From<SqlError> for AppError {
  fn from(e: SqlError) -> Self {
    AppError::Sql(e)
  }
}

/// The database: SQLite in memory, or PostgreSQL.
struct Db {
  client: Arc<dyn SqlClient>,
  dialect: Dialect,
  live: bool,
}

impl Db {
  async fn plain<A, E>(&self, eff: Effect<A, E>) -> Result<A, E>
  where
    A: Send + 'static,
    E: std::fmt::Debug + Send + 'static,
  {
    match TokioRuntime::current().expect("tokio").run(eff).await {
      Exit::Success(a) => Ok(a),
      Exit::Failure(Cause::Fail(e)) => Err(e),
      Exit::Failure(cause) => panic!("not a typed failure: {cause:?}"),
    }
  }

  /// Runs one request for `caller`: its tenant and audit context become capabilities, and the
  /// effect runs in one transaction that commits when it succeeds and rolls back when it fails.
  async fn request<A: Send + 'static>(
    &self,
    caller: &Caller,
    handler: Effect<A, AppError, RequestEnv>,
  ) -> Result<A, AppError> {
    // Fail closed: a principal without a tenant gets no tenant-scoped capabilities at all.
    let Some(tenant) = caller.tenant.clone() else {
      return Err(AppError::NoTenant);
    };
    let caps = Layer::succeed(hlist![
      SqlClientService(Arc::clone(&self.client)),
      CurrentTenant::new(tenant.clone()),
      AuditContext::for_tenant(caller.user, tenant),
      AuditLog::new(),
    ]);
    let per_request = transaction_layer::<AppError>().provide_merge(caps);
    self.plain(handler.provide_layer(per_request)).await
  }
}

/// `POST /invoices`: creates an invoice for the caller's tenant (whatever tenant it names).
fn create(dialect: Dialect, invoice: Invoice) -> Effect<Invoice, AppError, RequestEnv> {
  Repository::<Invoice>::for_current_tenant(dialect)
    .flat_map(move |repo| {
      need::<SqlTransactionService>()
        .flat_map(move |tx| repo.on(tx.0.clone()).insert(invoice).narrow())
    })
    .map_err(AppError::from)
}

/// `GET /invoices`: the caller's tenant's invoices.
fn list(dialect: Dialect) -> Effect<Vec<Invoice>, AppError, RequestEnv> {
  Repository::<Invoice>::for_current_tenant(dialect)
    .flat_map(move |repo| {
      need::<SqlTransactionService>().flat_map(move |tx| repo.on(tx.0.clone()).find_all().narrow())
    })
    .map_err(AppError::from)
}

/// `PUT /invoices/{id}`: renames an invoice of the caller's tenant; the state before and after.
/// Another tenant's invoice is not found (the filter is in the `UPDATE` too).
fn rename(
  dialect: Dialect,
  id: i64,
  name: String,
) -> Effect<(Invoice, Invoice), AppError, RequestEnv> {
  Repository::<Invoice>::for_current_tenant(dialect)
    .flat_map(move |repo| {
      need::<SqlTransactionService>().flat_map(move |tx| {
        let invoices = repo.on(tx.0.clone());
        let update = invoices.clone();
        invoices
          .find(id)
          .flat_map(move |found| match found {
            None => fail(SqlError::RowNotFound),
            Some(before) => update
              .update(Invoice {
                name,
                ..before.clone()
              })
              .map(move |after| (before, after)),
          })
          .narrow()
      })
    })
    .map_err(AppError::from)
}

/// [`rename`] plus its audit record, in the same transaction. `then_fail` simulates a later step
/// of the request failing: the rename and the record roll back together.
fn rename_audited(
  dialect: Dialect,
  id: i64,
  name: String,
  then_fail: bool,
) -> Effect<Invoice, AppError, RequestEnv> {
  rename(dialect, id, name).flat_map(move |(before, after)| {
    let entry = AuditEntry::new("invoice.rename", format!("invoice:{id}"))
      .with_before(before.state())
      .with_after(after.state());
    audit::record(entry)
      .map_err(AppError::from)
      .flat_map(move |record| {
        println!(
          "  audit: seq {} by {} for {:?}: {}",
          record.seq, record.actor, record.tenant, record.action
        );
        if then_fail {
          fail(AppError::Downstream("notification service unavailable"))
        } else {
          succeed(after)
        }
      })
  })
}

fn names(invoices: &[Invoice]) -> Vec<&str> {
  invoices.iter().map(|i| i.name.as_str()).collect()
}

async fn steps(db: &Db) -> Result<(), AppError> {
  let d = db.dialect;
  // Fresh tenants per live run, so earlier runs' audit chains do not interfere.
  let suffix = if db.live {
    let nanos = SystemTime::now()
      .duration_since(UNIX_EPOCH)
      .map_or(0, |t| t.as_nanos());
    format!("-{nanos}")
  } else {
    String::new()
  };
  let (acme, globex) = (format!("acme{suffix}"), format!("globex{suffix}"));
  let ada = Caller {
    user: "ada",
    tenant: Some(acme.clone()),
  };
  let bob = Caller {
    user: "bob",
    tenant: Some(globex.clone()),
  };
  let anonymous = Caller {
    user: "anonymous",
    tenant: None,
  };

  // Each tenant creates its invoices; the repository writes the tenant column.
  db.request(&ada, create(d, Invoice::new(1, "draft", 100)))
    .await?;
  db.request(&ada, create(d, Invoice::new(2, "hosting", 40)))
    .await?;
  db.request(&bob, create(d, Invoice::new(3, "consulting", 70)))
    .await?;
  let mine = db.request(&ada, list(d)).await?;
  let theirs = db.request(&bob, list(d)).await?;
  println!(
    "list: {acme} sees {:?}, {globex} sees {:?}",
    names(&mine),
    names(&theirs)
  );
  assert_eq!(names(&mine), ["draft", "hosting"]);
  assert_eq!(names(&theirs), ["consulting"]);

  // A rename in the caller's tenant (audited on PostgreSQL).
  let renamed = if db.live {
    db.request(&ada, rename_audited(d, 1, "final".into(), false))
      .await?
  } else {
    db.request(&ada, rename(d, 1, "final".into())).await?.1
  };
  assert_eq!((renamed.name.as_str(), renamed.version), ("final", 2));
  println!("rename: {acme} renamed invoice 1 to {:?}", renamed.name);

  // Another tenant's row is invisible: not found, and nothing changes.
  match db.request(&bob, rename(d, 1, "hijacked".into())).await {
    Err(AppError::Sql(SqlError::RowNotFound)) => {
      println!("rename: {globex} cannot see invoice 1 (not found)")
    }
    other => panic!("expected not found, got {other:?}"),
  }

  // Fail closed: no tenant, no data — at the request boundary and in the repository itself.
  match db.request(&anonymous, list(d)).await {
    Err(AppError::NoTenant) => println!("list: a principal without a tenant is refused"),
    other => panic!("expected a refusal, got {other:?}"),
  }
  let unscoped_by_mistake = Repository::<Invoice>::new(d).on(Arc::clone(&db.client));
  match db.plain(unscoped_by_mistake.find_all()).await {
    Err(SqlError::Unsupported(why)) => println!("repository without a tenant: {why}"),
    other => panic!("expected a fail-closed error, got {other:?}"),
  }

  if !db.live {
    println!("audit: skipped (PostgreSQL only: set DATABASE_URL or pass --live)");
    return Ok(());
  }

  // A request that fails after writing: the rename and its audit record roll back together.
  match db
    .request(&ada, rename_audited(d, 2, "renamed".into(), true))
    .await
  {
    Err(AppError::Downstream(why)) => println!("rename: rolled back ({why})"),
    other => panic!("expected a downstream failure, got {other:?}"),
  }
  let after = db.request(&ada, list(d)).await?;
  assert_eq!(names(&after), ["final", "hosting"]);
  db.request(&ada, rename_audited(d, 2, "hosting 2026".into(), false))
    .await?;

  // The tenant's chain: two records (the rolled-back one never existed), intact.
  let verify = |tenant: String| {
    audit::verify_chain(Some(tenant.as_str())).provide_all(hlist![
      AuditLog::new(),
      SqlClientService(Arc::clone(&db.client)),
    ])
  };
  let report = db.plain(verify(acme.clone())).await?;
  assert!(report.is_intact(), "{report:?}");
  assert_eq!(report.records, 2);
  // Anchor the head outside the database (a log line here), so a rewrite of the whole chain
  // is detectable too.
  println!(
    "verify: {acme}: {} records intact, head {}",
    report.records,
    report.head.as_deref().unwrap_or("-")
  );
  let other = db.plain(verify(globex.clone())).await?;
  assert_eq!((other.records, other.is_intact()), (0, true));

  // Someone edits a stored record: the chain names it.
  let edited = db
    .plain(db.client.execute(
      "UPDATE audit_log SET after = '{\"name\": \"forged\"}' WHERE chain = $1 AND seq = 1",
      &[acme.as_str().into()],
    ))
    .await?;
  assert_eq!(edited, 1);
  let broken = db.plain(verify(acme.clone())).await?;
  let link = broken.first_broken.expect("the edit is detected");
  println!("verify after an edit: {link}");
  assert_eq!(link.seq, 1);
  Ok(())
}

/// The target from `DATABASE_URL` / `--live`, or SQLite in memory.
async fn open() -> Result<Db, String> {
  let live = std::env::args().skip(1).any(|a| a == "--live");
  match std::env::var("DATABASE_URL") {
    Ok(url) if !url.is_empty() => {
      let pool = pg_pool_from_config(PgPoolConfig::from_url(url))
        .await
        .map_err(|e| format!("cannot build pool: {}", e.0))?;
      let client: Arc<dyn SqlClient> = Arc::new(PgSqlClient::new(pool));
      let db = Db {
        client,
        dialect: Dialect::Postgres,
        live: true,
      };
      // The audit schema, with its own tracking table (idempotent).
      db.plain(audit::migrator().run_on(Arc::clone(&db.client)))
        .await
        .map_err(|e| format!("audit schema: {e}"))?;
      for sql in ["DROP TABLE IF EXISTS example_050_invoices", TABLE] {
        db.plain(db.client.execute(sql, &[]))
          .await
          .map_err(|e| format!("{sql}: {e}"))?;
      }
      Ok(db)
    }
    _ if live => Err("--live needs DATABASE_URL (postgres://user:pass@host:port/db)".into()),
    _ => {
      let pool = sqlite_pool_from_config(SqlitePoolConfig::memory())
        .await
        .map_err(|e| format!("sqlite: {}", e.0))?;
      let db = Db {
        client: Arc::new(SqliteSqlClient::new(pool)),
        dialect: Dialect::Sqlite,
        live: false,
      };
      db.plain(db.client.execute(TABLE, &[]))
        .await
        .map_err(|e| format!("{TABLE}: {e}"))?;
      Ok(db)
    }
  }
}

#[tokio::main]
async fn main() -> ExitCode {
  let db = match open().await {
    Ok(db) => db,
    Err(why) => {
      eprintln!("050_tenancy_audit: {why}");
      return ExitCode::from(2);
    }
  };
  let mode = if db.live {
    "on PostgreSQL: tenant filter, transaction, audit chain"
  } else {
    "SQLite in memory: tenant filter, transaction"
  };
  let outcome = steps(&db).await;
  if db.live {
    let _ = db
      .plain(
        db.client
          .execute("DROP TABLE IF EXISTS example_050_invoices", &[]),
      )
      .await;
  }
  match outcome {
    Ok(()) => {
      println!("050_tenancy_audit ok ({mode})");
      ExitCode::SUCCESS
    }
    Err(e) => {
      eprintln!("050_tenancy_audit failed ({mode}): {e:?}");
      ExitCode::FAILURE
    }
  }
}
