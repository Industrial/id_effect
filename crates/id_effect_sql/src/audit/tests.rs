//! Unit tests of the audit log on `TestSqlClient` (scripted rows). The PostgreSQL tests are in
//! `tests/audit.rs`.

use std::sync::Arc;

use id_effect::{Cause, Effect, Exit, HCons, hlist};
use serde_json::json;

use super::*;
use crate::client::{SqlClient, SqlClientEnv, TestSqlClient, sql_client_env};
use crate::transaction::{SqlTransactionEnv, with_transaction};

fn run<A: Send + 'static>(eff: Effect<A, SqlError>) -> Result<A, SqlError> {
  match eff.block_on() {
    Exit::Success(a) => Ok(a),
    Exit::Failure(Cause::Fail(e)) => Err(e),
    Exit::Failure(c) => panic!("unexpected cause: {c:?}"),
  }
}

fn connected() -> TestSqlClient {
  let db = TestSqlClient::new();
  run(db.connect()).unwrap();
  db
}

fn at(micros: i64) -> jiff::Timestamp {
  jiff::Timestamp::from_microsecond(micros).unwrap()
}

/// A valid chain of `n` records for `tenant`.
fn chain(tenant: Option<&str>, n: i64) -> Vec<AuditRecord> {
  let mut prev = GENESIS_HASH.to_vec();
  (1..=n)
    .map(|seq| {
      let mut r = AuditRecord {
        id: 100 + seq,
        seq,
        actor: "ada".into(),
        tenant: tenant.map(str::to_owned),
        action: "invoice.update".into(),
        entity: format!("invoice:{seq}"),
        before: (seq > 1).then(|| json!({ "n": seq - 1 })),
        after: Some(json!({ "n": seq, "tags": ["a", { "z": 1, "b": 2.5 }] })),
        at: at(1_700_000_000_000_000 + seq),
        prev_hash: prev.clone(),
        hash: Vec::new(),
      };
      r.hash = r.computed_hash();
      prev.clone_from(&r.hash);
      r
    })
    .collect()
}

fn row_of(r: &AuditRecord) -> SqlRow {
  SqlRow::from_pairs([
    ("id", SqlValue::I64(r.id)),
    ("seq", SqlValue::I64(r.seq)),
    ("actor", r.actor.clone().into()),
    ("tenant", r.tenant.clone().into()),
    ("action", r.action.clone().into()),
    ("entity", r.entity.clone().into()),
    ("before", r.before.clone().into()),
    ("after", r.after.clone().into()),
    ("at", r.at.into()),
    ("prev_hash", r.prev_hash.clone().into()),
    ("hash", r.hash.clone().into()),
  ])
}

fn head_row(seq: i64, hash: &[u8]) -> SqlRow {
  SqlRow::from_pairs([
    ("seq", SqlValue::I64(seq)),
    ("hash", SqlValue::Bytes(hash.to_vec())),
  ])
}

/// Scripts `records` (and a head at the last one, or `head`) for the chain `""`.
fn script(db: &TestSqlClient, log: &AuditLog, records: &[AuditRecord], head: Option<SqlRow>) {
  db.script_query(log.rows_sql(), records.iter().map(row_of).collect());
  let head = head.or_else(|| records.last().map(|r| head_row(r.seq, &r.hash)));
  db.script_query(log.head_sql(), head.into_iter().collect());
}

fn verify(db: &TestSqlClient, log: &AuditLog) -> ChainReport {
  run(log.verify_chain_on(Arc::new(db.clone()), None)).unwrap()
}

#[test]
fn context_and_entry_accessors() {
  let ctx = AuditContext::new("ada");
  assert_eq!((ctx.actor(), ctx.tenant()), ("ada", None));
  let ctx = AuditContext::for_tenant("bob", "acme");
  assert_eq!((ctx.actor(), ctx.tenant()), ("bob", Some("acme")));
  let entry = AuditEntry::new("a", "e")
    .with_before(json!(1))
    .with_after(json!(2));
  assert_eq!((entry.action(), entry.entity()), ("a", "e"));
  assert_eq!(
    (entry.before(), entry.after()),
    (Some(&json!(1)), Some(&json!(2)))
  );
  assert_eq!(AuditLog::default(), AuditLog::new());
  assert_eq!(AuditLog::new().table(), DEFAULT_TABLE);
}

#[test]
fn canonical_form_sorts_keys_and_is_compact() {
  let r = &chain(Some("acme"), 2)[1];
  assert_eq!(
    r.canonical(),
    r#"{"action":"invoice.update","actor":"ada","after":{"n":2,"tags":["a",{"b":2.5,"z":1}]},"at":1700000000000002,"before":{"n":1},"entity":"invoice:2","seq":2,"tenant":"acme"}"#
  );
  assert_eq!(r.chain(), "acme");
  let mut out = String::new();
  write_canonical(
    &json!({ "z": [-0.0, 0.0, 1.0, -1, "q\"\n"], "a": null, "m": true }),
    &mut out,
  );
  assert_eq!(out, r#"{"a":null,"m":true,"z":[0.0,0.0,1.0,-1,"q\"\n"]}"#);
  assert_eq!(hex(&[0, 0xab, 0x10]), "00ab10");
  assert_eq!(r.computed_hash().len(), 32);
}

#[test]
fn table_names_are_validated() {
  for ok in ["audit", "app.audit_2", "_x", &"a".repeat(58)] {
    assert!(valid_table(ok), "{ok}");
    assert_eq!(AuditLog::new().with_table(ok).table(), ok);
  }
  for bad in [
    "",
    "1a",
    "a b",
    "a.b.c",
    ".a",
    "a.",
    "x;drop",
    &"a".repeat(59),
    &format!("{}.a", "s".repeat(64)),
  ] {
    assert!(!valid_table(bad), "{bad}");
  }
  let log = AuditLog::new().with_table("app.events");
  assert_eq!(log.head_table(), "app.events_head");
  assert!(log.insert_sql().starts_with("INSERT INTO app.events ("));
}

#[test]
#[should_panic(expected = "invalid audit table name")]
fn an_invalid_table_name_panics() {
  let _ = AuditLog::new().with_table("x; DROP TABLE y");
}

fn script_append(db: &TestSqlClient, log: &AuditLog, head: Option<SqlRow>, id: Option<SqlRow>) {
  db.script_query(log.lock_sql(), head.into_iter().collect());
  db.script_query(log.insert_sql(), id.into_iter().collect());
}

fn locked(seq: i64, hash: &[u8], micros: i64) -> SqlRow {
  SqlRow::from_pairs([
    ("seq", SqlValue::I64(seq)),
    ("hash", SqlValue::Bytes(hash.to_vec())),
    ("at", SqlValue::Timestamp(at(micros))),
  ])
}

#[test]
fn append_links_to_the_locked_head_and_moves_it() {
  let (db, log) = (connected(), AuditLog::new());
  let prev = chain(Some("acme"), 1).remove(0);
  script_append(
    &db,
    &log,
    Some(locked(1, &prev.hash, 5)),
    Some(SqlRow::from_pairs([("id", 77i64)])),
  );
  let ctx = AuditContext::for_tenant("ada", "acme");
  let entry = AuditEntry::new("invoice.delete", "invoice:1").with_before(json!({ "n": 1 }));
  let (log2, ctx2) = (log.clone(), ctx.clone());
  let rec = run(with_transaction(Arc::new(db.clone()), move |tx| {
    log2.append(&tx, &ctx2, entry)
  }))
  .unwrap();
  assert_eq!((rec.id, rec.seq, rec.at), (77, 2, at(5)));
  assert_eq!(rec.prev_hash, prev.hash);
  assert_eq!(rec.hash, rec.computed_hash());
  assert_eq!(
    (rec.after.as_ref(), rec.tenant.as_deref()),
    (None, Some("acme"))
  );

  let queries = db.recorded_queries();
  assert_eq!(queries[0].0, log.lock_sql());
  assert_eq!(
    queries[0].1,
    [SqlValue::Text("acme".into()), SqlValue::Bytes(vec![0; 32])]
  );
  assert_eq!(queries[1].1, insert_params("acme", &rec));
  assert_eq!(queries[1].1[7], SqlValue::Null, "no `after`");
  assert_eq!(
    db.recorded_executes(),
    [(
      log.move_sql(),
      vec![
        SqlValue::Text("acme".into()),
        SqlValue::I64(2),
        SqlValue::Bytes(rec.hash.clone())
      ]
    )]
  );
}

#[test]
fn append_reports_missing_and_malformed_rows() {
  let ctx = AuditContext::new("ada");
  let attempt = |head: Option<SqlRow>, id: Option<SqlRow>| {
    let (db, log, ctx) = (connected(), AuditLog::new(), ctx.clone());
    script_append(&db, &log, head, id);
    let err = run(with_transaction(Arc::new(db.clone()), move |tx| {
      log.append(&tx, &ctx, AuditEntry::new("a", "e"))
    }))
    .unwrap_err();
    assert_eq!((db.commits(), db.rollbacks()), (0, 1));
    err
  };
  let head = || Some(locked(0, &GENESIS_HASH, 1));
  assert_eq!(attempt(None, None), SqlError::RowNotFound);
  assert_eq!(attempt(head(), None), SqlError::RowNotFound);
  let no_id = Some(SqlRow::from_pairs([("other", 1i64)]));
  assert!(matches!(
    attempt(head(), no_id),
    SqlError::ColumnNotFound { .. }
  ));
  let bad_head = Some(SqlRow::from_pairs([("seq", "x")]));
  assert!(matches!(
    attempt(bad_head, None),
    SqlError::TypeMismatch { .. }
  ));
}

#[test]
fn an_intact_chain_verifies() {
  let (db, log) = (connected(), AuditLog::new());
  let records = chain(None, 5);
  script(&db, &log, &records, None);
  let report = verify(&db, &log);
  assert!(report.is_intact(), "{report:?}");
  assert_eq!(report.records, 5);
  assert_eq!(report.head, Some(hex(&records[4].hash)));
  assert_eq!(report.tenant, None);
  assert_eq!(db.open_streams(), 0);

  // An empty chain, and records without a head row (all verified, no end check).
  let db = connected();
  script(&db, &log, &[], None);
  let report = verify(&db, &log);
  assert_eq!(
    (report.records, report.head, report.first_broken),
    (0, None, None)
  );
  db.script_query(log.rows_sql(), records.iter().map(row_of).collect());
  assert_eq!(verify(&db, &log).records, 5);
}

#[test]
fn records_after_the_head_are_not_verified() {
  let (db, log) = (connected(), AuditLog::new());
  let mut records = chain(None, 4);
  records[3].hash = vec![9; 32]; // appended after the head was read: not checked
  script(&db, &log, &records, Some(head_row(3, &records[2].hash)));
  let report = verify(&db, &log);
  assert!(report.is_intact(), "{report:?}");
  assert_eq!(report.records, 3);
}

fn first_broken(records: &[AuditRecord], head: Option<SqlRow>) -> (ChainReport, BrokenLink) {
  let (db, log) = (connected(), AuditLog::new());
  script(&db, &log, records, head);
  let report = verify(&db, &log);
  let broken = report.first_broken.clone().expect("a broken link");
  (report, broken)
}

#[test]
fn an_edited_record_breaks_its_hash() {
  let mut records = chain(None, 4);
  records[1].actor = "mallory".into();
  let (report, b) = first_broken(&records, None);
  assert_eq!((b.id, b.seq, b.kind), (Some(102), 2, BreakKind::Hash));
  assert_eq!(b.expected, hex(&records[1].computed_hash()));
  assert_eq!(b.actual, hex(&records[1].hash));
  assert_eq!(report.records, 1);
  assert_eq!(report.head, Some(hex(&records[0].hash)));
  assert!(!report.is_intact());
  assert!(
    b.to_string()
      .starts_with("record 102 (seq 2): hash mismatch: expected ")
  );
}

#[test]
fn a_removed_record_breaks_the_sequence() {
  let mut records = chain(None, 4);
  records.remove(1);
  let (_, b) = first_broken(&records, None);
  assert_eq!((b.seq, b.kind), (3, BreakKind::Sequence));
  assert_eq!((b.expected.as_str(), b.actual.as_str()), ("2", "3"));
}

#[test]
fn a_relinked_record_breaks_prev_hash() {
  let mut records = chain(None, 3);
  records[2].prev_hash = vec![7; 32];
  records[2].hash = records[2].computed_hash();
  let (_, b) = first_broken(&records, None);
  assert_eq!((b.seq, b.kind), (3, BreakKind::PrevHash));
  assert_eq!(b.expected, hex(&records[1].hash));
  assert_eq!(b.actual, "07".repeat(32));
  assert_eq!(BreakKind::PrevHash.to_string(), "prev_hash mismatch");
  assert_eq!(BreakKind::Sequence.to_string(), "sequence gap");
}

#[test]
fn a_truncated_chain_does_not_reach_its_head() {
  let records = chain(None, 3);
  let head = Some(head_row(3, &records[2].hash));
  let (report, b) = first_broken(&records[..2], head.clone());
  assert_eq!((b.id, b.seq, b.kind), (None, 3, BreakKind::Head));
  assert_eq!(b.expected, format!("3:{}", hex(&records[2].hash)));
  assert_eq!(b.actual, format!("2:{}", hex(&records[1].hash)));
  assert_eq!(report.records, 2);
  assert!(
    b.to_string()
      .starts_with("head (seq 3): chain does not reach its head")
  );
  // Every record removed.
  let (report, b) = first_broken(&[], head);
  assert_eq!(b.actual, format!("0:{}", hex(&GENESIS_HASH)));
  assert_eq!((report.records, report.head), (0, None));
  // Same length, but the head row was changed.
  let (_, b) = first_broken(&records, Some(head_row(3, &[1; 32])));
  assert_eq!(b.kind, BreakKind::Head);
}

#[test]
fn verification_stops_at_the_first_break() {
  let mut records = chain(None, 4);
  records[0].entity = "edited".into();
  records[2].entity = "edited too".into();
  let (report, b) = first_broken(&records, None);
  assert_eq!((b.seq, report.records), (1, 0));
}

#[test]
fn undecodable_rows_and_heads_fail_the_verification() {
  let (db, log) = (connected(), AuditLog::new());
  db.script_query(log.head_sql(), vec![SqlRow::from_pairs([("seq", "x")])]);
  let err = run(log.verify_chain_on(Arc::new(db.clone()), None)).unwrap_err();
  assert!(matches!(err, SqlError::TypeMismatch { .. }), "{err:?}");

  let db = connected();
  let mut rows: Vec<SqlRow> = chain(None, 2).iter().map(row_of).collect();
  rows[1] = SqlRow::from_pairs([("id", 1i64)]);
  db.script_query(log.rows_sql(), rows);
  let err = run(log.verify_chain_on(Arc::new(db.clone()), None)).unwrap_err();
  assert!(matches!(err, SqlError::ColumnNotFound { .. }), "{err:?}");
}

type Env = HCons<AuditLog, HCons<AuditContext, SqlTransactionEnv>>;

#[test]
fn environment_functions_read_their_capabilities() {
  let (db, log) = (connected(), AuditLog::new());
  script_append(
    &db,
    &log,
    Some(locked(0, &GENESIS_HASH, 9)),
    Some(SqlRow::from_pairs([("id", 1i64)])),
  );
  // `record`: log, context and transaction from the environment.
  let ctx = AuditContext::for_tenant("ada", "t1");
  let (log2, ctx2) = (log.clone(), ctx.clone());
  let rec = run(with_transaction(Arc::new(db.clone()), move |tx| {
    let program: Effect<AuditRecord, SqlError, Env> = record(AuditEntry::new("a", "e"));
    program.provide_all(hlist![log2, ctx2, SqlTransactionService(tx)])
  }))
  .unwrap();
  assert_eq!((rec.seq, rec.tenant.as_deref()), (1, Some("t1")));
  assert_eq!(db.commits(), 1);

  // `append`: the log from the environment (provided by its layer).
  let db2 = Arc::new(db.clone()) as Arc<dyn SqlClient>;
  let program = with_transaction(db2, move |tx| {
    append::<AuditLogEnv, _>(&tx, &ctx, AuditEntry::new("b", "e"))
      .provide_layer(AuditLog::new().layer::<SqlError>())
  });
  assert_eq!(run(program).unwrap().action, "b");

  // `verify_chain`: the log and the client from the environment.
  let records = chain(Some("t1"), 2);
  db.script_query(log.rows_sql(), records.iter().map(row_of).collect());
  let program: Effect<ChainReport, SqlError, HCons<AuditLog, SqlClientEnv>> =
    verify_chain(Some("t1"));
  let report = run(program.provide_all(HCons(Arc::new(log), sql_client_env(db.clone())))).unwrap();
  assert_eq!((report.records, report.tenant.as_deref()), (2, Some("t1")));
}

#[cfg(feature = "migrate")]
#[test]
fn the_bundled_migration_is_reversible_and_tracked_separately() {
  let m = migrator();
  assert_eq!(m.table(), MIGRATIONS_TABLE);
  assert_eq!(m.migrations().len(), 1);
  let up = &m.migrations()[0];
  assert_eq!((up.version(), up.description()), (1, "audit log"));
  assert!(up.is_reversible());
  assert!(
    up.sql()
      .contains("CREATE TABLE IF NOT EXISTS audit_log_head")
  );
}
