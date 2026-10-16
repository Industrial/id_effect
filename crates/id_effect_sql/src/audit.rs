//! **Audit log** (feature `audit`): who changed what, appended inside the caller's
//! transaction and hash-chained so that edits to stored records are detectable.
//!
//! - [`AuditLog`] is the capability (its [`layer`](AuditLog::layer) provides it).
//!   [`AuditLog::append`] / [`append`](fn@append) / [`record`] insert one [`AuditRecord`]
//!   `{actor, tenant, action, entity, before, after, at}` through the caller's
//!   [`SqlTransaction`], so the record commits or rolls back with the change it describes.
//! - The actor and tenant are an [`AuditContext`] value: explicit, or a capability of the
//!   environment (set per request, e.g. from the authenticated principal). This crate does not
//!   depend on any auth crate.
//! - Every tenant has its own chain (records without a tenant share the chain `""`). Record `n`
//!   stores `hash = sha256(prev_hash ‖ canonical(record))` with `prev_hash` the hash of record
//!   `n - 1` ([`GENESIS_HASH`] for the first), and the chain's head row holds the last
//!   `(seq, hash)`.
//! - [`AuditLog::verify_chain_on`] / [`verify_chain`] stream a chain in order and report the
//!   first broken link ([`BrokenLink`]: the record, what was expected and what is stored).
//!
//! # Concurrency
//!
//! An append first upserts the chain's head row (`INSERT … ON CONFLICT DO UPDATE … RETURNING`),
//! which row-locks it until the caller's transaction ends, then reads the previous `(seq, hash)`
//! from it, inserts the record and moves the head. Concurrent appends to one chain therefore
//! queue on that row lock and each links to the committed head before it: the chain never forks
//! (`UNIQUE (chain, seq)` backs this up). Appends to different tenants do not wait for each
//! other. A rolled-back transaction releases the lock and leaves the head untouched.
//!
//! Append in `READ COMMITTED` transactions (the PostgreSQL default). Under `REPEATABLE READ` or
//! `SERIALIZABLE`, an append racing another one fails with a serialization failure (SQLSTATE
//! `40001`) instead of waiting; rerun it with
//! [`retry_serialization_failures`](crate::retry_serialization_failures).
//!
//! # Canonical form
//!
//! The hashed bytes are `prev_hash` (32 bytes) followed by the record as JSON with object keys
//! sorted at every level and no whitespace: `{"action","actor","after","at","before","entity",
//! "seq","tenant"}`, where `at` is microseconds since the Unix epoch (what `TIMESTAMPTZ` stores)
//! and a missing `before` / `after` / `tenant` is `null`. `at` is the database's `now()` (the
//! transaction's start time). The record id is not hashed (the database assigns it).
//!
//! # What the chain detects
//!
//! An edited record (its hash no longer matches), a removed or inserted record (the next
//! record's `prev_hash` or `seq` no longer follows), and records removed from the end (the chain
//! no longer reaches its head row). Someone who can rewrite the whole chain and its head can
//! hide that: anchor [`ChainReport::head`] outside the database (a log line, a signed
//! checkpoint) to detect it.
//!
//! # Schema
//!
//! [`MIGRATIONS`] holds the PostgreSQL migration (`migrations/audit/` in this crate, tables
//! `audit_log` and `audit_log_head`); with feature `migrate`, `migrator()` runs it with its own
//! tracking table. [`AuditLog::with_table`] uses another table (create it from a copy of the
//! migration).
//!
//! ```rust,no_run
//! use std::sync::Arc;
//!
//! use id_effect::Effect;
//! use id_effect_sql::audit::{AuditContext, AuditEntry, AuditLog};
//! use id_effect_sql::{SqlClient, SqlError, params, with_transaction};
//!
//! /// Renames invoice 7 and records it, in one transaction.
//! fn rename(db: Arc<dyn SqlClient>, log: AuditLog, who: AuditContext) -> Effect<(), SqlError> {
//!   with_transaction(db, move |tx| {
//!     let entry = AuditEntry::new("invoice.rename", "invoice:7")
//!       .with_before(serde_json::json!({ "name": "draft" }))
//!       .with_after(serde_json::json!({ "name": "final" }));
//!     tx.execute("UPDATE invoices SET name = $1 WHERE id = $2", &params!["final", 7i64])
//!       .flat_map(move |_| log.append(&tx, &who, entry))
//!       .map(|_record| ())
//!   })
//! }
//! # let _ = rename;
//! ```

use std::fmt;
use std::sync::Arc;

use id_effect::{Effect, HCons, HList, HNil, Layer, Selector, fail, hlist, need, succeed, suspend};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::client::{SqlClient, SqlClientService};
use crate::error::SqlError;
use crate::row::SqlRow;
use crate::transaction::{SqlTransaction, SqlTransactionService};
use crate::value::SqlValue;

#[cfg(test)]
mod tests;

/// The default audit table (its chain heads are in `audit_log_head`).
pub const DEFAULT_TABLE: &str = "audit_log";

/// The `prev_hash` of a chain's first record.
pub const GENESIS_HASH: [u8; 32] = [0; 32];

/// The schema as `(file name, script)` migration pairs (`0001_audit_log.up.sql` /
/// `.down.sql`), in the naming `migrate::Migrator::from_files` reads.
pub const MIGRATIONS: [(&str, &str); 2] = [
  (
    "0001_audit_log.up.sql",
    include_str!("../migrations/audit/0001_audit_log.up.sql"),
  ),
  (
    "0001_audit_log.down.sql",
    include_str!("../migrations/audit/0001_audit_log.down.sql"),
  ),
];

/// The tracking table of `migrator()` (feature `migrate`), so the audit migration's version
/// never collides with the application's own.
pub const MIGRATIONS_TABLE: &str = "_id_effect_audit_migrations";

/// A [`Migrator`](crate::migrate::Migrator) for [`MIGRATIONS`], tracked in [`MIGRATIONS_TABLE`]:
/// run it next to the application's migrator (`migrator().run_on(client)`).
#[cfg(feature = "migrate")]
pub fn migrator() -> crate::migrate::Migrator {
  crate::migrate::Migrator::from_files(MIGRATIONS)
    .expect("the bundled audit migrations are well-formed")
    .with_table(MIGRATIONS_TABLE)
}

/// Who acts, and for which tenant: the `actor` and `tenant` of the records appended with it.
///
/// Pass it explicitly ([`AuditLog::append`]) or provide it as a capability of the environment
/// ([`record`]), e.g. per request from the authenticated principal.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct AuditContext {
  actor: String,
  tenant: Option<String>,
}

impl AuditContext {
  /// `actor` acting without a tenant (records go to the chain `""`).
  pub fn new(actor: impl Into<String>) -> Self {
    Self {
      actor: actor.into(),
      tenant: None,
    }
  }

  /// `actor` acting for `tenant` (records go to the tenant's chain).
  pub fn for_tenant(actor: impl Into<String>, tenant: impl Into<String>) -> Self {
    Self::new(actor).with_tenant(tenant)
  }

  /// The same actor, for `tenant`.
  #[must_use]
  pub fn with_tenant(mut self, tenant: impl Into<String>) -> Self {
    self.tenant = Some(tenant.into());
    self
  }

  /// The actor (a user id, a service name).
  pub fn actor(&self) -> &str {
    &self.actor
  }

  /// The tenant, if any.
  pub fn tenant(&self) -> Option<&str> {
    self.tenant.as_deref()
  }
}

/// What happened: an `action` on an `entity`, with the entity's state `before` and `after` it
/// (JSON; `None` for a creation's `before` or a deletion's `after`).
#[derive(Clone, Debug, PartialEq)]
pub struct AuditEntry {
  action: String,
  entity: String,
  before: Option<Value>,
  after: Option<Value>,
}

impl AuditEntry {
  /// `action` (e.g. `invoice.update`) on `entity` (e.g. `invoice:42`), without states.
  pub fn new(action: impl Into<String>, entity: impl Into<String>) -> Self {
    Self {
      action: action.into(),
      entity: entity.into(),
      before: None,
      after: None,
    }
  }

  /// The entity's state before the action.
  #[must_use]
  pub fn with_before(mut self, before: Value) -> Self {
    self.before = Some(before);
    self
  }

  /// The entity's state after the action.
  #[must_use]
  pub fn with_after(mut self, after: Value) -> Self {
    self.after = Some(after);
    self
  }

  /// The action.
  pub fn action(&self) -> &str {
    &self.action
  }

  /// The entity.
  pub fn entity(&self) -> &str {
    &self.entity
  }

  /// The state before, if recorded.
  pub fn before(&self) -> Option<&Value> {
    self.before.as_ref()
  }

  /// The state after, if recorded.
  pub fn after(&self) -> Option<&Value> {
    self.after.as_ref()
  }
}

/// One stored audit record (a row of the audit table).
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub struct AuditRecord {
  /// Database id (not hashed).
  pub id: i64,
  /// Position in the tenant's chain, from 1.
  pub seq: i64,
  /// [`AuditContext::actor`].
  pub actor: String,
  /// [`AuditContext::tenant`].
  pub tenant: Option<String>,
  /// [`AuditEntry::action`].
  pub action: String,
  /// [`AuditEntry::entity`].
  pub entity: String,
  /// [`AuditEntry::before`].
  pub before: Option<Value>,
  /// [`AuditEntry::after`].
  pub after: Option<Value>,
  /// When (the database's `now()` in the appending transaction).
  pub at: jiff::Timestamp,
  /// The previous record's hash ([`GENESIS_HASH`] for the first).
  pub prev_hash: Vec<u8>,
  /// `sha256(prev_hash ‖ canonical(record))`.
  pub hash: Vec<u8>,
}

impl AuditRecord {
  /// The record's chain: its tenant, or `""`.
  pub fn chain(&self) -> &str {
    chain_of(self.tenant.as_deref())
  }

  /// The hashed form of the record (see the [module docs](self)), without `prev_hash`.
  pub fn canonical(&self) -> String {
    let at = self.at.as_microsecond();
    let record = serde_json::json!({
      "action": self.action,
      "actor": self.actor,
      "after": self.after,
      "at": at,
      "before": self.before,
      "entity": self.entity,
      "seq": self.seq,
      "tenant": self.tenant,
    });
    let mut out = String::new();
    write_canonical(&record, &mut out);
    out
  }

  /// `sha256(prev_hash ‖ canonical())`: what [`hash`](Self::hash) must be.
  pub fn computed_hash(&self) -> Vec<u8> {
    Sha256::new()
      .chain_update(&self.prev_hash)
      .chain_update(self.canonical().as_bytes())
      .finalize()
      .to_vec()
  }

  /// Decodes a row with the audit table's columns.
  pub fn from_row(row: &SqlRow) -> Result<Self, SqlError> {
    Ok(Self {
      id: row.get("id")?,
      seq: row.get("seq")?,
      actor: row.get("actor")?,
      tenant: row.get("tenant")?,
      action: row.get("action")?,
      entity: row.get("entity")?,
      before: row.get("before")?,
      after: row.get("after")?,
      at: row.get("at")?,
      prev_hash: row.get("prev_hash")?,
      hash: row.get("hash")?,
    })
  }
}

/// How a chain is broken ([`BrokenLink::kind`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum BreakKind {
  /// The record's `seq` does not follow the previous record's (a record was removed or
  /// inserted). `expected` / `actual` are sequence numbers.
  Sequence,
  /// The record's `prev_hash` is not the previous record's hash. `expected` / `actual` are hex.
  PrevHash,
  /// The record's stored hash is not `sha256(prev_hash ‖ record)`: the record was edited.
  /// `expected` (recomputed) / `actual` (stored) are hex.
  Hash,
  /// The chain does not end at its head row (records were removed from the end, or the head
  /// was changed). `expected` (head) / `actual` (last record) are `seq:hex`.
  Head,
}

impl fmt::Display for BreakKind {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.write_str(match self {
      BreakKind::Sequence => "sequence gap",
      BreakKind::PrevHash => "prev_hash mismatch",
      BreakKind::Hash => "hash mismatch",
      BreakKind::Head => "chain does not reach its head",
    })
  }
}

/// The first broken link of a chain.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct BrokenLink {
  /// The offending record's id (`None` for [`BreakKind::Head`]).
  pub id: Option<i64>,
  /// The offending record's `seq` (for [`BreakKind::Head`], the head's).
  pub seq: i64,
  /// What is wrong.
  pub kind: BreakKind,
  /// The value the chain requires (format per [`BreakKind`]).
  pub expected: String,
  /// The value stored.
  pub actual: String,
}

impl fmt::Display for BrokenLink {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    match self.id {
      Some(id) => write!(f, "record {id} (seq {})", self.seq)?,
      None => write!(f, "head (seq {})", self.seq)?,
    }
    write!(
      f,
      ": {}: expected {}, found {}",
      self.kind, self.expected, self.actual
    )
  }
}

/// The outcome of verifying one chain.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct ChainReport {
  /// The verified tenant.
  pub tenant: Option<String>,
  /// How many records verified intact (from the start of the chain).
  pub records: u64,
  /// The hash (hex) of the last record verified intact; `None` when there is none.
  pub head: Option<String>,
  /// The first broken link, if any.
  pub first_broken: Option<BrokenLink>,
}

impl ChainReport {
  /// Whether no link is broken.
  pub fn is_intact(&self) -> bool {
    self.first_broken.is_none()
  }
}

/// Capability: where audit records go ([`DEFAULT_TABLE`] unless [`with_table`](Self::with_table)).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct AuditLog {
  table: Arc<str>,
}

/// The minimal typed environment holding only [`AuditLog`].
pub type AuditLogEnv = HCons<AuditLog, HNil>;

impl Default for AuditLog {
  fn default() -> Self {
    Self::new()
  }
}

impl AuditLog {
  /// The audit log in [`DEFAULT_TABLE`].
  pub fn new() -> Self {
    Self {
      table: Arc::from(DEFAULT_TABLE),
    }
  }

  /// The audit log in `table` (`name` or `schema.name`; chain heads in `<table>_head`), with the
  /// columns of the bundled migration.
  ///
  /// # Panics
  ///
  /// When `table` is not one or two `.`-separated identifiers of ASCII letters, digits and `_`
  /// (not starting with a digit), the last at most 58 bytes: it is spliced into SQL.
  #[must_use]
  pub fn with_table(mut self, table: impl Into<String>) -> Self {
    let table = table.into();
    assert!(valid_table(&table), "invalid audit table name `{table}`");
    self.table = table.into();
    self
  }

  /// The audit table.
  pub fn table(&self) -> &str {
    &self.table
  }

  /// A [`Layer`] providing this log as a capability.
  pub fn layer<E: Send + 'static>(self) -> Layer<AuditLogEnv, E> {
    Layer::succeed(hlist![self])
  }

  /// Appends `entry` by `ctx` to `ctx`'s chain through `tx`: the record commits or rolls back
  /// with `tx`. Holds the chain's head row lock until `tx` ends (see the
  /// [module docs](self#concurrency)).
  pub fn append(
    &self,
    tx: &Arc<dyn SqlTransaction>,
    ctx: &AuditContext,
    entry: AuditEntry,
  ) -> Effect<AuditRecord, SqlError> {
    let (tx, ctx) = (Arc::clone(tx), ctx.clone());
    let (lock_sql, insert_sql, move_sql) = (self.lock_sql(), self.insert_sql(), self.move_sql());
    suspend(move || {
      let chain = chain_of(ctx.tenant()).to_owned();
      let lock = tx.query(&lock_sql, &[chain.clone().into(), GENESIS_HASH[..].into()]);
      lock.flat_map(move |rows| {
        let (seq, prev_hash, at) = match locked_head(&rows) {
          Ok(head) => head,
          Err(e) => return fail(e),
        };
        let mut record = AuditRecord {
          id: 0,
          seq: seq + 1,
          actor: ctx.actor,
          tenant: ctx.tenant,
          action: entry.action,
          entity: entry.entity,
          before: entry.before,
          after: entry.after,
          at,
          prev_hash,
          hash: Vec::new(),
        };
        record.hash = record.computed_hash();
        let insert = tx.query(&insert_sql, &insert_params(&chain, &record));
        insert.flat_map(move |rows| {
          match rows.first().map(|r| r.get::<i64>("id")) {
            Some(Ok(id)) => record.id = id,
            Some(Err(e)) => return fail(e),
            None => return fail(SqlError::RowNotFound),
          }
          let params = [chain.into(), record.seq.into(), record.hash.clone().into()];
          tx.execute(&move_sql, &params).map(move |_| record)
        })
      })
    })
  }

  /// Verifies `tenant`'s chain (`None`: the chain without a tenant) on `client`, streaming it in
  /// `seq` order and stopping at the first broken link. Records appended after the head row is
  /// read are not verified.
  pub fn verify_chain_on(
    &self,
    client: Arc<dyn SqlClient>,
    tenant: Option<&str>,
  ) -> Effect<ChainReport, SqlError> {
    let tenant = tenant.map(str::to_owned);
    let (head_sql, rows_sql) = (self.head_sql(), self.rows_sql());
    suspend(move || {
      let chain = chain_of(tenant.as_deref()).to_owned();
      let head = client.query(&head_sql, &[chain.clone().into()]);
      head.flat_map(move |rows| {
        let head = match rows.first().map(stored_head).transpose() {
          Ok(head) => head,
          Err(e) => return fail(e),
        };
        let limit = head.as_ref().map(|(seq, _)| *seq);
        let mut verifier = Verifier::default();
        let mut ended = false;
        client
          .query_stream(&rows_sql, &[chain.into()])
          .map_serial(|row| AuditRecord::from_row(&row))
          .take_while(move |r| match (r, limit) {
            (Ok(r), Some(last)) => r.seq <= last,
            _ => true,
          })
          .map_serial(move |r| r.map(|record| verifier.check(&record)))
          .take_while(move |checked| {
            let take = !ended;
            ended = !matches!(checked, Ok(Checked { broken: None, .. }));
            take
          })
          .run_fold_effect(Fold::default(), |mut acc, checked| match checked {
            Err(e) => fail(e),
            Ok(Checked {
              broken: Some(b), ..
            }) => {
              acc.broken = Some(b);
              succeed(acc)
            }
            Ok(Checked { seq, hash, .. }) => {
              acc.records += 1;
              acc.last = Some((seq, hash));
              succeed(acc)
            }
          })
          .map(move |acc| acc.report(tenant, head))
      })
    })
  }

  /// `INSERT … ON CONFLICT DO UPDATE` on the head row: creates it at seq 0 for a new chain, and
  /// in either case row-locks it and returns the last `(seq, hash)` and `now()`.
  fn lock_sql(&self) -> String {
    format!(
      "INSERT INTO {head} AS h (chain, seq, hash) VALUES ($1, 0, $2) \
       ON CONFLICT (chain) DO UPDATE SET seq = h.seq RETURNING h.seq, h.hash, now() AS at",
      head = self.head_table()
    )
  }

  fn insert_sql(&self) -> String {
    format!(
      "INSERT INTO {} (chain, seq, actor, tenant, action, entity, before, after, at, prev_hash, \
       hash) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11) RETURNING id",
      self.table
    )
  }

  fn move_sql(&self) -> String {
    format!(
      "UPDATE {} SET seq = $2, hash = $3 WHERE chain = $1",
      self.head_table()
    )
  }

  fn head_sql(&self) -> String {
    format!(
      "SELECT seq, hash FROM {} WHERE chain = $1",
      self.head_table()
    )
  }

  fn rows_sql(&self) -> String {
    format!(
      "SELECT id, seq, actor, tenant, action, entity, before, after, at, prev_hash, hash \
       FROM {} WHERE chain = $1 ORDER BY seq",
      self.table
    )
  }

  fn head_table(&self) -> String {
    format!("{}_head", self.table)
  }
}

/// [`AuditLog::append`] with the [`AuditLog`] of the environment.
pub fn append<R, I>(
  tx: &Arc<dyn SqlTransaction>,
  ctx: &AuditContext,
  entry: AuditEntry,
) -> Effect<AuditRecord, SqlError, R>
where
  R: Selector<AuditLog, I> + HList,
{
  let (tx, ctx) = (Arc::clone(tx), ctx.clone());
  need::<AuditLog>().flat_map(move |log| log.append(&tx, &ctx, entry).narrow())
}

/// [`AuditLog::append`] with the [`AuditLog`], the [`AuditContext`] and the transaction
/// ([`transaction_layer`](crate::transaction_layer)) of the environment.
pub fn record<R, I, J, K>(entry: AuditEntry) -> Effect<AuditRecord, SqlError, R>
where
  R: Selector<AuditLog, I> + Selector<AuditContext, J> + Selector<SqlTransactionService, K> + HList,
{
  need::<SqlTransactionService>().flat_map::<_, _, R, K>(move |tx| {
    need::<AuditContext>().flat_map::<_, _, R, J>(move |ctx| append::<R, I>(&tx.0, &ctx, entry))
  })
}

/// [`AuditLog::verify_chain_on`] with the [`AuditLog`] and the [`SqlClientService`] of the
/// environment.
pub fn verify_chain<R, I, J>(tenant: Option<&str>) -> Effect<ChainReport, SqlError, R>
where
  R: Selector<AuditLog, I> + Selector<SqlClientService, J> + HList,
{
  let tenant = tenant.map(str::to_owned);
  need::<AuditLog>().flat_map::<_, _, R, I>(move |log| {
    need::<SqlClientService>().flat_map::<_, _, R, J>(move |client| {
      log
        .verify_chain_on(Arc::clone(&client.0), tenant.as_deref())
        .narrow()
    })
  })
}

/// The chain of `tenant`.
fn chain_of(tenant: Option<&str>) -> &str {
  tenant.unwrap_or("")
}

/// `(seq, hash, at)` of the locked head row.
fn locked_head(rows: &[SqlRow]) -> Result<(i64, Vec<u8>, jiff::Timestamp), SqlError> {
  let row = rows.first().ok_or(SqlError::RowNotFound)?;
  Ok((row.get("seq")?, row.get("hash")?, row.get("at")?))
}

/// `(seq, hash)` of a head row read for verification.
fn stored_head(row: &SqlRow) -> Result<(i64, Vec<u8>), SqlError> {
  Ok((row.get("seq")?, row.get("hash")?))
}

fn insert_params(chain: &str, r: &AuditRecord) -> Vec<SqlValue> {
  vec![
    chain.into(),
    r.seq.into(),
    r.actor.as_str().into(),
    r.tenant.clone().into(),
    r.action.as_str().into(),
    r.entity.as_str().into(),
    r.before.clone().into(),
    r.after.clone().into(),
    r.at.into(),
    r.prev_hash.clone().into(),
    r.hash.clone().into(),
  ]
}

/// One record checked against the chain so far.
struct Checked {
  seq: i64,
  hash: Vec<u8>,
  broken: Option<BrokenLink>,
}

/// The chain so far: the last `seq` and hash (0 and [`GENESIS_HASH`] before the first record).
struct Verifier {
  seq: i64,
  hash: Vec<u8>,
}

impl Default for Verifier {
  fn default() -> Self {
    Self {
      seq: 0,
      hash: GENESIS_HASH.to_vec(),
    }
  }
}

impl Verifier {
  fn check(&mut self, r: &AuditRecord) -> Checked {
    let link = |kind, expected: String, actual: String| BrokenLink {
      id: Some(r.id),
      seq: r.seq,
      kind,
      expected,
      actual,
    };
    let broken = if r.seq != self.seq + 1 {
      Some(link(
        BreakKind::Sequence,
        (self.seq + 1).to_string(),
        r.seq.to_string(),
      ))
    } else if r.prev_hash != self.hash {
      Some(link(
        BreakKind::PrevHash,
        hex(&self.hash),
        hex(&r.prev_hash),
      ))
    } else {
      let computed = r.computed_hash();
      (computed != r.hash).then(|| link(BreakKind::Hash, hex(&computed), hex(&r.hash)))
    };
    self.seq = r.seq;
    self.hash.clone_from(&r.hash);
    Checked {
      seq: r.seq,
      hash: r.hash.clone(),
      broken,
    }
  }
}

/// What [`AuditLog::verify_chain_on`] accumulates.
#[derive(Default)]
struct Fold {
  records: u64,
  last: Option<(i64, Vec<u8>)>,
  broken: Option<BrokenLink>,
}

impl Fold {
  /// The report, checking that an intact chain ends at its head row.
  fn report(self, tenant: Option<String>, head: Option<(i64, Vec<u8>)>) -> ChainReport {
    let broken = self.broken.or_else(|| {
      let (head_seq, head_hash) = head?;
      let (seq, hash) = self.last.clone().unwrap_or((0, GENESIS_HASH.to_vec()));
      (seq != head_seq || hash != head_hash).then(|| BrokenLink {
        id: None,
        seq: head_seq,
        kind: BreakKind::Head,
        expected: format!("{head_seq}:{}", hex(&head_hash)),
        actual: format!("{seq}:{}", hex(&hash)),
      })
    });
    ChainReport {
      tenant,
      records: self.records,
      head: self.last.map(|(_, hash)| hex(&hash)),
      first_broken: broken,
    }
  }
}

/// Writes `v` as JSON with every object's keys sorted (byte order) and no whitespace. A float
/// zero is written `0.0` (PostgreSQL `jsonb` has no negative zero).
fn write_canonical(v: &Value, out: &mut String) {
  match v {
    Value::Array(items) => {
      out.push('[');
      for (i, item) in items.iter().enumerate() {
        if i > 0 {
          out.push(',');
        }
        write_canonical(item, out);
      }
      out.push(']');
    }
    Value::Object(map) => {
      let mut entries: Vec<_> = map.iter().collect();
      entries.sort_unstable_by(|a, b| a.0.cmp(b.0));
      out.push('{');
      for (i, (key, value)) in entries.into_iter().enumerate() {
        if i > 0 {
          out.push(',');
        }
        out.push_str(&Value::from(key.as_str()).to_string());
        out.push(':');
        write_canonical(value, out);
      }
      out.push('}');
    }
    Value::Number(n) if !n.is_i64() && !n.is_u64() && n.as_f64() == Some(0.0) => {
      out.push_str("0.0");
    }
    scalar => out.push_str(&scalar.to_string()),
  }
}

/// Lowercase hex.
fn hex(bytes: &[u8]) -> String {
  use std::fmt::Write as _;
  bytes.iter().fold(String::new(), |mut s, b| {
    let _ = write!(s, "{b:02x}");
    s
  })
}

/// `name` or `schema.name` of identifiers, the name short enough for its `_head` table.
fn valid_table(name: &str) -> bool {
  let ident = |p: &str| {
    let mut chars = p.chars();
    chars
      .next()
      .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
      && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
      && p.len() <= 63
  };
  let mut parts = name.rsplit('.');
  let last = parts.next().unwrap_or_default();
  let schema = parts.next();
  ident(last) && last.len() <= 58 && schema.is_none_or(ident) && parts.next().is_none()
}
