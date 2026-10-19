//! Idempotency keys of the RPC mutations in PostgreSQL ([`PgIdempotencyStore`]), the Ledger's
//! pattern: a `Relay.send` retried with the same `Idempotency-Key` posts once and gets the first
//! result back, on whichever node the retry lands (ADR 0014 D14). The RPC server's default store
//! keeps keys in one process, which three nodes cannot share.
//!
//! - **begin** reserves the key (`INSERT … ON CONFLICT DO NOTHING`); an existing row answers the
//!   stored result (same payload digest), `Mismatch` (another payload) or `InProgress` (no
//!   result yet). The caller's keys older than [`KEY_TTL_HOURS`] are deleted first.
//! - **complete** stores the result; **abandon** deletes a reservation without one.
//!
//! When the database is away `begin` answers `InProgress` (a `409`, the client retries) rather
//! than run the mutation unguarded.

use std::sync::Arc;

use id_effect::{Effect, Exit, succeed};
use id_effect_kit::axum::EffectExecutor;
use id_effect_kit::rpc::idempotency::{
  Begin, IdempotencyKey, IdempotencyScope, IdempotencyStore, StoreFuture, StoredResult,
};
use id_effect_kit::sql::{FromRow, SqlClient, SqlError, SqlValue, params};

/// How long a key answers retries.
pub const KEY_TTL_HOURS: i64 = 24;

const EXPIRE: &str = "DELETE FROM relay_idempotency \
   WHERE scope = $1 AND created_at < now() - make_interval(hours => ($2::int8)::int4)";

const RESERVE: &str = "INSERT INTO relay_idempotency (scope, tag, key, digest) \
   VALUES ($1, $2, $3, $4) ON CONFLICT DO NOTHING RETURNING digest, status, body";

const STORED: &str = "SELECT digest, status, body FROM relay_idempotency \
   WHERE scope = $1 AND tag = $2 AND key = $3";

const COMPLETE: &str = "UPDATE relay_idempotency SET status = $4, body = $5 \
   WHERE scope = $1 AND tag = $2 AND key = $3";

const ABANDON: &str = "DELETE FROM relay_idempotency \
   WHERE scope = $1 AND tag = $2 AND key = $3 AND body IS NULL";

/// Idempotency keys in the `relay_idempotency` table ([`crate::rooms::SCHEMA`]).
#[derive(Clone)]
pub struct PgIdempotencyStore {
  db: Arc<dyn SqlClient>,
}

impl std::fmt::Debug for PgIdempotencyStore {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.debug_struct("PgIdempotencyStore").finish_non_exhaustive()
  }
}

impl PgIdempotencyStore {
  /// Keys in `db`.
  pub fn new(db: Arc<dyn SqlClient>) -> Self {
    Self { db }
  }

  fn rows(&self, sql: &'static str, values: Vec<SqlValue>) -> Effect<Vec<Row>, SqlError> {
    self
      .db
      .query(sql, &values)
      .flat_map(|rows| match rows.iter().map(Row::from_row).collect() {
        Ok(rows) => succeed(rows),
        Err(e) => id_effect::fail(e),
      })
  }

  fn begin_effect(&self, key: &IdempotencyKey, digest: String) -> Effect<Begin, SqlError> {
    let (scope, tag, raw) = (scope_text(&key.scope), key.tag.clone(), key.key.clone());
    let this = self.clone();
    self
      .db
      .execute(EXPIRE, &params![scope.as_str(), KEY_TTL_HOURS])
      .flat_map(move |_| {
        let reserved = this.rows(
          RESERVE,
          params![scope.as_str(), tag.as_str(), raw.as_str(), digest.as_str()],
        );
        reserved.flat_map(move |reserved| {
          if !reserved.is_empty() {
            return succeed(Begin::Fresh);
          }
          this
            .rows(STORED, params![scope.as_str(), tag.as_str(), raw.as_str()])
            // Gone between the two statements (abandoned): the caller retries.
            .map(move |rows| {
              rows
                .into_iter()
                .next()
                .map_or(Begin::InProgress, |row| answer(row, &digest))
            })
        })
      })
  }
}

/// The stored row of a key.
#[derive(FromRow)]
#[from_row(crate = id_effect_kit::sql)]
struct Row {
  digest: String,
  status: Option<i64>,
  body: Option<String>,
}

/// `scope` as stored: `principal:<id>` or `ip:<address>`.
fn scope_text(scope: &IdempotencyScope) -> String {
  match scope {
    IdempotencyScope::Principal(p) => format!("principal:{p}"),
    IdempotencyScope::Anonymous(ip) => format!("ip:{ip}"),
  }
}

/// `digest` in lower-case hex.
fn hex(digest: &[u8; 32]) -> String {
  digest.iter().map(|b| format!("{b:02x}")).collect()
}

/// What the stored `row` answers a call whose payload digest is `digest`.
fn answer(row: Row, digest: &str) -> Begin {
  if row.digest != digest {
    return Begin::Mismatch;
  }
  match (
    row.status.map(u16::try_from),
    row.body,
    crate::config::parse_sha256(&row.digest),
  ) {
    (Some(Ok(status)), Some(body), Some(digest)) => Begin::Replay(StoredResult {
      digest,
      status,
      body: body.into_bytes(),
    }),
    _ => Begin::InProgress,
  }
}

async fn run<A: Send + 'static>(effect: Effect<A, SqlError>) -> Result<A, String> {
  match EffectExecutor::global().run_effect(effect).await {
    Exit::Success(a) => Ok(a),
    Exit::Failure(cause) => Err(format!("{cause:?}")),
  }
}

impl IdempotencyStore for PgIdempotencyStore {
  fn begin<'a>(&'a self, key: &'a IdempotencyKey, digest: [u8; 32]) -> StoreFuture<'a, Begin> {
    let effect = self.begin_effect(key, hex(&digest));
    Box::pin(async move {
      run(effect).await.unwrap_or_else(|why| {
        eprintln!("relay: idempotency key not reserved ({why}); answering in progress");
        Begin::InProgress
      })
    })
  }

  fn complete<'a>(&'a self, key: &'a IdempotencyKey, result: StoredResult) -> StoreFuture<'a, ()> {
    let body = String::from_utf8_lossy(&result.body).into_owned();
    let effect = self.db.execute(
      COMPLETE,
      &params![
        scope_text(&key.scope),
        key.tag.as_str(),
        key.key.as_str(),
        i64::from(result.status),
        body
      ],
    );
    Box::pin(async move {
      if let Err(why) = run(effect).await {
        eprintln!("relay: idempotent result not stored ({why})");
      }
    })
  }

  fn abandon<'a>(&'a self, key: &'a IdempotencyKey) -> StoreFuture<'a, ()> {
    let effect = self.db.execute(
      ABANDON,
      &params![scope_text(&key.scope), key.tag.as_str(), key.key.as_str()],
    );
    Box::pin(async move {
      if let Err(why) = run(effect).await {
        eprintln!("relay: idempotency reservation not released ({why})");
      }
    })
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  fn row(digest: &str, status: Option<i64>, body: Option<&str>) -> Row {
    Row {
      digest: digest.to_owned(),
      status,
      body: body.map(str::to_owned),
    }
  }

  /// With the database away no call runs unguarded: `begin` answers in progress, and storing or
  /// releasing a key only logs.
  #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
  async fn a_database_away_answers_in_progress() {
    let store = PgIdempotencyStore::new(Arc::new(id_effect_kit::sql::TestSqlClient::new()));
    let key = IdempotencyKey {
      scope: IdempotencyScope::Principal("ada".into()),
      tag: "Relay.send@1".into(),
      key: "a-key-of-sixteen-bytes".into(),
    };
    assert_eq!(store.begin(&key, [0; 32]).await, Begin::InProgress);
    let result = StoredResult {
      digest: [0; 32],
      status: 200,
      body: b"{}".to_vec(),
    };
    store.complete(&key, result).await;
    store.abandon(&key).await;
  }

  #[test]
  fn stored_rows_answer_by_digest_and_result() {
    let digest = hex(&[1; 32]);
    assert_eq!(
      answer(row(&hex(&[2; 32]), None, None), &digest),
      Begin::Mismatch
    );
    assert_eq!(answer(row(&digest, None, None), &digest), Begin::InProgress);
    assert_eq!(
      answer(row(&digest, Some(70_000), Some("{}")), &digest),
      Begin::InProgress
    );
    assert_eq!(
      answer(row(&digest, Some(200), Some("{}")), &digest),
      Begin::Replay(StoredResult {
        digest: [1; 32],
        status: 200,
        body: b"{}".to_vec(),
      })
    );
    assert_eq!(
      scope_text(&IdempotencyScope::Anonymous([127, 0, 0, 1].into())),
      "ip:127.0.0.1"
    );
    assert_eq!(
      scope_text(&IdempotencyScope::Principal("ada".into())),
      "principal:ada"
    );
    assert!(
      format!(
        "{:?}",
        PgIdempotencyStore::new(Arc::new(id_effect_kit::sql::TestSqlClient::new()))
      )
      .contains("PgIdempotencyStore")
    );
  }
}
