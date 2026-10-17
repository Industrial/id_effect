//! `id_effect_sql::audit` on PostgreSQL (features `audit` and `pg`).
//!
//! Every test needs a database at `DATABASE_URL`; with `ID_EFFECT_REQUIRE_PG=1` a missing or
//! unreachable database fails the test, otherwise it prints `SKIP` and returns (as in
//! `tests/pg.rs`). The schema is the bundled migration; each test works on its own tenant's
//! chain, so the tests run in parallel against one database.

use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use id_effect::{Cause, Effect, Exit, fail};
use id_effect_sql::audit::{
  AuditContext, AuditEntry, AuditLog, AuditRecord, BreakKind, ChainReport, MIGRATIONS,
};
use id_effect_sql::pg::{PgPoolConfig, PgSqlClient, pg_pool_from_config};
use id_effect_sql::{
  Isolation, SqlClient, SqlError, SqlValue, is_serialization_failure, with_transaction,
};
use id_effect_tokio::TokioRuntime;
use serde_json::json;

async fn run<A: Send + 'static>(eff: Effect<A, SqlError>) -> Result<A, SqlError> {
  match TokioRuntime::current().expect("tokio").run(eff).await {
    Exit::Success(a) => Ok(a),
    Exit::Failure(Cause::Fail(e)) => Err(e),
    Exit::Failure(c) => panic!("unexpected cause: {c:?}"),
  }
}

fn unavailable<T>(test: &str, why: &str) -> Option<T> {
  assert!(
    !std::env::var("ID_EFFECT_REQUIRE_PG").is_ok_and(|v| v == "1"),
    "{test}: ID_EFFECT_REQUIRE_PG=1 but Postgres is unavailable: {why}"
  );
  eprintln!("SKIP {test}: {why} (set DATABASE_URL, and ID_EFFECT_REQUIRE_PG=1 to require it)");
  None
}

/// The migrator's advisory lock key, so schema setup never races `migrate`.
const SETUP_LOCK: i64 = 0x6964_6566_6d69_6772;

/// A connected client with the audit schema, or `None` after [`unavailable`] decided to skip.
async fn live(test: &str, pool_size: u32) -> Option<Arc<dyn SqlClient>> {
  let url = match std::env::var("DATABASE_URL") {
    Ok(url) => url,
    Err(_) => return unavailable(test, "DATABASE_URL is not set"),
  };
  let pool = match pg_pool_from_config(PgPoolConfig::from_url(url).with_max_size(pool_size)).await {
    Ok(p) => p,
    Err(e) => return unavailable(test, &format!("cannot build pool: {}", e.0)),
  };
  let client: Arc<dyn SqlClient> = Arc::new(PgSqlClient::new(pool));
  if let Err(e) = run(client.connect()).await {
    return unavailable(test, &format!("cannot connect: {e:?}"));
  }
  let schema = with_transaction(Arc::clone(&client), |tx| {
    // The script's statements, without its comments.
    let script: Vec<&str> = MIGRATIONS[0]
      .1
      .lines()
      .filter(|l| !l.trim_start().starts_with("--"))
      .collect();
    let statements: Vec<String> = script
      .join("\n")
      .split(';')
      .filter(|s| !s.trim().is_empty())
      .map(str::to_owned)
      .collect();
    let lock = tx.execute("SELECT pg_advisory_xact_lock($1)", &[SETUP_LOCK.into()]);
    statements.into_iter().fold(lock, |eff, sql| {
      let tx = Arc::clone(&tx);
      eff.flat_map(move |_| tx.execute(&sql, &[]))
    })
  });
  run(schema).await.expect("audit schema");
  Some(client)
}

/// A tenant no other test (or earlier run) uses.
fn tenant(test: &str) -> String {
  let nanos = SystemTime::now()
    .duration_since(UNIX_EPOCH)
    .unwrap()
    .as_nanos();
  format!("{test}-{nanos}")
}

/// Appends `entry` by `ctx` in its own committed transaction.
async fn append(
  client: &Arc<dyn SqlClient>,
  ctx: &AuditContext,
  entry: AuditEntry,
) -> Result<AuditRecord, SqlError> {
  let (log, ctx) = (AuditLog::new(), ctx.clone());
  run(with_transaction(Arc::clone(client), move |tx| {
    log.append(&tx, &ctx, entry)
  }))
  .await
}

fn invoice(n: i64) -> AuditEntry {
  AuditEntry::new("invoice.update", format!("invoice:{n}"))
    .with_before(json!({ "amount": n * 100, "lines": [{ "sku": "a", "qty": 1.5 }] }))
    .with_after(
      json!({ "amount": n * 100 + 1, "note": "ünïcödé \"q\"", "zero": -0.0, "big": 1e21 }),
    )
}

async fn verify(client: &Arc<dyn SqlClient>, tenant: &str) -> ChainReport {
  run(AuditLog::new().verify_chain_on(Arc::clone(client), Some(tenant)))
    .await
    .expect("verify")
}

/// Runs `sql` with the tenant as `$1`.
async fn tamper(client: &Arc<dyn SqlClient>, sql: &str, tenant: &str) {
  let n = run(client.execute(sql, &[tenant.into()]))
    .await
    .expect("tamper");
  assert!(n > 0, "{sql} changed nothing");
}

#[tokio::test]
async fn live_postgres_appends_link_and_verify() {
  let Some(db) = live("live_postgres_appends_link_and_verify", 2).await else {
    return;
  };
  let t = tenant("link");
  let ctx = AuditContext::for_tenant("ada", t.as_str());
  let mut records = Vec::new();
  for n in 1..=3 {
    records.push(append(&db, &ctx, invoice(n)).await.expect("append"));
  }
  assert_eq!(records.iter().map(|r| r.seq).collect::<Vec<_>>(), [1, 2, 3]);
  assert_eq!(records[0].prev_hash, [0; 32]);
  assert_eq!(records[1].prev_hash, records[0].hash);
  assert_eq!(records[2].prev_hash, records[1].hash);

  // What is stored decodes to what was returned (jsonb and timestamptz round-trip the hash).
  let rows = run(db.query(
    "SELECT id, seq, actor, tenant, action, entity, before, after, at, prev_hash, hash \
     FROM audit_log WHERE chain = $1 ORDER BY seq",
    &[t.as_str().into()],
  ))
  .await
  .unwrap();
  let stored: Vec<AuditRecord> = rows
    .iter()
    .map(|r| AuditRecord::from_row(r).unwrap())
    .collect();
  for (s, r) in stored.iter().zip(&records) {
    assert_eq!(s.computed_hash(), r.hash);
    assert_eq!((s.id, s.seq, &s.actor, s.at), (r.id, r.seq, &r.actor, r.at));
  }

  let report = verify(&db, &t).await;
  assert!(report.is_intact(), "{report:?}");
  assert_eq!(report.records, 3);
  assert_eq!(report.tenant.as_deref(), Some(t.as_str()));
  let hex: String = records[2].hash.iter().map(|b| format!("{b:02x}")).collect();
  assert_eq!(report.head, Some(hex));

  // Another tenant's chain is separate.
  let other = tenant("link-other");
  let first = append(
    &db,
    &AuditContext::for_tenant("bob", other.as_str()),
    invoice(9),
  )
  .await
  .unwrap();
  assert_eq!((first.seq, first.prev_hash), (1, vec![0; 32]));
  assert_eq!(verify(&db, &t).await.records, 3);
}

#[tokio::test]
async fn tampered_row_breaks_chain() {
  let Some(db) = live("tampered_row_breaks_chain", 2).await else {
    return;
  };
  let t = tenant("tamper");
  let ctx = AuditContext::for_tenant("ada", t.as_str());
  let mut ids = Vec::new();
  for n in 1..=4 {
    ids.push(append(&db, &ctx, invoice(n)).await.unwrap().id);
  }
  tamper(
    &db,
    "UPDATE audit_log SET after = '{\"amount\": 1000000}' WHERE chain = $1 AND seq = 2",
    &t,
  )
  .await;
  let report = verify(&db, &t).await;
  let broken = report.first_broken.expect("the edit is detected");
  assert_eq!(
    (broken.id, broken.seq, broken.kind),
    (Some(ids[1]), 2, BreakKind::Hash)
  );
  assert_ne!(broken.expected, broken.actual);
  assert_eq!(
    report.records, 1,
    "only the record before the edit is intact"
  );
}

#[tokio::test]
async fn live_postgres_removed_records_break_chain() {
  let Some(db) = live("live_postgres_removed_records_break_chain", 2).await else {
    return;
  };
  let t = tenant("remove");
  let ctx = AuditContext::for_tenant("ada", t.as_str());
  for n in 1..=4 {
    append(&db, &ctx, invoice(n)).await.unwrap();
  }
  // From the end: the chain no longer reaches its head.
  tamper(
    &db,
    "DELETE FROM audit_log WHERE chain = $1 AND seq = 4",
    &t,
  )
  .await;
  let b = verify(&db, &t).await.first_broken.expect("truncation");
  assert_eq!((b.id, b.seq, b.kind), (None, 4, BreakKind::Head));
  // From the middle: the sequence has a gap.
  tamper(
    &db,
    "DELETE FROM audit_log WHERE chain = $1 AND seq = 2",
    &t,
  )
  .await;
  let b = verify(&db, &t).await.first_broken.expect("gap");
  assert_eq!((b.seq, b.kind), (3, BreakKind::Sequence));
  // Renumbered to hide the gap: the link is broken.
  tamper(
    &db,
    "UPDATE audit_log SET seq = seq - 1 WHERE chain = $1 AND seq = 3",
    &t,
  )
  .await;
  let b = verify(&db, &t).await.first_broken.expect("relinked");
  assert_eq!((b.seq, b.kind), (2, BreakKind::PrevHash));
}

#[derive(Debug)]
enum AppError {
  #[allow(dead_code)] // read through `Debug`
  Sql(SqlError),
  Rejected,
}

impl From<SqlError> for AppError {
  fn from(e: SqlError) -> Self {
    AppError::Sql(e)
  }
}

#[tokio::test]
async fn audit_rolls_back_with_tx() {
  let Some(db) = live("audit_rolls_back_with_tx", 2).await else {
    return;
  };
  let t = tenant("rollback");
  let ctx = AuditContext::for_tenant("ada", t.as_str());
  let first = append(&db, &ctx, invoice(1)).await.unwrap();

  // The change fails after its audit record was appended: both roll back.
  let (log, ctx2) = (AuditLog::new(), ctx.clone());
  let failed: Effect<(), AppError> = with_transaction(Arc::clone(&db), move |tx| {
    log
      .append(&tx, &ctx2, invoice(2))
      .map_err(AppError::from)
      .flat_map(|_| fail(AppError::Rejected))
  });
  let exit = TokioRuntime::current().unwrap().run(failed).await;
  assert!(
    matches!(exit, Exit::Failure(Cause::Fail(AppError::Rejected))),
    "{exit:?}"
  );
  let count = run(db.query(
    "SELECT COUNT(*)::int8 AS n FROM audit_log WHERE chain = $1",
    &[t.as_str().into()],
  ))
  .await
  .unwrap();
  assert_eq!(count[0].get::<i64>("n"), Ok(1));

  // The head did not move: the next record follows the first.
  let next = append(&db, &ctx, invoice(3)).await.unwrap();
  assert_eq!((next.seq, &next.prev_hash), (2, &first.hash));
  let report = verify(&db, &t).await;
  assert!(report.is_intact(), "{report:?}");
  assert_eq!(report.records, 2);

  // A rolled-back first record of a new chain leaves no head behind.
  let fresh = tenant("rollback-fresh");
  let (log, ctx3) = (
    AuditLog::new(),
    AuditContext::for_tenant("ada", fresh.as_str()),
  );
  let failed: Effect<(), AppError> = with_transaction(Arc::clone(&db), move |tx| {
    log
      .append(&tx, &ctx3, invoice(1))
      .map_err(AppError::from)
      .flat_map(|_| fail(AppError::Rejected))
  });
  let _ = TokioRuntime::current().unwrap().run(failed).await;
  let report = verify(&db, &fresh).await;
  assert_eq!(
    (report.records, report.head, report.first_broken),
    (0, None, None)
  );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn live_postgres_concurrent_appends_never_fork_the_chain() {
  const WRITERS: i64 = 12;
  const EACH: i64 = 5;
  let Some(db) = live("live_postgres_concurrent_appends_never_fork_the_chain", 8).await else {
    return;
  };
  let (t, other) = (tenant("concurrent"), tenant("concurrent-other"));
  let mut writers = Vec::new();
  for w in 0..WRITERS {
    // Even writers share one chain; odd ones write to a second chain at the same time.
    let tenant = if w % 2 == 0 { t.clone() } else { other.clone() };
    let db = Arc::clone(&db);
    writers.push(tokio::spawn(async move {
      let ctx = AuditContext::for_tenant(format!("writer-{w}"), tenant);
      for n in 0..EACH {
        append(&db, &ctx, invoice(w * 100 + n))
          .await
          .expect("append");
      }
    }));
  }
  for w in writers {
    w.await.expect("writer");
  }
  for chain in [&t, &other] {
    let report = verify(&db, chain).await;
    assert!(report.is_intact(), "{report:?}");
    assert_eq!(report.records, (WRITERS / 2 * EACH) as u64);
  }
  let seqs = run(db.query(
    "SELECT COUNT(DISTINCT seq)::int8 AS n, MAX(seq) AS max FROM audit_log WHERE chain = $1",
    &[SqlValue::from(t.as_str())],
  ))
  .await
  .unwrap();
  assert_eq!(seqs[0].get::<i64>("n"), Ok(WRITERS / 2 * EACH));
  assert_eq!(seqs[0].get::<i64>("max"), Ok(WRITERS / 2 * EACH));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn live_postgres_repeatable_read_race_is_a_serialization_failure() {
  let Some(db) = live(
    "live_postgres_repeatable_read_race_is_a_serialization_failure",
    4,
  )
  .await
  else {
    return;
  };
  let t = tenant("repeatable");
  let (log, ctx) = (AuditLog::new(), AuditContext::for_tenant("ada", t.as_str()));
  append(&db, &ctx, invoice(1)).await.unwrap();
  let a = run(db.begin_with(Isolation::RepeatableRead.into()))
    .await
    .unwrap();
  let b = run(db.begin_with(Isolation::RepeatableRead.into()))
    .await
    .unwrap();
  // `b` takes its snapshot, then `a` appends and holds the head row.
  run(b.query("SELECT 1 AS one", &[])).await.unwrap();
  run(log.append(&a, &ctx, invoice(2))).await.unwrap();
  let (log2, ctx2, b2) = (log.clone(), ctx.clone(), Arc::clone(&b));
  let racing = tokio::spawn(async move { run(log2.append(&b2, &ctx2, invoice(3))).await });
  tokio::time::sleep(Duration::from_millis(300)).await;
  run(a.commit()).await.unwrap();
  // `b` waited for the lock, then cannot link to a head its snapshot does not see.
  let err = racing.await.unwrap().unwrap_err();
  assert!(is_serialization_failure(&err), "{err:?}");
  run(b.rollback()).await.unwrap();
  let report = verify(&db, &t).await;
  assert!(report.is_intact(), "{report:?}");
  assert_eq!(report.records, 2);
}

#[cfg(feature = "migrate")]
#[tokio::test]
async fn live_postgres_bundled_migrator_applies_the_schema() {
  let Some(db) = live("live_postgres_bundled_migrator_applies_the_schema", 2).await else {
    return;
  };
  let migrator = id_effect_sql::audit::migrator();
  let status = migrator.clone();
  let applied = TokioRuntime::current()
    .unwrap()
    .run(
      migrator
        .run_on(Arc::clone(&db))
        .flat_map(move |_| status.status_on(db)),
    )
    .await;
  let Exit::Success(statuses) = applied else {
    panic!("{applied:?}")
  };
  assert!(
    statuses.iter().all(|s| s.state.is_applied()),
    "{statuses:?}"
  );
}
