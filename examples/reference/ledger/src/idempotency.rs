//! Idempotency keys of the RPC mutations in PostgreSQL ([`PgIdempotencyStore`]).
//!
//! A mutation called with `Idempotency-Key: <key>` runs once; a retry with the same key and
//! payload gets the stored result back, on whichever instance it lands (ADR 0014 D14). The RPC
//! server's own store keeps keys in one process, which a ledger behind a load balancer cannot
//! use: a retried posting on another instance would post twice. This one keeps them in the
//! `rpc_idempotency` table (`migrations/0010_rpc_idempotency.up.sql`):
//!
//! - **begin** reserves the key with an `INSERT … ON CONFLICT DO NOTHING`; when the row exists
//!   it answers the stored result (same payload digest), `Mismatch` (another payload) or
//!   `InProgress` (no result yet). Keys of the same caller older than [`KEY_TTL_HOURS`] are
//!   deleted first.
//! - **complete** stores the result; **abandon** deletes a reservation without one.
//!
//! The store cannot fail a call: when the database is away, `begin` answers `InProgress` (the
//! client gets a `409` and retries) rather than run the mutation unguarded.

use std::sync::Arc;

use id_effect::{Effect, Exit};
use id_effect_kit::axum::EffectExecutor;
use id_effect_kit::rpc::idempotency::{
  Begin, IdempotencyKey, IdempotencyScope, IdempotencyStore, StoreFuture, StoredResult,
};
use id_effect_kit::sql::{FromRow, SqlClient, SqlError, params};

use crate::db;

/// How long a key answers retries.
pub const KEY_TTL_HOURS: i64 = 24;

/// Idempotency keys in the `rpc_idempotency` table of the primary ([module docs](self)).
#[derive(Clone)]
pub struct PgIdempotencyStore {
  db: Arc<dyn SqlClient>,
}

impl PgIdempotencyStore {
  /// Keys in `db` (the primary).
  pub fn new(db: Arc<dyn SqlClient>) -> Self {
    Self { db }
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
  match (row.status, row.body) {
    (Some(status), Some(body)) => match (u16::try_from(status), decode_hex(&row.digest)) {
      (Ok(status), Some(digest)) => Begin::Replay(StoredResult {
        digest,
        status,
        body: body.into_bytes(),
      }),
      _ => Begin::InProgress,
    },
    _ => Begin::InProgress,
  }
}

fn decode_hex(text: &str) -> Option<[u8; 32]> {
  let mut out = [0u8; 32];
  if text.len() != 64 {
    return None;
  }
  for (i, byte) in out.iter_mut().enumerate() {
    *byte = u8::from_str_radix(text.get(2 * i..2 * i + 2)?, 16).ok()?;
  }
  Some(out)
}

async fn run<A: Send + 'static>(effect: Effect<A, SqlError>) -> Result<A, String> {
  match EffectExecutor::global().run_effect(effect).await {
    Exit::Success(a) => Ok(a),
    Exit::Failure(cause) => Err(format!("{cause:?}")),
  }
}

impl PgIdempotencyStore {
  fn begin_effect(&self, key: &IdempotencyKey, digest: String) -> Effect<Begin, SqlError> {
    let db = Arc::clone(&self.db);
    let (scope, tag, raw) = (scope_text(&key.scope), key.tag.clone(), key.key.clone());
    let expire = "DELETE FROM rpc_idempotency \
                  WHERE scope = $1 AND created_at < now() - make_interval(hours => ($2::int8)::int4)";
    let reserve = "INSERT INTO rpc_idempotency (scope, tag, key, digest) VALUES ($1, $2, $3, $4) \
                   ON CONFLICT DO NOTHING RETURNING digest, status, body";
    let stored = "SELECT digest, status, body FROM rpc_idempotency \
                  WHERE scope = $1 AND tag = $2 AND key = $3";
    let ttl = KEY_TTL_HOURS;
    db.execute(expire, &params![scope.as_str(), ttl])
      .flat_map({
        let (db, scope, tag, raw, digest) = (
          Arc::clone(&db),
          scope.clone(),
          tag.clone(),
          raw.clone(),
          digest.clone(),
        );
        move |_expired| {
          db::query_all::<Row>(
            &db,
            reserve,
            params![scope.as_str(), tag.as_str(), raw.as_str(), digest.as_str()],
          )
        }
      })
      .flat_map(move |reserved| {
        if !reserved.is_empty() {
          return id_effect::succeed(Begin::Fresh);
        }
        db::query_all::<Row>(
          &db,
          stored,
          params![scope.as_str(), tag.as_str(), raw.as_str()],
        )
        .map(move |rows| {
          // Gone between the two statements (abandoned): the caller retries.
          rows
            .into_iter()
            .next()
            .map_or(Begin::InProgress, |row| answer(row, &digest))
        })
      })
  }
}

impl IdempotencyStore for PgIdempotencyStore {
  fn begin<'a>(&'a self, key: &'a IdempotencyKey, digest: [u8; 32]) -> StoreFuture<'a, Begin> {
    let effect = self.begin_effect(key, hex(&digest));
    Box::pin(async move {
      run(effect).await.unwrap_or_else(|why| {
        eprintln!("ledger: idempotency key not reserved ({why}); answering in progress");
        Begin::InProgress
      })
    })
  }

  fn complete<'a>(&'a self, key: &'a IdempotencyKey, result: StoredResult) -> StoreFuture<'a, ()> {
    let sql = "UPDATE rpc_idempotency SET status = $4, body = $5 \
               WHERE scope = $1 AND tag = $2 AND key = $3";
    let body = String::from_utf8_lossy(&result.body).into_owned();
    let effect = self.db.execute(
      sql,
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
        eprintln!("ledger: idempotent result not stored ({why})");
      }
    })
  }

  fn abandon<'a>(&'a self, key: &'a IdempotencyKey) -> StoreFuture<'a, ()> {
    let sql = "DELETE FROM rpc_idempotency \
               WHERE scope = $1 AND tag = $2 AND key = $3 AND body IS NULL";
    let effect = self.db.execute(
      sql,
      &params![scope_text(&key.scope), key.tag.as_str(), key.key.as_str()],
    );
    Box::pin(async move {
      if let Err(why) = run(effect).await {
        eprintln!("ledger: idempotency reservation not released ({why})");
      }
    })
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn digests_round_trip_through_hex() {
    let digest: [u8; 32] = std::array::from_fn(|i| u8::try_from(i * 7).unwrap());
    assert_eq!(decode_hex(&hex(&digest)), Some(digest));
    assert_eq!(decode_hex("00"), None);
    assert_eq!(decode_hex(&"zz".repeat(32)), None);
  }

  #[test]
  fn stored_rows_answer_by_digest_and_result() {
    let digest = hex(&[1; 32]);
    let row = |d: &str, status: Option<i64>, body: Option<&str>| Row {
      digest: d.to_owned(),
      status,
      body: body.map(str::to_owned),
    };
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
      scope_text(&IdempotencyScope::Principal("acme/ada".into())),
      "principal:acme/ada"
    );
  }
}
